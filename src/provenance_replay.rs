//! Deterministic evidence reconciliation. Local hashes prove consistency only.
//! An authenticated actor binding must be checked by an explicitly trusted adapter.
use crate::{
    provenance::{Authorship, ContentState, Event, Evidence},
    provenance_store::{self as store, Checkpoint, Entry, FileState, Files, Origin, Store},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    VerifiedEvent,
    VerifiedWorktree,
    Manual,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    pub source: Source,
    pub source_worktree: Option<String>,
    pub owner_task: Option<String>,
    pub author_agent: Option<String>,
    pub event_ids: Vec<String>,
    pub snapshot_hash: String,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub event: Event,
    pub attribution: Attribution,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub origin: Origin,
    pub journal_hash: Option<String>,
    pub snapshot_hash: String,
    pub baseline: Files,
    pub files: Files,
    pub paths: BTreeMap<String, Attribution>,
    pub timeline: Vec<Timeline>,
    pub diagnostics: Vec<String>,
    pub corrections: Vec<Entry>,
}
/// Implementors are trusted integrations, not data loaded from a journal. They
/// must validate the actor/session capability and observation receipts against
/// their own trust anchor. Merely accepting an agent name or hash is insufficient.
pub trait ActorVerifier {
    fn verify(&self, event: &Event, before: Option<&[u8]>, after: Option<&[u8]>) -> bool;
}
type EventsByPath<'a> = BTreeMap<String, Vec<(&'a Checkpoint, &'a Event)>>;
fn id(e: &Evidence<crate::provenance::Id>) -> Option<String> {
    match e {
        Evidence::Known { value } => Some(value.as_str().into()),
        _ => None,
    }
}
fn state(s: Option<&FileState>) -> ContentState {
    match s {
        Some(s) => ContentState::Hash {
            hash: crate::provenance::ContentHash {
                algorithm: "sha256".to_string().try_into().unwrap(),
                digest: s.hash.clone().try_into().unwrap(),
            },
        },
        None => ContentState::Absent {},
    }
}

