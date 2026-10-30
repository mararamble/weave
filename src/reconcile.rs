//! Read-only evidence about Weave records and Git registrations.
use crate::{git, git_path, git_process, status_counts, workspace};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{ffi::OsStr, fs, path::Path};

#[derive(Serialize)]
struct Managed {
    name: String,
    path: String,
    branch: String,
    record: String,
    registration: bool,
    states: Vec<String>,
    moved_candidates: Vec<String>,
    staged: Option<usize>,
    unstaged: Option<usize>,
    untracked: Option<usize>,
    ignored: Option<usize>,
}
#[derive(Serialize)]
struct Unknown {
    path: String,
    paths_lossy: bool,
    branch: Option<String>,
    locked: bool,
    prunable: bool,
}
#[derive(Serialize)]
struct Inventory {
    schema_version: u32,
    managed: Vec<Managed>,
    unknown: Vec<Unknown>,
}

fn target_git(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git_process::output(git_process::command_at(path).args(args))?;
    if !out.status.success() {
        anyhow::bail!("Cannot inspect worktree identity");
    }
    Ok(out.stdout)
}
fn identity_matches(path: &Path, common: &Path, entry: &workspace::GitWorktree) -> Result<bool> {
    let root = target_git(path, &["rev-parse", "--show-toplevel"])?;
    let repo = target_git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let head = target_git(path, &["rev-parse", "--verify", "HEAD"])?;
    Ok(crate::path_from_bytes(&root).canonicalize()? == path
        && crate::path_from_bytes(&repo).canonicalize()? == common
        && Some(crate::line(&head)) == entry.head.as_deref())
}

