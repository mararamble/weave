# Weave

**Many agents. Many machines. One coherent codebase.**

Weave is an open-source, Git-native tool for isolating, tracking, and managing code changes produced by parallel AI coding agents.

Run multiple agents against the same repository, keep their changes independent, inspect overlapping modifications, and maintain control over what eventually enters your codebase.

Built in Rust. Terminal-first. Agent-agnostic.

> **Project status: Pre-alpha.** Local worktree management, diff inspection, overlap detection, non-destructive integration planning, and the `weave untangle` workflow are implemented. Remote synchronization is planned.

**Naming:** Weave is the tool; `weave untangle` is the action, powered by the Vineweaver engine and interactive review.

## Why Weave?

Running multiple coding agents simultaneously is becoming a normal development workflow.

But things get complicated when several agents work on the same repository:

- Different agents modify the same files.
- Uncommitted changes become difficult to attribute.
- Separate tasks produce overlapping patches.
- Developers need to review changes before combining them.
- Work may be distributed between laptops, workstations, and remote servers.

Git already provides powerful primitives for managing independent work, including branches, worktrees, and patches.

**Weave builds a workflow around those primitives.**

The goal isn't to replace Git, lazygit, Neovim, Codex, opencode, or your preferred development environment. It's to make parallel development easier to manage alongside them.

## Features

### Implemented

**Isolated workspaces**

Create independent Git worktrees for different tasks or agents. Each workspace has its own working directory and branch, allowing agents to modify the same files without sharing a working tree.

**Repository inspection**

Inspect repository metadata, branch state, Git directories, and staged, unstaged, and untracked changes.

**Worktree reconciliation and removal**

Inspect managed and unknown worktrees, and preview or explicitly confirm removal of an eligible clean managed worktree. Removal retains its branch and refuses unsafe or uncertain states.

**Changeset inspection**

Examine changes relative to a workspace's original base commit.

Weave distinguishes committed, staged, unstaged, and untracked changes, and provides an aggregate view without requiring agents to commit their work first.

**Overlap detection**

Compare managed workspaces to identify modifications involving the same files.

For compatible source states, Weave can distinguish changes affecting separate source regions from overlapping or adjacent changes.

Overlap detection is informational: it does not guarantee that Git can merge the changes successfully.

**Integration planning**

Preview ordered workspace changes with Git merge analysis in temporary state, preserving the original workspaces.

**Integration branches**

Create a separate review branch from selected workspaces and optionally run an explicitly supplied validation command in a disposable checkout.

**Untangle review and export**

Capture a mixed workspace into byte-preserving patch units, assign separate same-file hunks to named groups, and validate each group and an ordered sequence without modifying the source. Review assignments interactively and explicitly export ordered patches or portable independent commits. Ownership remains unknown.

**Structured output**

Machine-readable JSON is available for repository status, workspace listing, reconciliation, diff inspection, overlap analysis, integration planning, integration results, and `weave untangle` previews.

### Planned

- **Integration enhancements:** Persistent, replayable plans and richer recovery and validation workflows.
- **Vineweaver engine and review enhancements:** Saved review-session loading and finer splitting beyond existing safe Git hunk boundaries.
- **Remote Link:** Retrieve committed and uncommitted changes from other machines over SSH.
- **Distributed integration:** Combine and validate changesets originating from multiple machines.
- **Terminal UI:** A keyboard-first interface built with Ratatui.
- **Neovim integration:** Native workflows for Neovim and LazyVim users.
- **MCP integration:** Allow compatible coding agents to interact with Weave's workspace management tools.
- **Native macOS application:** A Swift/AppKit interface built around the same portable core.

These capabilities are part of the development roadmap and are not currently available.

---

## Quick start

### Requirements

- Rust and Cargo
- Git available on `PATH`
- A Git repository containing at least one commit

### Build

```bash
git clone https://github.com/mararamble/weave.git
cd weave

cargo build --release
```

### Install locally

```bash
cargo install --path . --locked
```

This installs the `weave` executable into Cargo's binary directory, normally `~/.cargo/bin`.

Make sure that directory is available on your `PATH`.

### Terminal interface

