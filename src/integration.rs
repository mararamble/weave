//! Integration previews in an isolated object database and index.
use crate::{changeset, git, git_path, workspace};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
};

#[derive(Serialize)]
struct Step {
    workspace: String,
    base_commit: String,
    head_commit: String,
    status: &'static str,
    staged: usize,
    unstaged: usize,
    untracked: usize,
    tree: Option<String>,
    conflict_details: Option<String>,
}
#[derive(Serialize)]
struct Report {
    schema_version: u32,
    target_base: String,
    status: &'static str,
    selected_workspaces: Vec<String>,
    steps: Vec<Step>,
    warnings: Vec<&'static str>,
}

fn run_with(
    dir: &Path,
    args: &[&str],
    worktree: Option<&Path>,
    input: Option<&[u8]>,
) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_INDEX_FILE", dir.join("preview-index"))
        .env("GIT_AUTHOR_NAME", "Weave Preview")
        .env("GIT_AUTHOR_EMAIL", "preview@weave.invalid")
        .env("GIT_COMMITTER_NAME", "Weave Preview")
        .env("GIT_COMMITTER_EMAIL", "preview@weave.invalid")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "--git-dir",
        ])
        .arg(dir)
        .args(args);
    if let Some(path) = worktree {
        command.env("GIT_WORK_TREE", path);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command
        .spawn()
        .context("Could not run Git for integration preview")?;
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let data = bytes.to_vec();
        std::thread::spawn(move || stdin.write_all(&data))
    });
    let mut stderr = child.stderr.take().context("Missing Git stderr")?;
    let stderr_reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut saved = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let n = stderr.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            let keep = n.min(65536usize.saturating_sub(saved.len()));
            saved.extend_from_slice(&buffer[..keep]);
        }
        Ok(saved)
    });
    let mut stdout = Vec::new();
    let read_result = child
        .stdout
        .take()
        .context("Missing Git stdout")?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut stdout);
    let too_large = stdout.len() > 16 * 1024 * 1024;
    if too_large || read_result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let write_result = writer
        .map(|w| {
            w.join()
                .map_err(|_| anyhow::anyhow!("Git input writer failed"))
        })
        .transpose()?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr reader failed"))??;
    read_result?;
    if too_large {
        bail!("Git preview output exceeds 16 MiB");
    }
    if status.success()
        && let Some(result) = write_result
    {
        result.context("Could not write Git input")?;
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    run_with(dir, args, None, None)
}
fn check_output(output: Output) -> Result<Vec<u8>> {
    if !output.status.success() {
        bail!(
            "Git preview failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.len() > 16 * 1024 * 1024 {
        bail!("Git preview output exceeds 16 MiB");
    }
    Ok(output.stdout)
}
fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("Invalid captured bytes");
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(Into::into))
        .collect()
}
fn snapshot(dir: &Path, s: &changeset::Changeset) -> Result<String> {
    let record = workspace::get(&s.name)?;
    let worktree = Path::new(&record.path);
    let output = Command::new("git")
        .args(["-C"])
        .arg(worktree)
        .args(["rev-parse", "--absolute-git-dir"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    if !output.status.success() {
        bail!("Cannot locate workspace index for {}", s.name);
    }
    let index = crate::path_from_bytes(&output.stdout).join("index");
    let before = fs::read(&index).context("Cannot read workspace index")?;
    if before.len() > 64 * 1024 * 1024 {
        bail!("Workspace index exceeds 64 MiB");
    }
    fs::write(dir.join("preview-index"), &before)?;
    let unmerged = check_output(run(dir, &["ls-files", "--unmerged", "-z"])?)?;
    if !unmerged.is_empty() {
        bail!("Workspace {} has unresolved index conflicts", s.name);
    }
    let flags = check_output(run(dir, &["ls-files", "-v", "-z"])?)?;
    for entry in flags.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        if entry.len() < 3 || entry[1] != b' ' {
            bail!("Invalid Git index listing");
        }
        if entry[0] == b'S' || entry[0].is_ascii_lowercase() {
            bail!(
                "Workspace {} has skip-worktree or assume-unchanged entries; planning cannot safely capture them",
                s.name
            );
        }
    }
    let args = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--binary",
        "--full-index",
        "--no-renames",
        &s.head_commit,
        "--",
    ];
    let patch = check_output(run_with(dir, &args, Some(worktree), None)?)?;
    // Reset the private index before re-reading in case Git refreshed it.
    fs::write(dir.join("preview-index"), &before)?;
    let again = check_output(run_with(dir, &args, Some(worktree), None)?)?;
    if patch != again || fs::read(&index)? != before {
        bail!("Workspace {} changed during snapshot; retry", s.name);
    }
    checked(dir, &["read-tree", &s.head_commit])?;
    if !patch.is_empty() {
        check_output(run_with(
            dir,
            &["apply", "--cached", "--binary", "--whitespace=nowarn", "-"],
            None,
            Some(&patch),
        )?)?;
    }
    for file in &s.untracked_changes {
        if let Some(reason) = &file.incomplete {
            bail!(
                "Cannot capture untracked file {}: {reason}",
                file.path.escape_default()
            );
        }
        let data = decode_hex(
            file.content_hex
                .as_deref()
                .context("Missing untracked content")?,
        )?;
        let path = decode_hex(&file.path_hex)?;
        let mode = file
            .new_mode
            .as_deref()
            .context("Missing untracked file mode")?;
        if !matches!(mode, "100644" | "100755") {
            bail!("Unsupported untracked file mode");
        }
        let oid = String::from_utf8(check_output(run_with(
            dir,
            &["hash-object", "-w", "--stdin"],
            None,
            Some(&data),
        )?)?)?;
        let mut entry = format!("{mode} {}\t", oid.trim()).into_bytes();
        entry.extend_from_slice(&path);
        entry.push(0);
        check_output(run_with(
            dir,
            &["update-index", "-z", "--index-info"],
            None,
            Some(&entry),
        )?)?;
    }
    let tree = checked(dir, &["write-tree"])?;
    checked(
        dir,
        &[
            "commit-tree",
            &tree,
            "-p",
            &s.head_commit,
            "-m",
            "Weave workspace snapshot",
        ],
    )
}
fn checked(dir: &Path, args: &[&str]) -> Result<String> {
    let output = run(dir, args)?;
    if !output.status.success() {
        bail!(
            "Git preview failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

// The existing collector runs Git against the source repository. Refuse configured
// clean/process filters before collection because Git may invoke them while diffing.
fn ensure_no_external_filters(name: &str) -> Result<()> {
    let record = workspace::get(name)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&record.path)
        .args(["config", "--get-regexp", r"^filter\..*\.(clean|process)$"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("Could not inspect Git filter configuration")?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => bail!(
            "Workspace {name} has configured Git clean/process filters; planning refuses to execute repository commands that may run them"
        ),
        _ => bail!(
            "Cannot inspect Git filter configuration for {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

pub(crate) fn show(names: &[String], json: bool) -> Result<()> {
    let mut unique = HashSet::new();
    for name in names {
        if !unique.insert(name) {
            bail!("Duplicate workspace name: {name}");
        }
    }
    for name in names {
        ensure_no_external_filters(name)?;
    }
    let sets: Vec<_> = names
        .iter()
        .map(|n| changeset::collect(n))
        .collect::<Result<_>>()?;
    let common = git_path("--git-common-dir")?;
    let objects = common.join("objects").canonicalize()?;
    let object_path = objects
        .to_str()
        .context("Object directory path is not UTF-8")?;
    if object_path.contains(['\n', '\r']) {
        bail!("Object directory path contains a newline");
    }
    let format = String::from_utf8(git(["rev-parse", "--show-object-format"])?)?
        .trim()
        .to_owned();
    if format != "sha1" && format != "sha256" {
        bail!("Unsupported Git object format: {format}");
    }
    let temp = tempfile::tempdir().context("Cannot create temporary preview repository")?;
    let dir = temp.path();
    checked(
        dir,
        &["init", "--bare", &format!("--object-format={format}")],
    )?;
    fs::write(
        dir.join("objects/info/alternates"),
        format!("{object_path}\n"),
    )?;
    let base = sets[0].base_commit.clone();
    let mut candidate = base.clone();
    let snapshots: Vec<String> = sets
        .iter()
        .map(|s| snapshot(dir, s))
        .collect::<Result<_>>()?;
    let mut steps = Vec::new();
    let mut status = "clean";
    for (s, snapshot_head) in sets.iter().zip(&snapshots) {
        let output = run(
            dir,
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                &candidate,
                snapshot_head,
            ],
        )?;
        let code = output.status.code();
        if code != Some(0) && code != Some(1) {
            bail!(
                "Git merge-tree failed for {}: {}",
                s.name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let tree = text
            .lines()
            .next()
            .context("Git merge-tree returned no tree")?
            .trim();
        if !tree.bytes().all(|b| b.is_ascii_hexdigit()) || tree.len() != base.len() {
            bail!("Git merge-tree returned an invalid tree ID");
        }
        let conflict = code == Some(1);
        steps.push(Step {
            workspace: s.name.clone(),
            base_commit: s.base_commit.clone(),
            head_commit: s.head_commit.clone(),
            status: if conflict { "conflict" } else { "clean" },
            staged: s.staged_changes.len(),
            unstaged: s.unstaged_changes.len(),
            untracked: s.untracked_changes.len(),
            tree: Some(tree.to_owned()),
            conflict_details: conflict.then(|| text.to_string()),
        });
        if conflict {
            status = "conflict";
            break;
        }
        // Synthetic commits exist only in the temporary object database.
        candidate = checked(
            dir,
            &[
                "commit-tree",
                tree,
                "-p",
                &candidate,
                "-p",
                snapshot_head,
                "-m",
                "Weave preview",
            ],
        )?;
    }
    // Check for changes after preview; this cannot guarantee an atomic capture.
    for original in &sets {
        let now = changeset::collect(&original.name)?;
        if serde_json::to_vec(original)? != serde_json::to_vec(&now)? {
            bail!("Workspace {} changed during planning; retry", original.name);
        }
    }
    let report = Report {
        schema_version: 1,
        target_base: base,
        status,
        selected_workspaces: names.to_vec(),
        steps,
        warnings: vec![
            "Staged and unstaged changes are combined into each workspace’s final tracked state; untracked content is included when safely captured.",
            "Preview trees and synthetic commits are temporary and cannot be used after this command exits.",
            "Workspace capture is not atomic; concurrent changes may escape detection.",
            "Git merge drivers and custom configuration are disabled; preview may differ from a configured merge.",
        ],
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Target base: {}\nPlan: {}",
            report.target_base, report.status
        );
        for step in &report.steps {
            println!(
                "{}: {} (base {}, head {})",
                step.workspace, step.status, step.base_commit, step.head_commit
            );
            if let Some(detail) = &step.conflict_details {
                println!("{detail}");
            }
        }
        for warning in &report.warnings {
            eprintln!("Warning: {warning}");
        }
    }
    Ok(())
}
