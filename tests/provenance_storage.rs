use std::{collections::BTreeMap, fs};
use weave::{
    provenance::*,
    provenance_replay::{self, ActorVerifier, Source},
    provenance_store::*,
};
fn id(v: &str) -> Id {
    v.to_owned().try_into().unwrap()
}
fn k(v: &str) -> Evidence<Id> {
    Evidence::Known { value: id(v) }
}
fn state(data: &[u8]) -> ContentState {
    ContentState::Hash {
        hash: ContentHash {
            algorithm: id("sha256"),
            digest: id(&digest(data)),
        },
    }
}
fn cp(
    sequence: u64,
    before: &[u8],
    after: &[u8],
    authorship: Authorship,
) -> (Checkpoint, BTreeMap<String, Vec<u8>>) {
    let files = |b: &[u8]| {
        BTreeMap::from([(
            "66696c65".into(),
            FileState {
                hash: digest(b),
                mode: "100644".into(),
            },
        )])
    };
    let snapshot = files(after);
    let snapshot_hash = digest(&serde_json::to_vec(&snapshot).unwrap());
    let event = Event::new(EventData {
        event_id: id(&format!("event-{sequence}")),
        stream_id: id("stream"),
        sequence,
        worktree_id: k("worktree"),
        task_id: k("task"),
        task_owner_agent_id: k("owner"),
        base_commit: k("base"),
        path_hex: "66696c65".to_string().try_into().unwrap(),
        previous_path_hex: Evidence::Missing {
            reason: MissingReason::NotApplicable,
        },
        operation: Operation::Modify,
        before: state(before),
        after: state(after),
        snapshot_ref: k(&snapshot_hash),
        patch_ref: Evidence::Missing {
            reason: MissingReason::Unavailable,
        },
        source: ObservationSource::ExplicitCheckpoint {
            checkpoint_id: id(&format!("key-{sequence}")),
        },
        timestamp: Evidence::Missing {
            reason: MissingReason::NotObserved,
        },
        authorship,
    })
    .unwrap();
    (
        Checkpoint {
            key: format!("key-{sequence}"),
            request_hash: digest(b"fixture-request"),
            origin: Origin {
                stream_id: "stream".into(),
                worktree_id: "worktree".into(),
                workspace: "name".into(),
                repository: "/fixture/repo".into(),
                worktree: "/fixture/worktree".into(),
                base_commit: "base".into(),
            },
            baseline: files(b"base"),
            files: snapshot,
            events: vec![event],
            excluded: vec![],
            snapshot_hash,
        },
        BTreeMap::from([
            (digest(b"base"), b"base".to_vec()),
            (digest(before), before.to_vec()),
            (digest(after), after.to_vec()),
        ]),
    )
}
fn append(store: &Store, cp: Checkpoint, blobs: BTreeMap<String, Vec<u8>>) {
    let h = store.history().unwrap().head;
    store.append(Entry::Checkpoint(cp), &blobs, &h).unwrap();
}

#[test]
fn deterministic_replay_keeps_worktree_owner_and_author_separate_and_corrections_append() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("store"), true).unwrap();
    let (c, b) = cp(0, b"base", b"one", Authorship::WorktreeOnly {});
    append(&store, c, b);
    let a = provenance_replay::replay(&store, "stream", None).unwrap();
    let b = provenance_replay::replay(&store, "stream", None).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/provenance-replay-v1-hash.json")).unwrap();
    assert_eq!(
        digest(&serde_json::to_vec(&a).unwrap()),
        fixture["report_sha256"].as_str().unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );
    assert_eq!(a.paths["66696c65"].source, Source::VerifiedWorktree);
    assert_eq!(a.paths["66696c65"].author_agent, None);
    assert_eq!(a.paths["66696c65"].owner_task.as_deref(), Some("task"));
    let original = fs::read(dir.path().join("store/records/00000000000000000001.json")).unwrap();
    let h = store.history().unwrap().head;
    store
        .append(
            Entry::Correction {
                event_id: "event-0".into(),
                assertion: Authorship::Manual {
                    claimed_agent_id: k("human-claim"),
                    claimed_session_id: Evidence::Missing {
                        reason: MissingReason::NotObserved,
                    },
                    assertion_ref: id("assertion"),
                },
                reason: "reviewed externally".into(),
            },
            &BTreeMap::new(),
            &h,
        )
        .unwrap();
    let r = provenance_replay::replay(&store, "stream", None).unwrap();
    assert_eq!(r.paths["66696c65"].source, Source::Manual);
    assert_eq!(r.corrections.len(), 1);
    assert_eq!(
        original,
        fs::read(dir.path().join("store/records/00000000000000000001.json")).unwrap()
    );
}