Run `weave` (or `weave tui`) inside a Git worktree on an interactive terminal. To opt in to untracked content capture for the session, run `weave tui --include-untracked-content`; this also applies to planning, integration and Vineweaver review. Navigate with `j`/`k` or the arrow keys, change panes with `h`/`l` or Tab, and press `Enter` to open a workspace or file. `/` filters workspaces, Space marks them in the order used for planning and integration, `o` reviews overlaps, `p` previews integration, and `u` opens Vineweaver. `s` shows repository status, `w` shows the selected workspace details, and `c` reconciles worktrees. `t` shows diff statistics, `J` shows diff JSON, and `[`/`]` page through large captured patches or untracked content. `n` creates a workspace, `D` previews removal, `d` removes the selected workspace, and `i` creates an integration branch. Integration accepts an optional validation executable and its arguments, entered one at a time; these run directly without a shell; F2 adds an empty argument. These mutations require confirmation; Esc cancels. `:` opens the action palette, `?` shows shortcuts, `r` refreshes, and `q` returns to the workspace pane or exits. Integration uses workspaces in marking order, or all workspaces in listing order when none are marked. On narrow terminals the active pane fills the screen.

The CLI remains available for automation and JSON. Current CLI/TUI parity audit (WEV-36):

| CLI action | TUI coverage |
|---|---|
| [x] `status [--json]` | `s` shows the current repository status. |
| [x] `list [--json]` | Browse and filter workspaces; `w` shows the selected record. |
| [x] `reconcile [--json]` | `c` shows the reconciliation report. |
| [x] `new NAME --from REF` | `n` prompts for the name and base, then confirms creation. |
| [x] `remove NAME [--dry-run\|--yes]` | `D` previews removal; `d` confirms it and runs the same validation. |
| [x] `diff NAME [--stat\|--json]` | Workspace files and tracked patches are browsable; untracked content is displayed as text or hex only when explicitly enabled; `t` and `J` open statistics and JSON. Tracked patches are spooled to a temporary file and browsed in pages. Existing collector limits still apply. |
| [x] `overlaps [--json] [NAMES...]` | `o` reviews marked workspaces, or all workspaces. |
| [x] `plan [--json] NAMES...` | `p` previews marked workspaces in selection order, or all workspaces. |
| [x] `integrate --branch BRANCH [--json] NAMES... [-- VALIDATION...]` | Available for marked workspaces in selection order, or all workspaces, with optional validation arguments. |
| [x] `untangle NAME` and grouping, ordering, validation and export options | `u` opens Vineweaver for interactive assignment, ordering, validation and confirmed patch or bundle export. Snapshot identity, base and partial-export acknowledgement are handled in the session. |
| [x] `tui` | Available via `weave` or `weave tui`. |

JSON and snapshot IDs remain available in the CLI for machine consumers; the TUI presents interactive equivalents. The core has no workspace rename or shell-switch operation to expose. A local SSH pseudo-terminal smoke test covered launch, confirmed creation and exit. The diff collector retains its existing output and untracked-content limits; the pager uses temporary disk space and displays invalid UTF-8 lossily. Long-running mutations must finish before the browser exits. Filter safety checks refuse effective non-empty Git clean/process commands and report their configuration origin and key; review that configuration before retrying.

### Inspect a repository

Navigate into an existing Git repository:

```bash
cd /path/to/your/project

weave status
```

For structured output:

```bash
weave status --json
```

### Create isolated workspaces

Create a separate workspace for each agent or task:

```bash
weave new auth --from main
weave new parser --from main
```

Replace `main` with an existing branch, tag, or commit reference.

Each command prints the newly created workspace directory.

Both workspaces reference the same Git repository history, but have independent working directories and branches.

### List workspaces

```bash
weave list
```

Or:

```bash
weave list --json
```

Only Weave-managed workspaces are included.

### Inspect an agent's changes

```bash
weave diff auth
```

Display a summary:

```bash
weave diff auth --stat
```

Get structured data:

```bash
weave diff auth --json
```

Diffs are calculated relative to the workspace's recorded creation commit, not necessarily the current default branch.

### Detect overlapping changes

Compare all ready managed workspaces:

```bash
weave overlaps
```

Or select specific tasks:

```bash
weave overlaps auth parser
```

Machine-readable output:

```bash
weave overlaps auth parser --json
```

Weave identifies files modified by multiple workspaces and, when analysis is supported, compares changed source regions.

**Note:** A detected overlap isn't necessarily a merge conflict, and non-overlapping changes aren't guaranteed to be semantically compatible.

---

## Untangle grouping, review and export

```bash
weave untangle auth --json
# Copy snapshot.id and unit IDs from that capture:
weave untangle auth --snapshot SNAPSHOT_ID \
  --group feature --group cleanup \
  --assign UNIT_ID_1=feature --assign UNIT_ID_2=cleanup --json
# Optional: --order cleanup --order feature
# Optional: --unassign UNIT_ID_1 (processed after assignments)
```

