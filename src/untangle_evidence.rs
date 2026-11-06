//! Separate evidence/audit contract; existing preview and export schema v1 stays intact.
use crate::{integration as git, untangle::Engine};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use weave::{
    provenance_replay::{Attribution, Report, Source},
    provenance_store::{FileState, Files, digest},
};

#[derive(Clone, Serialize)]
pub(crate) struct UnitEvidence {
    pub path_hex: String,
    pub attribution: Attribution,
    pub original_patch_hex: String,
    pub current_patch_hex: String,
    pub requires_resolution: bool,
    pub resolution: Option<String>,
}
#[derive(Clone, Serialize)]
pub(crate) struct Audit {
    pub sequence: usize,
    pub action: String,
    pub unit: Option<String>,
    pub reason: String,
    pub before: Option<Attribution>,
    pub after: Option<Attribution>,
}
#[derive(Clone, Serialize)]
pub(crate) struct ReviewEvidence {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub base_commit: String,
    pub replay: Option<Report>,
    pub units: BTreeMap<String, UnitEvidence>,
    pub audit: Vec<Audit>,
    pub acknowledged: bool,
}
impl ReviewEvidence {
    pub fn record(&mut self, action: &str, reason: &str) {
        self.acknowledged = false;
        self.audit.push(Audit {
            sequence: self.audit.len(),
            action: action.into(),
            unit: None,
            reason: reason.into(),
            before: None,
            after: None,
        });
    }
    pub fn resolve(&mut self, unit: &str, agent: Option<&str>, reason: &str) -> Result<()> {
        ensure!(
            !reason.trim().is_empty() && reason.len() <= 4096,
            "A bounded nonempty resolution reason is required"
        );
        ensure!(
            agent.is_none_or(|a| !a.trim().is_empty()
                && a.len() <= 256
                && !a.chars().any(char::is_control)),
            "Invalid manual agent identity"
        );
        let u = self.units.get_mut(unit).context("Unknown evidence unit")?;
        let before = u.attribution.clone();
        u.attribution.source = if agent.is_some() {
            Source::Manual
        } else {
            Source::Unknown
        };
        u.attribution.author_agent = agent.map(str::to_owned);
        u.resolution = Some(reason.into());
        self.audit.push(Audit {
            sequence: self.audit.len(),
            action: if agent.is_some() {
                "manual_override"
            } else {
                "leave_unknown"
            }
            .into(),
            unit: Some(unit.into()),
            reason: reason.into(),
            before: Some(before),
            after: Some(u.attribution.clone()),
        });
        self.acknowledged = false;
        Ok(())
    }
    pub fn acknowledge(&mut self, reason: &str) -> Result<()> {
        ensure!(
            !reason.trim().is_empty() && reason.len() <= 4096,
            "Acknowledgment requires a reason"
        );
        self.record("export_acknowledgment", reason);
        self.acknowledged = true;
        Ok(())
    }
    pub fn summary(&self) -> String {
        let unresolved = self
            .units
            .values()
            .filter(|u| u.requires_resolution && u.resolution.is_none())
            .count();
        format!(
            "Evidence: {} units; {} unresolved; acknowledged={}. Applicability does not establish semantic independence.",
            self.units.len(),
            unresolved,
            self.acknowledged
        )
    }
    pub fn unit_preview(&self, unit: &str) -> String {
        let Some(u) = self.units.get(unit) else {
            return "Evidence unavailable".into();
        };
        let a = &u.attribution;
        let mut text = format!(
            "Evidence {:?}\nSource worktree: {:?}\nOwner task: {:?}\nAuthor agent: {:?}\nSnapshot hash: {}\nEvents: {}\nDiagnostics: {}\nResolution: {}\nOriginal/current: original patch bytes preserved; current grouping only.\n",
            a.source,
            a.source_worktree,
            a.owner_task,
            a.author_agent,
            a.snapshot_hash,
            a.event_ids.join(", "),
            a.diagnostics.join(", "),
            u.resolution
                .as_deref()
                .unwrap_or("unresolved / not required")
        );
        if let Some(report) = &self.replay {
            for t in report.timeline.iter().filter(|t| {
                a.event_ids
                    .iter()
                    .any(|id| id == t.event.data().event_id.as_str())
            }) {
                let e = t.event.data();
                text.push_str(&format!(
                    "Timeline sequence={} event={} source={:?} attribution={:?}\n",
                    e.sequence,
                    e.event_id.as_str(),
                    e.source,
                    t.attribution.source
                ));
            }
        }
        text
    }
    pub fn check_export(&self, engine: &Engine) -> Result<()> {
        ensure!(
            self.snapshot_id == engine.snapshot.id
                && self.base_commit == engine.snapshot.base_commit,
            "Stale evidence review"
        );
        ensure!(
            self.acknowledged,
            "Provenance export requires explicit evidence/semantic review acknowledgment"
        );
        for unit in engine.snapshot.units.iter().filter(|u| u.group.is_some()) {
            let evidence = self.units.get(&unit.id).context("Missing unit evidence")?;
            ensure!(
                evidence.original_patch_hex == unit.patch_hex
                    && evidence.current_patch_hex == unit.patch_hex,
                "Original patch evidence changed"
            );
            ensure!(
                !evidence.requires_resolution || evidence.resolution.is_some(),
                "Conflicting evidence must be explicitly resolved or left unknown before export"
            );
        }
        Ok(())
    }
}

