//! Vineweaver: snapshot-scoped manual grouping. Temporary objects live exactly as long as Engine.
use crate::{changeset, integration as isolated, workspace};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;
use std::{collections::HashSet, path::Path};

#[derive(Args)]
pub(crate) struct Options {
    pub name: String,
    /// Allow capture and export of untracked file content
    #[arg(long)]
    pub include_untracked_content: bool,
    /// Open the Vineweaver interactive review session
    #[arg(long, conflicts_with_all = ["json", "export_patches", "export_commits"])]
    pub review: bool,
    /// Export ordered patches into a new directory; never overwrite
    #[arg(long, requires = "base", conflicts_with = "export_commits")]
    pub export_patches: Option<std::path::PathBuf>,
    /// Export independent commits as a portable bundle in a new directory
    #[arg(long, requires = "base")]
    pub export_commits: Option<std::path::PathBuf>,
    /// Explicit export base; must equal the recorded capture base commit
    #[arg(long)]
    pub base: Option<String>,
    /// Acknowledge export of a selection with unassigned captured units
    #[arg(long)]
    pub allow_partial: bool,
    #[arg(long)]
    pub json: bool,
    /// Expected capture identity, required when assigning previously displayed units
    #[arg(long)]
    pub snapshot: Option<String>,
    /// Create a named group (repeatable); order is creation order unless --order is supplied
    #[arg(long = "group")]
    pub groups: Vec<String>,
    /// UNIT=GROUP (repeatable); a unit may have only one assignment
    #[arg(long = "assign", requires = "snapshot")]
    pub assignments: Vec<String>,
    /// Clear an assignment after processing --assign (repeatable)
    #[arg(long = "unassign", requires = "snapshot")]
    pub unassign: Vec<String>,
    /// Group name in validation sequence; supply every group exactly once
    #[arg(long = "order")]
    pub order: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ownership {
    Unknown,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnitKind {
    Hunk,
    WholeFile,
}
#[derive(Serialize)]
pub(crate) struct Hunk {
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
}
fn hunk_range(patch: &[u8]) -> Result<Option<Hunk>> {
    let Some(header) = patch.split(|b| *b == b'\n').find(|l| l.starts_with(b"@@ ")) else {
        return Ok(None);
    };
    let mut fields = header.split(|b| *b == b' ');
    fields.next();
    fn range(value: &[u8], prefix: u8) -> Result<(u64, u64)> {
        let raw = value
            .strip_prefix(&[prefix])
            .context("Invalid hunk range")?;
        let text = std::str::from_utf8(raw)?;
        let (start, count) = text.split_once(',').unwrap_or((text, "1"));
        Ok((start.parse()?, count.parse()?))
    }
    let (old_start, old_lines) = range(fields.next().context("Missing old range")?, b'-')?;
    let (new_start, new_lines) = range(fields.next().context("Missing new range")?, b'+')?;
    Ok(Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
    }))
}
#[derive(Serialize)]
pub(crate) struct Unit {
    pub id: String,
    pub path: String,
    pub path_hex: String,
    pub kind: UnitKind,
    pub hunk: Option<Hunk>,
    pub ownership: Ownership,
    pub group: Option<String>,
    /// Exact Git patch bytes, including non-UTF-8 content and quoted paths.
    pub patch_hex: String,
    #[serde(skip)]
    pub(crate) patch: Vec<u8>,
}
#[derive(Serialize)]
pub(crate) struct File {
    pub path_hex: String,
    pub units: Vec<String>,
    pub split_reason: String,
}
#[derive(Serialize)]
pub(crate) struct Group {
    pub name: String,
    pub units: Vec<String>,
}
#[derive(Serialize)]
pub(crate) struct Diagnostic {
    pub code: String,
    pub message: String,
}
#[derive(Serialize)]
pub(crate) struct Snapshot {
    pub schema_version: u32,
    pub id: String,
    pub base_commit: String,
    pub captured_tree: String,
    pub complete: bool,
    pub context: changeset::Changeset,
    pub files: Vec<File>,
    pub units: Vec<Unit>,
}
#[derive(Serialize)]
pub(crate) struct Application {
    pub group: String,
    pub independently_applicable: bool,
    pub sequence_applicable: Option<bool>,
    /// Observed applicability after this prefix, not a claim of a causal dependency.
    pub applicable_after_prefix: Vec<String>,
    pub warnings: Vec<Diagnostic>,
}
#[derive(Serialize)]
pub(crate) struct Preview<'a> {
    schema_version: u32,
    snapshot: &'a Snapshot,
    groups: &'a [Group],
    unassigned: Vec<&'a str>,
    selection_complete: bool,
    sequence_applicable: bool,
    applications: Vec<Application>,
    warnings: Vec<Diagnostic>,
}

