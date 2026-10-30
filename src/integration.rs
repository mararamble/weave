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
    let mut command = crate::git_process::command();
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
    let mut child = crate::git_process::spawn(&mut command)
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
    let output = crate::git_process::output(
        crate::git_process::command_at(worktree).args(["rev-parse", "--absolute-git-dir"]),
    )?;
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
    for entry in changeset::parse_path_list(&flags)? {
        if entry.len() < 3 || entry[1] != b' ' {
            bail!("Invalid Git index listing");
        }
        if entry[0] == b'S' || entry[0].is_ascii_lowercase() {
            bail!(
                "Workspace {} has skip-worktree or assume-unchanged entries; planning cannot safely capture them",
                s.name
            );
        }
        if !matches!(entry[0], b'H' | b'M' | b'R' | b'C' | b'K' | b'U') {
            bail!("Invalid Git index listing");
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
    let output = crate::git_process::output(
        crate::git_process::command_at(Path::new(&record.path)).args([
            "config",
            "--get-regexp",
            r"^filter\..*\.(clean|process)$",
        ]),
    )
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

struct PreparedCandidate {
    // Owns the temporary object database for as long as the candidate is needed.
    temp: tempfile::TempDir,
    report: Report,
    candidate_commit: String,
    snapshots: Vec<String>,
    sources: Vec<changeset::Changeset>,
}

impl PreparedCandidate {
    fn verify_sources(&self) -> Result<()> {
        for (original, captured) in self.sources.iter().zip(&self.snapshots) {
            let now = changeset::collect(&original.name)?;
            if serde_json::to_vec(original)? != serde_json::to_vec(&now)? {
                bail!("Workspace {} changed during planning; retry", original.name);
            }
            // Counts and names can stay identical while tracked bytes change.
            let current = snapshot(self.temp.path(), &now)?;
            let previous_tree = checked(
                self.temp.path(),
                &["rev-parse", &format!("{captured}^{{tree}}")],
            )?;
            let current_tree = checked(
                self.temp.path(),
                &["rev-parse", &format!("{current}^{{tree}}")],
            )?;
            if previous_tree != current_tree {
                bail!("Workspace {} changed during planning; retry", original.name);
            }
        }
        Ok(())
    }
}

fn prepare(names: &[String]) -> Result<PreparedCandidate> {
    if names.is_empty() {
        bail!("Select at least one workspace");
    }
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
    let prepared = PreparedCandidate {
        temp,
        report,
        candidate_commit: candidate,
        snapshots,
        sources: sets,
    };
    // Recheck after candidate construction; this is not an atomic snapshot.
    prepared.verify_sources()?;
    Ok(prepared)
}

pub(crate) fn show(names: &[String], json: bool) -> Result<()> {
    let prepared = prepare(names)?;
    // The owned candidate and snapshot IDs remain available to future consumers
    // until the temporary object database is dropped.
    let _candidate = (&prepared.candidate_commit, &prepared.snapshots);
    let report = prepared.report;
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

#[derive(Serialize)]
struct IntegrationSource {
    workspace: String,
    base: String,
    head: String,
    snapshot: String,
    included_commits: Vec<String>,
}
#[derive(Serialize)]
struct Validation {
    command: Vec<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    output_truncated: bool,
    changed_tracked_files: bool,
}
#[derive(Serialize)]
struct IntegrationResult {
    schema_version: u32,
    branch: String,
    commit: String,
    tree: String,
    target_base: String,
    sources: Vec<IntegrationSource>,
    validation: Option<Validation>,
    warnings: Vec<&'static str>,
}

fn validate_candidate(
    prepared: &PreparedCandidate,
    args: &[std::ffi::OsString],
) -> Result<Option<Validation>> {
    if args.is_empty() {
        return Ok(None);
    }
    let dir = prepared.temp.path();
    let checkout = dir.join("validation-worktree");
    fs::create_dir(&checkout)?;
    check_output(run_with(
        dir,
        &[
            "-c",
            "core.bare=false",
            "read-tree",
            "--reset",
            "-u",
            &prepared.candidate_commit,
        ],
        Some(&checkout),
        None,
    )?)?;
    checked(
        dir,
        &[
            "update-ref",
            "refs/weave/validation-candidate",
            &prepared.candidate_commit,
        ],
    )?;
    checked(dir, &["update-ref", "HEAD", &prepared.candidate_commit])?;
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .current_dir(&checkout)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Git invoked by validation must use only the disposable repository and index.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_DIR", dir)
        .env("GIT_WORK_TREE", &checkout)
        .env("GIT_INDEX_FILE", dir.join("preview-index"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "3")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_1", "false")
        .env("GIT_CONFIG_KEY_2", "core.bare")
        .env("GIT_CONFIG_VALUE_2", "false");
    let mut child = command
        .spawn()
        .context("Could not start requested validation command")?;
    fn drain<R: Read>(mut reader: R) -> std::io::Result<(Vec<u8>, bool)> {
        let mut result = Vec::new();
        let mut truncated = false;
        let mut buf = [0; 8192];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let keep = n.min(65536usize.saturating_sub(result.len()));
            result.extend_from_slice(&buf[..keep]);
            truncated |= keep < n;
        }
        Ok((result, truncated))
    }
    let stdout = child.stdout.take().context("Missing validation stdout")?;
    let stderr = child.stderr.take().context("Missing validation stderr")?;
    let out_reader = std::thread::spawn(move || drain(stdout));
    let err_reader = std::thread::spawn(move || drain(stderr));
    let status = child.wait()?;
    let (out_bytes, out_truncated) = out_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Validation output reader failed"))??;
    let (err_bytes, err_truncated) = err_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Validation error reader failed"))??;
    let stdout = String::from_utf8_lossy(&out_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&err_bytes).into_owned();
    let diff = run_with(
        dir,
        &[
            "-c",
            "core.bare=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--quiet",
            &prepared.candidate_commit,
            "--",
        ],
        Some(&checkout),
        None,
    )?;
    if !matches!(diff.status.code(), Some(0 | 1)) {
        bail!(
            "Cannot inspect validation changes: {}",
            String::from_utf8_lossy(&diff.stderr)
        );
    }
    let validation = Validation {
        command: args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect(),
        exit_code: status.code(),
        stdout,
        stderr,
        output_truncated: out_truncated || err_truncated,
        changed_tracked_files: diff.status.code() == Some(1),
    };
    if !status.success() {
        bail!(
            "Validation failed with exit {:?}; no branch was created. stdout: {} stderr: {}",
            validation.exit_code,
            validation.stdout,
            validation.stderr
        );
    }
    Ok(Some(validation))
}

pub(crate) fn integrate(
    names: &[String],
    branch: &str,
    validation_args: &[std::ffi::OsString],
    json: bool,
) -> Result<()> {
    let common = git_path("--git-common-dir")?;
    let reference = format!("refs/heads/{branch}");
    if branch.is_empty() || branch.starts_with('-') {
        bail!("Invalid integration branch name");
    }
    check_output(run(&common, &["check-ref-format", &reference])?)
        .context("Invalid integration branch name")?;
    let exists = run(&common, &["show-ref", "--verify", "--quiet", &reference])?;
    match exists.status.code() {
        Some(0) => bail!("Integration branch already exists: {branch}"),
        Some(1) => {}
        _ => bail!(
            "Cannot check integration branch: {}",
            String::from_utf8_lossy(&exists.stderr)
        ),
    }
    let prepared = prepare(names)?;
    if prepared.report.status != "clean" {
        let details = prepared
            .report
            .steps
            .last()
            .and_then(|s| s.conflict_details.as_deref())
            .unwrap_or("Unknown conflict");
        bail!("Integration has conflicts; no branch was created:\n{details}");
    }
    let mut sources = Vec::new();
    for (source, snapshot_id) in prepared.sources.iter().zip(&prepared.snapshots) {
        let commits = checked(
            prepared.temp.path(),
            &[
                "rev-list",
                "--reverse",
                &source.head_commit,
                "--not",
                &source.base_commit,
            ],
        )?;
        sources.push(IntegrationSource {
            workspace: source.name.clone(),
            base: source.base_commit.clone(),
            head: source.head_commit.clone(),
            snapshot: snapshot_id.clone(),
            included_commits: commits.lines().map(str::to_owned).collect(),
        });
    }
    let tree = checked(
        prepared.temp.path(),
        &[
            "rev-parse",
            &format!("{}^{{tree}}", prepared.candidate_commit),
        ],
    )?;
    eprintln!(
        "Integration preview: {branch} from {} with {} workspace(s); candidate tree {tree}",
        prepared.report.target_base,
        names.len()
    );
    let validation = validate_candidate(&prepared, validation_args)?;
    prepared.verify_sources()?;
    // Import only objects not already reachable from the source heads/base. No refs
    // are updated by index-pack; a failed operation can leave unreachable objects.
    let mut revisions = format!(
        "{}\n^{}\n",
        prepared.candidate_commit, prepared.report.target_base
    );
    for source in &prepared.sources {
        revisions.push_str(&format!("^{}\n", source.head_commit));
    }
    let pack = check_output(run_with(
        prepared.temp.path(),
        &["pack-objects", "--stdout", "--revs"],
        None,
        Some(revisions.as_bytes()),
    )?)?;
    let metadata_dir = common.join("weave");
    let resolved_metadata = metadata_dir
        .canonicalize()
        .context("Cannot resolve Weave metadata directory")?;
    if !resolved_metadata.starts_with(&common) {
        bail!("Weave metadata directory escapes the Git common directory");
    }
    let journal_dir = resolved_metadata.join("integrations");
    if let Ok(info) = fs::symlink_metadata(&journal_dir) {
        if !info.is_dir() || info.file_type().is_symlink() {
            bail!("Unsafe integration journal directory");
        }
    } else {
        fs::create_dir(&journal_dir).context("Cannot create integration journal directory")?;
    }
    let journal_dir = journal_dir.canonicalize()?;
    if !journal_dir.starts_with(&common) {
        bail!("Integration journal directory escapes the Git common directory");
    }
    let mut journal = tempfile::Builder::new()
        .prefix("integration-")
        .suffix(".json")
        .tempfile_in(&journal_dir)?;
    serde_json::to_writer(
        &mut journal,
        &serde_json::json!({
            "schema_version": 1, "branch": branch, "reference": reference,
            "candidate": prepared.candidate_commit, "tree": tree,
            "base": prepared.report.target_base, "sources": sources,
            "note": "Inspect the branch ref: if it equals candidate, publication succeeded; otherwise no matching branch was published. Never reset or delete a ref automatically."
        }),
    )?;
    journal.write_all(b"\n")?;
    journal.as_file().sync_all()?;
    let (_journal_file, journal_path) = journal
        .keep()
        .context("Cannot preserve integration recovery journal")?;
    let publish = || -> Result<()> {
        check_output(run_with(
            &common,
            &["index-pack", "--stdin", "--fix-thin"],
            None,
            Some(&pack),
        )?)
        .context("Candidate object transfer failed; no branch was created")?;
        check_output(run(
            &common,
            &[
                "cat-file",
                "-e",
                &format!("{}^{{commit}}", prepared.candidate_commit),
            ],
        )?)
        .context("Transferred candidate could not be verified")?;
        check_output(run(
            &common,
            &[
                "rev-list",
                "--objects",
                "--missing=error",
                &prepared.candidate_commit,
            ],
        )?)
        .context("Transferred candidate has missing objects")?;
        prepared.verify_sources()?;
        let zero = "0".repeat(prepared.candidate_commit.len());
        check_output(run(&common, &["update-ref", &reference, &prepared.candidate_commit, &zero])?).context("Could not create integration branch; it may have appeared concurrently. No existing branch was overwritten")?;
        Ok(())
    };
    publish().with_context(|| {
        format!(
            "Inspect recovery journal {} before retrying",
            journal_path.display()
        )
    })?;
    let result = IntegrationResult {
        schema_version: 1,
        branch: branch.to_owned(),
        commit: prepared.candidate_commit.clone(),
        tree,
        target_base: prepared.report.target_base.clone(),
        sources,
        validation,
        warnings: vec![
            "Capture is not atomic; concurrent changes may escape detection.",
            "Custom merge drivers and user Git configuration are disabled during preview.",
            "Validation commands can execute arbitrary code and are run only when explicitly supplied.",
        ],
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Created integration branch {} at {}\nTree: {}\nTarget base: {}",
            result.branch, result.commit, result.tree, result.target_base
        );
        for source in &result.sources {
            println!(
                "{}: head {}, snapshot {}, included commits: {}",
                source.workspace,
                source.head,
                source.snapshot,
                source.included_commits.join(", ")
            );
        }
        if let Some(validation) = &result.validation {
            println!(
                "Validation {:?}: exit {:?}{}",
                validation.command,
                validation.exit_code,
                if validation.changed_tracked_files {
                    " (modified checkout; changes excluded)"
                } else {
                    ""
                }
            );
            if !validation.stdout.is_empty() {
                println!("Validation stdout:\n{}", validation.stdout);
            }
            if !validation.stderr.is_empty() {
                eprintln!("Validation stderr:\n{}", validation.stderr);
            }
            if validation.output_truncated {
                eprintln!("Validation output was truncated to 64 KiB per stream.");
            }
        }
    }
    if let Err(error) = fs::remove_file(&journal_path) {
        eprintln!(
            "Integration succeeded; could not remove recovery journal {}: {error}",
            journal_path.display()
        );
    }
    Ok(())
}
