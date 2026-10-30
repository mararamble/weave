//! Shared Git process construction. Callers retain control over arguments, I/O,
//! working directory, and any operation-specific isolation environment.
use anyhow::{Context, Result};
use std::{
    io::Read,
    process::{Child, Command, Output, Stdio},
};

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
    command.output().map_err(launch_error)
}

/// Spawn a configured command when a caller needs bounded streaming or stdin.
pub(crate) fn spawn(command: &mut Command) -> Result<Child> {
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

/// Git status may run configured clean/process filters; refuse before inspection.
pub(crate) fn ensure_no_filters(path: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::bail;
    let output =
        output(command_at(path).args(["config", "--get-regexp", r"^filter\..*\.(clean|process)$"]))
            .context("Cannot inspect Git filter configuration")?;
    match output.status.code() {
        Some(1) if output.stdout.is_empty() => Ok(()),
        Some(0) => bail!("Configured clean/process filters prevent safe inspection"),
        _ => bail!("Cannot inspect Git filter configuration"),
    }
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
