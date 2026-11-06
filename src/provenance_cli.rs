//! Opt-in, filtered checkpoint capture. This never runs tool commands or writes
//! source content. External tool integrations use the library adapter boundary.
use crate::{changeset, git_process, workspace};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use weave::{
    provenance::{
        Authorship, ContentHash, ContentState, Event, EventData, Evidence, Id, MissingReason,
        ObservationSource, Operation,
    },
    provenance_replay::{self as replay, Report},
    provenance_store::{self as storage, Checkpoint, Entry, FileState, Files, Origin, Store},
};

#[derive(Args)]
pub(crate) struct Options {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Capture a filtered worktree checkpoint (explicit opt-in; no agent inferred)
    Checkpoint {
        name: String,
        /// Stable retry key; reusing a key with different content is refused
        #[arg(long)]
        key: String,
        #[arg(long)]
        include_untracked_content: bool,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        owner_agent: Option<String>,
        /// Manual assertion only; never verified authorship
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Replay recorded evidence without modifying source or provenance storage
    Replay {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Append a manual/unknown correction; original events remain intact
    Correct {
        event: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        reason: String,
    },
    /// List orphan blobs; --yes records a receipt and removes only unreferenced blobs
    Prune {
        #[arg(long)]
        yes: bool,
    },
    /// Explicitly finish an intact interrupted journal-head publication
    Recover {
        #[arg(long)]
        yes: bool,
    },
}

fn git(path: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
    let out =
        git_process::bounded_output(git_process::command_at(path).args(args), 16 * 1024 * 1024)?;
    ensure!(
        out.status.success(),
        "Provenance Git operation failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(out.stdout)
}
fn g(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git(
        path,
        &args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>(),
    )
}
fn text(path: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(g(path, args)?)?.trim().into())
}
pub(crate) fn root() -> Result<PathBuf> {
    let parent = crate::git_path("--git-common-dir")?.join("weave");
    let metadata = fs::symlink_metadata(&parent).context("Missing provenance metadata parent")?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink() && parent.canonicalize()? == parent,
        "Unsafe provenance metadata parent; symlinks or redirected paths are refused"
    );
    Ok(parent.join("provenance"))
}
fn id(s: String) -> Result<Id> {
    s.try_into().map_err(anyhow::Error::msg)
}
fn known(s: String) -> Result<Evidence<Id>> {
    Ok(Evidence::Known { value: id(s)? })
}
fn optional(s: Option<String>) -> Result<Evidence<Id>> {
    s.map(known).unwrap_or(Ok(Evidence::Missing {
        reason: MissingReason::NotObserved,
    }))
}

pub(crate) fn origin(name: &str) -> Result<Origin> {
    let record = workspace::get(name)?;
    let path = Path::new(&record.path).canonicalize()?;
    let common = crate::path_from_bytes(&g(
        &path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .canonicalize()?;
    ensure!(
        common == Path::new(&record.repository).canonicalize()?,
        "Workspace repository mismatch"
    );
    let gitdir =
        crate::path_from_bytes(&g(&path, &["rev-parse", "--absolute-git-dir"])?).canonicalize()?;
    let mut identity = gitdir.as_os_str().as_encoded_bytes().to_vec();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = gitdir.metadata()?;
        identity.extend_from_slice(&m.dev().to_le_bytes());
        identity.extend_from_slice(&m.ino().to_le_bytes());
    }
    // Separate worktree identity from task and author; include base in stream
    // identity so a changed baseline cannot silently extend an old chain.
    let worktree_id = storage::digest(&identity);
    identity.extend_from_slice(record.base_commit.as_bytes());
    Ok(Origin {
        stream_id: storage::digest(&identity),
        worktree_id,
        workspace: name.into(),
        repository: common
            .to_str()
            .context("Metadata repository path requires UTF-8")?
            .into(),
        worktree: path
            .to_str()
            .context("Metadata worktree path requires UTF-8")?
            .into(),
        base_commit: record.base_commit,
    })
}

pub(crate) fn load(name: &str) -> Result<Option<Report>> {
    let root = root()?;
    if !root.try_exists()? {
        return Ok(None);
    }
    let store = Store::open(&root, false)?;
    let origin = origin(name)?;
    if !store
        .history()?
        .records
        .iter()
        .any(|r| matches!(&r.entry,Entry::Checkpoint(c) if c.origin.stream_id==origin.stream_id))
    {
        return Ok(None);
    }
    let report = replay::replay(&store, &origin.stream_id, None)?;
    ensure!(
        report.origin == origin,
        "Recorded origin no longer matches workspace"
    );
    Ok(Some(report))
}

fn excluded(root: &Path, raw: &[u8]) -> Result<bool> {
    if changeset::sensitive(raw) {
        return Ok(true);
    }
    let mut c = git_process::command_at(root);
    c.args(["check-ignore", "--no-index", "-q", "--"])
        .arg(raw_path(raw)?);
    let out = git_process::bounded_output(&mut c, 65536)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("Cannot inspect provenance privacy exclusions"),
    }
}
fn raw_path(raw: &[u8]) -> Result<PathBuf> {
    let path: weave::provenance::RawPath = hex(raw).try_into().map_err(anyhow::Error::msg)?;
    ensure!(
        !path
            .as_bytes()
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
            && raw.first() != Some(&b'/'),
        "Unsafe raw repository path"
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(raw)))
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(std::str::from_utf8(raw)?))
    }
}
fn hex(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}
fn add(bytes: Vec<u8>, blobs: &mut BTreeMap<String, Vec<u8>>, total: &mut usize) -> Result<String> {
    ensure!(
        bytes.len() <= storage::FILE_LIMIT,
        "Checkpoint file exceeds 1 MiB"
    );
    *total = total
        .checked_add(bytes.len())
        .context("Capture size overflow")?;
    ensure!(
        *total <= storage::SNAPSHOT_LIMIT,
        "Checkpoint exceeds 8 MiB"
    );
    let hash = storage::digest(&bytes);
    blobs.insert(hash.clone(), bytes);
    Ok(hash)
}

