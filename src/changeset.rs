//! Typed, byte-preserving file changes for future changeset collection.
use anyhow::{Result, bail};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub(crate) kind: ChangeKind,
    pub(crate) path: Vec<u8>,
    pub(crate) previous_path: Option<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
}

/// Parse `git diff --name-status -z` without interpreting file names as UTF-8.
pub(crate) fn parse_name_status(input: &[u8]) -> Result<Vec<FileChange>> {
    if !input.is_empty() && !input.ends_with(&[0]) {
        bail!("Truncated Git name-status output");
    }
    let mut fields = input.split(|b| *b == 0);
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        if status.is_empty() {
            if fields.next().is_some() {
                bail!("Empty Git name-status record");
            }
            break;
        }
        let code = status[0];
        let kind = match code {
            b'A' => ChangeKind::Added,
            b'M' => ChangeKind::Modified,
            b'D' => ChangeKind::Deleted,
            b'R' => ChangeKind::Renamed,
            b'C' => ChangeKind::Copied,
            b'T' => ChangeKind::TypeChanged,
            b'U' => ChangeKind::Unmerged,
            _ => bail!("Unknown Git name-status code"),
        };
        let suffix = &status[1..];
        if (!suffix.is_empty() && !suffix.iter().all(u8::is_ascii_digit))
            || (!matches!(code, b'R' | b'C' | b'M') && !suffix.is_empty())
        {
            bail!("Invalid Git name-status score");
        }
        let first = fields
            .next()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing Git change path"))?
            .to_vec();
        let (path, previous_path) = if matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) {
            let dest = fields
                .next()
                .filter(|p| !p.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Missing Git rename destination"))?
                .to_vec();
            (dest, Some(first))
        } else {
            (first, None)
        };
        changes.push(FileChange {
            kind,
            path,
            previous_path,
        });
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_nul_delimited_paths_and_renames() {
        let changes = parse_name_status(b"M\0line\nname\0R100\0old\0new\xff\0").unwrap();
        assert_eq!(changes[0].path, b"line\nname");
        assert_eq!(changes[1].path, b"new\xff");
        assert_eq!(changes[1].previous_path.as_deref(), Some(&b"old"[..]));
        assert_eq!(changes[1].kind, ChangeKind::Renamed);
    }
    #[test]
    fn rejects_malformed_output() {
        for bytes in [
            &b"M\0file"[..],
            &b"R100\0old\0"[..],
            &b"X\0file\0"[..],
            &b"M\0\0"[..],
        ] {
            assert!(parse_name_status(bytes).is_err());
        }
    }
}

