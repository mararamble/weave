use serde_json::{Value, json};
use weave::provenance::{Event, Gap, ordering_gaps};

fn assert_fields(value: &Value, expected: &[&str]) {
    let mut actual: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/provenance-v1.json")).unwrap()
}
fn event(value: Value) -> Event {
    serde_json::from_value(value).unwrap()
}

#[test]
fn v1_round_trip_preserves_exact_non_utf8_git_path_and_evidence() {
    let value = fixture();
    let parsed = event(value.clone());
    assert_eq!(parsed.data().path_hex.as_bytes(), b"src/\xff\n.rs");
    assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    assert_fields(&value, &["schema_version", "event"]);
    assert_fields(
        &value["event"],
        &[
            "event_id",
            "stream_id",
            "sequence",
            "worktree_id",
            "task_id",
            "task_owner_agent_id",
            "base_commit",
            "path_hex",
            "previous_path_hex",
            "operation",
            "before",
            "after",
            "snapshot_ref",
            "patch_ref",
            "source",
            "timestamp",
            "authorship",
        ],
    );
}

#[test]
fn confidence_states_never_inherit_authorship_from_task_ownership() {
    for authorship in [
        json!({"confidence":"unknown", "reason":"not_observed"}),
        json!({"confidence":"worktree_only"}),
        json!({"confidence":"manual", "claimed_agent_id":{"state":"known","value":"claimed"},
            "claimed_session_id":{"state":"missing","reason":"unavailable"}, "assertion_ref":"manual-1"}),
        json!({"confidence":"verified", "agent_id":"actual-author", "session_id":"session-1",
            "actor_binding_ref":"binding-1", "before_observation_ref":"observation-1", "after_observation_ref":"observation-2"}),
    ] {
        let mut value = fixture();
        value["event"]["authorship"] = authorship.clone();
        if authorship["confidence"] == "verified" {
            value["event"]["source"] = json!({"kind":"agent_hook","hook_id":"hook-1"});
        }
        assert_eq!(serde_json::to_value(event(value.clone())).unwrap(), value);
    }
    let mut value = fixture();
    value["event"].as_object_mut().unwrap().remove("authorship");
    assert!(serde_json::from_value::<Event>(value).is_err());
}

#[test]
fn missing_evidence_is_required_and_distinct_from_absent_file() {
    let mut value = fixture();
    value["event"]["before"] = json!({"state":"unobserved","reason":"suppressed"});
    value["event"]["after"] = json!({"state":"unobserved","reason":"not_observed"});
    value["event"]["timestamp"] = json!({"state":"missing","reason":"unavailable"});
    assert_eq!(serde_json::to_value(event(value.clone())).unwrap(), value);
    value["event"]["operation"] = json!("create");
    assert!(serde_json::from_value::<Event>(value.clone()).is_err());
    value["event"]["before"] = json!({"state":"absent"});
    assert!(serde_json::from_value::<Event>(value.clone()).is_ok());
    value["event"].as_object_mut().unwrap().remove("patch_ref");
    assert!(serde_json::from_value::<Event>(value).is_err());
}

#[test]
fn unsupported_versions_and_extensions_fail_closed_without_guessing() {
    for version in [json!(0), json!(2), json!(null), json!("1")] {
        let mut value = fixture();
        value["schema_version"] = version;
        assert!(serde_json::from_value::<Event>(value).is_err());
    }
    for pointer in ["/extra", "/event/extra", "/event/authorship/agent_id"] {
        let mut value = fixture();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let object = if parent.is_empty() {
            &mut value
        } else {
            value.pointer_mut(parent).unwrap()
        };
        object[key] = json!("unexpected");
        assert!(serde_json::from_value::<Event>(value).is_err());
    }
}

#[test]
fn invalid_paths_ids_timestamps_and_conflicting_evidence_are_rejected() {
    for path in ["", "f", "zz", "00", "FF", "aé"] {
        let mut value = fixture();
        value["event"]["path_hex"] = json!(path);
        assert!(serde_json::from_value::<Event>(value).is_err());
    }
    for (pointer, invalid) in [
        ("/event/event_id", json!("")),
        ("/event/timestamp/value/nanoseconds", json!(1_000_000_000)),
        ("/event/sequence", json!(-1)),
        ("/event/operation", json!("rename")),
        ("/event/before", json!({"state":"absent"})),
    ] {
        let mut value = fixture();
        *value.pointer_mut(pointer).unwrap() = invalid;
        assert!(serde_json::from_value::<Event>(value).is_err());
    }
    let mut value = fixture();
    value["event"]["authorship"] = json!({"confidence":"verified","agent_id":"agent","session_id":"session",
        "actor_binding_ref":"binding","before_observation_ref":"before","after_observation_ref":"after"});
    assert!(serde_json::from_value::<Event>(value.clone()).is_err());
    value["event"]["source"] = json!({"kind":"explicit_checkpoint","checkpoint_id":"checkpoint"});
    assert!(serde_json::from_value::<Event>(value.clone()).is_ok());
    value["event"]["before"] = json!({"state":"unobserved","reason":"not_observed"});
    assert!(serde_json::from_value::<Event>(value).is_err());
}

fn at(sequence: u64) -> Event {
    let mut value = fixture();
    value["event"]["event_id"] = json!(format!("event-{sequence}"));
    value["event"]["sequence"] = json!(sequence);
    event(value)
}

#[test]
fn deterministic_order_fixture_reports_only_observable_gaps_without_replay() {
    let events = [at(10), at(11), at(14), at(u64::MAX)];
    assert_eq!(
        ordering_gaps(&events).unwrap(),
        [
            Gap {
                first_missing: 12,
                last_missing: 13
            },
            Gap {
                first_missing: 15,
                last_missing: u64::MAX - 1
            },
        ]
    );
    assert!(ordering_gaps(&[]).unwrap().is_empty());
    assert!(ordering_gaps(&[at(10)]).unwrap().is_empty());
    assert!(ordering_gaps(&[at(1), at(0)]).is_err());
    assert!(ordering_gaps(&[at(1), at(1)]).is_err());
    let mut other = fixture();
    other["event"]["stream_id"] = json!("other-stream");
    assert!(ordering_gaps(&[at(1), event(other)]).is_err());
    let mut duplicate = fixture();
    duplicate["event"]["event_id"] = json!("event-1");
    assert!(ordering_gaps(&[at(1), event(duplicate)]).is_err());
    let mut backwards_clock = fixture();
    backwards_clock["event"]["event_id"] = json!("later-event");
    backwards_clock["event"]["timestamp"]["value"]["unix_seconds"] = json!(-1);
    assert!(ordering_gaps(&[at(1), event(backwards_clock)]).is_ok());
}
