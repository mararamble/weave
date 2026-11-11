//! CLI-only SSH metadata discovery. Existing local contracts do not use this module.
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const PROBE: &str = include_str!("remote_probe.py");
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
// Fixed remote shell program; profile values are passed only in stdin JSON.
const REMOTE_COMMAND: &str = "exec env -i PATH=/usr/bin:/bin LC_ALL=C python3 -I -B -";

#[derive(Args)]
pub(crate) struct Options {
    /// Override the user-level registry (primarily for isolated testing)
    #[arg(long, global = true)]
    registry: Option<PathBuf>,
    #[command(subcommand)]
    command: Operation,
}
#[derive(Subcommand)]
enum Operation {
    /// Save a new local profile; does not connect or trust a host key
    Add {
        name: String,
        destination: String,
        #[arg(long = "root", required = true, require_equals = true)]
        roots: Vec<String>,
    },
    /// Show all local profiles without connecting
    List,
    /// Show one local profile without connecting
    Show { name: String },
    /// Remove only a local profile; never removes a remote worktree
    Remove { name: String },
    /// Verify known-host trust, authentication and helper availability
    Check {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// Discover read-only metadata within the profile's roots (Python 3 required)
    Discover {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(0..=16))]
        max_depth: u8,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    destination: String,
    roots: Vec<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: u32,
    profiles: BTreeMap<String, Profile>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profiles: BTreeMap::new(),
        }
    }
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('-')
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "Profile names must be 1–64 ASCII letters, digits, underscores or hyphens and cannot start with '-'"
    );
    Ok(())
}
fn validate_destination(destination: &str) -> Result<()> {
    ensure!(
        !destination.is_empty()
            && destination.len() <= 253
            && !destination.starts_with('-')
            && destination
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-@".contains(&c))
            && destination.matches('@').count() <= 1
            && !destination.starts_with('@')
            && !destination.ends_with('@'),
        "Use an SSH alias or [user@]hostname; configure ports/IPv6 in OpenSSH config"
    );
    Ok(())
}
fn normalize_root(root: &str) -> Result<String> {
    ensure!(
        root.starts_with('/')
            && !root.starts_with("//")
            && root.len() <= 4096
            && !root.chars().any(char::is_control)
            && !root.split('/').any(|p| p == ".."),
        "Roots must be absolute POSIX paths without '..', control characters or a double leading slash"
    );
    let parts: Vec<_> = root
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    ensure!(
        !parts.is_empty(),
        "Searching the filesystem root is not supported"
    );
    Ok(format!("/{}", parts.join("/")))
}
fn validate_profile(profile: &Profile) -> Result<()> {
    validate_destination(&profile.destination)?;
    ensure!(
        !profile.roots.is_empty() && profile.roots.len() <= 16,
        "Use 1–16 roots"
    );
    for root in &profile.roots {
        ensure!(
            normalize_root(root)? == *root,
            "Registry roots must be normalized"
        );
    }
    Ok(())
}
fn registry_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        ensure!(path.is_absolute(), "Registry override must be absolute");
        return Ok(path);
    }
    let base = if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path)
    } else {
        PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable; use --registry")?)
            .join(".config")
    };
    ensure!(base.is_absolute(), "User config directory must be absolute");
    Ok(base.join("weave/remotes.json"))
}
fn load(path: &Path) -> Result<Registry> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
        Err(e) => return Err(e).context("Cannot inspect registry"),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Registry must be a regular file, not a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "Registry permissions must be private (0600)"
        );
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= OUTPUT_LIMIT, "Registry exceeds size limit");
    let registry: Registry = serde_json::from_slice(&bytes).context("Malformed remote registry")?;
    ensure!(
        registry.schema_version == 1,
        "Unsupported remote registry version"
    );
    for (name, profile) in &registry.profiles {
        validate_name(name)?;
        validate_profile(profile)?;
    }
    Ok(registry)
}
fn update(path: &Path, change: impl FnOnce(&mut Registry) -> Result<()>) -> Result<()> {
    let parent = path.parent().context("Registry has no parent")?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)?;
    let lock_path = path.with_extension("json.lock");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _lock = options
        .open(&lock_path)
        .context("Registry is locked; inspect a stale lock before removing it")?;
    struct Unlock(PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _unlock = Unlock(lock_path);
    let mut registry = load(path)?;
    change(&mut registry)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let bytes = serde_json::to_vec_pretty(&registry)?;
    ensure!(bytes.len() < OUTPUT_LIMIT, "Registry exceeds size limit");
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .context("Cannot atomically save registry")?;
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    path: String,
    path_hex: String,
    paths_lossy: bool,
    git_dir_hex: String,
    common_dir_hex: String,
    kind: String,
    branch: Option<String>,
    head: Option<String>,
    unborn: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    path_hex: String,
    reason: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Discovery {
    schema_version: u32,
    state: String,
    repositories: Vec<Repository>,
    issues: Vec<Issue>,
}
#[derive(Serialize)]
struct Response<'a> {
    schema_version: u32,
    profile: &'a str,
    destination: &'a str,
    roots: &'a [String],
    state: &'a str,
    /// Stable category only; never echo raw SSH stderr or remote shell banners.
    diagnostic: Option<&'a str>,
    repositories: &'a [Repository],
    issues: &'a [Issue],
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len().is_multiple_of(2)
            && value.len() <= 32768
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "Invalid remote path_hex"
    );
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| Ok(u8::from_str_radix(std::str::from_utf8(v)?, 16)?))
        .collect()
}
fn validate_remote_path(value: &str, roots: &[String]) -> Result<Vec<u8>> {
    let bytes = unhex(value)?;
    ensure!(
        !bytes.contains(&0)
            && bytes.starts_with(b"/")
            && !bytes
                .split(|c| *c == b'/')
                .skip(1)
                .any(|p| p.is_empty() || p == b"." || p == b"..")
            && roots
                .iter()
                .any(|r| bytes == r.as_bytes() || bytes.starts_with(format!("{r}/").as_bytes())),
        "Remote metadata path escaped permitted roots"
    );
    Ok(bytes)
}
fn parse_frame(bytes: &[u8], token: &str, profile: &Profile) -> Result<Discovery> {
    let begin = format!("\nWEAVE_REMOTE_{token}_BEGIN\n");
    let end = format!("\nWEAVE_REMOTE_{token}_END\n");
    let starts: Vec<_> = bytes
        .windows(begin.len())
        .enumerate()
        .filter(|(_, v)| *v == begin.as_bytes())
        .map(|(i, _)| i)
        .collect();
    let ends: Vec<_> = bytes
        .windows(end.len())
        .enumerate()
        .filter(|(_, v)| *v == end.as_bytes())
        .map(|(i, _)| i)
        .collect();
    ensure!(
        starts.len() == 1 && ends.len() == 1 && ends[0] >= starts[0] + begin.len(),
        "Missing, duplicated or malformed remote sentinel"
    );
    let result: Discovery = serde_json::from_slice(&bytes[starts[0] + begin.len()..ends[0]])?;
    ensure!(
        result.schema_version == 1
            && matches!(result.state.as_str(), "complete" | "partial")
            && result.repositories.len() <= 1000
            && result.issues.len() <= 1000
            && (result.state == "complete") == result.issues.is_empty(),
        "Invalid remote result state or limits"
    );
    let mut seen = std::collections::BTreeSet::new();
    for repo in &result.repositories {
        let path = validate_remote_path(&repo.path_hex, &profile.roots)?;
        validate_remote_path(&repo.git_dir_hex, &profile.roots)?;
        validate_remote_path(&repo.common_dir_hex, &profile.roots)?;
        ensure!(
            repo.path == String::from_utf8_lossy(&path)
                && repo.paths_lossy == std::str::from_utf8(&path).is_err()
                && seen.insert(&repo.path_hex),
            "Inconsistent remote path"
        );
        ensure!(
            matches!(repo.kind.as_str(), "worktree" | "linked" | "bare")
                && repo.unborn == repo.head.is_none()
                && (!repo.unborn || repo.branch.is_some()),
            "Invalid repository record"
        );
        if let Some(head) = &repo.head {
            ensure!(
                [40, 64].contains(&head.len())
                    && head
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "Invalid object ID"
            );
        }
        if let Some(branch) = &repo.branch {
            ensure!(
                !branch.is_empty() && branch.len() <= 4096 && !branch.chars().any(char::is_control),
                "Invalid branch"
            );
        }
    }
    for issue in &result.issues {
        validate_remote_path(&issue.path_hex, &profile.roots)?;
        ensure!(
            !issue.reason.is_empty()
                && issue.reason.len() <= 80
                && issue
                    .reason
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c == b'_'),
            "Invalid remote issue"
        );
    }
    Ok(result)
}
fn failure(stderr: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if text.contains("host key verification failed")
        || text.contains("host identification has changed")
        || text.contains("no matching host key")
    {
        "host_key"
    } else if text.contains("permission denied") || text.contains("authentication failed") {
        "authentication"
    } else if text.contains("could not resolve hostname") {
        "dns"
    } else if text.contains("timed out") {
        "timeout"
    } else if text.contains("connection refused")
        || text.contains("no route to host")
        || text.contains("network is unreachable")
        || text.contains("connection closed")
    {
        "connectivity"
    } else {
        "remote_command"
    }
}
fn ssh_command(destination: &str) -> Command {
    let mut command = Command::new("ssh");
    // Preserve HOME and SSH_AUTH_SOCK for OpenSSH configuration/agent, but no
    // inherited repository routing, proxy command environment or credential vars.
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C");
    for key in ["HOME", "SSH_AUTH_SOCK"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.args([
        "-T",
        "-oBatchMode=yes",
        "-oStrictHostKeyChecking=yes",
        "-oUpdateHostKeys=no",
        "-oVerifyHostKeyDNS=no",
        "-oKnownHostsCommand=none",
        "-oConnectTimeout=10",
        "-oConnectionAttempts=1",
        "-oClearAllForwardings=yes",
        "-oForwardAgent=no",
        "-oForwardX11=no",
        "-oTunnel=no",
        "-oPermitLocalCommand=no",
        "-oRemoteCommand=none",
        "-oRequestTTY=no",
        "-oControlMaster=no",
        "-oControlPath=none",
        "-oControlPersist=no",
        "-oServerAliveInterval=5",
        "-oServerAliveCountMax=1",
        "--",
        destination,
        REMOTE_COMMAND,
    ]);
    command
}
fn query(profile: &Profile, operation: &str, max_depth: u8, timeout: u64) -> Result<Discovery> {
    // Random local tempfile name supplies a per-request sentinel nonce without
    // adding a remote file. It is framing, not authentication (SSH supplies that).
    let nonce = tempfile::Builder::new().prefix("weave-nonce-").tempfile()?;
    let token = hex(nonce.path().as_os_str().as_encoded_bytes());
    let request = serde_json::json!({"roots":profile.roots,"operation":operation,"max_depth":max_depth,"token":token});
    let mut input = tempfile::tempfile()?;
    writeln!(
        input,
        "import json\nREQUEST = json.loads(bytes.fromhex('{}').decode('utf-8'))",
        hex(&serde_json::to_vec(&request)?)
    )?;
    input.write_all(PROBE.as_bytes())?;
    input.rewind()?;
    let mut command = ssh_command(&profile.destination);
    command.stdin(input);
    let output = capture_ssh(&mut command, timeout)?;
    if !output.status.success() {
        bail!("{}", failure(&output.stderr));
    }
    parse_frame(&output.stdout, &token, profile).map_err(|_| anyhow::anyhow!("protocol"))
}

fn capture_ssh(command: &mut Command, timeout: u64) -> Result<std::process::Output> {
    let control = crate::tui_process::Control::new();
    let deadline = control.clone();
    let (finish, finished) = std::sync::mpsc::channel();
    let timer = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(timeout)).is_err() {
            deadline.cancel();
            true
        } else {
            false
        }
    });
    let result = crate::tui_process::capture(command, &control, Some(OUTPUT_LIMIT as u64), 65536);
    let _ = finish.send(());
    let expired = timer.join().unwrap_or(true);
    if expired {
        bail!("timeout");
    }
    let output = result
        .map_err(|_| anyhow::anyhow!("capture_failed"))?
        .into_output()?;
    Ok(output)
}