#[test]
fn gaps_conflicts_and_multiple_writes_remain_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("store"), true).unwrap();
    let (c, b) = cp(0, b"base", b"one", Authorship::WorktreeOnly {});
    append(&store, c, b);
    let (c, b) = cp(2, b"wrong-before", b"two", Authorship::WorktreeOnly {});
    append(&store, c, b);
    let r = provenance_replay::replay(&store, "stream", None).unwrap();
    let a = &r.paths["66696c65"];
    assert_eq!(a.source, Source::Unknown);
    assert!(
        a.diagnostics
            .contains(&"missing_or_out_of_order_events".into())
    );
    assert!(
        a.diagnostics
            .contains(&"conflicting_or_concurrent_write".into())
    );
    assert!(
        a.diagnostics
            .contains(&"multiple_writes_require_hunk_review".into())
    );
    assert_eq!(a.event_ids.len(), 2);
}

#[test]
fn claimed_verified_identity_needs_trusted_adapter_validation() {
    struct FixtureVerifier;
    impl ActorVerifier for FixtureVerifier {
        fn verify(&self, e: &Event, b: Option<&[u8]>, a: Option<&[u8]>) -> bool {
            e.data().event_id.as_str() == "event-0" && b == Some(b"base") && a == Some(b"one")
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("store"), true).unwrap();
    let (c, b) = cp(
        0,
        b"base",
        b"one",
        Authorship::Verified {
            agent_id: id("agent"),
            session_id: id("session"),
            actor_binding_ref: id("fixture-binding"),
            before_observation_ref: id("fixture-before"),
            after_observation_ref: id("fixture-after"),
        },
    );
    append(&store, c, b);
    assert_eq!(
        provenance_replay::replay(&store, "stream", None)
            .unwrap()
            .paths["66696c65"]
            .source,
        Source::Unknown
    );
    let r = provenance_replay::replay(&store, "stream", Some(&FixtureVerifier)).unwrap();
    assert_eq!(r.paths["66696c65"].source, Source::VerifiedEvent);
    assert_eq!(r.paths["66696c65"].author_agent.as_deref(), Some("agent"));
}

#[test]
fn missing_tampered_and_partial_evidence_fail_closed_and_recovery_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(&root, true).unwrap();
    let old = fs::read(root.join("head.json")).unwrap();
    let (c, b) = cp(0, b"base", b"one", Authorship::WorktreeOnly {});
    append(&store, c, b);
    let final_head = fs::read(root.join("head.json")).unwrap();
    fs::write(root.join("head.json"), old).unwrap();
    assert!(store.history().is_err());
    store.recover().unwrap();
    assert_eq!(fs::read(root.join("head.json")).unwrap(), final_head);
    let blob = root.join("blobs").join(digest(b"one"));
    fs::write(&blob, b"tamper").unwrap();
    assert!(provenance_replay::replay(&store, "stream", None).is_err());
    fs::remove_file(&blob).unwrap();
    assert!(provenance_replay::replay(&store, "stream", None).is_err());
    fs::write(root.join("records/00000000000000000002.json"), b"{partial").unwrap();
    assert!(store.history().is_err());
    assert!(store.recover().is_err());
}

#[test]
fn pruning_removes_only_orphans_and_preserves_reachable_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(&root, true).unwrap();
    let (c, b) = cp(0, b"base", b"one", Authorship::WorktreeOnly {});
    append(&store, c, b);
    let orphan = digest(b"interrupted");
    fs::write(root.join("blobs").join(&orphan), b"interrupted").unwrap();
    assert_eq!(store.prune(false).unwrap(), vec![orphan.clone()]);
    assert_eq!(store.history().unwrap().records.len(), 1);
    assert_eq!(store.prune(true).unwrap(), vec![orphan.clone()]);
    assert!(!root.join("blobs").join(orphan).exists());
    assert_eq!(store.history().unwrap().records.len(), 2);
    provenance_replay::replay(&store, "stream", None).unwrap();
    let huge = vec![0; FILE_LIMIT + 1];
    let h = store.history().unwrap().head;
    assert!(
        store
            .append(
                Entry::Prune {
                    orphan_blobs: vec![],
                    incomplete_files: vec![],
                },
                &BTreeMap::from([(digest(&huge), huge)]),
                &h
            )
            .is_err()
    );
}

