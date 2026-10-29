use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
mod changeset;
mod integration;
mod overlap;
mod workspace;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Parser)]
#[command(version, about = "Coordinate work across Git worktrees")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Preview ordered integration of committed workspace changes
    Plan {
        #[arg(long)]
        json: bool,
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Compare changed files and source regions across workspaces
    Overlaps {
        #[arg(long)]
        json: bool,
        names: Vec<String>,
    },
    /// Collect changes in a managed workspace
    Diff {
        name: String,
        #[arg(long, conflicts_with = "json")]
        stat: bool,
        #[arg(long)]
        json: bool,
    },
    /// Create a managed worktree from an explicit commit reference
    New {
        name: String,
        #[arg(long)]
        from: String,
    },
    /// List managed worktrees
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show the current repository and worktree state
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct Status {
    schema_version: u32,
    repository_root: String,
    worktree_path: String,
    git_dir: String,
    common_dir: String,
    branch: Option<String>,
    detached_head: Option<String>,
    unborn: bool,
    dirty: bool,
    staged: usize,
    unstaged: usize,
    untracked: usize,
    paths_lossy: bool,
}

fn git<I, S>(args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args
        .into_iter()
        .map(|s| s.as_ref().to_os_string())
        .collect();
    let output = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(&args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("Could not run Git; install Git and ensure it is on PATH")?;
    if !output.status.success() {
        bail!(
            "Git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(line(bytes)))
}
#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(line(bytes)).into_owned())
}
fn git_path(arg: &str) -> Result<PathBuf> {
    let p = path_from_bytes(&git(["rev-parse", "--path-format=absolute", arg])?);
    p.canonicalize()
        .with_context(|| format!("Cannot resolve Git path {}", p.display()))
}
fn counts(bytes: &[u8]) -> Result<(usize, usize, usize)> {
    let mut records = bytes.split(|b| *b == 0);
    let (mut staged, mut unstaged, mut untracked) = (0, 0, 0);
    while let Some(r) = records.next() {
        if r.is_empty() {
            continue;
        }
        if r.len() < 4 || r[2] != b' ' {
            bail!("Unexpected Git porcelain status record");
        }
        let (x, y) = (r[0], r[1]);
        if x == b'?' && y == b'?' {
            untracked += 1;
        } else {
            if x != b' ' {
                staged += 1;
            }
            if y != b' ' {
                unstaged += 1;
            }
        }
        if (x == b'R' || x == b'C' || y == b'R' || y == b'C')
            && records.next().filter(|r| !r.is_empty()).is_none()
        {
            bail!("Missing rename source in Git status");
        }
    }
    Ok((staged, unstaged, untracked))
}
fn status() -> Result<Status> {
    let root = git_path("--show-toplevel")
        .context("Not in a Git worktree; run this command inside a Git repository")?;
    let gd = git_path("--absolute-git-dir")?;
    let common = git_path("--git-common-dir")?;
    let branch_output = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("Could not run Git to inspect HEAD")?;
    let branch = if branch_output.status.success() {
        Some(String::from_utf8_lossy(line(&branch_output.stdout)).into_owned())
    } else if branch_output.status.code() == Some(1) {
        None
    } else {
        bail!(
            "Git symbolic-ref failed: {}",
            String::from_utf8_lossy(&branch_output.stderr).trim()
        )
    };
    let head = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("Could not run Git to inspect commit")?;
    if !head.status.success() && head.status.code() != Some(1) {
        bail!(
            "Git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&head.stderr).trim()
        );
    }
    let unborn = !head.status.success() && branch.is_some();
    let detached_head = if branch.is_none() {
        Some(String::from_utf8_lossy(line(&git(["rev-parse", "--short", "HEAD"])?)).into_owned())
    } else {
        None
    };
    let (staged, unstaged, untracked) = counts(&git([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    ])?)?;
    let lossy = [&root, &gd, &common].iter().any(|p| p.to_str().is_none());
    let display = |p: &Path| p.to_string_lossy().into_owned();
    Ok(Status {
        schema_version: 1,
        repository_root: display(&root),
        worktree_path: display(&root),
        git_dir: display(&gd),
        common_dir: display(&common),
        branch,
        detached_head,
        unborn,
        dirty: staged + unstaged + untracked > 0,
        staged,
        unstaged,
        untracked,
        paths_lossy: lossy,
    })
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Plan { json, names } => integration::show(&names, json)?,
        Commands::Overlaps { json, names } => overlap::show(&names, json)?,
        Commands::Diff { name, stat, json } => changeset::show(&name, stat, json)?,
        Commands::New { name, from } => workspace::create(&name, &from)?,
        Commands::List { json } => workspace::list(json)?,
        Commands::Status { json } => {
            let s = status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "Git repository: yes\nRepository root: {}\nWorktree: {}\nGit directory: {}\nCommon directory: {}",
                    s.repository_root, s.worktree_path, s.git_dir, s.common_dir
                );
                println!(
                    "HEAD: {}{}",
                    s.branch
                        .as_deref()
                        .or(s.detached_head.as_deref())
                        .unwrap_or("unknown"),
                    if s.unborn {
                        " (unborn)"
                    } else if s.detached_head.is_some() {
                        " (detached)"
                    } else {
                        ""
                    }
                );
                println!(
                    "{}: {} staged, {} unstaged, {} untracked",
                    if s.dirty { "Dirty" } else { "Clean" },
                    s.staged,
                    s.unstaged,
                    s.untracked
                );
                if s.paths_lossy {
                    println!("Note: paths contain non-UTF-8 bytes and are displayed lossily.");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn porcelain() {
        assert_eq!(
            counts(b"M  a\0 M b\0?? x\ny\0R  new\0old\0").unwrap(),
            (2, 1, 1)
        );
        assert!(counts(b"R  a\0").is_err());
    }
}
