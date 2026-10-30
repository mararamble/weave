//! Conservative removal of one explicitly selected managed worktree.
use crate::{git, git_path, git_process, status_counts, workspace};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    io::{self, BufRead, IsTerminal, Write},
    path::{Component, Path, PathBuf},
};

struct Checked {
    path: PathBuf,
    record_path: PathBuf,
    record_bytes: Vec<u8>,
    branch: String,
    head: Vec<u8>,
    identity: (u64, u64),
}
#[cfg(unix)]
fn file_identity(info: &fs::Metadata) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((info.dev(), info.ino()))
}
#[cfg(not(unix))]
fn file_identity(_: &fs::Metadata) -> Result<(u64, u64)> {
    bail!("Safe worktree identity checks are not supported on this platform")
}
fn output_at(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git_process::output(git_process::command_at(path).args(args))
        .context("Could not run Git; install Git and ensure it is on PATH")?;
    if !out.status.success() {
        bail!(
            "Git inspection failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}
#[cfg(unix)]
fn index_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let path = PathBuf::from(OsStr::from_bytes(bytes));
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("Invalid path in Git index");
    }
    Ok(path)
}
#[cfg(not(unix))]
fn index_path(_: &[u8]) -> Result<PathBuf> {
    bail!("Safe worktree identity checks are not supported on this platform")
}

// Git status omits empty directories and nested .git entries. Inspect every
// directory without following symlinks, allowing only paths represented by the index.
fn ensure_no_untracked_entries(root: &Path, tracked: &HashSet<PathBuf>) -> Result<()> {
    let mut directories = HashSet::new();
    for path in tracked {
        let mut parent = path.parent();
        while let Some(dir) = parent {
            if dir.as_os_str().is_empty() {
                break;
            }
            directories.insert(dir.to_path_buf());
            parent = dir.parent();
        }
    }
    let mut pending = vec![PathBuf::new()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(root.join(&dir))
            .with_context(|| format!("Cannot inspect directory {}", root.join(&dir).display()))?
        {
            let entry = entry?;
            let relative = dir.join(entry.file_name());
            let info = fs::symlink_metadata(entry.path())?;
            if relative == Path::new(".git") {
                if !info.is_file() || info.file_type().is_symlink() {
                    bail!("Unexpected worktree .git entry");
                }
                continue;
            }
            if info.is_dir() && !info.file_type().is_symlink() {
                if !directories.contains(&relative) {
                    bail!(
                        "Untracked directory or nested data at {}; refusing removal",
                        relative.display()
                    );
                }
                pending.push(relative);
            } else if !tracked.contains(&relative) {
                bail!(
                    "Untracked filesystem entry at {}; refusing removal",
                    relative.display()
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn inspect_index(path: &Path) -> Result<HashSet<PathBuf>> {
    let flags = output_at(path, &["ls-files", "-v", "-z"])?;
    if !flags.is_empty() && !flags.ends_with(&[0]) {
        bail!("Truncated Git index listing");
    }
    let mut flagged_paths = HashSet::new();
    for f in flags
        .strip_suffix(&[0])
        .unwrap_or(&flags)
        .split(|b| *b == 0)
    {
        if flags.is_empty() {
            break;
        }
        if f.len() < 3 || f[1] != b' ' || !matches!(f[0], b'H' | b'M' | b'R' | b'C' | b'K' | b'U') {
            bail!("Index entries are invalid or hidden; refusing removal");
        }
        if !flagged_paths.insert(index_path(&f[2..])?) {
            bail!("Duplicate Git index flag record");
        }
    }
    let staged = output_at(path, &["ls-files", "--stage", "-z"])?;
    if !staged.is_empty() && !staged.ends_with(&[0]) {
        bail!("Truncated Git index listing");
    }
    let mut tracked = HashSet::new();
    for f in staged
        .strip_suffix(&[0])
        .unwrap_or(&staged)
        .split(|b| *b == 0)
    {
        if staged.is_empty() {
            break;
        }
        let tab = f
            .iter()
            .position(|b| *b == b'\t')
            .context("Invalid Git index listing")?;
        if tab + 1 == f.len() {
            bail!("Invalid Git index path");
        }
        let fields: Vec<_> = f[..tab].split(|b| *b == b' ').collect();
        if fields.len() != 3
            || !matches!(fields[0], b"100644" | b"100755" | b"120000" | b"160000")
            || !matches!(fields[1].len(), 40 | 64)
            || !fields[1].iter().all(u8::is_ascii_hexdigit)
            || fields[2] != b"0"
        {
            bail!("Invalid or unmerged Git index listing");
        }
        if fields[0] == b"160000" {
            bail!("Workspaces containing submodules cannot be removed safely");
        }
        if !tracked.insert(index_path(&f[tab + 1..])?) {
            bail!("Duplicate Git index path");
        }
    }
    if flagged_paths != tracked {
        bail!("Git index listings disagree; refusing removal");
    }
    Ok(tracked)
}

pub(crate) fn inspect_filesystem(path: &Path, tracked: &HashSet<PathBuf>) -> Result<()> {
    ensure_no_untracked_entries(path, tracked)
}

fn inspect(name: &str) -> Result<Checked> {
    workspace::validate(name)?;
    let common = git_path("--git-common-dir")?;
    let directory = common.join("weave");
    let dir_info = fs::symlink_metadata(&directory).context("Cannot inspect metadata directory")?;
    if !dir_info.is_dir() || dir_info.file_type().is_symlink() {
        bail!("Unsafe metadata directory");
    }
    let record_path = directory.join(format!("{name}.json"));
    let info = fs::symlink_metadata(&record_path).context("Cannot inspect workspace metadata")?;
    if !info.is_file() || info.file_type().is_symlink() {
        bail!("Workspace metadata must be a regular file");
    }
    let record_bytes = fs::read(&record_path)?;
    let record: workspace::Record =
        serde_json::from_slice(&record_bytes).context("Invalid workspace metadata")?;
    let path = PathBuf::from(&record.path);
    if record.schema_version != 1
        || record.name != name
        || record.state != "ready"
        || record.branch != format!("weave/{name}")
        || Path::new(&record.repository) != common
        || !path.is_absolute()
        || !matches!(record.base_commit.len(), 40 | 64)
        || !record.base_commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("Workspace metadata is incomplete or has drifted; inspect with weave reconcile");
    }
    let info = fs::symlink_metadata(&path)
        .context("Workspace missing or inaccessible; inspect with weave reconcile")?;
    if !info.is_dir() || info.file_type().is_symlink() || path.canonicalize()? != path {
        bail!("Workspace path is not a canonical directory; inspect with weave reconcile");
    }
    let identity = file_identity(&info)?;
    // Ambiguous or malformed metadata anywhere prevents deciding ownership.
    for entry in fs::read_dir(&directory)? {
        let other = entry?.path();
        if other == record_path || other.extension() != Some(OsStr::new("json")) {
            continue;
        }
        let stat = fs::symlink_metadata(&other)?;
        if !stat.is_file() || stat.file_type().is_symlink() {
            bail!("Unsafe workspace metadata at {}", other.display());
        }
        let r: workspace::Record = serde_json::from_slice(&fs::read(&other)?)
            .with_context(|| format!("Invalid metadata: {}", other.display()))?;
        if r.path == record.path || r.name == record.name || r.branch == record.branch {
            bail!("Ambiguous workspace metadata: {}", other.display());
        }
    }
    let entries = workspace::parse_worktrees(&git(["worktree", "list", "--porcelain", "-z"])?)?;
    let matches: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.path == path)
        .collect();
    if matches.len() != 1 {
        bail!("Workspace registration is missing or ambiguous; inspect with weave reconcile");
    }
    let (index, item) = matches[0];
    let expected_branch = format!("refs/heads/{}", record.branch);
    if index == 0
        || item.bare
        || item.detached
        || item.locked
        || item.prunable
        || item.branch.as_deref() != Some(expected_branch.as_bytes())
    {
        bail!("Workspace is primary, locked, prunable, detached, or has branch drift");
    }
    let worktree_common = output_at(
        &path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if crate::path_from_bytes(&worktree_common).canonicalize()? != common {
        bail!("Workspace belongs to a different repository");
    }
    let top = output_at(&path, &["rev-parse", "--show-toplevel"])?;
    if crate::path_from_bytes(&top).canonicalize()? != path {
        bail!("Workspace root does not match its registered path");
    }
    let head = output_at(&path, &["rev-parse", "--verify", "HEAD"])?;
    if Some(crate::line(&head)) != item.head.as_deref() {
        bail!("Workspace HEAD changed during inspection");
    }
    let tracked = inspect_index(&path)?;
    git_process::ensure_no_filters(&path)?;
    let status = output_at(
        &path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--ignore-submodules=none",
        ],
    )?;
    let (a, b, c, ignored) = status_counts(&status)?;
    if a + b + c + ignored != 0 {
        bail!("Workspace contains changes, untracked files, or ignored files; refusing removal");
    }
    inspect_filesystem(&path, &tracked)?;
    Ok(Checked {
        path,
        record_path,
        record_bytes,
        branch: record.branch,
        head,
        identity,
    })
}

fn confirm(terminal: bool, input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    if !terminal {
        bail!("Confirmation required; use --yes for this exact target in noninteractive use");
    }
    write!(output, "Remove this worktree and its metadata? Type yes: ")?;
    output.flush()?;
    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 || answer.trim() != "yes" {
        bail!("Removal cancelled");
    }
    Ok(())
}

pub(crate) fn remove(name: &str, dry_run: bool, yes: bool) -> Result<()> {
    let before = match inspect(name) {
        Ok(checked) => checked,
        Err(error) if dry_run => {
            if let Err(diagnostic) = crate::reconcile::preview(name) {
                eprintln!("Could not produce detailed preview: {diagnostic:#}");
            }
            bail!("Removal blocked: {error:#}. No deletion was attempted");
        }
        Err(error) => return Err(error),
    };
    println!(
        "Target: {}\nBranch retained: {}\nMetadata: {}\nGit registration: verified\nState: clean and eligible\nOperations: Git worktree remove, then delete this metadata record\nRemaining: branch and commits",
        before.path.display(),
        before.branch,
        before.record_path.display()
    );
    if dry_run {
        println!("Preview only; no files removed.");
        return Ok(());
    }
    if !yes {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut output = io::stdout().lock();
        confirm(stdin.is_terminal(), &mut input, &mut output)?;
    }
    let after = inspect(name)?;
    if before.path != after.path
        || before.record_path != after.record_path
        || before.record_bytes != after.record_bytes
        || before.branch != after.branch
        || before.head != after.head
        || before.identity != after.identity
    {
        bail!("Workspace changed after preview; no removal attempted");
    }
    let args: Vec<&OsStr> = vec![
        OsStr::new("worktree"),
        OsStr::new("remove"),
        OsStr::new("--"),
        after.path.as_os_str(),
    ];
    git(args).context("Git removal failed; metadata remains. Inspect the workspace and retry")?;
    let unchanged = fs::symlink_metadata(&after.record_path)
        .map(|m| m.is_file() && !m.file_type().is_symlink())
        .unwrap_or(false)
        && fs::read(&after.record_path)
            .map(|bytes| bytes == after.record_bytes)
            .unwrap_or(false);
    if !unchanged {
        bail!(
            "Git removed {}; metadata changed or cannot be read at {}. Inspect before manual recovery",
            after.path.display(),
            after.record_path.display()
        );
    }
    fs::remove_file(&after.record_path).with_context(|| format!("Git removed {}; metadata remains at {}. Inspect it and remove only that record after confirming the workspace is absent", after.path.display(), after.record_path.display()))?;
    println!(
        "Removed {}. Branch {} remains.",
        after.path.display(),
        after.branch
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_requires_positive_interactive_answer() {
        for (terminal, response, accepted) in [
            (true, "yes\n", true),
            (true, "no\n", false),
            (true, "", false),
            (true, "YES\n", false),
            (false, "yes\n", false),
        ] {
            let mut input = std::io::Cursor::new(response.as_bytes());
            assert_eq!(
                confirm(terminal, &mut input, &mut Vec::new()).is_ok(),
                accepted
            );
        }
    }
}
