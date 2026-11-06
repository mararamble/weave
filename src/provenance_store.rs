//! Durable, bounded local evidence storage. Hash chains detect inconsistency,
//! not a malicious local owner rewriting the entire store and its head.
use crate::provenance::{Authorship, Event};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const FILE_LIMIT: usize = 1024 * 1024;
pub const SNAPSHOT_LIMIT: usize = 8 * FILE_LIMIT;
pub const STORE_LIMIT: u64 = 256 * 1024 * 1024;
const RECORD_LIMIT: usize = 16 * FILE_LIMIT;
const RECORD_COUNT: u64 = 10_000;

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileState {
    pub hash: String,
    pub mode: String,
}
pub type Files = BTreeMap<String, FileState>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub stream_id: String,
    pub worktree_id: String,
    pub workspace: String,
    pub repository: String,
    pub worktree: String,
    pub base_commit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub key: String,
    pub request_hash: String,
    pub origin: Origin,
    pub baseline: Files,
    pub files: Files,
    pub events: Vec<Event>,
    pub excluded: Vec<String>,
    pub snapshot_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    Checkpoint(Checkpoint),
    Correction {
        event_id: String,
        assertion: Authorship,
        reason: String,
    },
    Prune {
        orphan_blobs: Vec<String>,
        incomplete_files: Vec<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
    pub sequence: u64,
    pub previous_hash: Option<String>,
    pub entry: Entry,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub sequence: u64,
    pub hash: Option<String>,
}

pub struct History {
    pub records: Vec<Record>,
    pub head: Head,
}
pub struct Store {
    root: PathBuf,
    _lock: File,
    writable: bool,
}

fn directory(path: &Path, create: bool) -> Result<()> {
    if create {
        match fs::create_dir(path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                }
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Unsafe provenance directory"
    );
    Ok(())
}

fn open(path: &Path, write: bool, create: bool) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(write).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    let file = o.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Evidence must be a regular file"
    );
    Ok(file)
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut file = open(path, false, false)?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "Evidence size limit exceeded"
    );
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "Evidence size limit exceeded");
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn publish(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().context("Missing evidence parent")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".pending-")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if replace {
        ensure!(
            fs::symlink_metadata(path)?.is_file(),
            "Unsafe evidence head"
        );
        temp.persist(path).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(path).map_err(|e| e.error)?;
    }
    sync_directory(parent)
}

