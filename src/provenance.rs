//! Optional provenance event schema. This module records evidence claims; it
//! does not collect, authenticate, persist or replay edits. See docs/provenance.md.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA_VERSION: u32 = 1;

/// An opaque, nonempty identifier. Never infer identity from a display name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);
impl TryFrom<String> for Id {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err("identifier must not be empty".into());
        }
        Ok(Self(value))
    }
}
impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}
impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Canonical lowercase hex on the wire; bytes are never decoded lossily.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RawPath(Vec<u8>);
impl TryFrom<String> for RawPath {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || !value.len().is_multiple_of(2) {
            return Err("path_hex must contain complete bytes".into());
        }
        let mut bytes = Vec::new();
        for pair in value.as_bytes().as_chunks::<2>().0 {
            fn digit(b: u8) -> Result<u8, String> {
                match b {
                    b'0'..=b'9' => Ok(b - b'0'),
                    b'a'..=b'f' => Ok(b - b'a' + 10),
                    _ => Err("path_hex must be lowercase hexadecimal".into()),
                }
            }
            bytes.push(digit(pair[0])? * 16 + digit(pair[1])?);
        }
        if bytes.contains(&0) {
            return Err("Git paths cannot contain NUL".into());
        }
        Ok(Self(bytes))
    }
}
impl From<RawPath> for String {
    fn from(value: RawPath) -> Self {
        value.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
impl RawPath {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    NotObserved,
    Unavailable,
    Suppressed,
    NotApplicable,
}

/// Missing evidence is explicit, rather than an omitted field or guessed value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Evidence<T> {
    Known { value: T },
    Missing { reason: MissingReason },
}

/// Algorithm and digest are opaque content-store identifiers. A reference alone
/// does not establish that bytes were captured, retained or authenticated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentHash {
    pub algorithm: Id,
    pub digest: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentState {
    Hash { hash: ContentHash },
    Absent {},
    Unobserved { reason: MissingReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationSource {
    ExplicitCheckpoint { checkpoint_id: Id },
    AgentHook { hook_id: Id },
    WorktreeBaseline {},
    Manual {},
    Unknown {},
}

/// Verified means the producer claims an authenticated actor/session binding and
/// before/after observations. This schema checks structure, not authenticity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "confidence", rename_all = "snake_case", deny_unknown_fields)]
pub enum Authorship {
    Verified {
        agent_id: Id,
        session_id: Id,
        actor_binding_ref: Id,
        before_observation_ref: Id,
        after_observation_ref: Id,
    },
    WorktreeOnly {},
    Manual {
        claimed_agent_id: Evidence<Id>,
        claimed_session_id: Evidence<Id>,
        assertion_ref: Id,
    },
    Unknown {
        reason: MissingReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    Modify,
    Delete,
    Rename,
    Copy,
    Unknown,
}

/// UTC instant; wall-clock time never determines event ordering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timestamp {
    pub unix_seconds: i64,
    pub nanoseconds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventData {
    pub event_id: Id,
    pub stream_id: Id,
    pub sequence: u64,
    pub worktree_id: Evidence<Id>,
    pub task_id: Evidence<Id>,
    pub task_owner_agent_id: Evidence<Id>,
    pub base_commit: Evidence<Id>,
    pub path_hex: RawPath,
    pub previous_path_hex: Evidence<RawPath>,
    pub operation: Operation,
    pub before: ContentState,
    pub after: ContentState,
    pub snapshot_ref: Evidence<Id>,
    pub patch_ref: Evidence<Id>,
    pub source: ObservationSource,
    pub timestamp: Evidence<Timestamp>,
    pub authorship: Authorship,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WireEvent", into = "WireEvent")]
pub struct Event(EventData);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent {
    schema_version: u32,
    event: EventData,
}
impl TryFrom<WireEvent> for Event {
    type Error = String;
    fn try_from(value: WireEvent) -> Result<Self, Self::Error> {
        if value.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported provenance schema version {}",
                value.schema_version
            ));
        }
        Self::new(value.event)
    }
}
impl From<Event> for WireEvent {
    fn from(value: Event) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            event: value.0,
        }
    }
}
impl Event {
    pub fn new(data: EventData) -> Result<Self, String> {
        if let Evidence::Known { value } = &data.timestamp
            && value.nanoseconds >= 1_000_000_000
        {
            return Err("timestamp nanoseconds must be less than one billion".into());
        }
        let absent_before = matches!(data.before, ContentState::Absent { .. });
        let absent_after = matches!(data.after, ContentState::Absent { .. });
        if absent_before && absent_after {
            return Err("both content states cannot be absent".into());
        }
        match data.operation {
            Operation::Create if !absent_before => {
                return Err("create requires absent before content".into());
            }
            Operation::Delete if !absent_after => {
                return Err("delete requires absent after content".into());
            }
            Operation::Modify | Operation::Rename | Operation::Copy
                if absent_before || absent_after =>
            {
                return Err("modify, rename and copy require non-absent content".into());
            }
            _ => {}
        }
        if matches!(data.operation, Operation::Rename | Operation::Copy)
            && !matches!(data.previous_path_hex, Evidence::Known { .. })
        {
            return Err("rename and copy require the previous raw path".into());
        }
        if matches!(data.authorship, Authorship::Verified { .. })
            && (!matches!(
                data.source,
                ObservationSource::AgentHook { .. } | ObservationSource::ExplicitCheckpoint { .. }
            ) || matches!(data.before, ContentState::Unobserved { .. })
                || matches!(data.after, ContentState::Unobserved { .. }))
        {
            return Err(
                "verified authorship requires a supported source and before/after evidence".into(),
            );
        }
        Ok(Self(data))
    }
    /// Read-only access: updating an observation requires a separate new event.
    pub fn data(&self) -> &EventData {
        &self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Gap {
    pub first_missing: u64,
    pub last_missing: u64,
}

/// Validate one supplied stream in recorded order. No sorting, cross-stream
/// chronology, inferred edits, or claims about unsupplied prefix/suffix events.
pub fn ordering_gaps(events: &[Event]) -> Result<Vec<Gap>, String> {
    let mut gaps = Vec::new();
    let mut ids = HashSet::new();
    for event in events {
        if !ids.insert(&event.0.event_id) {
            return Err("duplicate event ID".into());
        }
    }
    for pair in events.windows(2) {
        let (before, after) = (&pair[0].0, &pair[1].0);
        if before.stream_id != after.stream_id {
            return Err("cannot compare sequences from different streams".into());
        }
        if before.sequence >= after.sequence {
            return Err("sequences must increase strictly".into());
        }
        if after.sequence - before.sequence > 1 {
            gaps.push(Gap {
                first_missing: before.sequence + 1,
                last_missing: after.sequence - 1,
            });
        }
    }
    Ok(gaps)
}
