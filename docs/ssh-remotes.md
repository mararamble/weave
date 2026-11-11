# SSH profiles and read-only remote discovery (WEV-20)

`weave remote` registers local SSH profiles and discovers Git metadata. It does
not transfer objects or dirty content, run Git remotely, or add TUI screens.
Requires OpenSSH locally and POSIX Python **3.9+** remotely (available through
`/usr/bin:/bin`). There is no installed helper, remote temporary file or daemon.

## CLI contract and setup

The approved boundary is a private user-level registry, existing OpenSSH host
trust, and CLI/JSON first. Commands are available outside a local Git repository:

```sh
weave remote add workstation my-ssh-alias --root=/home/me/projects
weave remote list
weave remote show workstation
weave remote check workstation --json --timeout=30
weave remote discover workstation --json --max-depth=4 --timeout=30
weave remote remove workstation
```

Repeat `--root=` for up to 16 roots. Names are 1–64 ASCII letters/digits/`_`/`-`
and must not start with `-`. Destinations are an SSH alias or `[user@]hostname`
using ASCII letters/digits/`.`/`_`/`-` and at most one `@`. Configure ports, IPv6,
keys, proxies and usernames in OpenSSH config. Roots are absolute UTF-8 POSIX
paths; `.` and repeated internal slashes normalize, but `..`, control characters,
`//` prefixes and the filesystem root `/` are rejected. Discovery preserves
non-UTF-8 **discovered** paths using exact lowercase `path_hex` values. Their
human-readable `path` uses UTF-8 replacement and sets `paths_lossy: true`.

The registry defaults to `$XDG_CONFIG_HOME/weave/remotes.json`, or
`$HOME/.config/weave/remotes.json`. Config directories must be absolute. Explicit
`--registry=/absolute/file` supports isolated registries and testing. The v1
registry contains `schema_version: 1` and a `profiles` object mapping names to
`{ "destination": "alias", "roots": ["/absolute/root"] }`. No credentials are
stored. New directories use 0700 and registry files 0600 on Unix; existing
non-private files and symlink registry files are refused. Updates use an exclusive
sibling `.json.lock` and atomic same-directory replacement. A stale lock causes
refusal and must be inspected by the user. Existing profiles cannot be overwritten:
remove the intended profile explicitly before adding its replacement. Add/remove
print the exact local effect; list/show always emit JSON and never connect.

## Trust and processes

Verify the host's SHA256 fingerprint with its administrator through an independent
channel, then configure OpenSSH `known_hosts` through your normal trusted setup.
`ssh-keyscan` output alone is not identity verification. Weave never scans keys,
prompts for trust, writes `known_hosts`, or accepts unknown/changed keys. It forces
`StrictHostKeyChecking=yes`, `UpdateHostKeys=no`, `VerifyHostKeyDNS=no`,
`KnownHostsCommand=none` and `BatchMode=yes`. Host-key
failure identifies the destination and explains fingerprint verification. Raw SSH
stderr is suppressed because it may include key paths, proxy credentials or
untrusted text. JSON supplies stable failure categories instead.

The system `ssh` is invoked with an argument vector and a scrubbed environment:
fixed `PATH=/usr/bin:/bin`, `LC_ALL=C`, and (when set) `HOME` and
`SSH_AUTH_SOCK`; the capture runner also sets disposable local `TMPDIR`/`TMP`/`TEMP`. The user's OpenSSH config still controls identity, host-key files,
proxies and known-host files. Dynamic host-key commands and DNS key trust are
disabled so existing known-host trust is required. Custom proxy commands depending on
other inherited environment variables may need explicit config adjustment. Local
commands, TTYs, agent/X11 forwarding, tunnels, configured forwards and connection
sharing are disabled. SSH login shell startup, server auditing and configured
proxy commands belong to OpenSSH/the host and can have their own effects.

The **fixed** remote command is:

```sh
exec env -i PATH=/usr/bin:/bin LC_ALL=C python3 -I -B -
```

The Python source and hex-encoded request JSON travel on stdin; destinations and
roots are never interpolated into shell code. `-I -B` isolates Python from user
modules/environment and disables bytecode writes. The helper reads `.git` files,
`HEAD`, `commondir`, loose/packed refs and `worktrees/*/gitdir` directly. It never
loads Git config, invokes Git, hooks, filters or fsmonitor, inspects indexes or file
contents, runs status, changes refs or checks out files. Consequently no remote
Git environment is needed. Any future Git invocation must use a scrubbed environment,
`GIT_OPTIONAL_LOCKS=0`, disabled hooks and disabled fsmonitor.

