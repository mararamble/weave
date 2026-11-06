# Git filter safety refusals

Planning, integration, Untangle, reconciliation and removal check the effective
Git `filter.*.clean` and `filter.*.process` commands before operations that might
execute them. A non-empty effective command blocks inspection even if no current
file appears to use it. Failure to read or parse configuration also blocks
inspection. No filter command is executed by this check; Weave never edits Git
configuration to get past it.

From the affected workspace, inspect definitions and their origins:

```sh
git config --show-origin --get-regexp '^filter\..*\.(clean|process)$'
git config --show-origin --get 'filter.example.clean'
git config --show-origin --get 'filter.example.process'
```

The first command lists definitions, including overridden ones. Replace the
example key in the other commands with each reported key to inspect its effective
value and origin. An empty effective value is inactive; a local empty value can
override a global command. Smudge-only and required-only definitions do not
trigger this guard. These inspection commands print configuration values: inspect
them locally and avoid sharing secrets they might contain. Weave's own diagnostic
prints keys and origins, not command values.

If configuration cannot be inspected, check the workspace path, permissions,
configuration syntax and Git availability before retrying. A refusal does not
prove that a filter is malicious. The correct configuration depends on the
repository; consult its owner before making changes. Do not disable Git LFS
indiscriminately: repositories using LFS may need their filters for correct file
contents. Weave offers no bypass for this safety check.

Human reconciliation output and the TUI reconciliation review explain filter
refusals. Schema-v1 reconciliation JSON retains `inspection_unavailable` and null
counts with no additional fields. It is a degraded diagnostic result, not proof
that the workspace is clean, and cannot authorize removal. Use `weave reconcile`
without `--json` for the explanation. Other blocked commands exit nonzero;
`--json` does not turn an error into a successful JSON report.

This guard does not extend to ordinary `status`, `list`, or `diff`, or workspace
creation. Those retain their existing trusted-Git-configuration requirements;
checkout can also invoke smudge filters. See the README security limitations.
