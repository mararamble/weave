# Project instructions

## Workflow
- Work in this project directory for Weave changes.
- Do not add implementation report Markdown files to the repository root. Keep reports outside the repository or in a dedicated Git-ignored folder.
- Update the README roadmap when a step is completed, reflecting its actual scope and any pending verification.
- Implement only the requested scope; roadmap features remain out of scope unless requested.

## Required checks
- `cargo fmt --check`
- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`

## Safety
- Inspection, planning and Untangle must not write to source worktrees, indexes, branches or refs.
- Exports and integration branches must not overwrite existing paths or refs.
- Worktree removal requires explicit confirmation and must refuse unsafe or uncertain states.
- Preserve repository path bytes and exact `path_hex` values; workspace metadata paths currently require UTF-8.
- Never weaken sensitive untracked-path exclusions or expose excluded file contents.
- Do not run user-supplied commands with the parent environment inherited.

## Tests and compatibility
- Use disposable repositories with explicit branch names. Test behavior changes and refusal paths.
- JSON outputs are versioned (currently schema version 1); document and version incompatible field changes.
- Keep detailed behavior in `docs/` where appropriate.

## Terminology
- Weave is the tool; Untangle is the action and `weave untangle` command; Vineweaver is the engine and interactive review UI.
- Preserve `weave untangle` and its flags, JSON schema v1 fields and values including manifest `engine: "vineweaver"`, bundle refs `refs/heads/vineweaver-0001` and following, `.vineweaver-export-*` staging names, artifact filenames, and the `Weave Preview` synthetic identity.
