# Opt-in provenance collection, replay and review

The event schema, store, replay report and Untangle evidence sidecar each use an
explicit version. Existing status/diff/Untangle preview, manifest and review-plan
JSON retain schema version 1 and their established fields and values. Event data
continues to use the [WEV-19 event contract](provenance.md). New storage records,
replay reports, evidence and audit sidecars use their own version 1 contracts.
Unknown fields and unsupported journal/event versions are refused, not discarded.

## Explicit collection

```sh
weave provenance checkpoint auth --key before-review
weave provenance checkpoint auth --key after-edit --task auth-task --owner-agent owner
# Optional untracked capture, on each invocation:
weave provenance checkpoint auth --key untracked-review --include-untracked-content
# A supplied agent is a human assertion, not authenticated authorship:
weave provenance checkpoint auth --key external-assertion --agent claimed-agent
weave provenance replay auth --json
```

Nothing observes edits in the background. A checkpoint reads a Git baseline and
the current workspace, then compares it with the previous recorded checkpoint.
Events describe the observed delta; they do not reconstruct edits that occurred
between observations. Normal Git and Weave commands work without checkpoints.

The stream binds the immutable recorded base and worktree identity. Worktree IDs
hash the canonical Git worktree directory plus device/inode identity; a stream
also includes its base commit. These are local lifetime identities, not portable
global agent identities; copying/recreating metadata is not an identity migration.
Task/owner IDs are separate, explicitly supplied claims. Missing task/agent/session
information stays missing. No timestamp is used to order or attribute changes.

The filtered Git baseline is immutable within a stream. If changed ignore rules
would change that baseline, checkpoint recording refuses before appending rather
than corrupting existing replay history. Retain the existing evidence and use a
new worktree/base stream with the desired privacy policy. Newly excluded content
is not read or written during the refused capture.

A retry key is scoped to a stream. An identical retry returns the existing result;
the same key with different content, privacy selection or attribution metadata is
refused. Keep the same key for retrying a failed invocation, and use a new key for
a new observation. Eventless checkpoints also retain a request hash so changing
their metadata cannot silently pass as an identical retry.

## Privacy, limits and source preservation

The frontend checks effective Git clean/process filters and refuses configured
commands. It never runs hooks, filters, textconv, external diffs or user commands.
It inventories paths before reading content and applies the existing sensitive
path exclusions to both tracked and untracked evidence. Git-ignored paths are
excluded even if tracked (`check-ignore --no-index`). Excluded paths are listed as
raw `path_hex` metadata; their bytes never enter checkpoint blobs. This is a
path-based privacy policy, not a content secret scanner. Existing excluded bytes
are not retroactively erased when ignore rules change.

Untracked ordinary content requires the explicit flag. Only regular files and
ordinary executable modes are supported; symlinks, submodules and unsafe path
traversal are refused. Safe file reads use the existing no-follow traversal.
Git baseline objects are read directly without conversion. Non-UTF-8 path bytes
are preserved where the filesystem supports them; the schema fixture exercises
lossless byte serialization on both macOS and Linux.

Limits: 1 MiB per file, 8 MiB per baseline/current snapshot, 10,000 paths, 16 MiB
per journal record, 10,000 records, and 256 MiB total store bytes. Ordinary writes
reserve 16 MiB of that ceiling for maintenance receipts. Limits fail closed.
Referenced history is retained; pruning never removes evidence referenced by any
checkpoint/event. A full live store requires a separately retained store epoch;
there is no automatic history eviction or silent loss of provenance.

Source files, indexes and refs are not written by collection/replay/review.
Capture repeats the file inventory/content pass and checks HEAD, recorded origin
and the source index. Detected drift is an explicit inconsistent-capture error.
These checks are non-atomic: concurrent edits between checks can still escape
detection. A checkpoint is not proof that one actor exclusively authored a delta.

## Storage and durability

Storage lives at `<Git common directory>/weave/provenance`, shared by worktrees:

```text
lock
head.json
records/00000000000000000001.json
records/00000000000000000002.json
blobs/<lowercase SHA-256 digest>
```

The root/subdirectories are private when created; files are private regular files.
Symlinked store entries and the frontend's intermediate `weave` metadata parent
are refused before any provenance files are created. The caller must anchor the store below the
trusted, canonical repository metadata directory; low-level embedding APIs do not
turn an attacker-controlled parent directory into a trusted location.

Writers hold an exclusive OS file lock for capture and publication. Readers open
existing storage and hold a shared lock; they create no journal, lock or blob
files. Locks are released by the OS on process death. This implementation supports
macOS and Linux local filesystems. Network filesystem lock/rename/durability
behavior is not promised.

Publication order is: write/hash/flush immutable blobs; publish/flush an immutable
record with sequence and previous-record hash; then atomically replace/flush the
head. Temporary files stay on the same filesystem. New immutable paths use
no-clobber publication. Directory entries are synced. Successful returns rely on
the OS/filesystem's `sync_all`/rename guarantees; tests simulate interruption and
corruption, not physical power loss or a disk controller ignoring flush requests.

The hash chain and head detect missing/changed records and partial publication.
They do not authenticate a local owner who can rewrite the entire store and head;
there is no external trusted history anchor. Unsupported versions, malformed
records, unexpected journal entries and missing/tampered blobs are explicit errors.

