# Human terminal output boundaries

Weave escapes C0 and C1 terminal controls and DEL in human-readable CLI output and errors. Line feeds are retained to preserve multiline output, including line feeds that originated in untrusted text. Ordinary Unicode remains readable. Existing lossy handling of non-UTF-8 text is unchanged; byte-exact path data remains available in `path_hex`.

`weave diff` preserves its existing redirected stdout, including any control bytes, for pipelines. When stdout is a terminal, its displayed diff and untracked summaries use the human-output encoder. The TUI reads redirected diff output and escapes it when drawing. JSON is not passed through the human-output encoder.

The human-output inventory and regression fixtures are:

| Output route | ESC fixture |
| --- | --- |
| Status, workspace creation/listing, reconciliation, removal preview and confirmed removal | `human_cli_routes_escape_fixture_values_and_errors` |
| Unknown worktrees, blocked removals, and untracked summaries | `human_refusals_unknown_worktrees_and_untracked_summaries_are_safe` |
| Overlap output, conflict plans, and Untangle preview/export | `human_cli_routes_escape_fixture_values_and_errors` |
| Integration output, validation output, and integration or Git errors | `human_cli_routes_escape_fixture_values_and_errors` and `human_refusals_unknown_worktrees_and_untracked_summaries_are_safe` |
| Argument-parser and top-level errors | `human_cli_routes_escape_fixture_values_and_errors` |
| Displayed versus redirected diff | `terminal_diff_escapes_content_while_piped_output_does_not` and `json_and_path_bytes_are_unchanged_and_redirected_diff_stays_exact` |
| Workspace browser panels and confirmations | `every_browser_panel_and_confirmation_escapes_terminal_controls` |
| Vineweaver review panels | `every_review_panel_escapes_terminal_controls` |
| Both interactive interfaces on a pseudo-terminal | `both_interactive_interfaces_render_escape_fixture_safely` |

Static prompts do not interpolate repository data. Ratatui emits its own terminal control sequences for screen management; repository-supplied text is encoded separately before rendering. A JSON consumer, redirected diff reader, or other program that later displays raw data is responsible for its own terminal-safe rendering.