pub fn replay(store: &Store, stream: &str, verifier: Option<&dyn ActorVerifier>) -> Result<Report> {
    let history = store.history()?;
    let mut selected: Vec<&Checkpoint> = Vec::new();
    let mut corrections = Vec::new();
    for r in &history.records {
        match &r.entry {
            Entry::Checkpoint(cp) if cp.origin.stream_id == stream => selected.push(cp),
            Entry::Correction { .. } => corrections.push(r.entry.clone()),
            _ => {}
        }
    }
    let first = selected
        .first()
        .ok_or_else(|| anyhow::anyhow!("No recorded baseline for this stream"))?;
    let mut report = Report {
        schema_version: 1,
        origin: first.origin.clone(),
        journal_hash: history.head.hash.clone(),
        snapshot_hash: first.snapshot_hash.clone(),
        baseline: first.baseline.clone(),
        files: first.baseline.clone(),
        paths: BTreeMap::new(),
        timeline: Vec::new(),
        diagnostics: vec!["Capture is non-atomic; local hashes do not authenticate the journal owner or establish semantic independence.".into()],
        corrections: Vec::new(),
    };
    let mut ids = BTreeSet::new();
    let mut previous_sequence: Option<u64> = None;
    for cp in selected {
        store.validate_checkpoint(cp)?;
        ensure!(
            cp.origin == report.origin && cp.baseline == report.baseline,
            "Contradictory stream identity or baseline"
        );
        ensure!(
            cp.snapshot_hash == store::digest(&serde_json::to_vec(&cp.files)?),
            "Snapshot hash mismatch"
        );
        for s in cp.baseline.values().chain(cp.files.values()) {
            store.blob(&s.hash)?;
        }
        let mut changed = BTreeSet::new();
        for event in &cp.events {
            let e = event.data();
            let path = String::from(e.path_hex.clone());
            let mut a = Attribution {
                source: Source::Unknown,
                source_worktree: id(&e.worktree_id),
                owner_task: id(&e.task_id),
                author_agent: None,
                event_ids: vec![e.event_id.as_str().into()],
                snapshot_hash: cp.snapshot_hash.clone(),
                diagnostics: Vec::new(),
            };
            let sequence_ok = match previous_sequence {
                None => e.sequence == 0,
                Some(previous) => previous.checked_add(1) == Some(e.sequence),
            };
            previous_sequence = Some(e.sequence);
            let identity_ok = e.stream_id.as_str() == stream
                && id(&e.worktree_id).as_deref() == Some(&cp.origin.worktree_id)
                && id(&e.base_commit).as_deref() == Some(&cp.origin.base_commit);
            let snapshot_ok = id(&e.snapshot_ref).as_deref() == Some(cp.snapshot_hash.as_str());
            let chain_ok =
                e.before == state(report.files.get(&path)) && e.after == state(cp.files.get(&path));
            let unique = ids.insert(e.event_id.as_str().to_owned()) && changed.insert(path.clone());
            let observed = matches!(
                e.before,
                ContentState::Hash { .. } | ContentState::Absent { .. }
            ) && matches!(
                e.after,
                ContentState::Hash { .. } | ContentState::Absent { .. }
            );
            if !sequence_ok {
                a.diagnostics.push("missing_or_out_of_order_events".into());
            }
            if !identity_ok {
                a.diagnostics.push("contradictory_identity".into());
            }
            if !snapshot_ok {
                a.diagnostics
                    .push("missing_or_contradictory_snapshot_reference".into());
            }
            if !chain_ok || !observed {
                a.diagnostics.push("conflicting_or_concurrent_write".into());
            }
            if !unique {
                a.diagnostics.push("duplicate_or_overlapping_event".into());
            }
            if sequence_ok && identity_ok && snapshot_ok && chain_ok && unique && observed {
                a.source = Source::VerifiedWorktree;
                match &e.authorship {
                    Authorship::Verified { agent_id, .. } => {
                        let before = report
                            .files
                            .get(&path)
                            .map(|f| store.blob(&f.hash))
                            .transpose()?;
                        let after = cp
                            .files
                            .get(&path)
                            .map(|f| store.blob(&f.hash))
                            .transpose()?;
                        if verifier
                            .is_some_and(|v| v.verify(event, before.as_deref(), after.as_deref()))
                        {
                            a.source = Source::VerifiedEvent;
                            a.author_agent = Some(agent_id.as_str().into());
                        } else {
                            a.source = Source::Unknown;
                            a.diagnostics.push("untrusted_actor_binding".into());
                        }
                    }
                    Authorship::Manual {
                        claimed_agent_id, ..
                    } => {
                        a.source = Source::Manual;
                        a.author_agent = id(claimed_agent_id);
                    }
                    Authorship::Unknown { .. } => {
                        a.source = Source::Unknown;
                        a.diagnostics.push("unobserved_authorship".into());
                    }
                    Authorship::WorktreeOnly {} => {}
                }
            }
            let previous = report.paths.remove(&path);
            if previous.is_some() {
                // Without verified line-level observations, multiple writes to a
                // path cannot be assigned to resulting hunks without ambiguity.
                a.source = Source::Unknown;
                a.author_agent = None;
                a.diagnostics
                    .push("multiple_writes_require_hunk_review".into());
            }
            report.timeline.push(Timeline {
                event: event.clone(),
                attribution: a.clone(),
            });
            if let Some(previous) = previous {
                let mut references = previous.event_ids;
                references.append(&mut a.event_ids);
                a.event_ids = references;
            }
            report.paths.insert(path, a);
        }
        for path in report
            .files
            .keys()
            .chain(cp.files.keys())
            .collect::<BTreeSet<_>>()
        {
            if report.files.get(path) != cp.files.get(path) && !changed.contains(path) {
                report.paths.insert(
                    path.clone(),
                    Attribution {
                        source: Source::Unknown,
                        source_worktree: Some(cp.origin.worktree_id.clone()),
                        owner_task: None,
                        author_agent: None,
                        event_ids: Vec::new(),
                        snapshot_hash: cp.snapshot_hash.clone(),
                        diagnostics: vec!["unrecorded_change".into()],
                    },
                );
            }
        }
        // Keep the observed state even after bad evidence; do not fabricate a
        // valid event chain to explain it. The uncertainty stays in the report.
        report.files = cp.files.clone();
        report.snapshot_hash = cp.snapshot_hash.clone();
        if !cp.excluded.is_empty() {
            report.diagnostics.push("privacy_exclusions_present".into());
        }
    }
    // Different worktrees are independent streams. Surface common changed paths
    // for explicit review; never order those streams by wall-clock timestamps or
    // assume that separate/disjoint patches are semantically independent.
    let mut other_streams = BTreeMap::new();
    let mut other_events: BTreeMap<String, EventsByPath<'_>> = BTreeMap::new();
    for record in &history.records {
        if let Entry::Checkpoint(cp) = &record.entry
            && cp.origin.stream_id != stream
            && cp.origin.base_commit == report.origin.base_commit
        {
            other_streams.insert(cp.origin.stream_id.clone(), cp);
            for event in &cp.events {
                other_events
                    .entry(cp.origin.stream_id.clone())
                    .or_default()
                    .entry(String::from(event.data().path_hex.clone()))
                    .or_default()
                    .push((cp, event));
            }
        }
    }
    for other in other_streams.values() {
        let overlapping = report
            .paths
            .keys()
            .filter(|path| other.baseline.get(*path) != other.files.get(*path))
            .cloned()
            .collect::<Vec<_>>();
        if overlapping.is_empty() {
            continue;
        }
        store.validate_checkpoint(other)?;
        for path in overlapping {
            let a = report.paths.get_mut(&path).unwrap();
            a.source = Source::Unknown;
            a.author_agent = None;
            a.diagnostics
                .push("cross_worktree_overlap_requires_review".into());
            if let Some(events) = other_events
                .get(&other.origin.stream_id)
                .and_then(|paths| paths.get(&path))
            {
                for (cp, event) in events {
                    let e = event.data();
                    a.event_ids.push(e.event_id.as_str().into());
                    report.timeline.push(Timeline {
                        event: (*event).clone(),
                        attribution: Attribution {
                            source: Source::Unknown,
                            source_worktree: id(&e.worktree_id),
                            owner_task: id(&e.task_id),
                            author_agent: None,
                            event_ids: vec![e.event_id.as_str().into()],
                            snapshot_hash: cp.snapshot_hash.clone(),
                            diagnostics: vec!["cross_worktree_overlap_requires_review".into()],
                        },
                    });
                }
            }
        }
    }
    report.timeline.sort_by(|a, b| {
        let a = a.event.data();
        let b = b.event.data();
        (a.stream_id.as_str(), a.sequence, a.event_id.as_str()).cmp(&(
            b.stream_id.as_str(),
            b.sequence,
            b.event_id.as_str(),
        ))
    });
    for correction in corrections {
        if let Entry::Correction {
            event_id,
            assertion,
            reason,
        } = &correction
        {
            if !ids.contains(event_id) {
                continue;
            }
            ensure!(!reason.trim().is_empty(), "Correction requires a reason");
            ensure!(
                matches!(
                    assertion,
                    Authorship::Manual { .. } | Authorship::Unknown { .. }
                ),
                "Correction cannot promote evidence to verified"
            );
            for a in report
                .paths
                .values_mut()
                .filter(|a| a.event_ids.contains(event_id))
            {
                a.source = Source::Unknown;
                a.author_agent = None;
                if let Authorship::Manual {
                    claimed_agent_id, ..
                } = assertion
                {
                    a.source = Source::Manual;
                    a.author_agent = id(claimed_agent_id);
                }
                a.diagnostics
                    .push(format!("append_only_correction: {reason}"));
            }
            report.corrections.push(correction);
        }
    }
    report.diagnostics.sort();
    report.diagnostics.dedup();
    Ok(report)
}