impl Store {
    /// `root` must be below a trusted, canonical repository metadata directory.
    /// Readers never create files or lock metadata; a shared OS lock prevents
    /// observing publication/pruning in progress. Locks die with their process.
    pub fn open(root: &Path, write: bool) -> Result<Self> {
        ensure!(
            cfg!(any(target_os = "macos", target_os = "linux")),
            "Provenance storage supports macOS and Linux only"
        );
        directory(root, write)?;
        let lock = open(&root.join("lock"), write, write)?;
        if write {
            lock.lock()?;
        } else {
            lock.lock_shared()?;
        }
        for name in ["records", "blobs"] {
            directory(&root.join(name), write)?;
        }
        if write && !root.join("head.json").exists() {
            publish(
                &root.join("head.json"),
                &serde_json::to_vec(&Head::default())?,
                false,
            )?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            _lock: lock,
            writable: write,
        })
    }
    pub fn history(&self) -> Result<History> {
        let head: Head = serde_json::from_slice(&read(&self.root.join("head.json"), 4096)?)?;
        ensure!(
            head.sequence <= RECORD_COUNT,
            "Journal record limit exceeded"
        );
        let mut records = Vec::new();
        let mut previous = None;
        let mut journal_bytes = 0usize;
        for seq in 1..=head.sequence {
            let bytes = read(&self.record_path(seq), RECORD_LIMIT)
                .context("Missing or incomplete journal record")?;
            journal_bytes = journal_bytes
                .checked_add(bytes.len())
                .context("Journal size overflow")?;
            ensure!(
                journal_bytes as u64 <= STORE_LIMIT,
                "Journal size limit exceeded"
            );
            let record: Record =
                serde_json::from_slice(&bytes).context("Corrupt journal record")?;
            ensure!(
                record.schema_version == 1
                    && record.sequence == seq
                    && record.previous_hash == previous,
                "Invalid journal chain or version"
            );
            previous = Some(digest(&bytes));
            records.push(record);
        }
        ensure!(previous == head.hash, "Journal head mismatch");
        // Extra published records are a recoverable interrupted head publication,
        // never silently adopted or discarded. Temporary files are not records.
        for item in fs::read_dir(self.root.join("records"))? {
            let name = item?.file_name();
            let name = name.to_str().context("Invalid record filename")?;
            if name.starts_with(".pending-") {
                continue;
            }
            let seq = name
                .strip_suffix(".json")
                .and_then(|v| v.parse::<u64>().ok())
                .context("Unexpected journal entry")?;
            ensure!(
                seq > 0 && seq <= head.sequence && name == format!("{seq:020}.json"),
                "Incomplete publication: run provenance recover explicitly"
            );
        }
        Ok(History { records, head })
    }
    fn record_path(&self, seq: u64) -> PathBuf {
        self.root.join("records").join(format!("{seq:020}.json"))
    }
    pub fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        ensure!(valid_hash(hash), "Invalid blob hash");
        let bytes = read(&self.root.join("blobs").join(hash), FILE_LIMIT)?;
        ensure!(digest(&bytes) == hash, "Blob hash mismatch");
        Ok(bytes)
    }
    pub(crate) fn validate_checkpoint(&self, cp: &Checkpoint) -> Result<()> {
        ensure!(
            valid_hash(&cp.request_hash) && !cp.key.is_empty() && cp.key.len() <= 256,
            "Invalid checkpoint identity"
        );
        ensure!(
            cp.snapshot_hash == digest(&serde_json::to_vec(&cp.files)?),
            "Invalid snapshot hash"
        );
        for files in [&cp.baseline, &cp.files] {
            let mut total = 0usize;
            ensure!(files.len() <= 10_000, "Checkpoint path count exceeded");
            for (path, state) in files {
                let raw: crate::provenance::RawPath =
                    path.clone().try_into().map_err(anyhow::Error::msg)?;
                ensure!(
                    raw.as_bytes().first() != Some(&b'/')
                        && !raw
                            .as_bytes()
                            .split(|b| *b == b'/')
                            .any(|p| p.is_empty() || p == b"." || p == b".."),
                    "Unsafe evidence path"
                );
                ensure!(
                    matches!(state.mode.as_str(), "100644" | "100755"),
                    "Unsupported evidence mode"
                );
                total = total
                    .checked_add(self.blob(&state.hash)?.len())
                    .context("Snapshot size overflow")?;
                ensure!(total <= SNAPSHOT_LIMIT, "Checkpoint size limit exceeded");
            }
        }
        Ok(())
    }
    /// Optional tool-write producers submit structurally checked schema-v1 events
    /// and blob payloads here. This records claims, never authenticates an actor.
    /// Retrying the identical checkpoint is idempotent under the writer lock.
    pub fn append_checkpoint(
        &self,
        cp: Checkpoint,
        blobs: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(Head, bool)> {
        let history = self.history()?;
        if let Some(existing) = history.records.iter().find_map(|r| match &r.entry {
            Entry::Checkpoint(c)
                if c.origin.stream_id == cp.origin.stream_id && c.key == cp.key =>
            {
                Some(c)
            }
            _ => None,
        }) {
            ensure!(
                serde_json::to_vec(existing)? == serde_json::to_vec(&cp)?,
                "Idempotency key reused with different evidence"
            );
            return Ok((history.head, true));
        }
        let head = self.append(Entry::Checkpoint(cp), blobs, &history.head)?;
        Ok((head, false))
    }
    fn usage(&self) -> Result<u64> {
        let mut size = 0u64;
        for dir in ["records", "blobs"] {
            for item in fs::read_dir(self.root.join(dir))? {
                let item = item?;
                let m = item.path().symlink_metadata()?;
                ensure!(
                    m.is_file() && !m.file_type().is_symlink(),
                    "Unsafe store entry"
                );
                size = size.checked_add(m.len()).context("Store size overflow")?;
            }
        }
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let m = item.path().symlink_metadata()?;
            if m.is_dir() {
                continue;
            }
            ensure!(
                m.is_file() && !m.file_type().is_symlink(),
                "Unsafe store metadata"
            );
            size = size.checked_add(m.len()).context("Store size overflow")?;
        }
        Ok(size)
    }
    pub fn append(
        &self,
        entry: Entry,
        blobs: &BTreeMap<String, Vec<u8>>,
        expected: &Head,
    ) -> Result<Head> {
        ensure!(self.writable, "Read-only evidence store");
        let history = self.history()?;
        ensure!(
            &history.head == expected,
            "Concurrent history change; recapture"
        );
        if let Entry::Checkpoint(cp) = &entry
            && let Some(previous) = history.records.iter().find_map(|r| match &r.entry {
                Entry::Checkpoint(c)
                    if c.origin.stream_id == cp.origin.stream_id && c.key == cp.key =>
                {
                    Some(c)
                }
                _ => None,
            })
        {
            ensure!(
                serde_json::to_vec(previous)? == serde_json::to_vec(cp)?,
                "Idempotency key reused with different evidence"
            );
            return Ok(history.head);
        }
        ensure!(
            expected.sequence < RECORD_COUNT,
            "Journal full; start a separately retained store epoch"
        );
        let record = Record {
            schema_version: 1,
            sequence: expected.sequence + 1,
            previous_hash: expected.hash.clone(),
            entry,
        };
        let bytes = serde_json::to_vec(&record)?;
        ensure!(bytes.len() <= RECORD_LIMIT, "Record size limit exceeded");
        let mut added = bytes.len() as u64;
        for (hash, data) in blobs {
            ensure!(
                data.len() <= FILE_LIMIT && valid_hash(hash) && digest(data) == *hash,
                "Invalid blob payload"
            );
            if !self.root.join("blobs").join(hash).exists() {
                added = added
                    .checked_add(data.len() as u64)
                    .context("Store size overflow")?;
            }
        }
        let ceiling = if matches!(record.entry, Entry::Prune { .. }) {
            STORE_LIMIT
        } else {
            STORE_LIMIT - RECORD_LIMIT as u64
        };
        ensure!(
            self.usage()?.saturating_add(added) <= ceiling,
            "Provenance store full; prune unreferenced blobs or retain a new store epoch"
        );
        for (hash, data) in blobs {
            let path = self.root.join("blobs").join(hash);
            if path.symlink_metadata().is_ok() {
                self.blob(hash)?;
            } else {
                publish(&path, data, false)?;
            }
        }
        if let Entry::Checkpoint(cp) = &record.entry {
            self.validate_checkpoint(cp)?;
        }
        publish(&self.record_path(record.sequence), &bytes, false)?;
        let head = Head {
            sequence: record.sequence,
            hash: Some(digest(&bytes)),
        };
        publish(
            &self.root.join("head.json"),
            &serde_json::to_vec(&head)?,
            true,
        )?;
        Ok(head)
    }
    /// Recover only an intact next record with the exact previous head. Corrupt
    /// or torn records are refused; this never rewrites or truncates history.
    pub fn recover(&self) -> Result<Head> {
        ensure!(self.writable, "Read-only evidence store");
        let path = self.root.join("head.json");
        let mut head: Head = serde_json::from_slice(&read(&path, 4096)?)?;
        ensure!(
            head.sequence < RECORD_COUNT,
            "Recovery record limit exceeded"
        );
        // Verify the committed prefix before examining the orphan.
        let mut previous = None;
        for seq in 1..=head.sequence {
            let bytes = read(&self.record_path(seq), RECORD_LIMIT)?;
            let r: Record = serde_json::from_slice(&bytes)?;
            if let Entry::Checkpoint(cp) = &r.entry {
                self.validate_checkpoint(cp)?;
            }
            ensure!(
                r.schema_version == 1 && r.sequence == seq && r.previous_hash == previous,
                "Corrupt committed history"
            );
            previous = Some(digest(&bytes));
        }
        ensure!(
            previous == head.hash && head.sequence < RECORD_COUNT,
            "Invalid recovery head"
        );
        let bytes = read(&self.record_path(head.sequence + 1), RECORD_LIMIT)
            .context("No complete pending record to recover")?;
        let record: Record = serde_json::from_slice(&bytes)?;
        ensure!(
            record.schema_version == 1
                && record.sequence == head.sequence + 1
                && record.previous_hash == head.hash,
            "Invalid recovery chain"
        );
        if let Entry::Checkpoint(cp) = &record.entry {
            self.validate_checkpoint(cp)?;
        }
        head = Head {
            sequence: record.sequence,
            hash: Some(digest(&bytes)),
        };
        publish(&path, &serde_json::to_vec(&head)?, true)?;
        self.history()?;
        Ok(head)
    }
    /// Only orphan blobs can be pruned. Every checkpoint, event and correction
    /// remains reachable and immutable; the receipt precedes deletion for retry.
    pub fn prune(&self, confirmed: bool) -> Result<Vec<String>> {
        ensure!(!confirmed || self.writable, "Read-only evidence store");
        let history = self.history()?;
        let mut live = HashSet::new();
        for r in &history.records {
            if let Entry::Checkpoint(cp) = &r.entry {
                for state in cp.baseline.values().chain(cp.files.values()) {
                    live.insert(state.hash.clone());
                }
                for e in &cp.events {
                    for state in [&e.data().before, &e.data().after] {
                        if let crate::provenance::ContentState::Hash { hash } = state {
                            live.insert(hash.digest.as_str().to_owned());
                        }
                    }
                }
            }
        }
        let mut orphan = Vec::new();
        let mut incomplete = Vec::new();
        for prefix in ["", "records/", "blobs/"] {
            for item in fs::read_dir(self.root.join(prefix))? {
                let item = item?;
                let name = item.file_name();
                let name = name.to_str().context("Invalid pending filename")?;
                if name.starts_with(".pending-") {
                    let m = item.path().symlink_metadata()?;
                    ensure!(
                        m.is_file() && !m.file_type().is_symlink(),
                        "Unsafe pending evidence"
                    );
                    incomplete.push(format!("{prefix}{name}"));
                }
            }
        }
        for item in fs::read_dir(self.root.join("blobs"))? {
            let name = item?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Invalid blob name"))?;
            if name.starts_with(".pending-") {
                continue;
            }
            ensure!(
                valid_hash(&name),
                "Incomplete blob publication; preserve pending evidence for inspection"
            );
            if !live.contains(&name) {
                self.blob(&name)?;
                orphan.push(name);
            }
        }
        orphan.sort();
        incomplete.sort();
        if confirmed && (!orphan.is_empty() || !incomplete.is_empty()) {
            self.append(
                Entry::Prune {
                    orphan_blobs: orphan.clone(),
                    incomplete_files: incomplete.clone(),
                },
                &BTreeMap::new(),
                &history.head,
            )?;
            for hash in &orphan {
                fs::remove_file(self.root.join("blobs").join(hash))?;
            }
            for name in &incomplete {
                fs::remove_file(self.root.join(name))?;
            }
            sync_directory(&self.root.join("blobs"))?;
            sync_directory(&self.root.join("records"))?;
            sync_directory(&self.root)?;
        }
        orphan.extend(incomplete);
        Ok(orphan)
    }
}
