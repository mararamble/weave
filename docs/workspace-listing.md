# Workspace listing contract (schema version 1)

`weave list` and `weave list --json` publish a listing only after every record and
required status inspection succeeds. An unreadable or malformed metadata record,
unsupported metadata version, failed Git command or malformed status output
returns nonzero, writes an explanation to stderr, and emits no stdout. Healthy
entries are withheld with the rest of the listing. There is no partial-success
JSON object or schema transition.

A record whose path is missing or not in Git's worktree registrations remains in
a successful listing with `exists: false` and `dirty: null`. The human display
uses `missing` for these unavailable entries. `exists` means both an existing
directory and a matching registration; it is not proof of ownership or removal
eligibility. `dirty: null` never means clean. A successful list may therefore
contain unavailable records, but never a silently omitted inspection error.

The TUI uses the same JSON command. A failed refresh displays the error and clears
cached workspace rows, marks, file selections and captured diffs. It cannot use
an old successful listing for a new review or integration selection. Refresh
again after repairing the problem. `weave reconcile` can provide a separate
diagnostic inventory; malformed metadata can also prevent reconciliation.

Removal always repeats its own ownership, identity and cleanliness checks after
confirmation. Neither a listing nor a reconciliation result authorizes deletion.
Unavailable or damaged targets are refused. Listing and refusal do not modify
workspace contents, indexes, branches or refs. Ordinary listing retains its
existing requirement for trusted Git configuration, including configured filters.
