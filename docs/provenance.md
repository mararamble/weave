# Optional provenance schema foundation (WEV-19)

`weave::provenance` defines a standalone event contract. Nothing invokes it from
the CLI, TUI, collector or Git operations. Capture remains disabled and ordinary
Git workflows, privacy exclusions and existing schema-v1 outputs are unchanged.
This is a foundation, not completion of WEV-19 or its dependency gate for WEV-38.

## Wire contract

An event is a UTF-8 JSON object with `schema_version: 1` and an `event` object.
See [the deterministic fixture](../tests/fixtures/provenance-v1.json). The version
is independent of the existing CLI/JSON schemas. Unsupported versions, unknown
fields/variants, omitted evidence fields and malformed values are rejected.
Schema extensions require an explicit version decision; readers never silently
discard evidence. Numeric sequences are unsigned 64-bit integers: consumers must
preserve integer precision rather than coercing to floating point.

IDs are nonempty opaque strings, not display names or paths. Producers must
allocate stable, immutable event, stream, agent, session, task and worktree IDs;
the schema does not allocate IDs or prove their uniqueness globally. An `Event`
offers read-only access; corrections must become new events. This is not a
persisted immutability or append-only guarantee.

`worktree_id`, `task_id`, `task_owner_agent_id`, `base_commit`, `snapshot_ref` and
`patch_ref` carry either `{state: "known", value: ...}` or
`{state: "missing", reason: ...}`. Reasons are `not_observed`, `unavailable`,
`suppressed` and `not_applicable`. Missing evidence must never be synthesized.
Base commits, snapshot/patch references, content hash algorithms and digests are
opaque identifiers: decoding an event neither resolves nor verifies them.

`path_hex` and known `previous_path_hex` contain exact raw Git path bytes as
lowercase hexadecimal, with no UTF-8 conversion. Empty, malformed and NUL paths
are refused; these values are data, never instructions to read a filesystem.
Before/after content states distinguish a hash from an absent file and from
unobserved content. Create requires absent before content; delete requires absent
after content; modify/rename/copy require non-absent states. Rename/copy also
require the previous path. These checks establish structural consistency only.

Observation sources are `explicit_checkpoint` (checkpoint ID), `agent_hook`
(hook ID), `worktree_baseline`, `manual`, and `unknown`. Timestamps are explicitly
missing or UTC Unix seconds plus nanoseconds in `[0, 1_000_000_000)`. Wall-clock
values may be missing, equal, or go backwards; never use them to order edits.

## Evidence and attribution

Task ownership is independent of edit authorship. Knowing a task owner never
assigns an edit to that agent. Authorship states mean:

| State | What the record claims |
|---|---|
| `verified` | A supported checkpoint/hook producer claims an authenticated agent/session binding plus before/after observation references and observed content hashes (or file absence). The decoder checks the required structure, **not authenticity, causality or exclusive authorship**. A future verifier must check those claims against the referenced evidence, including concurrent writes. |
| `worktree_only` | The observation is associated with a worktree; it establishes no agent/session authorship. Worktree identity can still be unavailable and is explicitly represented. |
| `manual` | A human/supplied assertion references its basis and explicitly known/missing claimed agent/session. It is not verified evidence. |
| `unknown` | Authorship evidence is missing, suppressed or inapplicable. No inferred agent is assigned. |

`verified` requires a checkpoint/hook source, nonempty agent/session and evidence
references, and observed before/after states. There is no automatic upgrade from
baseline, manual or unknown attribution. Hash equality alone proves no authorship.

## Ordering and gaps

`sequence` orders events within a single `stream_id`, starting at any recorded
value. It is not a global clock. `ordering_gaps` validates a supplied slice in its
given order: duplicates, repeated/decreasing sequences and mixed streams fail.
Missing interior sequence ranges are returned explicitly, without inventing
events, authors or edits. An unsupplied prefix/suffix is unknown. Consecutive
numbers do not prove capture completeness, and detection cannot prove that the
underlying log was not tampered with. The deterministic fixture tests the wire
contract and ordering only; it is neither replay nor concurrent-capture evidence.

## Remaining work

The rest of WEV-19 still needs explicit checkpoint/optional hook capture, stable
ID lifecycle, a local content-addressed store, atomic writes and crash recovery,
append-only/tamper and partial-write tests, same-file concurrent-write evidence,
gap detection in actual logs, deterministic replay, opt-out/privacy controls,
bounded retention and user-controlled pruning. No collector, hidden monitoring,
content store, hooks, retention or replay engine ships in this slice. Advanced
collector/replay consumers in WEV-38/39 require their own reviewed scope; the
schema foundation does not unblock them by itself.