If a workspace has untracked files, `weave untangle` and Vineweaver review refuse capture unless `--include-untracked-content` is explicitly supplied on every invocation. Opted-in untracked bytes can appear in snapshot JSON, patches and portable bundles; exclusions below still apply.

`weave untangle NAME` prints a read-only preview with unit IDs, hunk ranges, group assignments, unassigned count, applicability, and warnings. JSON version 1 includes `snapshot`, `groups`, `unassigned`, `selection_complete`, `sequence_applicable`, `applications`, and typed `warnings`. The versioned snapshot contains the recorded base commit, captured tree identity, completeness, existing changeset context (committed/staged/unstaged/untracked), files and units. Units contain exact `path_hex` and `patch_hex`, a display path, a hunk range where applicable, and `ownership: "unknown"`. The final diff establishes no authorship.

Each invocation recaptures the workspace. Assignments require `--snapshot` and full unit IDs from the same capture; a changed HEAD, source index, tracked/untracked content, or collection context invalidates them. Group names must be unique and nonempty. Repeated assignments are rejected; `--unassign` clears an assignment. Assignment argument order does not affect unit order. Groups run in creation order unless `--order` supplies every group exactly once. Grouping is held in memory for this invocation; persist the command arguments to replay it against an unchanged capture. There is no saved editable snapshot file yet.

Capture starts from the final workspace state relative to its recorded base, preserving staging layers as context rather than reproducing intermediate index states. Existing Git context hunks in ordinary text modifications can be assigned separately; edits within one contiguous or overlapping hunk remain indivisible. Additions, deletions, binary content, mode/type changes and symlinks remain whole-file operations. Renames are represented as deletion/addition units: assign both together to preserve a move. Submodule changes are rejected as unsupported. Git-generated patch headers are parsed in bytes, while NUL-delimited metadata supplies exact paths; non-UTF-8 filesystem paths are supported where the OS permits them. No arbitrary line-level splitting or authorship suggestions are provided.

Every captured unit is checked to reconstruct the exact captured tree. Each named group is applied independently against the base in a temporary index/object database, then groups are applied in the requested sequence. Sequence evaluation stops at the first failure. A group that fails alone but succeeds after a prefix reports `applicable_after_prefix` and an uncertainty warning; this observation does not prove a specific dependency. For example, replacing a file with a directory can require its deletion before adding a child path. Applicability failures are warnings and exit zero; capture, stale assignment and operational errors exit nonzero without a partial JSON preview. Applicability does not establish semantic correctness. Unassigned units stay visible; `selection_complete` is false while any remain.

Capture and validation compare source identities before/after capture and before/after validation. These checks are **not atomic** and cannot detect every concurrent edit. Source files, indexes, branches and refs are never written. Git output and aggregate generated patches are limited to 16 MiB, source indexes to 64 MiB; untracked files inherit the 1 MiB/file, 8 MiB total, regular-file-only and sensitive-path exclusions below. Unsupported indexes, effective non-empty clean/process filter commands, incomplete untracked reads and exceeded limits fail capture. Ignored files remain excluded. Private state is removed on normal completion and returned errors, including interrupted Git subprocesses. Abrupt process termination (for example SIGKILL) can leave an OS temporary directory; capture is not a persistent archive.

`untangle::Engine` provides `capture`, `add_group`, `assign` (including unassignment), `verify_source`, and `validate` with typed version-1 snapshot, file, hunk, unit, group, application and diagnostic records. The engine owns the temporary object database; tree IDs cease to be locally available when it is dropped (unless independently present in the source). The UI must retain the engine, use the snapshot ID on edits, and call `verify_source` before using stale state; validation also checks automatically. Vineweaver review retains the engine in a dedicated worker, applies versioned review plans, and rejects stale validation results.

Open interactive review directly, or select a workspace in `weave tui` and press `u`:

```bash
weave untangle auth --review
```

Review controls:

| Key | Action |
|---|---|
| Tab | Switch units, groups, and preview panes |
| ↑/↓ or j/k | Select a file/hunk unit or group; scroll the preview |
| g / n / d | Create, rename, or delete the active group (deletion unassigns its units) |
| Space / x | Assign or move the selected unit to the active group / unassign |
| [ / ] | Move the active group earlier / later in the export order |
| u | Undo an assignment/group/order edit (up to 32 edits within an 8 MiB history budget) |
| s | Explain safe split boundaries: existing Git hunks are already separate units; indivisible edits remain whole |
| v | Validate groups and reconstruct the selected aggregate |
| p / c | Export ordered patches / independent commits after validation |
| PageUp / PageDown | Scroll validation and patch details |
| r | Recapture, discarding session edits |
| Esc | Cancel a prompt or request cancellation of current work |
| ? / q / Ctrl-C | Help / quit |