fn tree_files(engine: &Engine, tree: &str, needed: &BTreeSet<String>) -> Result<Files> {
    let bytes = git::check_output(git::run_with(
        engine.temp.path(),
        &["ls-tree", "-rz", "--full-tree", tree],
        None,
        None,
    )?)?;
    let mut files = BTreeMap::new();
    for entry in bytes.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .context("Invalid evidence tree")?;
        let path = entry[tab + 1..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if !needed.contains(&path) {
            continue;
        }
        let fields = std::str::from_utf8(&entry[..tab])?
            .split(' ')
            .collect::<Vec<_>>();
        ensure!(fields.len() == 3, "Invalid evidence tree entry");
        let bytes = git::check_output(git::run_with(
            engine.temp.path(),
            &["cat-file", "blob", fields[2]],
            None,
            None,
        )?)?;
        files.insert(
            path,
            FileState {
                hash: digest(&bytes),
                mode: fields[0].into(),
            },
        );
    }
    Ok(files)
}
impl Engine {
    pub(crate) fn enable_evidence(&mut self) -> Result<()> {
        let report = crate::provenance_cli::load(&self.snapshot.context.name)?;
        let needed = self
            .snapshot
            .units
            .iter()
            .filter(|u| {
                report
                    .as_ref()
                    .is_some_and(|r| r.paths.contains_key(&u.path_hex))
            })
            .map(|u| u.path_hex.clone())
            .collect::<BTreeSet<_>>();
        let baseline = report
            .as_ref()
            .map(|_| tree_files(self, &self.snapshot.base_commit, &needed))
            .transpose()?
            .unwrap_or_default();
        let current = report
            .as_ref()
            .map(|_| tree_files(self, &self.snapshot.captured_tree, &needed))
            .transpose()?
            .unwrap_or_default();
        let mut units = BTreeMap::new();
        for unit in &self.snapshot.units {
            let mut a = Attribution {
                source: Source::Unknown,
                source_worktree: report.as_ref().map(|r| r.origin.worktree_id.clone()),
                owner_task: None,
                author_agent: None,
                event_ids: Vec::new(),
                snapshot_hash: report
                    .as_ref()
                    .map(|r| r.snapshot_hash.clone())
                    .unwrap_or_default(),
                diagnostics: vec!["no_event_mapping".into()],
            };
            let mut requires_resolution = false;
            if let Some(r) = &report
                && let Some(found) = r.paths.get(&unit.path_hex)
            {
                a = found.clone();
                if a.event_ids.len() > 256 {
                    a.event_ids.truncate(256);
                    a.diagnostics
                        .push("event_references_bounded_see_replay_path_and_timeline".into());
                }
                if baseline.get(&unit.path_hex) != r.baseline.get(&unit.path_hex)
                    || current.get(&unit.path_hex) != r.files.get(&unit.path_hex)
                    || r.origin.base_commit != self.snapshot.base_commit
                {
                    a.source = Source::Unknown;
                    a.author_agent = None;
                    a.diagnostics.push("snapshot_or_base_mismatch".into());
                }
                requires_resolution = !a.diagnostics.is_empty();
            }
            units.insert(
                unit.id.clone(),
                UnitEvidence {
                    path_hex: unit.path_hex.clone(),
                    attribution: a,
                    original_patch_hex: unit.patch_hex.clone(),
                    current_patch_hex: unit.patch_hex.clone(),
                    requires_resolution,
                    resolution: None,
                },
            );
        }
        self.evidence = Some(ReviewEvidence {
            schema_version: 1,
            snapshot_id: self.snapshot.id.clone(),
            base_commit: self.snapshot.base_commit.clone(),
            replay: report,
            units,
            audit: Vec::new(),
            acknowledged: false,
        });
        Ok(())
    }
    pub(crate) fn verify_evidence(&self) -> Result<()> {
        if let Some(evidence) = &self.evidence {
            let now = crate::provenance_cli::load(&self.snapshot.context.name)?;
            ensure!(
                now.as_ref().and_then(|r| r.journal_hash.as_ref())
                    == evidence
                        .replay
                        .as_ref()
                        .and_then(|r| r.journal_hash.as_ref()),
                "Provenance changed since capture; recapture and review"
            );
        }
        Ok(())
    }
}