pub(crate) fn run(options: Options) -> Result<()> {
    let path = registry_path(options.registry)?;
    match options.command {
        Operation::Add {
            name,
            destination,
            roots,
        } => {
            validate_name(&name)?;
            let profile = Profile {
                destination,
                roots: roots
                    .iter()
                    .map(|r| normalize_root(r))
                    .collect::<Result<_>>()?,
            };
            validate_profile(&profile)?;
            update(&path, |registry| {
                ensure!(
                    !registry.profiles.contains_key(&name),
                    "Profile already exists; remove it explicitly before replacing"
                );
                registry.profiles.insert(name.clone(), profile);
                Ok(())
            })?;
            humanln!(
                "Added local profile {name} in {}. No connection or host-key trust change.",
                path.display()
            );
        }
        Operation::Remove { name } => {
            validate_name(&name)?;
            update(&path, |registry| {
                ensure!(registry.profiles.remove(&name).is_some(), "Unknown profile");
                Ok(())
            })?;
            humanln!("Removed local profile {name} from {}.", path.display());
        }
        Operation::List => println!("{}", serde_json::to_string_pretty(&load(&path)?)?),
        Operation::Show { name } => {
            let registry = load(&path)?;
            let profile = registry.profiles.get(&name).context("Unknown profile")?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schema_version":1,"name":name,"profile":profile})
                )?
            );
        }
        Operation::Check {
            name,
            json,
            timeout,
        } => inspect(&path, &name, json, timeout, 0, "check")?,
        Operation::Discover {
            name,
            json,
            timeout,
            max_depth,
        } => inspect(&path, &name, json, timeout, max_depth, "discover")?,
    }
    Ok(())
}
fn inspect(
    path: &Path,
    name: &str,
    json: bool,
    timeout: u64,
    depth: u8,
    operation: &str,
) -> Result<()> {
    let registry = load(path)?;
    let profile = registry.profiles.get(name).context("Unknown profile")?;
    let result = query(profile, operation, depth, timeout);
    let error = result.as_ref().err().map(|e| e.to_string());
    let empty = Discovery {
        schema_version: 1,
        state: "failed".into(),
        repositories: vec![],
        issues: vec![],
    };
    let discovery = result.as_ref().unwrap_or(&empty);
    let response = Response {
        schema_version: 1,
        profile: name,
        destination: &profile.destination,
        roots: &profile.roots,
        state: &discovery.state,
        diagnostic: error.as_deref(),
        repositories: &discovery.repositories,
        issues: &discovery.issues,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        humanln!("{name} ({}): {}", profile.destination, discovery.state);
        for repo in &discovery.repositories {
            humanln!(
                "{} [{}] {}",
                repo.path,
                repo.kind,
                repo.branch.as_deref().unwrap_or("detached")
            );
        }
        for issue in &discovery.issues {
            humanerrln!("Skipped path_hex={}: {}", issue.path_hex, issue.reason);
        }
    }
    if let Some(error) = error {
        if error == "host_key" {
            bail!(
                "host_key for {}: verify the SHA256 host-key fingerprint with the host administrator through an independent channel, then manage known_hosts using OpenSSH. Weave does not scan, trust or replace keys.",
                profile.destination
            );
        }
        bail!(
            "Remote {} failed: {error}; raw SSH diagnostics suppressed to avoid exposing credentials",
            profile.destination
        );
    }
    ensure!(
        discovery.state == "complete",
        "Remote discovery is partial; inspect issues before relying on this listing"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile {
        Profile {
            destination: "fixture".into(),
            roots: vec!["/safe".into()],
        }
    }
    fn frame(value: serde_json::Value) -> Vec<u8> {
        format!("untrusted banner\nWEAVE_REMOTE_test_BEGIN\n{value}\nWEAVE_REMOTE_test_END\nuntrusted trailer").into_bytes()
    }
    #[test]
    fn validates_names_destinations_and_roots_before_ssh() {
        for value in ["", "-option", "a/b", "a\n"] {
            assert!(validate_name(value).is_err());
        }
        for value in [
            "",
            "-oProxyCommand=bad",
            "a;touch bad",
            "a b",
            "a@b@c",
            "a\n",
            "ssh://host",
            "a@",
        ] {
            assert!(validate_destination(value).is_err(), "{value}");
        }
        for value in [
            "relative",
            "/",
            "//server/path",
            "/safe/../escape",
            "/safe\0bad",
        ] {
            assert!(normalize_root(value).is_err(), "{value}");
        }
        assert_eq!(normalize_root("/safe/./dir//").unwrap(), "/safe/dir");
        assert!(validate_destination("user@alias-name.example").is_ok());
        assert!(validate_remote_path("2f736166652d6576696c", &profile().roots).is_err());
        assert!(validate_remote_path(&hex(b"/safe/../escape"), &profile().roots).is_err());
        assert!(validate_remote_path(&hex(b"/safe/\xff"), &profile().roots).is_ok());
    }
    #[test]
    fn registry_updates_are_atomic_private_and_preserve_other_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config/remotes.json");
        update(&path, |r| {
            r.profiles.insert("one".into(), profile());
            Ok(())
        })
        .unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            update(&path, |r| {
                r.profiles.clear();
                bail!("injected failure")
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        update(&path, |r| {
            r.profiles.insert("two".into(), profile());
            Ok(())
        })
        .unwrap();
        update(&path, |r| {
            r.profiles.remove("one");
            Ok(())
        })
        .unwrap();
        assert!(load(&path).unwrap().profiles.contains_key("two"));
        fs::write(path.with_extension("json.lock"), b"busy").unwrap();
        assert!(update(&path, |_| Ok(())).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let link = temp.path().join("link");
            symlink(&path, &link).unwrap();
            assert!(load(&link).is_err());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load(&path).is_err());
        }
    }
    #[test]
    fn sentinel_parser_refuses_malformed_partial_and_escaping_records() {
        let complete = serde_json::json!({"schema_version":1,"state":"complete","repositories":[],"issues":[]});
        assert!(parse_frame(&frame(complete.clone()), "test", &profile()).is_ok());
        assert!(parse_frame(b"{\"state\":\"complete\"}", "test", &profile()).is_err());
        let mut duplicated = frame(complete.clone());
        duplicated.extend(frame(complete.clone()));
        assert!(parse_frame(&duplicated, "test", &profile()).is_err());
        let mut partial = complete.clone();
        partial["state"] = "partial".into();
        assert!(parse_frame(&frame(partial.clone()), "test", &profile()).is_err());
        partial["issues"] = serde_json::json!([{"path_hex":hex(b"/safe"),"reason":"depth_limit"}]);
        assert!(parse_frame(&frame(partial.clone()), "test", &profile()).is_ok());
        partial["issues"][0]["path_hex"] = hex(b"/elsewhere").into();
        assert!(parse_frame(&frame(partial), "test", &profile()).is_err());
        let mut bad = complete;
        bad["repositories"] = serde_json::json!([{"path":"/elsewhere","path_hex":hex(b"/elsewhere"),"paths_lossy":false,"git_dir_hex":hex(b"/safe/.git"),"common_dir_hex":hex(b"/safe/.git"),"kind":"bare","branch":null,"head":null,"unborn":true}]);
        assert!(parse_frame(&frame(bad), "test", &profile()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn real_helper_frame_round_trip_preserves_records_and_path_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let gitdir = root.join("repo space/.git");
        fs::create_dir_all(gitdir.join("refs/heads")).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/trunk\n").unwrap();
        fs::write(
            gitdir.join("refs/heads/trunk"),
            format!("{}\n", "a".repeat(40)),
        )
        .unwrap();
        let profile = Profile {
            destination: "fixture".into(),
            roots: vec![root.to_str().unwrap().into()],
        };
        let request = serde_json::json!({"roots":profile.roots,"operation":"discover","max_depth":4,"token":"test"});
        let mut input = tempfile::tempfile().unwrap();
        writeln!(
            input,
            "import json\nREQUEST = json.loads(bytes.fromhex('{}').decode())",
            hex(&serde_json::to_vec(&request).unwrap())
        )
        .unwrap();
        input.write_all(PROBE.as_bytes()).unwrap();
        input.rewind().unwrap();
        let output = capture_ssh(
            Command::new("python3")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .args(["-I", "-B", "-"])
                .stdin(input),
            10,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let discovery = parse_frame(&output.stdout, "test", &profile).unwrap();
        assert_eq!(discovery.state, "complete");
        assert_eq!(discovery.repositories.len(), 1);
        assert_eq!(
            discovery.repositories[0].head.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(
            discovery.repositories[0].path_hex,
            hex(root.join("repo space").as_os_str().as_encoded_bytes())
        );
    }
    #[test]
    fn ssh_options_enforce_trust_and_environment_isolation() {
        let command = ssh_command("fixture");
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        for expected in [
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oUpdateHostKeys=no",
            "-oVerifyHostKeyDNS=no",
            "-oKnownHostsCommand=none",
            "-oForwardAgent=no",
            "-oPermitLocalCommand=no",
            "-oControlPath=none",
        ] {
            assert!(args.iter().any(|a| a == expected));
        }
        assert_eq!(args.last().unwrap(), REMOTE_COMMAND);
        let env: Vec<_> = command
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(
            env.iter()
                .all(|k| ["PATH", "LC_ALL", "HOME", "SSH_AUTH_SOCK"].contains(&k.as_str()))
        );
        for (message, category) in [
            ("Host key verification failed.", "host_key"),
            ("REMOTE HOST IDENTIFICATION HAS CHANGED!", "host_key"),
            ("Permission denied (publickey).", "authentication"),
            ("Could not resolve hostname x", "dns"),
            ("Connection refused", "connectivity"),
            ("Connection timed out", "timeout"),
            ("python3: not found /private/key", "remote_command"),
        ] {
            assert_eq!(failure(message.as_bytes()), category);
        }
    }
    #[cfg(unix)]
    #[test]
    fn deadline_and_output_limits_stop_owned_processes() {
        let start = std::time::Instant::now();
        let error =
            capture_ssh(Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]), 1).unwrap_err();
        assert_eq!(error.to_string(), "timeout");
        assert!(start.elapsed() < Duration::from_secs(5));
        let error = capture_ssh(
            Command::new("/bin/sh").args(["-c", "while :; do head -c 65536 /dev/zero; done"]),
            10,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "capture_failed");
    }
}
