# Status paths (schema version 1)

These fields describe the worktree where `weave status` runs:

| Field | Meaning |
|---|---|
| `repository_root` | Root of the current worktree, from Git's `--show-toplevel`. This is a historical v1 name, not a promise to locate the primary worktree. |
| `worktree_path` | The same current worktree root as `repository_root`. |
| `git_dir` | The current worktree's Git directory, from `--absolute-git-dir`. A linked worktree has its own administrative directory. |
| `common_dir` | Shared Git administrative directory, from `--git-common-dir`. It can be `.git` under the primary worktree, a separate Git directory, or a bare repository used by linked worktrees. It is not the primary worktree root. |

Paths are resolved by Git and canonicalized by Weave. Invocation from a nested
subdirectory reports its enclosing worktree's root, not the invocation directory.
Primary and linked worktrees therefore have different root fields but share
`common_dir`; both root fields remain equal within each individual status result.
No v1 field changes meaning, no new primary-worktree field is added, and no schema
version transition is introduced by this clarification.

Status does not search for a primary worktree. A linked worktree can remain
inspectable when its primary worktree is unavailable, provided its own root and
Git administrative directories remain accessible. If the current worktree or a
required administrative path cannot be resolved, status fails rather than
inventing paths. Outside a Git repository and inside a bare repository without a
current worktree, it exits nonzero with empty stdout, including with `--json`.
Unborn and detached HEAD states do not change path meanings.

On Unix, Git paths are retained as bytes while inspecting the repository. Status
JSON renders them as strings; invalid UTF-8 is replaced for display and
`paths_lossy: true` warns that at least one of the three distinct paths was lossy.
Status v1 has no byte-exact root-path companion. Consumers must not reconstruct
exact filesystem paths from lossy strings. This limitation is separate from
changeset records, whose existing `path_hex` fields retain exact file-path bytes.
Managed workspace metadata continues to require UTF-8 paths. Other platforms
retain their existing lossy path conversion. Filesystems that reject invalid
UTF-8 names cannot exercise the filesystem fixture; parser-level byte coverage
and Linux acceptance provide the other verification paths.

In-repository consumers are human CLI status output and the TUI JSON inspection
view; neither interprets `repository_root` as the primary worktree. External v1
consumers retain the existing fields and values. Human terminal display escapes
controls; JSON consumers are responsible for safe display of its raw strings.
