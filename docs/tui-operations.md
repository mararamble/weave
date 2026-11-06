# TUI operation lifecycle and recovery

The workspace browser and Vineweaver show the current operation and elapsed
seconds while work is running. This is activity feedback, not a percentage or a
promise that Git has made progress.

## Workspace browser reads

Esc cancels a running read. This includes workspace listing, status, diff
collection and streaming, overlap/plan review, reconciliation, and removal
**preview**. Starting another read first stops and joins the previous read.
Quitting or opening Vineweaver also cleans up the browser's active read.

On macOS and Linux, each browser read runs in its own process group. Cancellation
signals only that owned group, waits for and reaps its direct CLI child, joins the
worker, and discards its temporary planning data before reporting completion.
It does not merely discard an obsolete result. The parent owns the temporary
root even if the child cannot run Rust destructors. No create, integration, or
confirmed-removal command can enter this runner. A private CLI diff endpoint
keeps both collection and patch streaming within the same boundary; it refuses
terminal stdout and is not a public command contract.

The implementation uses SIGKILL for an owned read group, so a stalled pipe or Git
command cannot indefinitely defer a requested cancellation. This boundary is
safe because the selected operations do not write source worktrees, indexes,
branches or refs; temporary planning repositories are disposable. Trusted Git
configuration remains required for ordinary status/list/diff. A subprocess that
explicitly leaves the group is outside the termination guarantee; Weave does not
search the system for arbitrary descendants. Kernel-level uninterruptible I/O
can also delay termination. Cleanup failures report the temporary path or process
group for inspection; they are not reported as a successful cancellation.

JSON/text capture remains bounded at 32 MiB. Paged patch capture can use a larger
temporary file. Stderr is retained up to 64 KiB; an operation producing more than
32 MiB of stderr is refused. Output-limit failures terminate only owned reads.
Listing failures and cancelled listing refreshes invalidate cached workspace
selections; see the [v1 listing contract](workspace-listing.md).

## Confirmed mutations

Creation, removal and integration retain their confirmations and safety checks.
Their worker and CLI child remain owned until completion. q or Ctrl-C requests
exit after the mutation finishes. The status reads:

> Finishing operation, will exit when done

Elapsed time and the operation remain visible. Esc explains that the mutation
cannot be safely cancelled; it does not kill it or undo a pending quit. Once the
operation finishes, the TUI restores the terminal and exits without an extra key.
A failed mutation is printed after terminal restoration and exits nonzero.

Integration validation is part of the mutation. Its executable still runs
without a shell, with the existing minimal environment and optional explicit
environment opt-ins; it is not sandboxed. Quit does not kill that executable or
its arbitrary children. A stalled validation can therefore still delay exit.

There is no general rollback promise. If external intervention is unavoidable,
first identify the exact process and operation, arrange for the responsible
command to finish where possible, then inspect the resulting Git and filesystem
state before retrying:

| Operation | Evidence and recovery limit |
|---|---|
| Create | A pending record is written before Git adds the worktree. On interruption, inspect the record, destination, branch and Git registration. Do not blindly delete the record or directory. |
| Remove | Git removal precedes metadata deletion. If these diverge, inspect the printed record and registration; the branch is retained. Do not remove a workspace directory as a recovery shortcut. |
| Integrate | Validation uses a disposable checkout. Publication writes a recovery journal before object transfer and create-only ref update. Inspect that journal and compare the ref with its candidate; do not reset or delete an existing ref automatically. Unreachable imported objects can remain after failure. |

## Vineweaver

Vineweaver keeps its single owned worker and cooperative cancellation token.
During capture, validation and export preparation, Git commands now have an owned
process-group boundary too: cancellation terminates and reaps the in-flight Git
child, then lets Rust unwind and clean up its isolated repository or staging
directory. No user validation command runs within this cancellation scope.
The worker remains joined on quit; terminal restoration occurs before that join.
Cancelled validation/export can be revalidated and retried.

Export cancellation checks also run between groups and immediately before the
final no-replace publication. That final filesystem operation must finish; an
export published concurrently with a late cancellation remains published. Inspect
the destination after an uncertain interruption. Cancellation never authorizes
overwriting it, and does not roll back an already published export.

## Platforms and acceptance

The supported cancellation acceptance targets are macOS and Linux. Both run the
required formatting, full tests, and warnings-as-errors Clippy checks. The
existing GitHub Actions matrix includes Ubuntu and macOS; Linux acceptance must
execute the new PTY/process-lifecycle fixtures, not just cross-compile them.
The PTY fixtures require Git and Python 3 and check actual recorded PIDs, direct
child reaping, temporary cleanup, unchanged sources/indexes/refs, mutation
success/failure, validation ownership and terminal restoration. Linux zombies
are treated as terminated descendants; the direct child must also be reaped.

Other platforms wait for an in-flight read to finish before completing
cancellation; no process-tree termination or PTY acceptance is claimed there.
Keyboard Ctrl-C in raw mode is distinct from an externally delivered SIGINT or
SIGTERM. No new signal handler is installed. Forced termination, external
signals, terminal/SSH loss and process crashes cannot guarantee terminal or
filesystem cleanup. If the terminal is left in raw mode, restore it from the
shell with `stty sane`, then inspect operation-specific recovery evidence.