fn inventory() -> Result<Inventory> {
    let common = git_path("--git-common-dir")?;
    let entries = workspace::parse_worktrees(&git(["worktree", "list", "--porcelain", "-z"])?)?;
    let meta = common.join("weave");
    let mut managed = Vec::new();
    let mut matched = vec![false; entries.len()];
    let meta_info = match fs::symlink_metadata(&meta) {
        Ok(info) => Some(info),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Cannot inspect metadata directory {}", meta.display()));
        }
    };
    if let Some(info) = meta_info {
        if !info.is_dir() || info.file_type().is_symlink() {
            anyhow::bail!("Unsafe metadata directory: {}", meta.display());
        }
        let mut records = Vec::new();
        for item in fs::read_dir(&meta)? {
            let path = item?.path();
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }
            let info = fs::symlink_metadata(&path)?;
            if !info.is_file() || info.file_type().is_symlink() {
                anyhow::bail!("Unsafe metadata record: {}", path.display());
            }
            let record: workspace::Record = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("Invalid metadata: {}", path.display()))?;
            records.push((path, record));
        }
        for (path, record) in &records {
            let p = Path::new(&record.path);
            let mut states = Vec::new();
            let (mut staged, mut unstaged, mut untracked, mut ignored) = (None, None, None, None);
            let expected_file = format!("{}.json", record.name);
            if record.schema_version != 1
                || workspace::validate(&record.name).is_err()
                || path.file_name() != Some(OsStr::new(&expected_file))
                || Path::new(&record.repository) != common
                || record.branch != format!("weave/{}", record.name)
                || !p.is_absolute()
                || !matches!(record.base_commit.len(), 40 | 64)
                || !record.base_commit.bytes().all(|b| b.is_ascii_hexdigit())
            {
                states.push("metadata_drift".to_owned());
            }
            if records
                .iter()
                .filter(|(_, other)| {
                    other.path == record.path
                        || other.name == record.name
                        || other.branch == record.branch
                })
                .count()
                > 1
            {
                states.push("ambiguous_metadata".to_owned());
            }
            let metadata_valid = states.is_empty();
            if record.state != "ready" {
                states.push("pending_or_invalid_state".to_owned());
            }
            let matches: Vec<_> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.path == p)
                .collect();
            let registered = matches.len() == 1;
            if matches.len() > 1 {
                states.push("ambiguous_registration".to_owned());
            }
            let expected_branch = format!("refs/heads/{}", record.branch);
            let moved_candidates: Vec<String> = entries
                .iter()
                .filter(|e| e.path != p && e.branch.as_deref() == Some(expected_branch.as_bytes()))
                .map(|e| e.path.to_string_lossy().into_owned())
                .collect();
            if !registered {
                states.push("unregistered".to_owned());
            }
            if registered {
                let (index, entry) = matches[0];
                if index == 0 {
                    states.push("primary_worktree".to_owned());
                }
                if entry.branch.as_deref() != Some(expected_branch.as_bytes())
                    || entry.detached
                    || entry.bare
                {
                    states.push("branch_drift".to_owned());
                }
                if entry.locked {
                    states.push("locked".to_owned());
                }
                if entry.prunable {
                    states.push("prunable".to_owned());
                }
            }
            if !moved_candidates.is_empty() {
                states.push("moved_candidate".to_owned());
            }
            match fs::symlink_metadata(p) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    states.push("missing".to_owned())
                }
                Err(_) => states.push("inspection_unavailable".to_owned()),
                Ok(info) if info.file_type().is_symlink() || !info.is_dir() => {
                    states.push("path_drift".to_owned())
                }
                Ok(_) => {
                    if p.canonicalize().ok().as_deref() != Some(p) {
                        states.push("path_drift".to_owned());
                    }
                    if registered && metadata_valid && !states.contains(&"path_drift".to_owned()) {
                        let (index, entry) = matches[0];
                        let identity_valid = match identity_matches(p, &common, entry) {
                            Ok(true) => true,
                            Ok(false) => {
                                states.push("repository_or_head_drift".to_owned());
                                false
                            }
                            Err(_) => {
                                states.push("inspection_unavailable".to_owned());
                                false
                            }
                        };
                        if identity_valid
                            && entry.branch.as_deref() == Some(expected_branch.as_bytes())
                        {
                            matched[index] = true;
                        }
                        let index = if identity_valid {
                            crate::removal::inspect_index(p).ok()
                        } else {
                            None
                        };
                        let out = if identity_valid && index.is_some() {
                            git_process::ensure_no_filters(p).and_then(|_| {
                                git_process::output(git_process::command_at(p).args([
                                    "status",
                                    "--porcelain=v1",
                                    "-z",
                                    "--untracked-files=all",
                                    "--ignored=matching",
                                    "--ignore-submodules=none",
                                ]))
                            })
                        } else {
                            Err(anyhow::anyhow!("Identity inspection failed"))
                        };
                        match out {
                            Ok(out) if out.status.success() => match status_counts(&out.stdout) {
                                Ok((a, b, c, d)) => {
                                    (staged, unstaged, untracked, ignored) =
                                        (Some(a), Some(b), Some(c), Some(d));
                                    if a + b + c + d == 0 {
                                        match index.as_ref().map(|paths| {
                                            crate::removal::inspect_filesystem(p, paths)
                                        }) {
                                            Some(Ok(())) => states.push("clean".to_owned()),
                                            _ => states.push(
                                                "untracked_or_uninspectable_filesystem_data"
                                                    .to_owned(),
                                            ),
                                        }
                                    } else {
                                        states.push("dirty_or_ignored".to_owned());
                                        if a + b + c > 0 {
                                            states.push("dirty".to_owned());
                                        }
                                        if d > 0 {
                                            states.push("ignored_content".to_owned());
                                        }
                                    }
                                }
                                Err(_) => states.push("inspection_unavailable".to_owned()),
                            },
                            _ if !states.contains(&"inspection_unavailable".to_owned())
                                && identity_valid =>
                            {
                                states.push("inspection_unavailable".to_owned())
                            }
                            _ => {}
                        }
                    }
                }
            }
            managed.push(Managed {
                name: record.name.clone(),
                path: record.path.clone(),
                branch: record.branch.clone(),
                record: path.to_string_lossy().into_owned(),
                registration: registered,
                states,
                moved_candidates,
                staged,
                unstaged,
                untracked,
                ignored,
            });
        }
    }
    managed.sort_by(|a, b| a.name.cmp(&b.name));
    let unknown = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| !matched[*i])
        .map(|(_, e)| Unknown {
            path: e.path.to_string_lossy().into_owned(),
            paths_lossy: e.path.to_str().is_none(),
            branch: e
                .branch
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).into_owned()),
            locked: e.locked,
            prunable: e.prunable,
        })
        .collect();
    Ok(Inventory {
        schema_version: 1,
        managed,
        unknown,
    })
}

pub(crate) fn preview(name: &str) -> Result<()> {
    let report = inventory()?;
    let mut matches = report.managed.iter().filter(|item| item.name == name);
    let item = matches
        .next()
        .with_context(|| format!("No readable managed record for {name}"))?;
    if matches.next().is_some() {
        anyhow::bail!(
            "Multiple metadata records claim workspace {name}; inspect them before proceeding"
        );
    }
    println!(
        "Target: {}\nBranch retained: {}\nMetadata: {}\nGit registration: {}\nState: {}",
        item.path,
        item.branch,
        item.record,
        item.registration,
        item.states.join(", ")
    );
    for candidate in &item.moved_candidates {
        println!("Possible moved worktree: {candidate}");
    }
    println!("No operations will be performed while this target is blocked.");
    Ok(())
}

pub(crate) fn show(json: bool) -> Result<()> {
    let inventory = inventory()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&inventory)?);
    } else {
        for item in &inventory.managed {
            println!(
                "{}  {}  {}\n  record: {}\n  state: {}",
                item.name,
                item.branch,
                item.path,
                item.record,
                item.states.join(", ")
            );
            for candidate in &item.moved_candidates {
                println!("  possible moved worktree: {candidate}");
            }
        }
        for item in &inventory.unknown {
            println!("Unverified Git worktree: {}", item.path);
        }
        if inventory.managed.is_empty() {
            println!("No managed workspaces.");
        }
    }
    Ok(())
}