pub(crate) struct Engine {
    pub(crate) temp: tempfile::TempDir,
    pub snapshot: Snapshot,
    pub groups: Vec<Group>,
    include_untracked_content: bool,
}
fn diagnostic(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: message.into(),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(dir: &Path, bytes: &[u8]) -> Result<String> {
    Ok(String::from_utf8(isolated::check_output(isolated::run_with(
        dir,
        &["hash-object", "--stdin"],
        None,
        Some(bytes),
    )?)?)?
    .trim()
    .into())
}
fn index_bytes(name: &str) -> Result<Vec<u8>> {
    let record = workspace::get(name)?;
    let out = crate::git_process::bounded_output(
        crate::git_process::command_at(Path::new(&record.path))
            .args(["rev-parse", "--absolute-git-dir"]),
        65536,
    )?;
    if !out.status.success() {
        bail!("Cannot locate source index");
    }
    isolated::read_index(&crate::path_from_bytes(&out.stdout).join("index"))
}
fn identity(dir: &Path, set: &changeset::Changeset, tree: &str, index: &[u8]) -> Result<String> {
    let mut bytes = serde_json::to_vec(set)?;
    bytes.extend_from_slice(tree.as_bytes());
    bytes.extend_from_slice(index);
    hash(dir, &bytes)
}
/// Split only Git-generated, ordinary text modifications at existing context hunks.
/// A contiguous/overlapping hunk is indivisible. Special files always remain whole.
fn pieces(patch: &[u8]) -> (Vec<Vec<u8>>, &'static str) {
    let lines: Vec<_> = patch.split_inclusive(|b| *b == b'\n').collect();
    let ordinary = lines.iter().any(|l| {
        l.starts_with(b"index ") && l.ends_with(b" 100644\n")
            || l.starts_with(b"index ") && l.ends_with(b" 100755\n")
    }) && !lines.iter().any(|l| {
        l.starts_with(b"old mode ")
            || l.starts_with(b"new mode ")
            || l.starts_with(b"new file mode ")
            || l.starts_with(b"deleted file mode ")
            || l.starts_with(b"GIT binary patch")
    });
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with(b"@@ "))
        .map(|(i, _)| i)
        .collect();
    if !ordinary || starts.is_empty() {
        return (
            vec![patch.to_vec()],
            "Whole-file operation (binary, addition, deletion, mode/type change, or unsupported text split)",
        );
    }
    let header: Vec<u8> = lines[..starts[0]].concat();
    if header
        .len()
        .saturating_mul(starts.len().saturating_sub(1))
        .saturating_add(patch.len())
        > 16 * 1024 * 1024
    {
        return (
            vec![patch.to_vec()],
            "Whole-file operation: splitting would exceed the 16 MiB patch limit",
        );
    }
    let result = starts
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let end = starts.get(i + 1).copied().unwrap_or(lines.len());
            let mut part = header.clone();
            part.extend(lines[*start..end].concat());
            part
        })
        .collect();
    (
        result,
        "Existing Git context hunks; overlapping/adjacent edits within one hunk cannot be split",
    )
}
impl Engine {
    pub(crate) fn capture(name: &str, include_untracked_content: bool) -> Result<Self> {
        let record = workspace::get(name)?;
        crate::git_process::ensure_no_filters(Path::new(&record.path))?;
        let before = index_bytes(name)?;
        let context = changeset::collect(name, include_untracked_content)?;
        let temp = isolated::temporary_repository()?;
        let dir = temp.path();
        let commit = isolated::snapshot(dir, &context)?;
        let tree = isolated::checked(dir, &["rev-parse", &format!("{commit}^{{tree}}")])?;
        if before != index_bytes(name)? {
            bail!("Source index changed during capture; retry");
        }
        let id = identity(dir, &context, &tree, &before)?;
        let raw = isolated::check_output(isolated::run_with(
            dir,
            &[
                "diff",
                "--name-status",
                "-z",
                "--no-renames",
                &context.base_commit,
                &tree,
                "--",
            ],
            None,
            None,
        )?)?;
        let mut units = Vec::new();
        let mut files = Vec::new();
        let patch = isolated::check_output(isolated::run_with(
            dir,
            &[
                "-c",
                "core.quotePath=true",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--binary",
                "--full-index",
                "--no-renames",
                "--diff-algorithm=myers",
                "--unified=3",
                &context.base_commit,
                &tree,
                "--",
            ],
            None,
            None,
        )?)?;
        // Git quotes newline path bytes; body lines carry a diff prefix (binary
        // payloads have no spaces), so file boundaries are unambiguous in bytes.
        let mut patches: Vec<Vec<u8>> = Vec::new();
        for line in patch.split_inclusive(|b| *b == b'\n') {
            if line.starts_with(b"diff --git ") {
                patches.push(Vec::new());
            }
            patches
                .last_mut()
                .context("Unexpected Git patch header")?
                .extend_from_slice(line);
        }
        let changed = changeset::parse_name_status(&raw)?;
        if changed.len() != patches.len() {
            bail!("Patch/file listing mismatch; capture incomplete");
        }
        let mut unit_bytes = 0usize;
        for (file, patch) in changed.into_iter().zip(patches) {
            if patch.split(|b| *b == b'\n').any(|l| {
                (l.starts_with(b"index ")
                    || l.starts_with(b"old mode ")
                    || l.starts_with(b"new mode ")
                    || l.starts_with(b"new file mode ")
                    || l.starts_with(b"deleted file mode "))
                    && l.ends_with(b"160000")
            }) {
                bail!("Submodule changes are unsupported; capture incomplete");
            }
            let (parts, reason) = pieces(&patch);
            let mut ids = Vec::new();
            let split = parts.len() > 1 || reason.starts_with("Existing");
            for part in parts {
                unit_bytes = unit_bytes.saturating_add(part.len());
                if unit_bytes > 16 * 1024 * 1024 {
                    bail!("Generated patch units exceed 16 MiB");
                }
                let uid = format!("{id}:u{}", units.len() + 1);
                ids.push(uid.clone());
                units.push(Unit {
                    id: uid,
                    path: String::from_utf8_lossy(&file.path).into(),
                    path_hex: hex(&file.path),
                    kind: if split {
                        UnitKind::Hunk
                    } else {
                        UnitKind::WholeFile
                    },
                    hunk: if split { hunk_range(&part)? } else { None },
                    ownership: Ownership::Unknown,
                    group: None,
                    patch_hex: hex(&part),
                    patch: part,
                });
            }
            files.push(File {
                path_hex: hex(&file.path),
                units: ids,
                split_reason: reason.into(),
            });
        }
        let engine = Self {
            temp,
            snapshot: Snapshot {
                schema_version: 1,
                id,
                base_commit: context.base_commit.clone(),
                captured_tree: tree,
                complete: true,
                context,
                files,
                units,
            },
            groups: Vec::new(),
            include_untracked_content,
        };
        engine.verify_source()?;
        // Every generated unit must reconstruct the exact captured tree; never silently lose content.
        isolated::checked(
            engine.temp.path(),
            &["read-tree", &engine.snapshot.base_commit],
        )?;
        let all = engine
            .snapshot
            .units
            .iter()
            .flat_map(|u| u.patch.iter().copied())
            .collect::<Vec<_>>();
        if !all.is_empty() {
            isolated::check_output(isolated::run_with(
                engine.temp.path(),
                &["apply", "--cached", "--binary", "--whitespace=nowarn", "-"],
                None,
                Some(&all),
            )?)?;
        }
        if isolated::checked(engine.temp.path(), &["write-tree"])? != engine.snapshot.captured_tree
        {
            bail!("Patch units do not reconstruct capture; unsupported split");
        }
        Ok(engine)
    }
    pub(crate) fn verify_source(&self) -> Result<()> {
        let name = &self.snapshot.context.name;
        let record = workspace::get(name)?;
        crate::git_process::ensure_no_filters(Path::new(&record.path))?;
        let before = index_bytes(name)?;
        let now = changeset::collect(name, self.include_untracked_content)?;
        let commit = isolated::snapshot(self.temp.path(), &now)?;
        let tree = isolated::checked(
            self.temp.path(),
            &["rev-parse", &format!("{commit}^{{tree}}")],
        )?;
        if before != index_bytes(name)?
            || identity(self.temp.path(), &now, &tree, &before)? != self.snapshot.id
        {
            bail!("Source changed since capture; recapture and reassign");
        }
        Ok(())
    }
    pub(crate) fn add_group(&mut self, name: &str) -> Result<()> {
        if name.trim().is_empty()
            || name.chars().any(char::is_control)
            || self.groups.iter().any(|g| g.name == name)
        {
            bail!("Invalid or duplicate group name: {name:?}");
        }
        self.groups.push(Group {
            name: name.into(),
            units: Vec::new(),
        });
        Ok(())
    }
    pub(crate) fn assign(&mut self, snapshot: &str, unit: &str, group: Option<&str>) -> Result<()> {
        if snapshot != self.snapshot.id {
            bail!("Stale snapshot identity; recapture and reassign");
        }
        if let Some(g) = group
            && !self.groups.iter().any(|entry| entry.name == g)
        {
            bail!("Unknown group: {g}");
        }
        let u = self
            .snapshot
            .units
            .iter_mut()
            .find(|u| u.id == unit)
            .context("Unknown or stale unit ID")?;
        if group.is_some() && u.group.is_some() {
            bail!("Conflicting assignment; unassign unit first");
        }
        for g in &mut self.groups {
            g.units.retain(|id| id != unit);
        }
        u.group = group.map(str::to_owned);
        // Canonical captured unit order, irrespective of assignment argument order.
        for g in &mut self.groups {
            g.units = self
                .snapshot
                .units
                .iter()
                .filter(|u| u.group.as_deref() == Some(&g.name))
                .map(|u| u.id.clone())
                .collect();
        }
        Ok(())
    }
    pub(crate) fn apply(&self, group: &Group) -> Result<(bool, String)> {
        let patch: Vec<_> = self
            .snapshot
            .units
            .iter()
            .filter(|u| u.group.as_deref() == Some(&group.name))
            .flat_map(|u| u.patch.iter().copied())
            .collect();
        if patch.is_empty() {
            return Ok((true, String::new()));
        }
        let out = isolated::run_with(
            self.temp.path(),
            &["apply", "--cached", "--binary", "--whitespace=nowarn", "-"],
            None,
            Some(&patch),
        )?;
        match out.status.code() {
            Some(0) => Ok((true, String::new())),
            Some(_) => Ok((false, String::from_utf8_lossy(&out.stderr).into())),
            _ => bail!(
                "Git apply operational failure: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }
    pub(crate) fn validate(&self) -> Result<Preview<'_>> {
        self.verify_source()?;
        let dir = self.temp.path();
        let base = &self.snapshot.base_commit;
        let mut applications = Vec::new();
        for group in &self.groups {
            isolated::checked(dir, &["read-tree", base])?;
            let (ok, detail) = self.apply(group)?;
            applications.push(Application { group: group.name.clone(), independently_applicable: ok, sequence_applicable: None, applicable_after_prefix: Vec::new(), warnings: if ok { Vec::new() } else { vec![diagnostic("not_applicable", format!("Cannot apply against base. Review context or keep related units together; failure does not prove a dependency. {detail}"))] } });
        }
        isolated::checked(dir, &["read-tree", base])?;
        let mut sequence_ok = true;
        let mut prefix = Vec::new();
        for (group, result) in self.groups.iter().zip(&mut applications) {
            if !sequence_ok {
                result.warnings.push(diagnostic(
                    "sequence_not_evaluated",
                    "Earlier group failed; sequence stopped",
                ));
                continue;
            }
            let (ok, detail) = self.apply(group)?;
            result.sequence_applicable = Some(ok);
            if ok && !result.independently_applicable {
                result.applicable_after_prefix = prefix.clone();
                result.warnings.push(diagnostic("observed_prefix_dependency", "Applicable after this prefix but not independently; exact causal dependency is uncertain"));
            }
            if !ok {
                result
                    .warnings
                    .push(diagnostic("sequence_not_applicable", detail));
            }
            sequence_ok = ok;
            prefix.push(group.name.clone());
        }
        self.verify_source()?;
        let unassigned: Vec<_> = self
            .snapshot
            .units
            .iter()
            .filter(|u| u.group.is_none())
            .map(|u| u.id.as_str())
            .collect();
        let mut warnings = vec![
            diagnostic(
                "non_atomic_capture",
                "Source identities checked before/after capture and validation; concurrent edits between checks may be missed",
            ),
            diagnostic(
                "unknown_ownership",
                "Final diff does not establish authorship; all ownership is unknown",
            ),
            diagnostic(
                "rename_representation",
                "Renames are represented as whole-file deletion/addition units; assign both together to preserve a move",
            ),
        ];
        if !unassigned.is_empty() {
            warnings.push(diagnostic(
                "unassigned_changes",
                "Selected groups omit captured units; reconstruction is incomplete",
            ));
        }
        Ok(Preview {
            schema_version: 1,
            snapshot: &self.snapshot,
            groups: &self.groups,
            selection_complete: unassigned.is_empty(),
            unassigned,
            sequence_applicable: sequence_ok,
            applications,
            warnings,
        })
    }
}

pub(crate) fn show(options: Options) -> Result<()> {
    if options.review {
        return crate::untangle_review::run(options);
    }
    let mut engine = Engine::capture(&options.name, options.include_untracked_content)?;
    if let Some(expected) = &options.snapshot
        && expected != &engine.snapshot.id
    {
        bail!("Stale snapshot identity; recapture and reassign");
    }
    for group in &options.groups {
        engine.add_group(group)?;
    }
    for assignment in &options.assignments {
        let (unit, group) = assignment
            .split_once('=')
            .context("Use --assign UNIT=GROUP")?;
        engine.assign(
            options
                .snapshot
                .as_deref()
                .context("Assignment requires --snapshot")?,
            unit,
            Some(group),
        )?;
    }
    for unit in &options.unassign {
        engine.assign(
            options
                .snapshot
                .as_deref()
                .context("Unassignment requires --snapshot")?,
            unit,
            None,
        )?;
    }
    if !options.order.is_empty() {
        let unique: HashSet<_> = options.order.iter().collect();
        if unique.len() != engine.groups.len()
            || unique.len() != options.order.len()
            || engine.groups.iter().any(|g| !unique.contains(&g.name))
        {
            bail!("--order must include every group exactly once");
        }
        engine
            .groups
            .sort_by_key(|g| options.order.iter().position(|n| n == &g.name).unwrap());
    }
    if options.export_patches.is_some() || options.export_commits.is_some() {
        let (destination, kind) = if let Some(path) = &options.export_patches {
            (path, crate::untangle_export::Kind::Patches)
        } else {
            (
                options.export_commits.as_ref().unwrap(),
                crate::untangle_export::Kind::Commits,
            )
        };
        let report = crate::untangle_export::export(
            &engine,
            destination,
            kind,
            options.base.as_deref().context("Export requires --base")?,
            options.allow_partial,
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        if options.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("Vineweaver export: {}", destination.display());
        }
        return Ok(());
    }
    let preview = engine.validate()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&preview)?);
    } else {
        println!(
            "Snapshot {}\nBase {}\nCaptured tree {}",
            engine.snapshot.id, engine.snapshot.base_commit, engine.snapshot.captured_tree
        );
        for u in &engine.snapshot.units {
            if let Some(h) = &u.hunk {
                println!(
                    "Hunk: -{},{} +{},{}",
                    h.old_start, h.old_lines, h.new_start, h.new_lines
                );
            }
            println!(
                "{} {:?} {} -> {} (ownership unknown)",
                u.id,
                u.kind_name(),
                u.path.escape_default(),
                u.group.as_deref().unwrap_or("unassigned")
            );
        }
        for a in &preview.applications {
            println!(
                "Group {:?}: independently applicable={}, sequence={:?}",
                a.group, a.independently_applicable, a.sequence_applicable
            );
            for w in &a.warnings {
                println!("Warning [{}]: {}", w.code, w.message.escape_default());
            }
        }
        println!(
            "Unassigned: {}; sequence applicable: {}",
            preview.unassigned.len(),
            preview.sequence_applicable
        );
        for w in preview.warnings {
            println!("Warning [{}]: {}", w.code, w.message);
        }
    }
    Ok(())
}
impl Unit {
    fn kind_name(&self) -> &str {
        match self.kind {
            UnitKind::Hunk => "hunk",
            UnitKind::WholeFile => "whole_file",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_hunks_preserve_non_utf8_content_and_ranges() {
        let patch = b"diff --git \"a/bad\\377\" \"b/bad\\377\"\nindex aaa..bbb 100644\n--- \"a/bad\\377\"\n+++ \"b/bad\\377\"\n@@ -1 +1 @@\n-old\n+\xff\n@@ -20,0 +21,2 @@\n+diff --git fake\n+another\n";
        let (parts, _) = pieces(patch);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains(&0xff));
        let first = hunk_range(&parts[0]).unwrap().unwrap();
        assert_eq!(
            (first.old_start, first.old_lines, first.new_lines),
            (1, 1, 1)
        );
        let second = hunk_range(&parts[1]).unwrap().unwrap();
        assert_eq!(
            (
                second.old_start,
                second.old_lines,
                second.new_start,
                second.new_lines
            ),
            (20, 0, 21, 2)
        );
        assert!(hunk_range(b"@@ invalid @@\n").is_err());
    }
}
