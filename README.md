# Weave

**Many agents. Many machines. One coherent codebase.**

Weave is an open-source, Git-native tool for isolating, tracking, and managing code changes produced by parallel AI coding agents.

Run multiple agents against the same repository, keep their changes independent, inspect overlapping modifications, and maintain control over what eventually enters your codebase.

Built in Rust. Terminal-first. Agent-agnostic.

> **Project status: Pre-alpha.** Local worktree management, diff inspection, overlap detection, and non-destructive integration planning are implemented. Remote synchronization and interactive change separation are planned.

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

**Structured output**

Machine-readable JSON is available for repository status, workspace listing, reconciliation, diff inspection, overlap analysis, integration planning, and integration results.

### Planned

- **Integration enhancements:** Persistent, replayable plans and richer recovery and validation workflows.
- **Untangle:** Interactively separate mixed working-tree changes into independent patch groups.
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
git clone <your-repository-url>
cd weave

cargo build --release
```

Replace the URL with your actual GitHub repository URL.

### Install locally

```bash
cargo install --path . --locked
```

This installs the `weave` executable into Cargo's binary directory, normally `~/.cargo/bin`.

Make sure that directory is available on your `PATH`.

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

## Integration planning

`weave plan task-a task-b --json` previews the selected workspaces in the given order, starting at the first workspace's recorded creation commit. It includes committed changes and the final tracked working-tree state (including staged and unstaged changes), plus safely captured untracked files. Each workspace's base and HEAD are recorded. The preview uses a private index and object database and `git merge-tree --write-tree`; it does not update source trees, indexes, branches, or refs. It stops the merge sequence at the first conflict. Git must support `merge-tree --write-tree` and `rev-parse --show-object-format`.

Plan JSON schema version 1 contains `schema_version`, `target_base`, `status` (`clean` or `conflict`), `selected_workspaces`, ordered processed `steps`, and `warnings`. Each step records workspace, creation base, HEAD, status, staged/unstaged/untracked counts, temporary tree ID, and nullable Git conflict details. Steps after the first conflict are not evaluated. Tree IDs refer to temporary objects and are informational only; they disappear when the command exits. A conflict is a successfully computed plan and exits zero. Operational errors exit nonzero and produce no JSON plan.

The final worktree state is previewed as one snapshot per workspace; staging is reported separately but intermediate index contents are not independently integrated. Ignored files are excluded. Untracked capture follows the limits and sensitive-path exclusions described in Safety and limitations below; if any selected untracked file cannot be captured, the plan fails rather than omitting it. Unmerged indexes, assume-unchanged or skip-worktree entries, unsupported index features, and configured clean/process filters are rejected. Git output is capped at 16 MiB; the copied source index is limited to 64 MiB. Collection rechecks captured content and metadata but is not atomic. Custom merge drivers and configuration are disabled in the preview, so configured merges may differ. A clean merge does not establish semantic compatibility. Plans are not persisted or replayable; `weave integrate` builds and checks a fresh candidate when it runs.


## Create an integration branch

```bash
weave plan auth parser
weave integrate --branch review/auth-parser auth parser
weave integrate --branch review/tested --json auth parser -- cargo test
```

`weave integrate` captures and merges the named workspaces in order, starting from the first workspace's creation base. It builds a fresh candidate and stops before branch creation on conflicts, incomplete snapshots, detected source changes, or failed validation. The target branch must be new; creation uses an atomic create-only ref update. Original workspaces and source branches are not modified. The result is a branch ref, not a checked-out worktree. Create a separate Git worktree or check it out when ready to review.

The optional command after `--` is executed directly with its arguments in a disposable checkout of the candidate. Only supply commands you trust: validation executes code with your user permissions and can access files and the network. Git commands in validation are directed to the disposable Git state. Changes made in the validation checkout are not included in the branch; tracked modifications are reported. Stdout and stderr are each capped at 64 KiB, and failure creates no branch. Temporary validation state is cleaned up when the command exits normally.

JSON schema version 1 reports `branch`, `commit`, `tree`, `target_base`, ordered `sources` with each base, HEAD, snapshot commit and included source commit IDs, nullable `validation`, and `warnings`. Snapshot and integration commits are synthetic commits on the review branch; source branch commits are not rewritten. Validation arguments are displayed lossily if they contain non-UTF-8 bytes. A clean merge does not imply semantic compatibility.

Weave writes a recovery journal under the common Git directory at `weave/integrations` before transferring candidate objects and creating the branch. After interruption or a publication error, inspect the journal's `reference` and `candidate` and compare them with `git rev-parse --verify <reference>`. A matching ref means the branch was created; an absent ref means it was not. A different ref belongs to another operation and must not be overwritten. Once inspected, a stale journal may be removed manually. Failed transfers can leave unreferenced Git objects; no automatic cleanup of user refs or files is performed.

The planner's capture limitations also apply to integration. Capture checks reduce races but are not atomic. Custom Git merge drivers and hooks do not run, so results may differ from a configured merge. Candidate object transfer and Git output are limited to 16 MiB per operation. Validation can be omitted; in that case the result explicitly has no validation record.

---

## How it works

Weave uses Git's existing worktree functionality to give independent development tasks their own working directories.

For example:

```text
my-project/
│
├── main workspace
│
└── .git/
    └── weave-workspaces/
        ├── auth/
        └── parser/
```

This diagram is conceptual; the exact workspace location depends on the repository's Git common directory and Weave configuration.

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
<Git common directory>/weave-workspaces/
```

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

For successfully captured untracked regular files, `content_hex` contains their raw bytes.

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

Ignored files are excluded.

Untracked reads exclude sensitive filenames and directories including:

```text
.env
.env.*
.ssh
.aws
.gnupg
.netrc
credentials
secrets
id_rsa
id_ed25519
*.pem
*.key
*.p12
*.pfx
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

Changeset collection checks HEAD, tracked changes, index changes, and untracked content twice, rejecting detected inconsistencies.

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

Git must support `status --porcelain=v1 -z`, `worktree list --porcelain -z`, and `rev-parse --path-format=absolute`; planning additionally requires the capabilities listed in the integration section. Weave invokes Git with argument vectors, suppresses hooks and fsmonitor per invocation, and disables external diff and text conversion where it collects diffs. These settings do not modify user configuration. Git checkout during `new` may run configured smudge or process filters, and status or diff operations may invoke configured clean filters; use trusted repositories and Git configuration. Reconciliation and removal refuse inspection when clean or process filters are configured. The integration command runs a validation command only when explicitly supplied.

---

## Roadmap

| Phase | Focus | Status |
|---|---|---|
| 01 | Repository discovery | Implemented |
| 02 | Managed worktrees, reconciliation and confirmed removal | Implemented; hosted CI and safety review pending |
| 03 | Diff collection and overlap detection | Implemented |
| 04 | Non-destructive integration planning and review branches | Implemented |
| 05 | Ratatui terminal interface | Planned |
| 06 | Mixed-diff Untangle engine | Planned |
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

A license has not yet been selected.

---

**Weave — Many agents. Many machines. One coherent codebase.**
