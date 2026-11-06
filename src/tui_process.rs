//! Owned read-only subprocesses for the browser. Mutations never use this runner.
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct Control {
    cancel: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    cleanup_error: Arc<Mutex<Option<String>>>,
}
impl Control {
    pub(crate) fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(true)),
            cleanup_error: Arc::new(Mutex::new(None)),
        }
    }
    pub(crate) fn with_flag(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            stopped: Arc::new(AtomicBool::new(true)),
            cleanup_error: Arc::new(Mutex::new(None)),
        }
    }
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub(crate) fn cleanup_error(&self) -> Option<String> {
        self.cleanup_error.lock().ok().and_then(|e| e.clone())
    }
    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

struct OwnedChild {
    child: Child,
    control: Control,
}
impl OwnedChild {
    fn terminate(&mut self) -> Result<()> {
        if self.control.stopped() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            // Command::process_group(0) made this unreaped child's PID its own
            // process-group ID. Never signal a caller-supplied or reaped PID.
            // libc 0.2.191 declares kill(pid_t: i32, c_int) on macOS and Linux;
            // negative pid_t targets only this dedicated process group.
            let group = i32::try_from(self.child.id()).context("Invalid read child PID")?;
            // SAFETY: the positive group ID belongs to our still-unreaped child;
            // SIGKILL is a valid signal on both supported operating systems.
            if unsafe { libc::kill(-group, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                // macOS can return EPERM for a group containing only an
                // already-exited, unreaped leader. Confirm/reap that exit;
                // never retry a signal using the now-reusable PID.
                if error.raw_os_error() == Some(libc::EPERM) && self.child.try_wait()?.is_some() {
                    self.control.stopped.store(true, Ordering::Release);
                    return Ok(());
                }
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error)
                        .with_context(|| format!("Cannot terminate read process group {group}"));
                }
            }
        }
        #[cfg(not(unix))]
        self.child.kill().context("Cannot terminate read child")?;
        self.child.wait().context("Cannot reap read child")?;
        self.control.stopped.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Also covers early I/O errors and worker unwinding. A failed cleanup
        // leaves stopped=false, so the browser cannot claim cancellation finished.
        if let Err(error) = self.terminate()
            && let Ok(mut saved) = self.control.cleanup_error.lock()
        {
            *saved = Some(format!("{error:#}"));
        }
    }
}

pub(crate) struct Capture {
    pub(crate) stdout: File,
    pub(crate) len: u64,
    pub(crate) stderr: Vec<u8>,
    pub(crate) status: ExitStatus,
}
impl Capture {
    pub(crate) fn into_output(mut self) -> Result<Output> {
        let mut stdout = Vec::new();
        self.stdout.read_to_end(&mut stdout)?;
        Ok(Output {
            status: self.status,
            stdout,
            stderr: self.stderr,
        })
    }
    pub(crate) fn text(mut self) -> Result<String> {
        let mut text = String::new();
        self.stdout.read_to_string(&mut text)?;
        Ok(text)
    }
}

