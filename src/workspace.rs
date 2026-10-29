use super::*;
use std::{fs, io::Write};

#[derive(Serialize, Deserialize)]
pub(crate) struct Record {
    schema_version: u32,
    pub(crate) name: String,
    branch: String,
    pub(crate) base_commit: String,
    pub(crate) repository: String,
    pub(crate) path: String,
    pub(crate) state: String,
}
#[derive(Serialize)]
struct Entry {
    #[serde(flatten)]
    record: Record,
    exists: bool,
    dirty: Option<bool>,
}
#[derive(Serialize)]
struct Listing {
    schema_version: u32,
    workspaces: Vec<Entry>,
}
fn metadata() -> Result<PathBuf> {
    Ok(git_path("--git-common-dir")?.join("weave"))
}
fn validate(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!(
            "Task name must contain only letters, numbers, hyphens or underscores (maximum 80 characters)"
        );
    }
    Ok(())
}
pub fn create(name: &str, from: &str) -> Result<()> {
    validate(name)?;
    let common = git_path("--git-common-dir")?;
    let reference = format!("{from}^{{commit}}");
    let commit = String::from_utf8(
        git(["rev-parse", "--verify", "--end-of-options", &reference])
            .context("Cannot resolve the requested base ref to a commit")?,
    )?
    .trim()
    .to_string();
    let branch = format!("weave/{name}");
    let root = match std::env::var_os("WEAVE_WORKSPACE_DIR") {
        Some(p) => PathBuf::from(p),
        None => common.join("weave-workspaces"),
    };
    fs::create_dir_all(&root)
        .with_context(|| format!("Cannot create workspace root {}", root.display()))?;
    let root = root.canonicalize()?;
    let path = root.join(name);
    if path.symlink_metadata().is_ok() {
        bail!("Destination already exists: {}", path.display());
    }
    let path_string = path
        .to_str()
        .context("Workspace path must be valid UTF-8")?
        .to_owned();
    let repo_string = common
        .to_str()
        .context("Git common directory must be valid UTF-8")?
        .to_owned();
    let meta = metadata()?;
    fs::create_dir_all(&meta)?;
    let record_path = meta.join(format!("{name}.json"));
    let mut record = Record {
        schema_version: 1,
        name: name.to_owned(),
        branch: branch.clone(),
        base_commit: commit.clone(),
        repository: repo_string,
        path: path_string,
        state: "pending".into(),
    };
    let mut file = tempfile::NamedTempFile::new_in(&meta)?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&record_path).with_context(|| {
        format!(
            "Task already exists or metadata cannot be created: {}",
            record_path.display()
        )
    })?;
    let args: Vec<&OsStr> = vec![
        OsStr::new("-c"),
        OsStr::new("core.hooksPath=/dev/null"),
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("-b"),
        OsStr::new(&branch),
        path.as_os_str(),
        OsStr::new(&commit),
    ];
    if let Err(e) = git(args) {
        bail!(
            "Worktree creation failed: {e:#}. A pending record remains at {}. Inspect the destination and branch before manually removing the record; no files were deleted.",
            record_path.display()
        );
    }
    record.state = "ready".into();
    let tmp = meta.join(format!(".{name}.tmp-{}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    serde_json::to_writer(&mut f, &record)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &record_path).context("Worktree was created but metadata finalization failed; inspect the pending record and worktree")?;
    println!("{}", path.display());
    Ok(())
}
pub fn list(json: bool) -> Result<()> {
    let common = git_path("--git-common-dir")?;
    let output = git(["worktree", "list", "--porcelain", "-z"])?;
    let paths: Vec<PathBuf> = output
        .split(|b| *b == 0)
        .filter_map(|r| r.strip_prefix(b"worktree "))
        .map(|p| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                PathBuf::from(OsStr::from_bytes(p))
            }
            #[cfg(not(unix))]
            {
                PathBuf::from(String::from_utf8_lossy(p).into_owned())
            }
        })
        .collect();
    let meta = common.join("weave");
    let mut entries = Vec::new();
    if meta.exists() {
        for file in fs::read_dir(meta)? {
            let path = file?.path();
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }
            let record: Record = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("Invalid metadata: {}", path.display()))?;
            if record.schema_version != 1 {
                bail!("Unsupported metadata version in {}", path.display());
            }
            let p = PathBuf::from(&record.path);
            let exists = p.is_dir() && paths.iter().any(|x| x == &p);
            let dirty = if exists {
                let out = Command::new("git")
                    .args(["-c", "core.fsmonitor=false", "-C"])
                    .arg(&p)
                    .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
                    .env("GIT_OPTIONAL_LOCKS", "0")
                    .output()?;
                if !out.status.success() {
                    bail!(
                        "Cannot inspect {}: {}",
                        p.display(),
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                Some(!out.stdout.is_empty())
            } else {
                None
            };
            entries.push(Entry {
                record,
                exists,
                dirty,
            });
        }
    }
    entries.sort_by(|a, b| a.record.name.cmp(&b.record.name));
    let listing = Listing {
        schema_version: 1,
        workspaces: entries,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
    } else if listing.workspaces.is_empty() {
        println!("No managed workspaces.");
    } else {
        for e in listing.workspaces {
            println!(
                "{}  {}  {}\n  base: {}  {}  {}",
                e.record.name,
                e.record.branch,
                e.record.path,
                e.record.base_commit,
                if e.exists {
                    if e.dirty == Some(true) {
                        "dirty"
                    } else {
                        "clean"
                    }
                } else {
                    "missing"
                },
                e.record.state
            );
        }
    }
    Ok(())
}

pub(crate) fn get(name: &str) -> Result<Record> {
    validate(name)?;
    let path = metadata()?.join(format!("{name}.json"));
    let record: Record = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("Workspace {name} was not found"))?,
    )?;
    if record.schema_version != 1 || record.name != name || record.state != "ready" {
        bail!("Workspace metadata is invalid or creation is incomplete: {name}");
    }
    if Path::new(&record.repository) != git_path("--git-common-dir")? {
        bail!("Workspace metadata belongs to another repository");
    }
    Ok(record)
}

pub(crate) fn names() -> Result<Vec<String>> {
    let path = metadata()?;
    let mut result = Vec::new();
    if path.exists() {
        for f in fs::read_dir(path)? {
            let path = f?.path();
            if path.extension() == Some(OsStr::new("json")) {
                let r: Record = serde_json::from_slice(&fs::read(path)?)?;
                if r.schema_version != 1 {
                    bail!("Unsupported workspace metadata version");
                }
                if r.state == "ready" {
                    result.push(r.name);
                }
            }
        }
    }
    result.sort();
    Ok(result)
}
