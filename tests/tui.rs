#![cfg(any(target_os = "macos", target_os = "linux"))]
mod common;
use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Terminal {
    child: Child,
    master: fs::File,
    slave: fs::File,
    initial: libc::termios,
    output: Vec<u8>,
}
impl Terminal {
    fn new(repo: &Path, spaces: &Path) -> Self {
        let (mut master_fd, mut slave_fd) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 140,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // libc declares a mutable winsize pointer on macOS and const on Linux.
        #[cfg(target_os = "macos")]
        let size_ptr = &raw mut size;
        #[cfg(target_os = "linux")]
        let size_ptr = &raw const size;
        // openpty returns two owned file descriptors when successful.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master_fd,
                    &mut slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    size_ptr,
                )
            },
            0
        );
        // Each successful descriptor is transferred to exactly one File.
        let master = unsafe { fs::File::from_raw_fd(master_fd) };
        let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
        let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
        // The slave descriptor remains open throughout this test.
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), attrs.as_mut_ptr()) },
            0
        );
        let initial = unsafe { attrs.assume_init() };
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        let home = repo.parent().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_weave"));
        let child = common::isolate(&mut cmd)
            .current_dir(repo)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home)
            .env("WEAVE_WORKSPACE_DIR", spaces)
            .env("TERM", "xterm-256color")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            master,
            slave,
            initial,
            output: Vec::new(),
        }
    }
    fn read(&mut self) {
        let mut bytes = [0; 16384];
        while let Ok(n) = self.master.read(&mut bytes) {
            if n == 0 {
                break;
            }
            self.output.extend_from_slice(&bytes[..n]);
        }
    }
    fn wait_for(&mut self, needle: &str) {
        let start = Instant::now();
        loop {
            self.read();
            // Ratatui emits cursor movement between changed words. Remove CSI sequences
            // and whitespace when matching the newly rendered text.
            let mut text = String::new();
            let output = String::from_utf8_lossy(&self.output);
            let mut chars = output.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' && chars.peek() == Some(&'[') {
                    chars.next();
                    for part in chars.by_ref() {
                        if ('@'..='~').contains(&part) {
                            break;
                        }
                    }
                } else if !c.is_whitespace() && !c.is_control() {
                    text.push(c);
                }
            }
            if text.contains(
                &needle
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
            ) {
                self.output.clear();
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "TUI exited before {needle}: {text}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "TUI did not show {needle}: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn send(&mut self, keys: &[u8]) {
        self.master.write_all(keys).unwrap();
    }
    fn finish(&mut self) {
        let start = Instant::now();
        loop {
            self.read();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "TUI did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut final_attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), final_attrs.as_mut_ptr()) },
            0
        );
        let final_attrs = unsafe { final_attrs.assume_init() };
        assert_eq!(self.initial.c_lflag, final_attrs.c_lflag);
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let spaces = temp.path().join("spaces");
    fs::create_dir(&repo).unwrap();
    common::init(&repo, "main");
    fs::write(repo.join("file"), "original\n").unwrap();
    common::git(&repo, &["add", "file"]);
    common::git(&repo, &["commit", "-m", "initial"]);
    (temp, repo, spaces)
}
#[test]
fn keyboard_create_preview_cancel_remove_and_terminal_cleanup() {
    let (_temp, repo, spaces) = fixture();
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("0 workspaces; Enter opens files");
    terminal.send(b"n");
    terminal.wait_for("New workspace name:");
    terminal.send(b"task\r");
    terminal.wait_for("from ref:");
    terminal.send(b"\r");
    terminal.wait_for("y confirm");
    terminal.send(b"y");
    terminal.wait_for("1 workspaces; Enter opens files");
    assert!(spaces.join("task").is_dir());
    terminal.send(b"D");
    terminal.wait_for("Preview only; no files removed");
    terminal.send(b"d");
    terminal.wait_for("branch is retained");
    terminal.send(b"n");
    assert!(spaces.join("task").is_dir());
    terminal.send(b"d");
    terminal.wait_for("branch is retained");
    terminal.send(b"y");
    terminal.wait_for("0 workspaces; Enter opens files");
    assert!(!spaces.join("task").exists());
    terminal.send(b"qq");
    terminal.finish();
    assert_eq!(fs::read(repo.join("file")).unwrap(), b"original\n");
}

fn weave(repo: &Path, spaces: &Path, args: &[&str]) -> std::process::Output {
    use common::FixtureOutput;
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(repo)
        .env("WEAVE_WORKSPACE_DIR", spaces)
        .args(args)
        .fixture_output()
        .unwrap()
}

#[test]
fn ordered_integration_accepts_validation_argv_and_preserves_sources() {
    let (_temp, repo, spaces) = fixture();
    let status = weave(&repo, &spaces, &["status", "--json"]);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    common::assert_fields(
        &status,
        &[
            "schema_version",
            "repository_root",
            "worktree_path",
            "git_dir",
            "common_dir",
            "branch",
            "detached_head",
            "unborn",
            "dirty",
            "staged",
            "unstaged",
            "untracked",
            "paths_lossy",
        ],
    );
    for name in ["first", "second"] {
        let result = weave(&repo, &spaces, &["new", name, "--from", "HEAD"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::write(spaces.join(name).join(name), name).unwrap();
    }
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("2 workspaces; Enter opens files");
    terminal.send(b"j k ");
    terminal.send(b"i");
    terminal.wait_for("Integration branch:");
    terminal.send(b"invalid\r");
    terminal.wait_for("Next argument:");
    terminal.send(b"/weave-no-such-validation-executable\r\r");
    terminal.wait_for("y confirm");
    terminal.send(b"y");
    terminal.wait_for("Operation failed");
    let missing = common::isolate(&mut Command::new("git"))
        .current_dir(&repo)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/invalid"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    terminal.send(b"i");
    terminal.wait_for("Integration branch:");
    terminal.send(b"combined\r");
    terminal.wait_for("Next argument:");
    terminal.send(b"git\r--version\r\r");
    terminal.wait_for("y confirm");
    terminal.send(b"y");
    terminal.wait_for("Validation stdout: git version");
    terminal.send(b"qq");
    terminal.finish();
    let git = common::isolate(&mut Command::new("git"))
        .current_dir(&repo)
        .args(["show-ref", "--verify", "refs/heads/combined"])
        .output()
        .unwrap();
    assert!(git.status.success());
    let first_step = common::isolate(&mut Command::new("git"))
        .current_dir(&repo)
        .args(["ls-tree", "--name-only", "combined^1"])
        .output()
        .unwrap();
    assert!(first_step.status.success());
    let first_step = String::from_utf8(first_step.stdout).unwrap();
    assert!(first_step.lines().any(|name| name == "second"));
    assert!(!first_step.lines().any(|name| name == "first"));
    assert_eq!(fs::read(spaces.join("first/first")).unwrap(), b"first");
    assert_eq!(fs::read(spaces.join("second/second")).unwrap(), b"second");
}

#[test]
fn preview_reports_filter_origin_without_executing_filter() {
    let (temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    let marker = temp.path().join("filter-ran");
    let filter = format!("touch {}", marker.display());
    common::git(&repo, &["config", "filter.danger.clean", &filter]);
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"p");
    terminal.wait_for("filter.danger.clean");
    assert!(!marker.exists());
    terminal.send(b"qq");
    terminal.finish();
}

#[test]
fn large_diff_is_paged_past_two_mebibytes() {
    let (_temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    let data = "a line of changed text\n".repeat(120_000);
    fs::write(spaces.join("task/file"), &data).unwrap();
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"\r");
    terminal.wait_for("Enter opens diff");
    terminal.send(b"\r");
    terminal.wait_for("previous / next page");
    terminal.send(&[b']'; 33]);
    terminal.send(b"qq");
    terminal.finish();
    assert_eq!(fs::read_to_string(spaces.join("task/file")).unwrap(), data);
}

#[test]
fn unusual_untracked_filename_and_content_are_browsable() {
    let (_temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    let path = spaces.join("task").join("name\nwith space");
    fs::write(&path, b"visible content\n").unwrap();
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"\r");
    terminal.wait_for("Enter opens diff");
    terminal.send(b"\r");
    terminal.wait_for("visible content");
    terminal.send(b"qq");
    terminal.finish();
    assert_eq!(fs::read(path).unwrap(), b"visible content\n");
}

#[test]
fn vineweaver_returns_to_browser_with_selection_preserved() {
    let (_temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    fs::write(spaces.join("task/file"), "changed\n").unwrap();
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"u");
    terminal.wait_for("UNASSIGNED");
    terminal.send(b"q");
    terminal.wait_for("Workspaces");
    terminal.send(b"\r");
    terminal.wait_for("Enter opens diff");
    terminal.send(b"qq");
    terminal.finish();
    assert_eq!(fs::read(spaces.join("task/file")).unwrap(), b"changed\n");
}

#[test]
fn control_c_exits_from_text_modal_and_restores_terminal() {
    let (_temp, repo, spaces) = fixture();
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("0 workspaces; Enter opens files");
    terminal.send(b"n");
    terminal.wait_for("New workspace name:");
    terminal.send(b"\x03");
    terminal.finish();
    assert!(!spaces.exists());
}