pub(crate) fn read(args: &[String], control: &Control, limit: Option<u64>) -> Result<Capture> {
    // Keep the cancellable process-group boundary exclusive to read-only CLI
    // commands. In particular, no create/integrate/confirmed removal reaches it.
    match args.first().map(String::as_str) {
        Some(
            "list" | "status" | "diff" | "plan" | "overlaps" | "reconcile" | "__tui-read-diff",
        ) => {}
        Some("remove")
            if args.get(1).map(String::as_str) == Some("--dry-run")
                && args.get(2).map(String::as_str) == Some("--")
                && args.len() == 4 => {}
        _ => bail!("Only read-only commands may use TUI read cancellation"),
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.args(args).stdin(Stdio::null());
    let result = capture(&mut command, control, limit, 65536)?;
    if !result.status.success() {
        bail!("{}", String::from_utf8_lossy(&result.stderr).trim());
    }
    Ok(result)
}

// Shared only with Git commands under Vineweaver's explicit read/export
// cancellation scope. User validation and source mutations never enter here.
pub(crate) fn capture(
    command: &mut Command,
    control: &Control,
    limit: Option<u64>,
    stderr_limit: usize,
) -> Result<Capture> {
    if control.cancel.load(Ordering::Acquire) {
        bail!("Read cancelled before starting a child");
    }
    // The parent owns this root even if its CLI child is interrupted before Rust
    // destructors run. All Rust temporary planning repositories inherit TMPDIR.
    let temporary = tempfile::Builder::new()
        .prefix("weave-tui-read-")
        .tempdir()?;
    let path = temporary.path().to_path_buf();
    let result = (|| -> Result<Capture> {
        let mut stdout = tempfile::tempfile_in(temporary.path())?;
        let mut stderr = tempfile::tempfile_in(temporary.path())?;
        command
            .env("TMPDIR", temporary.path())
            .env("TMP", temporary.path())
            .env("TEMP", temporary.path())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().context("Cannot start read operation")?;
        control.stopped.store(false, Ordering::Release);
        let mut owned = OwnedChild {
            child,
            control: control.clone(),
        };
        let status = loop {
            // Inspect cancellation before reaping, keeping the group leader PID
            // reserved until the group signal has been delivered.
            if control.cancel.load(Ordering::Acquire) {
                #[cfg(unix)]
                {
                    owned.terminate()?;

                    bail!("Read cancelled; child process terminated and reaped");
                }
                // Other platforms wait for normal completion; no descendant-safe
                // termination contract is claimed there.
            }
            let size = stdout.metadata()?.len();
            if limit.is_some_and(|limit| size > limit)
                || stderr.metadata()?.len() > 32 * 1024 * 1024
            {
                owned.terminate()?;
                bail!("Read output exceeds capture limit; inspection is incomplete");
            }
            if let Some(status) = owned.child.try_wait()? {
                control.stopped.store(true, Ordering::Release);
                break status;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let len = stdout.metadata()?.len();
        if limit.is_some_and(|limit| len > limit) {
            bail!("Read output exceeds capture limit; inspection is incomplete");
        }
        stdout.seek(SeekFrom::Start(0))?;
        stderr.seek(SeekFrom::Start(0))?;
        let mut errors = Vec::new();
        stderr.take(stderr_limit as u64).read_to_end(&mut errors)?;
        if control.cancel.load(Ordering::Acquire) {
            bail!("Read cancelled; child process has exited and was reaped");
        }

        Ok(Capture {
            stdout,
            len,
            stderr: errors,
            status,
        })
    })();
    if !control.stopped() {
        let retained = temporary.keep();
        return result.context(format!(
            "Read child cleanup is uncertain; temporary data retained at {}",
            retained.display()
        ));
    }
    let cleanup = temporary
        .close()
        .with_context(|| format!("Temporary read cleanup failed; inspect {}", path.display()));
    match (result, cleanup) {
        (Ok(capture), Ok(())) => Ok(capture),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(error).context(format!("{cleanup:#}")),
    }
}

/// Private CLI endpoint so validation and Git streaming share the owned read
/// process group rather than running an uncancellable collector in a UI thread.
pub(crate) fn show_diff(
    name: &str,
    hex: &str,
    previous: Option<&str>,
    include_untracked_content: bool,
) -> Result<()> {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        bail!(
            "The internal diff reader requires captured stdout; use weave diff for terminal display"
        );
    }
    let changes = crate::changeset::collect(name, include_untracked_content)?;
    let record = crate::workspace::get(name)?;
    let mut command = crate::git_process::command_at(std::path::Path::new(&record.path));
    command
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--binary",
            "--no-renames",
            &changes.base_commit,
            "--",
        ])
        .arg(crate::tui::path_from_hex(hex)?)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(previous) = previous {
        command.arg(crate::tui::path_from_hex(previous)?);
    }
    let status = crate::git_process::spawn(&mut command)?.wait()?;
    if !status.success() {
        bail!("Git diff failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutations_cannot_enter_the_read_cancellation_runner() {
        for args in [
            vec!["new", "task"],
            vec!["integrate", "--branch", "review", "task"],
            vec!["remove", "--yes", "--", "task"],
            vec!["remove", "--dry-run", "--yes", "--", "task"],
        ] {
            let control = Control::new();
            let error = read(
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                &control,
                None,
            )
            .err()
            .expect("mutation refused");
            assert!(error.to_string().contains("Only read-only commands"));
            assert!(control.stopped());
        }
    }
    #[test]
    fn cancellation_before_start_does_not_launch_a_child() {
        let control = Control::new();
        control.cancel();
        let error = capture(
            &mut Command::new("weave-fixture-must-not-be-launched"),
            &control,
            None,
            65536,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("before starting a child"));
        assert!(control.stopped());
    }
    #[cfg(unix)]
    #[test]
    fn capture_keeps_exact_bytes_and_reaps_on_output_limit() {
        let control = Control::new();
        let result = capture(
            Command::new("sh").args(["-c", "printf '\\377\\000'; printf '\\376' >&2"]),
            &control,
            None,
            65536,
        )
        .unwrap()
        .into_output()
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"\xff\0");
        assert_eq!(result.stderr, b"\xfe");
        assert!(control.stopped());
        let control = Control::new();
        let error = capture(
            Command::new("sh").args(["-c", "head -c 100000 /dev/zero"]),
            &control,
            Some(8),
            65536,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("capture limit"), "{error:#}");
        assert!(control.stopped());
    }
}
