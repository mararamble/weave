# Weave

Build with `cargo build`; run `cargo run -- status` or `cargo run -- status --json` inside a Git worktree. Git must be available on PATH.

JSON schema version 1 contains `schema_version`, canonical local paths `repository_root`, `worktree_path`, `git_dir`, and `common_dir`, nullable `branch` and `detached_head`, booleans `unborn`, `dirty`, and `paths_lossy`, and integer counts `staged`, `unstaged`, and `untracked`. Paths are diagnostic, not persistent repository identities. `paths_lossy` indicates non-UTF-8 path bytes were replaced for display. Errors are printed to stderr and return a nonzero exit status.

## Managed worktrees

From a Git worktree, run `weave new auth --from main` and `weave new parser --from main`, replacing `main` with an existing commit reference. The command prints the new directory. Use `weave list` or `weave list --json` from any worktree of the same repository. `WEAVE_WORKSPACE_DIR` optionally sets the parent directory; otherwise workspaces live under the Git common directory in `weave-workspaces`. Task names use ASCII letters, digits, hyphens and underscores.

Metadata lives in the shared Git common directory under `weave`. If creation is interrupted or fails, `list` shows a pending record. Inspect the recorded path and branch before manually removing its metadata JSON file to permit a retry. Weave never automatically removes either. Paths used for managed workspaces and the common directory must be UTF-8.

List JSON schema version 1 has `schema_version` and a `workspaces` array. Each item contains `schema_version`, `name`, `branch`, `base_commit`, `repository` (the local Git common directory), `path`, `state` (`pending` or `ready`), `exists` (registered with Git and present on disk), and `dirty` (boolean when present, null otherwise). Ordinary Git worktrees are excluded.

## Workspace diffs

Run `weave diff TASK`, `weave diff TASK --stat`, or `weave diff TASK --json` from any worktree of the same repository. Tracked changes are compared with the workspace's recorded creation commit. The default output contains a Git patch and hex-encoded untracked content; `--stat` summarizes it.

The version 1 JSON object has `schema_version`, `workspace_id` (the task name within this repository), `name`, `repository` (local Git common directory), `base_commit`, `head_commit`, `warnings`, and arrays `committed_changes` (base to HEAD), `staged_changes` (HEAD to index), `unstaged_changes` (index to worktree), `untracked_changes`, and `aggregate_changes` (base to worktree plus untracked files). Categories describe layers and can contain the same path; use the aggregate to avoid double counting.

Each change has `kind`, `path` and `previous_path` (lossy display strings), `path_hex` and nullable `previous_path_hex` (exact path bytes), nullable `added`, `deleted` and `binary`, nullable Git `old_mode` and `new_mode` strings, nullable `content_hex`, and nullable `incomplete`. Counts are Git numstat counts for tracked changes and line counts for captured untracked text. Binary counts are null. `content_hex` contains raw bytes only for successfully captured untracked regular files; tracked content is represented by the human patch, not embedded in JSON. `incomplete` explains an omitted untracked file. An incomplete entry is also present in the aggregate.

Ignored files are excluded. Untracked reads exclude `.env` and `.env.*`, `.ssh`, `.aws`, `.gnupg`, `.netrc`, `credentials`, `secrets`, `id_rsa`, `id_ed25519`, and names ending in `.pem`, `.key`, `.p12`, or `.pfx` (case-insensitive). This name-based list cannot identify every secret: inspect output before sharing it. Symlinks and non-regular files are not read. Untracked files are limited to 1 MiB each and 8 MiB total; Git output is limited to 16 MiB per operation. On non-Unix systems untracked content is omitted because safe no-follow reads are unavailable.

Collection checks HEAD, tracked diffs, index diffs, and untracked contents twice and fails if it detects a change. It is not an atomic filesystem snapshot, so changes that occur and reverse between checks can escape detection. A successful diff does not imply semantic or merge compatibility.

## Workspace overlaps

Run `weave overlaps` to compare every ready managed workspace, or `weave overlaps auth parser --json` to compare selected workspaces. File matches use exact path bytes and include rename source paths. For compatible bases and text modifications, zero-context Git hunks are compared in original source coordinates; overlapping or adjacent regions are reported as `region_overlap`, and distant ones as `separate_regions`. Insertions use Git's zero-length original range. A shared file is an observation, not a confirmed merge conflict or a guarantee of semantic compatibility.

Overlap JSON schema version 1 contains `schema_version`, `warnings`, and `overlaps`. Each pairwise overlap has `path`, `path_hex`, two `workspaces`, `classification` (`file_overlap`, `region_overlap`, `separate_regions`, `add_add`, or `delete_modify`), nullable `region_overlap`, nullable `regions` (two arrays of original `start` and `length`), and nullable `incomplete`. Differing base commits, rename/copy provenance, binary files, missing content, additions, deletions, and mode-only changes can make region comparison incomplete. Git diffs are bounded to 16 MiB. Analysis checks each text diff twice but is not an atomic snapshot; concurrent edits can still escape detection. Missing workspaces or failed Git commands cause an error rather than a partial success report.