#[test]
fn partial_blob_and_record_staging_is_pruned_only_after_a_durable_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(&root, true).unwrap();
    fs::write(root.join("blobs/.pending-crashed-blob"), b"partial bytes").unwrap();
    fs::write(root.join("records/.pending-crashed-record"), b"{partial").unwrap();
    assert_eq!(store.history().unwrap().records.len(), 0);
    assert_eq!(store.prune(false).unwrap().len(), 2);
    assert!(root.join("blobs/.pending-crashed-blob").exists());
    assert_eq!(store.prune(true).unwrap().len(), 2);
    assert!(!root.join("blobs/.pending-crashed-blob").exists());
    let h = store.history().unwrap();
    assert!(
        matches!(&h.records[0].entry,Entry::Prune{incomplete_files,..} if incomplete_files.len()==2)
    );
}

#[test]
fn adapter_checkpoint_retries_and_symlinked_storage_fail_safely() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(&root, true).unwrap();
    let (c, b) = cp(0, b"base", b"one", Authorship::WorktreeOnly {});
    assert!(!store.append_checkpoint(c.clone(), &b).unwrap().1);
    assert!(store.append_checkpoint(c.clone(), &b).unwrap().1);
    let mut changed = c;
    changed.request_hash = digest(b"different");
    assert!(store.append_checkpoint(changed, &b).is_err());
    assert_eq!(store.history().unwrap().head.sequence, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let target = dir.path().join("private");
        fs::write(&target, b"untouched").unwrap();
        let hash = digest(b"one");
        fs::remove_file(root.join("blobs").join(&hash)).unwrap();
        symlink(&target, root.join("blobs").join(hash)).unwrap();
        assert!(provenance_replay::replay(&store, "stream", None).is_err());
        assert_eq!(fs::read(target).unwrap(), b"untouched");
    }
}

#[test]
fn contradictory_snapshot_reference_never_receives_verified_attribution() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("store"), true).unwrap();
    let (mut checkpoint, blobs) = cp(0, b"base", b"one", Authorship::WorktreeOnly {});
    let mut event = serde_json::to_value(&checkpoint.events[0]).unwrap();
    event["event"]["snapshot_ref"]["value"] = "wrong-snapshot-that-does-not-exist".into();
    checkpoint.events[0] = serde_json::from_value(event).unwrap();
    append(&store, checkpoint, blobs);
    let report = provenance_replay::replay(&store, "stream", None).unwrap();
    let evidence = &report.paths["66696c65"];
    assert_eq!(evidence.source, Source::Unknown);
    assert!(
        evidence
            .diagnostics
            .contains(&"missing_or_contradictory_snapshot_reference".into())
    );
}

#[test]
fn concurrent_appenders_serialize_without_lost_events_and_readers_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    drop(Store::open(&root, true).unwrap());
    std::thread::scope(|scope| {
        for n in 0..8 {
            let root = &root;
            scope.spawn(move || {
                let store = Store::open(root, true).unwrap();
                let h = store.history().unwrap().head;
                store
                    .append(
                        Entry::Correction {
                            event_id: format!("unresolved-{n}"),
                            assertion: Authorship::Unknown {
                                reason: MissingReason::Unavailable,
                            },
                            reason: "fixture".into(),
                        },
                        &BTreeMap::new(),
                        &h,
                    )
                    .unwrap();
            });
        }
    });
    let store = Store::open(&root, false).unwrap();
    let h = store.history().unwrap().head;
    assert_eq!(h.sequence, 8);
    assert!(
        store
            .append(
                Entry::Prune {
                    orphan_blobs: vec![],
                    incomplete_files: vec![],
                },
                &BTreeMap::new(),
                &h
            )
            .is_err()
    );
    assert!(store.recover().is_err());
}