struct Capture {
    baseline: Files,
    files: Files,
    blobs: BTreeMap<String, Vec<u8>>,
    excluded: Vec<String>,
}
fn capture(origin: &Origin, untracked: bool) -> Result<Capture> {
    let root = Path::new(&origin.worktree);
    git_process::ensure_no_filters(root)?;
    let mut result = Capture {
        baseline: BTreeMap::new(),
        files: BTreeMap::new(),
        blobs: BTreeMap::new(),
        excluded: Vec::new(),
    };
    let tree = g(
        root,
        &["ls-tree", "-rz", "--full-tree", &origin.base_commit],
    )?;
    let mut paths = BTreeSet::new();
    let mut total = 0;
    for item in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let (header, raw) = item
            .split_once_byte(b'\t')
            .context("Malformed baseline tree")?;
        if excluded(root, raw)? {
            result.excluded.push(hex(raw));
            continue;
        }
        let header = std::str::from_utf8(header)?;
        let fields = header.split(' ').collect::<Vec<_>>();
        ensure!(
            fields.len() == 3 && fields[1] == "blob" && matches!(fields[0], "100644" | "100755"),
            "Unsupported checkpoint baseline file type"
        );
        let size = text(root, &["cat-file", "-s", fields[2]])?.parse::<usize>()?;
        ensure!(
            size <= storage::FILE_LIMIT,
            "Checkpoint baseline file exceeds 1 MiB"
        );
        let bytes = g(root, &["cat-file", "blob", fields[2]])?;
        let hash = add(bytes, &mut result.blobs, &mut total)?;
        result.baseline.insert(
            hex(raw),
            FileState {
                hash,
                mode: fields[0].into(),
            },
        );
        paths.insert(raw.to_vec());
    }
    let tracked = g(root, &["ls-files", "--cached", "-z"])?;
    for raw in changeset::parse_path_list(&tracked)? {
        paths.insert(raw.to_vec());
    }
    let others = g(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for raw in changeset::parse_path_list(&others)? {
        if excluded(root, raw)? {
            result.excluded.push(hex(raw));
            continue;
        }
        ensure!(
            untracked,
            "Untracked provenance capture requires --include-untracked-content"
        );
        paths.insert(raw.to_vec());
    }
    ensure!(paths.len() <= 10_000, "Checkpoint path count exceeded");
    total = 0;
    for raw in paths {
        if excluded(root, &raw)? {
            result.excluded.push(hex(&raw));
            continue;
        }
        raw_path(&raw)?;
        let file = changeset::open_untracked(root, &raw);
        let mut file = match file {
            Ok(f) => f,
            Err(e) => {
                if root
                    .join(raw_path(&raw)?)
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    continue;
                }
                return Err(e).context("Unsafe checkpoint file");
            }
        };
        let before = file.metadata()?;
        ensure!(
            before.is_file() && before.len() <= storage::FILE_LIMIT as u64,
            "Unsupported or oversized checkpoint file"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take(storage::FILE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        ensure!(
            before.len() == after.len()
                && before.modified()? == after.modified()?
                && bytes.len() as u64 == after.len(),
            "Inconsistent checkpoint capture"
        );
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            if after.permissions().mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            }
        };
        #[cfg(not(unix))]
        let mode = "100644";
        let hash = add(bytes, &mut result.blobs, &mut total)?;
        result.files.insert(
            hex(&raw),
            FileState {
                hash,
                mode: mode.into(),
            },
        );
    }
    result.excluded.sort();
    result.excluded.dedup();
    Ok(result)
}