All path components are opened with `O_NOFOLLOW`; directory symlinks are skipped
and reported. All metadata must remain within a configured root, including
separate Git directories, linked worktrees and their common directory. Register
additional roots when these are separate. Missing/stale registrations, unsupported
ref encodings, malformed metadata and reftable repositories produce partial results;
reftable is not interpreted. Git repository integrity, object existence and dirty
state are not verified. Detection uses metadata layout; it is not `git fsck`.

Weave requests no remote writes. Reading files can update filesystem access times,
and SSH itself may write server audit logs. Preservation tests compare file bytes,
file modes, sizes, modification times, indexes, refs, config and worktree metadata;
they do not claim unchanged access times or host-wide auditing state.

## Bounds, framing and JSON

Connection timeout is 10 seconds (one attempt). The overall local deadline defaults
to 30 seconds, configurable from 1 to 300 seconds; it includes SSH startup and
helper execution. Local owned SSH/proxy process groups are terminated and reaped
on deadline or excessive output. SSH disconnect is not a remote process supervision
protocol; remote traversal also has its own fixed finite limits.

Default traversal depth is 4 (root is depth 0), configurable 0–16. Limits are
10,000 visited directories, 10,000 entries per directory, 1,000 repository records,
1,000 reported issues, 1 MiB per metadata file, 4 MiB captured stdout and 64 KiB
retained stderr (shared runner stops at 32 MiB total stderr). Any reached traversal
limit is reported as partial; excessive captured output fails the request. Additional
issues beyond the issue limit are omitted while the result remains partial. Worktree
registrations are followed within roots even beyond the search depth.

Each request includes a nonce. Output must have exactly one
`\nWEAVE_REMOTE_<nonce>_BEGIN\n` and one corresponding `_END\n`, in order.
Only the JSON between them is parsed; everything outside is discarded as untrusted
banner/trailer text. The sentinel is framing, not authentication: SSH supplies
transport identity. Responses are untrusted even inside the frame: schema, states,
counts, object IDs, path bytes/containment, encodings and duplicate repository paths
are checked locally. No remote strings are executed or used as local paths.

`check --json` and `discover --json` introduce their own **schema version 1**.
Existing local `status`, `list`, provenance and Untangle JSON contracts are unchanged.
Both commands return:

- `schema_version`, `profile`, `destination`, `roots`;
- `state`: `complete`, `partial` or `failed`;
- `diagnostic`: null, or a failure category (`host_key`, `authentication`, `dns`,
  `connectivity`, `timeout`, `remote_command`, `capture_failed`, `protocol`);
- `repositories`: records with `path`, `path_hex`, `paths_lossy`, `git_dir_hex`,
  `common_dir_hex`, `kind` (`worktree`, `linked`, `bare`), `branch` (name or null),
  `head` (40/64 lowercase hex digits or null), and `unborn`;
- `issues`: `{ "path_hex": "...", "reason": "..." }` records identifying skipped
  paths and limitations (for example `missing_path`, `permission_denied`,
  `metadata_outside_roots`, `symlink_skipped`, `depth_limit`, `traversal_limit`).

`check` verifies SSH trust/authentication and helper availability without searching
roots: a complete check is not proof that roots exist. It returns empty arrays.
`discover` reports root accessibility and metadata results. `partial` and `failed`
exit nonzero; machine-readable results still appear on stdout for connection,
protocol and discovery failures, while diagnostics go to stderr. Invalid arguments,
unknown profiles and registry errors fail before SSH and have no response JSON.
Offline/refused connections, DNS, authentication and trust errors are categorized
from known OpenSSH English diagnostics; unrecognized failures use `remote_command`.
No partial result should be treated as a complete inventory.

## Verification

`cargo test` runs registry, parser, process timeout/limit tests and a Python metadata
suite on disposable repositories with explicit branches. The suite snapshots all
fixture files and metadata before/after, covers normal/bare/linked/detached/unborn
repositories, packed refs, hostile Git config, unusual paths, symlinks, unsupported
encodings, FIFOs, missing/escaping metadata and traversal/file limits. Non-UTF-8
filenames are skipped explicitly on filesystems that reject them (APFS).

Opt-in real SSH transport/refusal harness:

```sh
python3 -I -B tests/remote_ssh_harness.py
```

It requires a runnable local `sshd`, creates temporary host/client keys and config,
and never edits `~/.ssh`. It checks known-key discovery, unknown/changed host-key
refusal, wrong-identity refusal, unchanged known-host files on refusal, and repository
byte/mode/mtime preservation. Same-machine SSH is not Mac–Linux acceptance. That
acceptance remains pending until a controlled Linux target is reachable.