use crate::workspace;
use anyhow::Context;
use serde::Serialize;
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Clone, Serialize)]
pub(crate) struct Change {
    pub(crate) path: String,
    pub(crate) path_hex: String,
    pub(crate) previous_path: Option<String>,
    pub(crate) previous_path_hex: Option<String>,
    pub(crate) kind: &'static str,
    added: Option<u64>,
    deleted: Option<u64>,
    pub(crate) binary: Option<bool>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    content_hex: Option<String>,
    pub(crate) incomplete: Option<String>,
}
#[derive(Clone, Serialize)]
pub(crate) struct Changeset {
    schema_version: u32,
    workspace_id: String,
    pub(crate) name: String,
    repository: String,
    pub(crate) base_commit: String,
    head_commit: String,
    committed_changes: Vec<Change>,
    staged_changes: Vec<Change>,
    unstaged_changes: Vec<Change>,
    untracked_changes: Vec<Change>,
    pub(crate) aggregate_changes: Vec<Change>,
    warnings: Vec<String>,
}
fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}
fn convert(c: FileChange) -> Change {
    let kind = match c.kind {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Renamed => "renamed",
        ChangeKind::Copied => "copied",
        ChangeKind::TypeChanged => "type_changed",
        ChangeKind::Unmerged => "unmerged",
    };
    Change {
        path: String::from_utf8_lossy(&c.path).into_owned(),
        path_hex: hex(&c.path),
        previous_path: c
            .previous_path
            .as_ref()
            .map(|p| String::from_utf8_lossy(p).into_owned()),
        previous_path_hex: c.previous_path.as_ref().map(|p| hex(p)),
        kind,
        added: None,
        deleted: None,
        binary: None,
        old_mode: None,
        new_mode: None,
        content_hex: None,
        incomplete: None,
    }
}
fn run(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(path)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not run Git")?;
    let stderr = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = stderr.take(65536).read_to_end(&mut data);
        data
    });
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .take(LIMIT + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > LIMIT {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stderr_reader.join();
        bail!("Git output exceeds 16 MiB; collection is incomplete");
    }
    let result = child.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !result.success() {
        bail!(
            "Git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&stderr)
        );
    }
    Ok(output)
}
fn changes(path: &Path, args: &[&str]) -> Result<Vec<Change>> {
    let mut result: Vec<Change> = parse_name_status(&run(path, args)?)?
        .into_iter()
        .map(convert)
        .collect();
    for format in ["--numstat", "--raw"] {
        let mapped: Vec<&str> = args
            .iter()
            .map(|a| if *a == "--name-status" { format } else { *a })
            .collect();
        let bytes = run(path, &mapped)?;
        if !bytes.is_empty() && !bytes.ends_with(&[0]) {
            bail!("Truncated Git {format} output");
        }
        let mut fields = bytes.split(|b| *b == 0);
        while let Some(record) = fields.next() {
            if record.is_empty() {
                break;
            }
            if format == "--numstat" {
                let mut parts = record.splitn(3, |b| *b == b'\t');
                let a = parts.next().context("Missing added count")?;
                let d = parts.next().context("Missing deleted count")?;
                let mut filename = parts.next().context("Missing numstat path")?;
                if filename.is_empty() {
                    fields.next().context("Missing rename source")?;
                    filename = fields.next().context("Missing rename destination")?;
                }
                if let Some(item) = result.iter_mut().find(|c| c.path_hex == hex(filename)) {
                    if a == b"-" && d == b"-" {
                        item.binary = Some(true);
                    } else {
                        item.added = Some(std::str::from_utf8(a)?.parse()?);
                        item.deleted = Some(std::str::from_utf8(d)?.parse()?);
                        item.binary = Some(false);
                    }
                }
            } else {
                let header = std::str::from_utf8(record)?;
                let mut parts = header.split_whitespace();
                let old = parts
                    .next()
                    .context("Missing old mode")?
                    .strip_prefix(':')
                    .context("Invalid raw header")?;
                let new = parts.next().context("Missing new mode")?;
                parts.next().context("Missing old object")?;
                parts.next().context("Missing new object")?;
                let status = parts.next().context("Missing raw status")?;
                let mut filename = fields.next().context("Missing raw path")?;
                if status.starts_with('R') || status.starts_with('C') {
                    filename = fields.next().context("Missing raw destination")?;
                }
                if let Some(item) = result.iter_mut().find(|c| c.path_hex == hex(filename)) {
                    item.old_mode = Some(old.to_owned());
                    item.new_mode = Some(new.to_owned());
                }
            }
        }
    }
    Ok(result)
}
const FILE_LIMIT: u64 = 1024 * 1024;
const TOTAL_LIMIT: u64 = 8 * 1024 * 1024;
fn sensitive(path: &[u8]) -> bool {
    path.split(|b| *b == b'/').any(|part| {
        let s = String::from_utf8_lossy(part).to_ascii_lowercase();
        s == ".env"
            || s.starts_with(".env.")
            || matches!(
                s.as_str(),
                ".ssh"
                    | ".aws"
                    | ".gnupg"
                    | ".netrc"
                    | "credentials"
                    | "secrets"
                    | "id_rsa"
                    | "id_ed25519"
            )
            || s.ends_with(".pem")
            || s.ends_with(".key")
            || s.ends_with(".p12")
            || s.ends_with(".pfx")
    })
}
#[cfg(unix)]
fn open_untracked(root: &Path, path: &[u8]) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
    };
    let mut dir = std::fs::File::open(root)?;
    let parts: Vec<_> = path.split(|b| *b == b'/').collect();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || *part == b"." || *part == b".." {
            bail!("invalid path");
        }
        let name = CString::new(*part)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if i + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        dir = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(dir)
}
#[cfg(not(unix))]
fn open_untracked(_root: &Path, _path: &[u8]) -> Result<std::fs::File> {
    bail!("safe untracked reads are unavailable on this platform")
}
fn capture(root: &Path, raw: &[u8], total: &mut u64) -> Change {
    let mut c = convert(FileChange {
        kind: ChangeKind::Added,
        path: raw.to_vec(),
        previous_path: None,
    });
    let read = || -> Result<(Vec<u8>, std::fs::Metadata)> {
        if sensitive(raw) {
            bail!("sensitive path excluded");
        }
        let mut file = open_untracked(root, raw)?;
        let before = file.metadata()?;
        if !before.is_file() {
            bail!("not a regular file");
        }
        if before.len() > FILE_LIMIT || before.len() > TOTAL_LIMIT.saturating_sub(*total) {
            bail!("content size limit exceeded");
        }
        let mut data = Vec::new();
        (&mut file).take(FILE_LIMIT + 1).read_to_end(&mut data)?;
        let after = file.metadata()?;
        if before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || data.len() as u64 != after.len()
        {
            bail!("file changed during collection");
        }
        Ok((data, after))
    };
    match read() {
        Ok((data, meta)) => {
            *total += data.len() as u64;
            c.binary = Some(data.contains(&0));
            if c.binary == Some(false) {
                c.added = Some(if data.is_empty() {
                    0
                } else {
                    data.iter().filter(|b| **b == b'\n').count() as u64
                        + u64::from(!data.ends_with(b"\n"))
                });
                c.deleted = Some(0);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                c.new_mode = Some(
                    if meta.permissions().mode() & 0o111 != 0 {
                        "100755"
                    } else {
                        "100644"
                    }
                    .to_owned(),
                );
            }
            #[cfg(not(unix))]
            let _ = meta;
            c.old_mode = Some("000000".into());
            c.content_hex = Some(hex(&data));
        }
        Err(e) => c.incomplete = Some(e.to_string()),
    }
    c
}
pub(crate) fn collect(name: &str) -> Result<Changeset> {
    let record = workspace::get(name)?;
    let path = Path::new(&record.path);
    let common = run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let actual = crate::path_from_bytes(&common).canonicalize()?;
    if actual != Path::new(&record.repository).canonicalize()? {
        bail!("Workspace no longer belongs to the recorded repository");
    }
    let before_snapshot = run(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            &record.base_commit,
            "--",
        ],
    )?;
    let index_snapshot = run(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--binary",
            "--full-index",
            "HEAD",
            "--",
        ],
    )?;
    let head = String::from_utf8(run(path, &["rev-parse", "--verify", "HEAD"])?)?
        .trim()
        .to_owned();
    let base = &record.base_commit;
    let committed_changes = changes(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "-z",
            "-M",
            base,
            &head,
            "--",
        ],
    )?;
    let staged_changes = changes(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--name-status",
            "-z",
            "-M",
            &head,
            "--",
        ],
    )?;
    let unstaged_changes = changes(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "-z",
            "-M",
            "--",
        ],
    )?;
    let mut aggregate_changes = changes(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "-z",
            "-M",
            base,
            "--",
        ],
    )?;
    let raw = run(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    if !raw.is_empty() && !raw.ends_with(&[0]) {
        bail!("Truncated untracked file list");
    }
    let mut total = 0;
    let untracked_changes: Vec<Change> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| capture(path, p, &mut total))
        .collect();
    aggregate_changes.extend(untracked_changes.iter().cloned());
    let after = String::from_utf8(run(path, &["rev-parse", "--verify", "HEAD"])?)?
        .trim()
        .to_owned();
    let after_snapshot = run(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            &record.base_commit,
            "--",
        ],
    )?;
    let untracked_after = run(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut verify_total = 0;
    let verify_untracked: Vec<Change> = untracked_after
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| capture(path, p, &mut verify_total))
        .collect();
    if untracked_changes
        .iter()
        .zip(&verify_untracked)
        .any(|(a, b)| {
            a.content_hex != b.content_hex
                || a.incomplete != b.incomplete
                || a.new_mode != b.new_mode
        })
    {
        bail!("Untracked file changed during collection; retry");
    }

    let index_after = run(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--binary",
            "--full-index",
            "HEAD",
            "--",
        ],
    )?;
    if before_snapshot != after_snapshot || index_snapshot != index_after || raw != untracked_after
    {
        bail!("Workspace changed during collection; retry");
    }
    if head != after {
        bail!("Workspace HEAD changed during collection; retry");
    }
    Ok(Changeset {
        schema_version: 1,
        workspace_id: record.name.clone(),
        name: record.name,
        repository: record.repository,
        base_commit: record.base_commit,
        head_commit: head,
        committed_changes,
        staged_changes,
        unstaged_changes,
        untracked_changes,
        aggregate_changes,
        warnings: vec![
            "Collection is not an atomic snapshot; changes between checks may go undetected."
                .into(),
        ],
    })
}
pub(crate) fn show(name: &str, stat: bool, json: bool) -> Result<()> {
    let s = collect(name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    let record = workspace::get(name)?;
    let args = if stat {
        vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--stat",
            &s.base_commit,
            "--",
        ]
    } else {
        vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            &s.base_commit,
            "--",
        ]
    };
    let diff = run(Path::new(&record.path), &args)?;
    print!("{}", String::from_utf8_lossy(&diff));
    for f in &s.untracked_changes {
        if let Some(content) = &f.content_hex {
            if stat {
                println!(
                    "Untracked: {} ({} bytes)",
                    f.path.escape_default(),
                    content.len() / 2
                );
                continue;
            }
            println!(
                "Untracked: {} ({} bytes, content hex: {})",
                f.path.escape_default(),
                content.len() / 2,
                content
            );
        } else {
            println!(
                "Untracked: {} (omitted: {})",
                f.path.escape_default(),
                f.incomplete.as_deref().unwrap_or("unknown")
            );
        }
    }
    eprintln!("Warning: collection is not an atomic snapshot; concurrent changes may be missed.");
    Ok(())
}
