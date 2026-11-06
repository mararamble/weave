# Workspace names beginning with hyphens (WEV-41)

Names retain the existing validation rules: 1–80 ASCII letters, digits, hyphens
or underscores. `-odd`, `--help`, `--yes`, `-` and `--` are valid names, including
their generated `weave/NAME` Git branches. Existing records remain usable.

## CLI syntax

For commands with workspace positionals, put all options before `--`, then give
the literal names. A name that looks like an option is data only after that
boundary. Examples:

```sh
weave new --from main -- -odd
weave diff --json -- -odd
weave diff --stat -- --help
weave diff --include-untracked-content -- --yes
weave untangle --json --include-untracked-content -- --help
weave untangle --review -- --help
weave plan --json --include-untracked-content -- ordinary -odd --yes
weave overlaps --json -- --help ordinary -odd
weave remove --dry-run -- --yes
weave remove --yes -- --yes
```

`weave diff -- --` addresses the name `--`; `weave diff -- -` addresses `-`.
Untracked-content opt-in remains necessary for capture, planning, integration
and Untangle when untracked files are present. `--yes` before the separator is
still confirmation; `--yes` after it is just a workspace name. Preview/refusal
and confirmed removal keep the same safety checks; removal retains the branch.

Integration already reserves `--` for the validation executable and its exact
arguments. Use repeatable `--workspace=NAME` for workspace names instead:

```sh
weave integrate --branch review/hyphens --workspace=-odd --workspace=ordinary
weave integrate --branch review/tested --json --include-untracked-content \
  --workspace=--help --workspace=ordinary --workspace=--yes -- cargo test --locked
weave integrate --branch review/delimiters --workspace=-- --workspace=-
```

The `=` and a nonempty value are required. Repeat the option once per workspace,
in integration order. Do not mix named selection with positional names: that is
rejected instead of guessing their combined order. Ordinary positional syntax
such as `weave integrate --branch review/ordinary first second -- cargo test`
remains supported. Recognized options retain their meanings, including `--help`,
and unknown options are rejected. Arguments after integration's `--` belong to
the validation command and are never interpreted as Weave options or names.

## CLI/TUI inventory and original behavior

The snapshot base was `66308af`. Its binary was exercised in a disposable `trunk`
repository with an existing `-odd` workspace. Unescaped leading-hyphen names
failed parsing (exit 2) in every name-carrying command. The CLI routes using `--`
below already worked, except integration. The TUI passed names before options
without a separator; those subprocess paths could therefore misparse them.

| Flow | Original path | Current path |
|---|---|---|
| Create (`new`; `n`, palette `new`) | TUI `new NAME --from REF` | `new --from REF -- NAME` |
| Listing, status, reconcile (`w`, `s`, `c`) | No CLI name positional; selected details read from listing JSON | Unchanged; literal selection retained |
| File loading; diff JSON (`Enter`, `J`) | TUI `diff NAME --json [opt-in]` | `diff --json [opt-in] -- NAME` |
| Diff statistics (`t`) | TUI `diff NAME --stat [opt-in]` | `diff --stat [opt-in] -- NAME` |
| Tracked patch / untracked content browsing | Direct collector; raw Git paths after Git `--` | Unchanged; keyboard coverage verifies it |
| Plan (`p`, palette `preview`) | TUI options then unescaped names | All options, `--`, selected names in order |
| Overlaps (`o`, palette `overlaps`) | TUI options then unescaped names | All options, `--`, selected names in order |
| Removal preview (`D`, palette `removal-preview`) | TUI `remove NAME --dry-run` | `remove --dry-run -- NAME` |
| Confirmed removal (`d`, palette `remove`) | TUI `remove NAME --yes` | `remove --yes -- NAME` |
| Integration (`i`, palette `integrate`) | Names before validation boundary; `-- NAME` alone could not work | Always `--workspace=NAME`, including ordinary names, then optional `-- VALIDATION...` |
| Untangle (`u`) | Direct `untangle_review::run_name`, no subprocess parser | Same direct core call and opt-in; keyboard regression coverage |
| CLI Untangle preview/review/export | One `NAME` positional | All options before `-- NAME`; existing parser behavior retained |

No parser-wide `allow_hyphen_values` is enabled: that could consume unknown
options as workspace names and blur confirmation flags. Only integration needs
the approved new syntax. Other commands use the conventional separator.

## Verification boundary

Disposable-repository regressions cover new and pre-existing records for all
five unusual names plus ordinary names, exact multi-name order, schema-v1 JSON,
opt-in/opt-out diff behavior, validation flags and empty arguments, source files,
index bytes and refs, dirty/uncertain refusal, and branch-preserving removal.
PTY tests exercise creation, palette actions, browsing, statistics/JSON, Untangle,
planning, overlaps, cancellation and confirmed removal for all five unusual
names, plus mixed-name ordered integration with/without validation.

macOS and Linux verification must be recorded separately. The implementation
report outside the repository records actual runs. Missing Linux execution is
an open WEV-41 gate; a local macOS pass does not close it.