The review shows snapshot/base identity, an always-visible unassigned count and validation summary, per-group independent/sequence applicability, dependency uncertainty, unit patches and group previews. Units are automatically separated at the safe Git hunk boundaries supported by the engine; manual splitting inside an indivisible hunk is unavailable. Long lines wrap and small terminals stack the panes. Previews have a 2 MiB raw-byte budget per capture, 64 KiB per unit and 128 KiB per displayed group; truncation is marked and never affects exported bytes. Capture, validation and export run on a worker. Edits are disabled while a job runs; results carry review revisions and stale results are discarded. Cancellation is cooperative between Git operations, so the current subprocess may finish before cleanup. Quit restores the terminal before waiting for worker cleanup. Local pseudo-terminal restoration is tested; SSH verification remains pending.

TUI exports require a new destination directory, display the exact captured base and group order, and require typing `export`. A selection with unassigned units additionally requires typing `partial`; it is never labeled a complete reconstruction. The export worker repeats validation and source checks rather than trusting a previously displayed result.

The CLI can export the same explicitly assigned groups:

```bash
weave untangle auth --snapshot SNAPSHOT_ID --base BASE_COMMIT \
  --group feature --group cleanup \
  --assign UNIT_ID_1=feature --assign UNIT_ID_2=cleanup \
  --export-patches ../review-patches --json
# Use --export-commits ../review-commits for independent commits.
# Partial selections additionally require --allow-partial.
```

`--base` must exactly equal the recorded base commit ID. Group creation order is the sequence unless every group is supplied through repeated `--order`. Destinations must have an existing parent and be absent, including dangling symlinks; existing files, directories and refs are never overwritten. Destinations inside the source workspace or its shared Git directory are refused. Export stages beside the destination, verifies patch application and tree identity (contents, paths and modes), rechecks the source, then publishes with an atomic no-overwrite rename. This publication is supported on Linux and macOS; other platforms refuse export. Returned errors clean staging state. Abrupt termination can leave `.vineweaver-export-*` siblings and OS temporary repositories; inspect and remove those only after the process has exited. Cancellation arriving after publication leaves the completed export intact.

Every export contains numbered **ordered** patches, `aggregate.patch`, `manifest.json`, `validation.json`, and `review-plan.json`. The manifest records the source identity, base, order, independent applicability, captured/selected tree identities, completeness and unknown ownership. `validation.json` retains exact engine warnings and the reconstruction comparison; `review-plan.json` is an audit record, with no session-loading command yet. Numbered patches are regenerated against successive trees and independently re-applied for verification. With a complete selection, the final tree must exactly match the captured aggregate or export fails. With a partial selection, the manifest explicitly reports the omitted units and partial reconstruction. A sequence that fails Git application cannot be exported.

Apply numbered patches in manifest order, or apply `aggregate.patch` once, against the recorded base. A dependent group can be exported in an applicable patch sequence; its manifest entry has `independent: false`. Commit export requires **every** group to apply independently and creates `commits.bundle` containing `refs/heads/vineweaver-0001`, etc. Each synthetic commit has the recorded base as its parent; these commits are sibling alternatives, not a linear history. The commit message identifies the Untangle group; author/committer use the synthetic Weave Preview identity and make no original-authorship claim. The manifest's entry tree is the ordered reconstruction tree; its commit ID refers to the independent commit. The portable bundle includes the base history and may be much larger than the patch budget. It is verified before publication and can be imported without access to the source repository:

```bash
git init imported-review
git -C imported-review fetch /absolute/path/review-commits/commits.bundle \
  refs/heads/vineweaver-0001:refs/heads/review-feature
```

No source branch or ref is created by either export mode; bundle refs exist only in the private repository. Applicability does not prove semantic compatibility. Capture remains non-atomic, and unsupported/incomplete content is refused under the engine limits above.

