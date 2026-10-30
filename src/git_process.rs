//! Shared Git process construction. Callers retain control over arguments, I/O,
//! working directory, and any operation-specific isolation environment.
use anyhow::{Context, Result};
use std::{
    io::Read,
    process::{Child, Command, Output, Stdio},
};

thread_local! {
    static CANCEL: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
}
pub(crate) fn check_cancelled() -> Result<()> {
    CANCEL.with(|cell| {
        if cell
            .borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            anyhow::bail!("Operation cancelled");
        }
        Ok(())
    })
}
pub(crate) fn cancellable<T>(
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CANCEL.with(|cell| *cell.borrow_mut() = None);
        }
    }
    CANCEL.with(|cell| *cell.borrow_mut() = Some(flag));
    let _reset = Reset;
    check_cancelled()?;
    work()
}

pub(crate) fn command() -> Command {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        if cfg!(windows) {
            "core.hooksPath=NUL"
        } else {
            "core.hooksPath=/dev/null"
        },
    ]);
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command
}

/// Run a configured argument-vector command and preserve its exit status for callers.
pub(crate) fn output(command: &mut Command) -> Result<Output> {
    check_cancelled()?;
    let result = command.output().map_err(launch_error);
    check_cancelled()?;
    result
}

/// Spawn a configured command when a caller needs bounded streaming or stdin.
pub(crate) fn spawn(command: &mut Command) -> Result<Child> {
    check_cancelled()?;
    command.spawn().map_err(launch_error)
}
/// Capture bounded stdout while draining stderr to avoid pipe deadlocks.
pub(crate) fn bounded_output(command: &mut Command, limit: usize) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn(command)?;
    let mut stderr = child.stderr.take().context("Missing Git stderr")?;
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut saved = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let n = stderr.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            let keep = n.min(65536usize.saturating_sub(saved.len()));
            saved.extend_from_slice(&buffer[..keep]);
        }
        Ok(saved)
    });
    let mut stdout = Vec::new();
    let read_result = child
        .stdout
        .take()
        .context("Missing Git stdout")?
        .take(limit as u64 + 1)
        .read_to_end(&mut stdout);
    let too_large = stdout.len() > limit;
    if too_large || read_result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let stderr = reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr reader failed"))??;
    read_result.context("Cannot read Git output")?;
    if too_large {
        anyhow::bail!("Git output exceeds {limit} bytes; inspection is incomplete");
    }
    check_cancelled()?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn launch_error(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!(error).context("Could not run Git; install Git and ensure it is on PATH")
    } else {
        anyhow::anyhow!(error).context("Could not start Git")
    }
}

/// Inspect a specific worktree without inherited repository or index routing.
pub(crate) fn command_at(path: &std::path::Path) -> Command {
    let mut cmd = command();
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_REPLACE_REF_BASE",
        "GIT_GRAFT_FILE",
        "GIT_QUARANTINE_PATH",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        cmd.env_remove(key);
    }
    cmd.arg("-C").arg(path).arg("--work-tree").arg(path);
    cmd
}

/// Git status may run active clean/process filters; refuse before inspection.
pub(crate) fn ensure_no_filters(path: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::bail;
    const FILTER_PATTERN: &str = r"^filter\..*\.(clean|process)$";

    // `--get-regexp` lists every matching definition from every config source.
    // Use that list only to discover keys, then ask Git for each key's effective
    // value and origin. In particular, an empty local value overrides a global
    // command and must not be treated as an active filter.
    let keys_output = output(command_at(path).args([
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        FILTER_PATTERN,
    ]))
    .context("Cannot inspect Git filter configuration")?;
    match keys_output.status.code() {
        Some(1) if keys_output.stdout.is_empty() => Ok(()),
        Some(0) => {
            let Some(configured_keys) = nul_records(&keys_output.stdout) else {
                bail!("Cannot inspect Git filter configuration");
            };
            let mut keys = std::collections::BTreeSet::new();
            for key in configured_keys {
                let Ok(key) = std::str::from_utf8(key) else {
                    bail!("Cannot inspect Git filter configuration");
                };
                if key.is_empty() {
                    bail!("Cannot inspect Git filter configuration");
                }
                keys.insert(key.to_owned());
            }

            let mut active = Vec::new();
            for key in keys {
                let effective = output(command_at(path).args([
                    "config",
                    "--null",
                    "--show-origin",
                    "--get",
                    key.as_str(),
                ]))
                .context("Cannot inspect Git filter configuration")?;
                if effective.status.code() != Some(0) {
                    bail!("Cannot inspect Git filter configuration");
                }
                let Some(fields) = nul_records(&effective.stdout) else {
                    bail!("Cannot inspect Git filter configuration");
                };
                if fields.len() != 2 || fields[0].is_empty() {
                    bail!("Cannot inspect Git filter configuration");
                }
                if !fields[1].is_empty() {
                    let origin = String::from_utf8_lossy(fields[0]);
                    active.push(format!("{origin} {key}"));
                }
            }
            if active.is_empty() {
                return Ok(());
            }
            let entries: String = active
                .join("\n")
                .chars()
                .map(|c| {
                    if c.is_control() && c != '\n' && c != '\t' {
                        '?'
                    } else {
                        c
                    }
                })
                .collect();
            bail!(
                "effective Git clean/process filters prevent safe inspection: {}. Inspect each effective value with `git config --show-origin --get '<key>'`; review or remove the configuration only if appropriate, then retry. Weave will not run these filters.",
                entries
            );
        }
        _ => bail!("Cannot inspect Git filter configuration"),
    }
}

fn nul_records(output: &[u8]) -> Option<Vec<&[u8]>> {
    if output.is_empty() || output.last() != Some(&0) {
        return None;
    }
    let records: Vec<_> = output.split(|byte| *byte == 0).collect();
    records
        .last()
        .is_some_and(|record| record.is_empty())
        .then(|| records[..records.len() - 1].to_vec())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn bounded_output_drains_large_stderr_and_reports_limits() {
        let output = bounded_output(
            Command::new("sh").args(["-c", "head -c 100000 /dev/zero >&2; printf ok; exit 3"]),
            32,
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"ok");
        assert_eq!(output.stderr.len(), 65536);
        let error = bounded_output(Command::new("sh").args(["-c", "head -c 32 /dev/zero"]), 16)
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }
}