## Recovery and retention

```sh
weave provenance recover --yes
weave provenance prune          # inspect eligible orphan/staging files
weave provenance prune --yes    # append a receipt, then remove eligible files
```

If an intact next record exists after an interrupted head publication, ordinary
reads/writes refuse it until explicit recovery verifies the committed prefix,
parent link, snapshot metadata and referenced blobs. Recovery advances the head
only; it does not truncate, overwrite or fabricate records. Torn/corrupt published
records require inspection/restoration from a trusted backup; they are never
silently repaired. A missing/corrupt committed blob is not guessed from Git.

Pruning removes only unreferenced valid blobs and unpublished `.pending-*` files.
It writes a durable append-only receipt first, so an interruption leaves an
auditable operation that can be retried. Existing referenced blobs/events and
published records are retained. Pruning defaults to inspection; deletion requires
`--yes`. A malformed or symlinked candidate is refused. No source file is pruned.

## Replay and optional tool adapters

Replay validates record/event versions, sequences, origin/base identities,
before/after content hashes and referenced blobs. Event snapshot references must match
the enclosing checkpoint snapshot; missing/contradictory references remain unknown.
Reports are deterministic for identical stored bytes and the same verifier. The fixed fixture checksum in
`tests/fixtures/provenance-replay-v1-hash.json` is intended for both OS runners.
Timeline entries are ordered by stream ID and sequence, not a fabricated global
wall-clock chronology. Missing/out-of-order/duplicate events, conflicting writes,
unrecorded changes and overlapping other-worktree paths remain explicit.

Evidence sources are `verified_event`, `verified_worktree`, `manual`, `unknown`.
Worktree verification means consistency of the locally recorded worktree/base and
observed content; it proves no agent identity. Multiple same-path writes cannot
be mapped to final hunks without additional line-level evidence, so attribution
is conservatively unknown and requires review. Disjoint hunks are not evidence
of semantic independence.

The optional embedding interface is `Store::append_checkpoint` plus WEV-19
`Event`/blob payloads and `ActorVerifier`. Producers must apply the same privacy
policy before reading/submitting content. The low-level store is not a public
untrusted event-import endpoint. An explicitly installed trusted verifier must
validate agent/session bindings and before/after receipts against its own trust
anchor. Merely accepting an agent name, session ID or hash is insufficient.
There is no built-in third-party hook adapter or automatic verified agent claim.
Without a verifier, structurally `verified` claims become unknown.

Corrections append manual/unknown assertions and a reason:

```sh
weave provenance correct EVENT_ID --agent claimed-agent --reason 'External review'
weave provenance correct EVENT_ID --reason 'Leave attribution unknown'
```

Original events remain unchanged. Corrections never promote evidence to verified.

## Provenance-aware Untangle

```sh
weave untangle auth --provenance --evidence-json
weave untangle auth --provenance --review
```

This mode loads evidence without recording a checkpoint. If hooks/checkpoints are
absent, it retains meaningful unknown attribution. It checks event mappings against
the immutable base and captured file hashes/modes. A changed/deleted/restored file
or changed journal cannot silently reuse stale evidence. Existing snapshot and
unit IDs and schema-v1 JSON remain unchanged. `--evidence-json` is the separate
evidence contract; existing JSON ownership remains conservatively `unknown`.

The review displays per-group evidence sources, task/worktree/author distinctions,
uncertainty and per-stream event timelines. It shows the original captured patch
and states that the current patch uses the same immutable bytes: this workflow
edits assignments/order, not hunk contents. Safe split granularity remains the
existing Git context hunks; contiguous/overlapping edits are indivisible.

Additional keyboard controls in provenance mode:

| Key | Action |
|---|---|
| `o` | Assert a manual author for the selected unit; enter agent ID, then reason |
| `z` | Explicitly leave conflicting/ambiguous evidence unknown; enter reason |
| `a` | Acknowledge attribution uncertainty and semantic dependency review; enter reason |
| `v` | Validate the resulting revision again |

Overrides and resolutions append audit records with before/after evidence.
Grouping undo never erases that audit. Grouping edits, overrides, resolutions and
undo invalidate export acknowledgment and validation. Display is bounded and
terminal controls are escaped; complete original patches and raw event evidence
remain available in exported data. Unit views retain at most 256 event IDs and
refer to the report's exact path and full timeline when a longer chain exists.

For CLI review, use `--override-author UNIT=AGENT` or `--resolve-unknown UNIT`, with
`--snapshot` and `--reason`; neither verifies an author. Provenance exports require
`--acknowledge-evidence`. Ambiguous assigned units additionally require an explicit
resolution (manual assertion or leaving unknown). Unassigned/partial selections
still require the existing partial acknowledgment. Applicability/sequence and
reconstruction checks still run in isolated temporary Git state.

Both export kinds add `provenance.json` and `audit.json` inside the newly published
directory. Original patches, replay evidence, event IDs, base, snapshot hashes,
manual assertions and acknowledgment stay auditable. Existing manifest fields,
synthetic commit identity, artifact names and bundle refs are preserved. An
existing destination/ref is never overwritten. Acknowledgment changes no evidence
confidence. Source or journal drift, failed validation, cancellation and unresolved
conflicts prevent publication; ordinary cleanup removes only private staging.