Disposable-repository coverage includes independent same-file groups, indivisible edits, file/directory ordering warnings, stale/conflicting assignments, source races, interrupted Git cleanup, binary/mode/rename/deletion changes, untracked byte paths where supported, limits and filter/index refusal, aggregate and portable bundle reconstruction, destination races, export interruption/source drift, keyboard undo/reorder, narrow layouts, and local pseudo-terminal quit/cancellation restoration (including cancellation during bundle export). Verification: `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.

---

## Integration planning

`weave plan task-a task-b --json` previews the selected workspaces in the given order, starting at the first workspace's recorded creation commit. It includes committed changes and the final tracked working-tree state (including staged and unstaged changes). A workspace with untracked files requires `--include-untracked-content`; planning refuses rather than silently omitting them. Each workspace's base and HEAD are recorded. The preview uses a private index and object database and `git merge-tree --write-tree`; it does not update source trees, indexes, branches, or refs. It stops the merge sequence at the first conflict. Git must support `merge-tree --write-tree` and `rev-parse --show-object-format`.

Plan JSON schema version 1 contains `schema_version`, `target_base`, `status` (`clean` or `conflict`), `selected_workspaces`, ordered processed `steps`, and `warnings`. Each step records workspace, creation base, HEAD, status, staged/unstaged/untracked counts, temporary tree ID, and nullable Git conflict details. Steps after the first conflict are not evaluated. Tree IDs refer to temporary objects and are informational only; they disappear when the command exits. A conflict is a successfully computed plan and exits zero. Operational errors exit nonzero and produce no JSON plan.

The final worktree state is previewed as one snapshot per workspace; staging is reported separately but intermediate index contents are not independently integrated. Ignored files are excluded. Untracked capture follows the limits and sensitive-path exclusions described in Safety and limitations below; if any selected untracked file cannot be captured, the plan fails rather than omitting it. Unmerged indexes, assume-unchanged or skip-worktree entries, unsupported index features, and effective non-empty clean/process filter commands are rejected. Git output is capped at 16 MiB; the copied source index is limited to 64 MiB. Collection rechecks captured content and metadata but is not atomic. Custom merge drivers and configuration are disabled in the preview, so configured merges may differ. A clean merge does not establish semantic compatibility. Plans are not persisted or replayable; `weave integrate` builds and checks a fresh candidate when it runs.


## Create an integration branch

```bash
weave plan auth parser
weave integrate --branch review/auth-parser auth parser
weave integrate --branch review/tested --json auth parser -- cargo test
```

`weave integrate` captures and merges the named workspaces in order, starting from the first workspace's creation base. It builds a fresh candidate and stops before branch creation on conflicts, incomplete snapshots, detected source changes, or failed validation. The target branch must be new; creation uses an atomic create-only ref update. Original workspaces and source branches are not modified. The result is a branch ref, not a checked-out worktree. Create a separate Git worktree or check it out when ready to review.

To pass specific parent variables to validation, repeat `--pass-env NAME` before `--`. For example, set `CARGO_HOME` and `RUSTUP_HOME` in your environment, then run `weave integrate --branch review --pass-env CARGO_HOME --pass-env RUSTUP_HOME task -- cargo test`. Only explicitly named values are passed; unset names and reserved `HOME` or `GIT_*` names are rejected.

The optional command after `--` is executed directly with its arguments in a disposable checkout of the candidate. Only supply commands you trust: validation executes code with your user permissions and can access files and the network. It receives a cleared environment with the inherited `PATH` and selected locale variables, a disposable `HOME`, and Git variables directing cooperative Git commands to the disposable Git state. This is not a sandbox; see [validation command security](docs/security.md). Tools depending on user configuration may need explicit configuration: for example, a rustup proxy may not find its toolchain with the temporary `HOME`, so supply a `PATH` to an installed toolchain's `bin` directory when invoking validation. Changes made in the validation checkout are not included in the branch; tracked modifications are reported. Stdout and stderr are each capped at 64 KiB, and failure creates no branch. Temporary validation state is cleaned up when the command exits normally.

JSON schema version 1 reports `branch`, `commit`, `tree`, `target_base`, ordered `sources` with each base, HEAD, snapshot commit and included source commit IDs, nullable `validation`, and `warnings`. Snapshot and integration commits are synthetic commits on the review branch; source branch commits are not rewritten. Validation arguments are displayed lossily if they contain non-UTF-8 bytes. A clean merge does not imply semantic compatibility.

Weave writes a recovery journal under the common Git directory at `weave/integrations` before transferring candidate objects and creating the branch. After interruption or a publication error, inspect the journal's `reference` and `candidate` and compare them with `git rev-parse --verify <reference>`. A matching ref means the branch was created; an absent ref means it was not. A different ref belongs to another operation and must not be overwritten. Once inspected, a stale journal may be removed manually. Failed transfers can leave unreferenced Git objects; no automatic cleanup of user refs or files is performed.

The planner's capture limitations also apply to integration. Capture checks reduce races but are not atomic. Custom Git merge drivers and hooks do not run, so results may differ from a configured merge. Candidate object transfer and Git output are limited to 16 MiB per operation. Validation can be omitted; in that case the result explicitly has no validation record.

---

## How it works

Weave uses Git's existing worktree functionality to give independent development tasks their own working directories.

For example:

```text
my-project/             # primary worktree
└── .git/               # Weave metadata remains here
my-project.weave/       # default workspace parent
├── auth/
└── parser/
```

The exact workspace location depends on the primary worktree, repository layout and Weave configuration.

Each workspace has:

- A task name
- Its own Git branch
- A working directory
- A recorded base commit
- Managed workspace metadata

Weave can inspect their changes independently and compare them before integration.

The longer-term objective is to extend this model across machines while preserving source workspaces and providing safe, reviewable integration operations.

## Design philosophy

**Agent-agnostic**

Use whichever coding agents or models you prefer. Weave doesn't require a particular vendor or runtime.

**Git-native**

Use Git's established version-control primitives rather than inventing a new version-control system.

**Terminal-first**

The CLI is the foundation. Future interfaces should use the same underlying operations instead of implementing separate Git behavior.

**Local-first**

No mandatory cloud service, account, or subscription.

**Non-destructive**

Inspection operations should preserve source workspaces. Future integration operations will prioritize preview, validation, and explicit approval.

**Honest about provenance**

A final Git diff does not always reveal which agent produced which line. Weave must distinguish recorded attribution from user-assigned or inferred change groups.

**Performance-conscious**

Keep startup responsive, limit unnecessary processing, use bounded operations, and optimize based on reproducible measurements.

---

## Configuration and storage

### Workspace location

By default, managed workspaces live under:

```text
<primary-worktree-parent>/<primary-worktree-name>.weave/<task>
```

Repositories with the same name in different parent directories have separate workspace roots. If the destination for a task already exists, creation refuses with `Destination already exists` and leaves it untouched. If the parent directory is not writable, creation fails with `Cannot create workspace root <path>: Permission denied` (the OS error detail may vary); set `WEAVE_WORKSPACE_DIR` to a writable location.

Git reports the same primary anchor from linked worktrees, so they share this default. For bare repositories and separate Git directories where Git reports the common directory as the primary anchor, the default is a sibling named `<common-directory-name>.weave`. The resolved default must be outside the common directory; an unsafe or symlinked default is refused. Existing workspaces at the former `<Git common directory>/weave-workspaces/` location continue to use their recorded paths.

Override the parent directory using:

```bash
export WEAVE_WORKSPACE_DIR=/path/to/workspaces
```

Task names accept ASCII letters, digits, hyphens, and underscores.

### Metadata

Weave stores managed-workspace metadata inside the shared Git common directory under `weave`.

Each workspace records its branch, creation commit, working directory, and lifecycle state.

If workspace creation fails or is interrupted, a pending metadata record may remain.

`weave list` exposes these records. Inspect the recorded branch and directory before manually deleting the corresponding metadata JSON file to permit a retry.

Weave does not automatically remove pending worktrees or their metadata.

Managed workspace paths and Git common-directory paths must currently be UTF-8.

---

## JSON API

Weave provides versioned JSON schemas for integration with external tools and scripts.

Current schema version: **1**.

### Repository status

```bash
weave status --json
```

Fields include:

- `schema_version`
- `repository_root`
- `worktree_path`
- `git_dir`
- `common_dir`
- `branch`
- `detached_head`
- `unborn`
- `dirty`
- `paths_lossy`
- `staged`
- `unstaged`
- `untracked`

Local paths are diagnostic and should not be treated as persistent repository identities.

`paths_lossy` indicates that non-UTF-8 path bytes were replaced for display.

### Managed workspaces

```bash
weave list --json
```

Top-level fields:

- `schema_version`
- `workspaces`

Each workspace contains:

- `schema_version`
- `name`
- `branch`
- `base_commit`
- `repository`
- `path`
- `state`
- `exists`
- `dirty`

`repository` identifies the local Git common directory.

`state` is either `pending` or `ready`.

`exists` means the worktree is registered with Git and present on disk.

`dirty` may be null when the workspace is unavailable.

Ordinary Git worktrees not managed by Weave are excluded.

### Workspace changesets

```bash
weave diff TASK --json
```

The top-level schema contains:

- `schema_version`
- `workspace_id`
- `name`
- `repository`
- `base_commit`
- `head_commit`
- `warnings`
- `committed_changes`
- `staged_changes`
- `unstaged_changes`
- `untracked_changes`
- `aggregate_changes`

Individual change records include:

- `kind`
- `path`
- `previous_path`
- `path_hex`
- `previous_path_hex`
- `added`
- `deleted`
- `binary`
- `old_mode`
- `new_mode`
- `content_hex`
- `incomplete`

Path display fields may be lossy. The hexadecimal path fields preserve exact path bytes.

Nullable fields represent unavailable or inapplicable information.

Tracked content is exposed through the human-readable patch output rather than embedded as raw bytes in JSON.

By default `content_hex` is null for untracked files. With `--include-untracked-content`, it contains raw bytes for successfully captured untracked regular files.

The change categories represent different Git layers, so a path can appear in more than one category. Use `aggregate_changes` to avoid double-counting the final change state.

### Workspace overlaps

```bash
weave overlaps --json
```

Top-level fields:

- `schema_version`
- `warnings`
- `overlaps`

Each overlap contains:

- `path`
- `path_hex`
- `workspaces`
- `classification`
- `region_overlap`
- `regions`
- `incomplete`

Possible classifications:

| Classification | Meaning |
|---|---|
| `file_overlap` | Shared changed file; more specific analysis unavailable |
| `region_overlap` | Overlapping or adjacent changed source regions |
| `separate_regions` | Changes affect distinct analyzed source regions |
| `add_add` | Multiple workspaces add the same path |
| `delete_modify` | One workspace deletes a file another modifies |

Region analysis uses zero-context Git hunks in original source coordinates when base commits and file states allow meaningful comparison.

Insertions use zero-length original ranges.

For some changes—such as differing base commits, ambiguous renames, binary modifications, and missing content—region analysis may be incomplete.

---

## Safety and limitations

Weave is still pre-alpha software. Avoid relying on it as the only record of valuable uncommitted work.

### Untracked file capture

Ignored files are excluded. By default, `diff` and `overlaps` report untracked paths without reading contents. `plan`, `integrate`, and `untangle` refuse workspaces with untracked files rather than omit them; pass `--include-untracked-content` to opt in to capture. Use the same flag on each `weave untangle` invocation or start `weave tui --include-untracked-content` for an interactive session. Captured content can be displayed, written to integration commits, and included in Untangle patches or bundles.

Untracked reads exclude sensitive filenames and directories including:

```text
.env
.env.*
.ssh
.aws
.gnupg
.netrc
.npmrc
.pypirc
.git-credentials
.docker/config.json
kubeconfig
credentials
secrets
id_rsa
id_ed25519
*.pem
*.key
*.p12
*.pfx
*.p8
*.keystore
*.jks
*service-account*.json
token*
```

Exclusions are case-insensitive.

This is a filename-based filter, not a comprehensive secret detector. Always inspect generated output before sharing it.

Symlinks and non-regular untracked files are not read.

Current limits:

| Operation | Limit |
|---|---|
| Individual untracked file | 1 MiB |
| Total captured untracked content | 8 MiB |
| Git output per operation | 16 MiB |

On non-Unix systems, untracked file content is omitted because the implementation does not provide the required safe no-follow read behavior.

Incomplete captures are indicated in the results rather than silently treated as complete.

### Concurrent modifications

Changeset collection checks HEAD, tracked changes, index changes, and untracked paths twice; opted-in untracked content is also checked twice. Detected inconsistencies are rejected.

However, this is **not an atomic filesystem snapshot**.

Files that change and return to their previous contents between observations may escape detection.

Overlap analysis likewise performs repeated checks without guaranteeing an atomic snapshot.

### Conflict analysis

Overlap detection is not a complete Git merge simulation.

Changes affecting different source regions may still be logically incompatible. Changes affecting nearby regions may be mergeable.

Weave does not currently provide automatic conflict resolution.

### Error behavior

Failed commands print errors to stderr and return a nonzero exit status.

Missing workspaces and failed Git operations produce errors instead of partial success reports.

---


## Development checks

CI uses the current stable Rust toolchain on Linux and macOS; the crate uses Rust edition 2024. Run `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings` from this directory. Tests use disposable repositories and explicit branch names. Some macOS filesystems reject non-UTF-8 filenames, so the filesystem-specific test skips when the OS reports that limitation; byte-level parser coverage still runs.

Status reads NUL-delimited Git output as bytes and rejects malformed or truncated records. On Unix, repository paths can contain non-UTF-8 bytes, with lossy display indicated by `paths_lossy`; managed workspace metadata still requires UTF-8 paths. Other platforms use lossy conversion for Git paths. Git requirements for integration planning are described above; other Git capability and helper-execution safeguards are still being audited.

## Worktree reconciliation and removal

`weave reconcile` (or `--json`) inspects Weave records alongside Git's worktree registrations. It reports missing or unregistered paths, possible moved candidates, branch or metadata drift, locks, and dirty or ignored contents. It also lists registrations whose identity could not be verified against a record; these are never cleanup targets. Possible moves are diagnostics, not proof of ownership. Invalid metadata or Git output can stop inspection; a failed inspection is not a clean result. Reconciliation is read-only.

`weave remove TASK --dry-run` checks a selected managed workspace and previews the path, branch and metadata operations. `weave remove TASK` requires an interactive `yes`; automation must specify `--yes` for that exact task. The branch is retained. Removal refuses pending, missing, moved, locked, primary, detached, drifted, ambiguous, dirty or ignored workspaces, untracked filesystem entries (including empty directories and nested Git data), submodules, hidden index entries, and paths that cannot be safely inspected. It uses Git's non-force removal and deletes the record only after Git succeeds. Stale records and unknown registrations are left for manual investigation. If Git removal succeeds but metadata deletion fails, inspect the printed record and Git registration before manually removing that record; do not remove any workspace directory as a recovery shortcut.

Checks are repeated after confirmation, but concurrent external Git or filesystem changes can still race with deletion; this is not an atomic transaction. Safe removal currently requires Unix file identity checks. `status` and `list` JSON retain their version 1 contracts; `reconcile --json` is a separate version 1 diagnostic object with `managed` and `unknown` arrays. Managed entries include staged, unstaged, untracked and ignored counts; these are null when inspection is unavailable. A blocked dry-run reports the record and reason when the inventory can be read, exits nonzero and changes nothing. A worktree with filesystem data that Git status omits is reported as `untracked_or_uninspectable_filesystem_data`, not clean.

Git must support `status --porcelain=v1 -z`, `worktree list --porcelain -z`, and `rev-parse --path-format=absolute`; planning additionally requires the capabilities listed in the integration section. Weave invokes Git with argument vectors, suppresses hooks and fsmonitor per invocation, and disables external diff and text conversion where it collects diffs. These settings do not modify user configuration. Git checkout during `new` may run configured smudge or process filters, and status or diff operations may invoke effective clean filters; use trusted repositories and Git configuration. Reconciliation and removal refuse inspection when effective clean/process filter commands are non-empty. The integration command runs a validation command only when explicitly supplied.

---

## Roadmap

| Phase | Focus | Status |
|---|---|---|
| 01 | Repository discovery | Implemented |
| 02 | Managed worktrees, reconciliation and confirmed removal (new workspaces default outside Git metadata) | Implemented, verification pending: safety review; hosted CI passed for cbba825 |
| 03 | Diff collection and overlap detection | Implemented; untracked content capture is opt-in |
| 04 | Non-destructive integration planning and review branches | Implemented; untracked content requires opt-in; validation uses a minimal environment but is not sandboxed |
| 05 | Ratatui terminal interface | Implemented; local and SSH launch/create/exit smoke tests passed |
| 05c | TUI-first parity and keyboard workflows (WEV-36) | Implemented: ordered review and integration, confirmed mutations, paged diffs, keyboard palette, narrow layouts, local and SSH smoke tests; limits documented |
| 06 | Vineweaver engine | Implemented: snapshot-scoped hunk/whole-file assignment and isolated validation; untracked content requires opt-in |
| 06b | Vineweaver review and Untangle export (WEV-18) | Implemented, SSH verification pending: local TUI review, ordering/undo, verified patch and independent-commit bundle exports |
| 07 | SSH remote linking | Planned |
| 08 | Remote uncommitted snapshots | Planned |
| 09 | Distributed integration | Planned |
| 10 | Neovim and MCP integrations | Planned |
| 11 | Native macOS client | Planned |
| 12 | Production hardening | Planned |

Implementation status indicates that the listed commands exist, not that every edge case or production requirement has been resolved.

The long-term goal is a unified workflow for collecting, reviewing, and integrating changes from parallel agents running across multiple machines.

## Contributing

Weave is under active early development.

Contributions, bug reports, design discussions, compatibility testing, and feedback from developers using parallel coding agents are welcome.

Particularly valuable areas include:

- Git edge cases and cross-platform compatibility
- Safe changeset collection
- Patch and conflict analysis
- Terminal user experience
- Agent-independent integrations
- Remote snapshot correctness

## License

MIT

---

**Weave — Many agents. Many machines. One coherent codebase.**
