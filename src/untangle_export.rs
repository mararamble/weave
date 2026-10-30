//! Untangle exports publish only a new directory. No source refs are updated.
use crate::{integration as git, untangle::Engine};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Patches,
    Commits,
}
#[derive(Serialize)]
pub(crate) struct Validation {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub engine_preview: serde_json::Value,
    pub reconstructed_tree: Option<String>,
    pub matches_capture: bool,
    pub selection_complete: bool,
    pub sequence_applicable: bool,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Plan {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub groups: Vec<String>,
    pub assignments: Vec<Option<String>>,
}
#[derive(Serialize)]
struct Entry {
    group: String,
    file: String,
    independent: bool,
    tree: String,
    commit: Option<String>,
}
#[derive(Serialize)]
pub(crate) struct Manifest {
    schema_version: u32,
    engine: &'static str,
    kind: Kind,
    snapshot_id: String,
    base: String,
    captured_tree: String,
    reconstructed_tree: String,
    complete_reconstruction: bool,
    ownership: &'static str,
    entries: Vec<Entry>,
    unassigned: Vec<String>,
    warnings: Vec<String>,
}
pub(crate) fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        bail!("Operation cancelled; no export published");
    }
    Ok(())
}
impl Engine {
    pub(crate) fn edit_plan(&self) -> Plan {
        Plan {
            schema_version: 1,
            snapshot_id: self.snapshot.id.clone(),
            groups: self.groups.iter().map(|g| g.name.clone()).collect(),
            assignments: self
                .snapshot
                .units
                .iter()
                .map(|u| u.group.clone())
                .collect(),
        }
    }
    pub(crate) fn set_plan(&mut self, plan: &Plan) -> Result<()> {
        if plan.schema_version != 1
            || plan.snapshot_id != self.snapshot.id
            || plan.assignments.len() != self.snapshot.units.len()
        {
            bail!("Stale or invalid Vineweaver review plan");
        }
        // Validate everything before changing the session.
        let mut names = std::collections::HashSet::new();
        for name in &plan.groups {
            if name.trim().is_empty() || name.chars().any(char::is_control) || !names.insert(name) {
                bail!("Invalid or duplicate group name");
            }
        }
        if plan
            .assignments
            .iter()
            .flatten()
            .any(|g| !names.contains(g))
        {
            bail!("Assignment references unknown group");
        }
        self.groups.clear();
        for name in &plan.groups {
            self.add_group(name)?;
        }
        for (unit, group) in self.snapshot.units.iter_mut().zip(&plan.assignments) {
            unit.group = group.clone();
        }
        for group in &mut self.groups {
            group.units = self
                .snapshot
                .units
                .iter()
                .filter(|u| u.group.as_deref() == Some(&group.name))
                .map(|u| u.id.clone())
                .collect();
        }
        Ok(())
    }
}
fn sequence(engine: &Engine, cancel: &AtomicBool) -> Result<Vec<String>> {
    git::checked(
        engine.temp.path(),
        &["read-tree", &engine.snapshot.base_commit],
    )?;
    let mut trees = Vec::new();
    for group in &engine.groups {
        check(cancel)?;
        let (ok, details) = engine.apply(group)?;
        if !ok {
            bail!(
                "Group {:?} does not apply in sequence: {details}",
                group.name
            );
        }
        trees.push(git::checked(engine.temp.path(), &["write-tree"])?);
    }
    Ok(trees)
}
pub(crate) fn validate(engine: &Engine, cancel: &AtomicBool) -> Result<Validation> {
    check(cancel)?;
    let preview = serde_json::to_value(engine.validate()?)?;
    check(cancel)?;
    let applicable = preview["sequence_applicable"] == true;
    let tree = if applicable {
        sequence(engine, cancel)?
            .last()
            .cloned()
            .or(Some(git::checked(
                engine.temp.path(),
                &[
                    "rev-parse",
                    &format!("{}^{{tree}}", engine.snapshot.base_commit),
                ],
            )?))
    } else {
        None
    };
    let matches_capture = tree.as_deref() == Some(&engine.snapshot.captured_tree);
    let complete = preview["selection_complete"] == true;
    if complete && applicable && !matches_capture {
        bail!("Reconstruction differs from captured aggregate; review order or ambiguous context");
    }
    engine.verify_source()?;
    check(cancel)?;
    Ok(Validation {
        schema_version: 1,
        snapshot_id: engine.snapshot.id.clone(),
        engine_preview: preview,
        reconstructed_tree: tree,
        matches_capture,
        selection_complete: complete,
        sequence_applicable: applicable,
    })
}
fn diff(dir: &Path, old: &str, new: &str) -> Result<Vec<u8>> {
    git::check_output(git::run_with(
        dir,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--binary",
            "--full-index",
            "--no-renames",
            old,
            new,
            "--",
        ],
        None,
        None,
    )?)
}
/// Verify emitted patch bytes using a clean index, not just the in-memory assignment.
fn verify_patch(dir: &Path, old: &str, new: &str, patch: &[u8]) -> Result<()> {
    git::checked(dir, &["read-tree", old])?;
    if !patch.is_empty() {
        git::check_output(git::run_with(
            dir,
            &["apply", "--cached", "--binary", "--whitespace=nowarn", "-"],
            None,
            Some(patch),
        )?)?;
    }
    if git::checked(dir, &["write-tree"])? != new {
        bail!("Export patch verification failed");
    }
    Ok(())
}
pub(crate) fn export(
    engine: &Engine,
    destination: &Path,
    kind: Kind,
    base: &str,
    allow_partial: bool,
    cancel: &AtomicBool,
) -> Result<Manifest> {
    if base != engine.snapshot.base_commit {
        bail!(
            "Export base must equal captured base {}",
            engine.snapshot.base_commit
        );
    }
    if engine.groups.is_empty() || engine.groups.iter().any(|g| g.units.is_empty()) {
        bail!("Export requires nonempty groups");
    }
    let name = destination
        .file_name()
        .context("Export requires a new directory name")?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .context("Export parent must already exist")?;
    let target = parent.join(name);
    let source = crate::workspace::get(&engine.snapshot.context.name)?;
    for protected in [
        Path::new(&source.path).canonicalize()?,
        Path::new(&source.repository).canonicalize()?,
    ] {
        if target.starts_with(protected) {
            bail!("Export destination must be outside the source workspace and Git directory");
        }
    }
    if target.symlink_metadata().is_ok() {
        bail!("Export destination already exists; nothing overwritten");
    }
    let validated = validate(engine, cancel)?;
    if !validated.sequence_applicable {
        bail!("Cannot export a non-applicable sequence; review validation warnings");
    }
    if !validated.selection_complete && !allow_partial {
        bail!("Unassigned changes remain; explicitly acknowledge partial export");
    }
    let applications = validated.engine_preview["applications"]
        .as_array()
        .context("Missing application results")?;
    if matches!(kind, Kind::Commits)
        && applications
            .iter()
            .any(|a| a["independently_applicable"] != true)
    {
        bail!(
            "Independent-commit export requires every group to apply independently; use ordered patch export for dependent groups"
        );
    }
    let staging = tempfile::Builder::new()
        .prefix(".vineweaver-export-")
        .tempdir_in(&parent)?;
    let staging_path = staging.path().to_path_buf();
    let result = (|| -> Result<Manifest> {
        let dir = engine.temp.path();
        let trees = sequence(engine, cancel)?;
        let final_tree = trees.last().context("No export trees")?.clone();
        let mut entries = Vec::new();
        let mut previous = engine.snapshot.base_commit.clone();
        // Bundle refs exist only in the disposable engine repository.
        let mut bundle_refs = Vec::new();
        for (i, (group, tree)) in engine.groups.iter().zip(&trees).enumerate() {
            check(cancel)?;
            let independent = applications[i]["independently_applicable"] == true;
            let filename = format!("{:04}.patch", i + 1);
            let patch = diff(dir, &previous, tree)?;
            verify_patch(dir, &previous, tree, &patch)?;
            fs::write(staging.path().join(&filename), &patch)?;
            let commit = if matches!(kind, Kind::Commits) {
                git::checked(dir, &["read-tree", base])?;
                let (ok, details) = engine.apply(group)?;
                if !ok {
                    bail!("Independent commit no longer applies: {details}");
                }
                let independent_tree = git::checked(dir, &["write-tree"])?;
                let commit = git::checked(
                    dir,
                    &[
                        "commit-tree",
                        &independent_tree,
                        "-p",
                        base,
                        "-m",
                        &format!(
                            "Untangle group {} (Vineweaver synthetic export)\n\nOriginal ownership unknown.",
                            group.name
                        ),
                    ],
                )?;
                let reference = format!("refs/heads/vineweaver-{:04}", i + 1);
                git::checked(dir, &["update-ref", &reference, &commit])?;
                bundle_refs.push(reference);
                Some(commit)
            } else {
                None
            };
            entries.push(Entry {
                group: group.name.clone(),
                file: filename,
                independent,
                tree: tree.clone(),
                commit,
            });
            previous = tree.clone();
        }
        let aggregate = diff(dir, base, &final_tree)?;
        verify_patch(dir, base, &final_tree, &aggregate)?;
        fs::write(staging.path().join("aggregate.patch"), aggregate)?;
        if matches!(kind, Kind::Commits) {
            let bundle = staging.path().join("commits.bundle");
            let bundle_path = bundle
                .to_str()
                .context("Bundle destination must be UTF-8")?;
            let mut args = vec!["bundle", "create", bundle_path];
            args.extend(bundle_refs.iter().map(String::as_str));
            git::checked(dir, &args)?;
            git::checked(dir, &["bundle", "verify", bundle_path])?;
        }
        let manifest = Manifest { schema_version:1,engine:"vineweaver",kind,snapshot_id:engine.snapshot.id.clone(),base:base.into(),captured_tree:engine.snapshot.captured_tree.clone(),reconstructed_tree:final_tree,complete_reconstruction:validated.selection_complete && validated.matches_capture,ownership:"unknown",entries,unassigned:engine.snapshot.units.iter().filter(|u|u.group.is_none()).map(|u|u.id.clone()).collect(),warnings:vec!["Final diff does not establish authorship. Git applicability does not establish semantic compatibility.".into(),if validated.selection_complete { "All captured units included.".into() } else { "PARTIAL: unassigned captured units are omitted.".into() },"Capture checks are non-atomic. Commit metadata identifies Vineweaver's synthetic export, not original authors.".into()] };
        fs::write(
            staging.path().join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::write(
            staging.path().join("validation.json"),
            serde_json::to_vec_pretty(&validated)?,
        )?;
        fs::write(
            staging.path().join("review-plan.json"),
            serde_json::to_vec_pretty(&engine.edit_plan())?,
        )?;
        engine.verify_source()?;
        check(cancel)?;
        publish(staging.path(), &target)?;
        // TempDir now points at the old staging name; it cannot remove the published result.
        Ok(manifest)
    })();
    match result {
        Ok(manifest) => Ok(manifest),
        Err(error) => {
            if let Err(cleanup) = staging.close() {
                return Err(error).context(format!("Export not published. Could not clean staging {}: {cleanup}. Inspect and remove that directory after the process exits.",staging_path.display()));
            }
            Err(error)
        }
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .context("Cannot publish export; destination must be absent");
    }
    Ok(())
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn publish(_: &Path, _: &Path) -> Result<()> {
    bail!("Atomic no-overwrite export publication is unsupported on this platform")
}