trait SplitByte {
    fn split_once_byte(&self, b: u8) -> Option<(&[u8], &[u8])>;
}
impl SplitByte for [u8] {
    fn split_once_byte(&self, b: u8) -> Option<(&[u8], &[u8])> {
        self.iter()
            .position(|x| *x == b)
            .map(|i| (&self[..i], &self[i + 1..]))
    }
}
fn state(file: Option<&FileState>) -> Result<ContentState> {
    Ok(match file {
        Some(f) => ContentState::Hash {
            hash: ContentHash {
                algorithm: id("sha256".into())?,
                digest: id(f.hash.clone())?,
            },
        },
        None => ContentState::Absent {},
    })
}

struct CaptureOptions {
    key: String,
    untracked: bool,
    task: Option<String>,
    owner: Option<String>,
    agent: Option<String>,
    session: Option<String>,
}
fn checkpoint(name: &str, options: CaptureOptions) -> Result<serde_json::Value> {
    let CaptureOptions {
        key,
        untracked,
        task,
        owner,
        agent,
        session,
    } = options;
    ensure!(
        !key.is_empty() && key.len() <= 256,
        "Invalid checkpoint key"
    );
    let origin = origin(name)?;
    let path = root()?;
    let store = Store::open(&path, true)?;
    let history = store.history()?;
    let source = Path::new(&origin.worktree);
    let gitdir = crate::path_from_bytes(&g(source, &["rev-parse", "--absolute-git-dir"])?);
    let before_index = crate::integration::read_index(&gitdir.join("index"))?;
    let before_head = text(source, &["rev-parse", "HEAD"])?;
    let first = capture(&origin, untracked)?;
    let second = capture(&origin, untracked)?;
    ensure!(
        first.files == second.files
            && first.baseline == second.baseline
            && first.excluded == second.excluded
            && before_index == crate::integration::read_index(&gitdir.join("index"))?
            && before_head == text(source, &["rev-parse", "HEAD"])?
            && origin == self::origin(name)?,
        "Inconsistent checkpoint capture; retry"
    );
    let snapshot_hash = storage::digest(&serde_json::to_vec(&first.files)?);
    let checkpoints = history
        .records
        .iter()
        .filter_map(|r| match &r.entry {
            Entry::Checkpoint(c) if c.origin.stream_id == origin.stream_id => Some(c),
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some(first_checkpoint) = checkpoints.first() {
        ensure!(
            first_checkpoint.baseline == first.baseline,
            "Checkpoint baseline/privacy selection changed; retain this history and start a new worktree/base stream"
        );
    }
    let task = optional(task)?;
    let owner = optional(owner)?;
    let authorship = if let Some(agent) = agent {
        Authorship::Manual {
            claimed_agent_id: known(agent)?,
            claimed_session_id: optional(session)?,
            assertion_ref: id(key.clone())?,
        }
    } else {
        ensure!(
            session.is_none(),
            "Session requires a manual agent assertion"
        );
        Authorship::WorktreeOnly {}
    };
    let request_hash = storage::digest(&serde_json::to_vec(&(
        &task,
        &owner,
        &authorship,
        untracked,
    ))?);
    if let Some(previous) = checkpoints.iter().find(|cp| cp.key == key) {
        ensure!(
            previous.request_hash == request_hash
                && previous.files == first.files
                && previous.baseline == first.baseline
                && previous.excluded == first.excluded
                && previous.events.iter().all(|e| e.data().task_id == task
                    && e.data().task_owner_agent_id == owner
                    && e.data().authorship == authorship),
            "Idempotency key reused with different evidence"
        );
        return Ok(
            serde_json::json!({"schema_version":1,"checkpoint":key,"snapshot_hash":snapshot_hash,"duplicate":true,"stream_id":origin.stream_id}),
        );
    }
    let previous = checkpoints
        .last()
        .map(|c| &c.files)
        .unwrap_or(&first.baseline);
    let mut sequence = checkpoints
        .iter()
        .map(|c| c.events.len() as u64)
        .sum::<u64>();
    let mut events = Vec::new();
    for path in previous
        .keys()
        .chain(first.files.keys())
        .collect::<BTreeSet<_>>()
    {
        if previous.get(path) == first.files.get(path) {
            continue;
        }
        let before = state(previous.get(path))?;
        let after = state(first.files.get(path))?;
        let operation = if !previous.contains_key(path) {
            Operation::Create
        } else if !first.files.contains_key(path) {
            Operation::Delete
        } else {
            Operation::Modify
        };
        let event_id = storage::digest(&serde_json::to_vec(&(&origin.stream_id, &key, path))?);
        events.push(
            Event::new(EventData {
                event_id: id(event_id)?,
                stream_id: id(origin.stream_id.clone())?,
                sequence,
                worktree_id: known(origin.worktree_id.clone())?,
                task_id: task.clone(),
                task_owner_agent_id: owner.clone(),
                base_commit: known(origin.base_commit.clone())?,
                path_hex: path.clone().try_into().map_err(anyhow::Error::msg)?,
                previous_path_hex: Evidence::Missing {
                    reason: MissingReason::NotApplicable,
                },
                operation,
                before,
                after,
                snapshot_ref: known(snapshot_hash.clone())?,
                patch_ref: Evidence::Missing {
                    reason: MissingReason::Unavailable,
                },
                source: ObservationSource::ExplicitCheckpoint {
                    checkpoint_id: id(key.clone())?,
                },
                timestamp: Evidence::Missing {
                    reason: MissingReason::NotObserved,
                },
                authorship: authorship.clone(),
            })
            .map_err(anyhow::Error::msg)?,
        );
        sequence += 1;
    }
    store.append(
        Entry::Checkpoint(Checkpoint {
            key: key.clone(),
            request_hash,
            origin: origin.clone(),
            baseline: first.baseline,
            files: first.files,
            events,
            excluded: first.excluded,
            snapshot_hash: snapshot_hash.clone(),
        }),
        &first.blobs,
        &history.head,
    )?;
    Ok(
        serde_json::json!({"schema_version":1,"checkpoint":key,"snapshot_hash":snapshot_hash,"duplicate":false,"stream_id":origin.stream_id}),
    )
}

pub(crate) fn run(options: Options) -> Result<()> {
    match options.command {
        Command::Checkpoint {
            name,
            key,
            include_untracked_content,
            task,
            owner_agent,
            agent,
            session,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&checkpoint(
                &name,
                CaptureOptions {
                    key,
                    untracked: include_untracked_content,
                    task,
                    owner: owner_agent,
                    agent,
                    session
                }
            )?)?
        ),
        Command::Replay { name, json } => {
            let report = load(&name)?
                .context("No provenance checkpoints; ordinary Git workflows remain available")?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                humanln!(
                    "Stream {}: {} events; {} paths; {}",
                    report.origin.stream_id,
                    report.timeline.len(),
                    report.paths.len(),
                    report.snapshot_hash
                );
                for (path, a) in report.paths {
                    humanln!("{} {:?}: {}", path, a.source, a.diagnostics.join(", "));
                }
            }
        }
        Command::Correct {
            event,
            agent,
            reason,
        } => {
            ensure!(
                !reason.trim().is_empty() && reason.len() <= 4096,
                "Correction requires a bounded reason"
            );
            let store = Store::open(&root()?, true)?;
            let h = store.history()?;
            ensure!(h.records.iter().any(|r|matches!(&r.entry,Entry::Checkpoint(cp) if cp.events.iter().any(|e|e.data().event_id.as_str()==event))),"Unknown event");
            let assertion = if let Some(agent) = agent {
                Authorship::Manual {
                    claimed_agent_id: known(agent)?,
                    claimed_session_id: optional(None)?,
                    assertion_ref: id(storage::digest(reason.as_bytes()))?,
                }
            } else {
                Authorship::Unknown {
                    reason: MissingReason::Unavailable,
                }
            };
            store.append(
                Entry::Correction {
                    event_id: event,
                    assertion,
                    reason,
                },
                &BTreeMap::new(),
                &h.head,
            )?;
            humanln!("Correction appended; original evidence preserved");
        }
        Command::Prune { yes } => {
            let store = Store::open(&root()?, yes)?;
            let orphan = store.prune(yes)?;
            humanln!(
                "{} orphan blobs {}",
                orphan.len(),
                if yes {
                    "pruned with receipt"
                } else {
                    "eligible; --yes confirms pruning"
                }
            );
        }
        Command::Recover { yes } => {
            ensure!(
                yes,
                "Recovery changes provenance metadata; pass --yes to confirm"
            );
            let store = Store::open(&root()?, true)?;
            store.recover()?;
            humanln!("Recovered intact journal publication");
        }
    }
    Ok(())
}
