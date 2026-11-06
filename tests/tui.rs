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
    seen: Vec<u8>,
}
impl Terminal {
    // Reconstruct cursor-addressed output: Ratatui can emit only changed letters,
    // so stripping CSI sequences alone loses words across successive frames.
    fn screen_text(&self) -> String {
        let mut screen = vec![vec![' '; 140]; 30];
        let (mut row, mut col) = (0usize, 0usize);
        let mut chars = String::from_utf8_lossy(&self.seen)
            .chars()
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' if chars.peek() == Some(&'[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut command = 'm';
                    for part in chars.by_ref() {
                        if ('@'..='~').contains(&part) {
                            command = part;
                            break;
                        }
                        params.push(part);
                    }
                    let values: Vec<usize> =
                        params.split(';').map(|v| v.parse().unwrap_or(0)).collect();
                    let first = values.first().copied().unwrap_or(0);
                    let count = first.max(1);
                    match command {
                        'H' | 'f' => {
                            row = count.saturating_sub(1).min(29);
                            col = values
                                .get(1)
                                .copied()
                                .unwrap_or(1)
                                .max(1)
                                .saturating_sub(1)
                                .min(139);
                        }
                        'A' => row = row.saturating_sub(count),
                        'B' => row = (row + count).min(29),
                        'C' => col = (col + count).min(139),
                        'D' => col = col.saturating_sub(count),
                        'G' => col = count.saturating_sub(1).min(139),
                        'J' if first == 2 || first == 3 => {
                            screen.iter_mut().for_each(|r| r.fill(' '))
                        }
                        'K' => match first {
                            1 => screen[row][..=col].fill(' '),
                            2 => screen[row].fill(' '),
                            _ => screen[row][col..].fill(' '),
                        },
                        _ => {}
                    }
                }
                '\x1b' => {
                    chars.next();
                }
                '\r' => col = 0,
                '\n' => row = (row + 1).min(29),
                c if !c.is_control() => {
                    screen[row][col] = c;
                    col = (col + 1).min(139);
                }
                _ => {}
            }
        }
        screen
            .into_iter()
            .flatten()
            .filter(|c| !c.is_whitespace())
            .collect()
    }
    fn new(repo: &Path, spaces: &Path) -> Self {
        Self::with_args(repo, spaces, &[])
    }
    fn with_args(repo: &Path, spaces: &Path, args: &[&str]) -> Self {
        Self::with_env(repo, spaces, args, &[])
    }
    fn with_env(repo: &Path, spaces: &Path, args: &[&str], env: &[(&str, String)]) -> Self {
        let (mut master_fd, mut slave_fd) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 140,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // libc declares a mutable winsize pointer on macOS and const on Linux.
        let size_ptr = &raw mut size;
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
            .args(args)
            .envs(env.iter().map(|(key, value)| (*key, value)))
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
            seen: Vec::new(),
        }
    }
    fn read(&mut self) {
        let mut bytes = [0; 16384];
        while let Ok(n) = self.master.read(&mut bytes) {
            if n == 0 {
                break;
            }
            self.output.extend_from_slice(&bytes[..n]);
            self.seen.extend_from_slice(&bytes[..n]);
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
            let needle_text = needle
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>();
            if text.contains(&needle_text)
                || (!self.output.is_empty() && self.screen_text().contains(&needle_text))
            {
                self.output.clear();
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "TUI exited before {needle}: {text}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "TUI did not show {needle}: {text}; screen: {}",
                self.screen_text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn send(&mut self, keys: &[u8]) {
        self.master.write_all(keys).unwrap();
    }
    fn finish(&mut self) {
        self.finish_with_success(true);
    }
    fn finish_with_success(&mut self, expected: bool) {
        let start = Instant::now();
        loop {
            self.read();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(
                    status.success(),
                    expected,
                    "TUI exit {status}: {}",
                    String::from_utf8_lossy(&self.seen)
                );
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
fn provenance_keyboard_override_resolution_acknowledgment_and_successful_export() {
    let (temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "main"])
            .status
            .success()
    );
    fs::write(spaces.join("task/file"), "changed\n").unwrap();
    let cp = weave(
        &repo,
        &spaces,
        &["provenance", "checkpoint", "task", "--key", "keyboard"],
    );
    assert!(
        cp.status.success(),
        "{}",
        String::from_utf8_lossy(&cp.stderr)
    );
    let before = source_state(&repo, &spaces, &["task"]);
    let mut terminal = Terminal::with_args(
        &repo,
        &spaces,
        &["untangle", "task", "--review", "--provenance"],
    );
    terminal.wait_for("UNASSIGNED");
    terminal.send(b"gchange\r ");
    terminal.wait_for("VerifiedWorktree");
    terminal.send(b"omanual-agent\rHuman assertion from review\r");
    terminal.wait_for("Evidence decision appended");
    terminal.send(b"zKeep authorship unknown\r");
    terminal.wait_for("Evidence decision appended");
    terminal.send(b"aReviewed uncertainty and semantic dependencies\r");
    terminal.wait_for("acknowledged=true");
    terminal.send(b"v");
    terminal.wait_for("Validation ready");
    let destination = temp.path().join("keyboard-export");
    terminal.send(format!("p{}\rexport\r", destination.display()).as_bytes());
    terminal.wait_for("Export published");
    terminal.send(b"q");
    terminal.finish();
    let evidence: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.join("provenance.json")).unwrap()).unwrap();
    assert_eq!(evidence["acknowledged"], true);
    assert!(
        evidence["units"]
            .as_object()
            .unwrap()
            .values()
            .all(|u| u["attribution"]["source"] == "unknown")
    );
    let audit: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.join("audit.json")).unwrap()).unwrap();
    let actions = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        actions.contains(&"manual_override")
            && actions.contains(&"leave_unknown")
            && actions.contains(&"export_acknowledgment")
    );
    assert_eq!(source_state(&repo, &spaces, &["task"]), before);
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
    let mut terminal = Terminal::with_args(&repo, &spaces, &["tui", "--include-untracked-content"]);
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
    terminal.send(b"c");
    terminal.wait_for("Reconciliation cannot inspect workspace task");
    terminal.wait_for("Reconciliation complete");
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
fn untracked_content_is_hidden_in_tui_by_default() {
    let (_temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    fs::write(spaces.join("task/private"), "not displayed").unwrap();
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"\r");
    terminal.wait_for("Enter opens diff");
    terminal.send(b"\r");
    terminal.wait_for("untracked content omitted");
    assert!(!String::from_utf8_lossy(&terminal.seen).contains("not displayed"));
    terminal.send(b"qq");
    terminal.finish();
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
    let mut terminal = Terminal::with_args(&repo, &spaces, &["tui", "--include-untracked-content"]);
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
fn tui_opt_in_is_preserved_when_opening_vineweaver() {
    let (_temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    fs::write(spaces.join("task/new"), "visible in review\n").unwrap();
    let mut terminal = Terminal::with_args(&repo, &spaces, &["tui", "--include-untracked-content"]);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"u");
    terminal.wait_for("visible in review");
    terminal.send(b"q");
    terminal.wait_for("Workspaces");
    terminal.send(b"q");
    terminal.finish();
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

#[test]
fn both_interactive_interfaces_render_escape_fixture_safely() {
    const INJECTED: &[u8] = b"\x1b]777;WEV40\x07";
    for vineweaver in [false, true] {
        let (_temp, repo, spaces) = fixture();
        assert!(
            weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
                .status
                .success()
        );
        let mut content = b"changed ".to_vec();
        content.extend_from_slice(INJECTED);
        content.extend_from_slice(b"\n");
        fs::write(spaces.join("task/file"), &content).unwrap();
        let mut terminal = Terminal::new(&repo, &spaces);
        terminal.wait_for("1 workspaces; Enter opens files");
        if vineweaver {
            terminal.send(b"u");
        } else {
            terminal.send(b"\r");
            terminal.wait_for("Enter opens diff");
            terminal.send(b"\r");
        }
        terminal.wait_for("WEV40");
        if vineweaver {
            terminal.send(b"q");
            terminal.wait_for("Workspaces");
            terminal.send(b"q");
        } else {
            terminal.send(b"qq");
        }
        terminal.finish();
        assert!(!terminal.seen.windows(INJECTED.len()).any(|w| w == INJECTED));
        assert_eq!(fs::read(spaces.join("task/file")).unwrap(), content);
    }
}

fn source_state(repo: &Path, spaces: &Path, names: &[&str]) -> Vec<Vec<u8>> {
    use common::FixtureOutput;
    let git = |dir: &Path, args: &[&str]| {
        let out = common::isolate(&mut Command::new("git"))
            .current_dir(dir)
            .args(args)
            .fixture_output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    let mut state = vec![
        git(repo, &["show-ref"]),
        fs::read(repo.join("file")).unwrap(),
        fs::read(repo.join(".git/index")).unwrap(),
    ];
    for name in names {
        let path = spaces.join(name);
        state.push(fs::read(path.join("file")).unwrap());
        if path.join("new").exists() {
            state.push(fs::read(path.join("new")).unwrap());
        }
        let raw = git(
            &path,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        );
        state.push(fs::read(String::from_utf8(raw).unwrap().trim()).unwrap());
        state.push(fs::read(path.join(".git")).unwrap());
        state.push(fs::read(repo.join(".git/weave").join(format!("{name}.json"))).unwrap());
    }
    state
}

#[test]
fn flag_like_names_keyboard_create_browse_review_preview_cancel_and_remove() {
    for name in ["-odd", "--help", "--yes", "-", "--"] {
        eprintln!("keyboard name: {name}");
        let (_temp, repo, spaces) = fixture();
        let before_create = source_state(&repo, &spaces, &[]);
        let mut terminal =
            Terminal::with_args(&repo, &spaces, &["tui", "--include-untracked-content"]);
        terminal.wait_for("0 workspaces; Enter opens files");
        // Palette creation, cancellation, then actual creation through the shortcut.
        terminal.send(b":new\r");
        terminal.wait_for("New workspace name:");
        terminal.send(format!("{name}\r").as_bytes());
        terminal.wait_for("from ref:");
        terminal.send(b"\r");
        terminal.wait_for("y confirm");
        terminal.send(b"\x1b");
        terminal.wait_for("? help");
        assert_eq!(before_create, source_state(&repo, &spaces, &[]));
        assert!(!spaces.join(name).exists());
        terminal.send(b"n");
        terminal.wait_for("New workspace name:");
        terminal.send(format!("{name}\r").as_bytes());
        terminal.wait_for("from ref:");
        terminal.send(b"\r");
        terminal.wait_for("y confirm");
        terminal.send(b"y");
        terminal.wait_for("1 workspaces; Enter opens files");
        let path = spaces.join(name);
        fs::write(path.join("file"), "tracked hyphen content\n").unwrap();
        fs::write(path.join("new"), "untracked hyphen content\n").unwrap();
        let before_review = source_state(&repo, &spaces, &[name]);
        terminal.send(b"q\r");
        terminal.wait_for("Enter opens diff");
        terminal.send(b"\r");
        terminal.wait_for("tracked hyphen content");
        terminal.send(b"q\r");
        terminal.wait_for("Enter opens diff");
        terminal.send(b"j\r");
        terminal.wait_for("untracked hyphen content");
        terminal.send(b"q");
        terminal.send(b"t");
        terminal.wait_for("Diff statistics complete");
        terminal.send(b"J");
        terminal.wait_for("Inspection complete");
        terminal.send(b"w");
        terminal.wait_for("Selected workspace details");
        terminal.send(b":preview\r");
        terminal.wait_for("Integration preview: clean");
        terminal.send(b":overlaps\r");
        terminal.wait_for("Overlap observations");
        terminal.send(b"u");
        terminal.wait_for("UNASSIGNED");
        terminal.send(b"q");
        terminal.wait_for("Workspaces");
        // Both palette and shortcut removal must retain dirty/uncertain checks.
        terminal.send(b":removal-preview\r");
        terminal.wait_for("Removal preview failed");
        terminal.send(b":remove\r");
        terminal.wait_for("branch is retained");
        terminal.send(b"y");
        terminal.wait_for("Operation failed");
        assert_eq!(before_review, source_state(&repo, &spaces, &[name]));

        common::git(&path, &["add", "file", "new"]);
        common::git(&path, &["commit", "-m", "save fixture changes"]);
        let before_remove = source_state(&repo, &spaces, &[name]);
        terminal.send(b"D");
        terminal.wait_for("Preview only; no files removed");
        terminal.send(b"d");
        terminal.wait_for("branch is retained");
        terminal.send(b"n");
        terminal.wait_for("? help");
        assert_eq!(before_remove, source_state(&repo, &spaces, &[name]));
        terminal.send(b"d");
        terminal.wait_for("branch is retained");
        terminal.send(b"y");
        terminal.wait_for("0 workspaces; Enter opens files");
        assert!(!path.exists());
        assert_eq!(before_remove[0], source_state(&repo, &spaces, &[])[0]);
        terminal.send(b"qq");
        terminal.finish();
    }
}

#[test]
fn flag_like_names_keyboard_ordered_integration_with_and_without_validation() {
    let (_temp, repo, spaces) = fixture();
    for name in ["-odd", "ordinary"] {
        assert!(
            weave(&repo, &spaces, &["new", "--from", "HEAD", "--", name])
                .status
                .success()
        );
        fs::write(spaces.join(name).join("new"), format!("{name}\n")).unwrap();
    }
    // Avoid a same-path add/add conflict so order can be inspected in the result.
    fs::rename(
        spaces.join("ordinary/new"),
        spaces.join("ordinary/contribution"),
    )
    .unwrap();
    let before = source_state(&repo, &spaces, &["-odd", "ordinary"]);
    let mut terminal = Terminal::with_args(&repo, &spaces, &["tui", "--include-untracked-content"]);
    terminal.wait_for("2 workspaces; Enter opens files");
    terminal.send(b"j k "); // ordinary, then -odd
    terminal.send(b"p");
    terminal.wait_for("Integration preview: clean");
    terminal.send(b"o");
    terminal.wait_for("Overlap observations");
    terminal.send(b":integrate\r");
    terminal.wait_for("Integration branch:");
    terminal.send(b"review/cancelled\r");
    terminal.wait_for("Next argument:");
    terminal.send(b"\r");
    terminal.wait_for("y confirm");
    terminal.send(b"n");
    terminal.wait_for("? help");
    assert_eq!(before, source_state(&repo, &spaces, &["-odd", "ordinary"]));

    terminal.send(b"i");
    terminal.wait_for("Integration branch:");
    terminal.send(b"review/unchecked\r");
    terminal.wait_for("Next argument:");
    terminal.send(b"\r");
    terminal.wait_for("y confirm");
    terminal.send(b"y");
    terminal.wait_for("Created integration branch review/unchecked");
    let after_unchecked = source_state(&repo, &spaces, &["-odd", "ordinary"]);
    assert_eq!(before[1..], after_unchecked[1..]);

    terminal.send(b"i");
    terminal.wait_for("Integration branch:");
    terminal.send(b"review/validated\r");
    terminal.wait_for("Next argument:");
    terminal.send(b"sh\r-c\rtest \"$1\" = --help && test \"$2\" = --yes && test \"$3\" = -odd && test -z \"$4\"\rvalidation\r--help\r--yes\r-odd\r");
    terminal.send(b"\x1b[12~"); // F2 adds an empty validation argument.
    terminal.send(b"\r");
    terminal.wait_for("y confirm");
    terminal.send(b"y");
    terminal.wait_for("Created integration branch review/validated");
    let after_validated = source_state(&repo, &spaces, &["-odd", "ordinary"]);
    assert_eq!(before[1..], after_validated[1..]);
    terminal.send(b"qq");
    terminal.finish();

    use common::FixtureOutput;
    for branch in ["review/unchecked", "review/validated"] {
        let out = common::isolate(&mut Command::new("git"))
            .current_dir(&repo)
            .args(["ls-tree", "--name-only", &format!("{branch}^1")])
            .fixture_output()
            .unwrap();
        assert!(out.status.success());
        let tree = String::from_utf8(out.stdout).unwrap();
        assert!(tree.lines().any(|p| p == "contribution"));
        assert!(!tree.lines().any(|p| p == "new"));
    }
}

#[test]
fn failed_listing_refresh_clears_selection_before_review_or_integration() {
    let (_temp, repo, spaces) = fixture();
    for name in ["healthy", "damaged"] {
        assert!(
            weave(&repo, &spaces, &["new", name, "--from", "HEAD"])
                .status
                .success()
        );
    }
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("2 workspaces; Enter opens files");
    terminal.send(b" ");
    let record = repo.join(".git/weave/damaged.json");
    fs::write(&record, "{broken").unwrap();
    terminal.send(b"r");
    terminal.wait_for("Workspace listing failed; inspect review");
    terminal.send(b"p");
    terminal.wait_for("No workspaces to review");
    terminal.send(b"i");
    terminal.wait_for("Integration branch:");
    terminal.send(b"must-not-create\r");
    terminal.wait_for("Validation argv:");
    terminal.send(b"\r");
    terminal.wait_for("No workspaces to integrate");
    terminal.send(b"qq");
    terminal.finish();
    assert_eq!(fs::read(&record).unwrap(), b"{broken");
    assert!(spaces.join("healthy/file").exists());
    assert!(spaces.join("damaged/file").exists());
}

// A fixture-owned Git shim and child, held until cancellation or explicit release.
// The PID assertion distinguishes termination from ignoring an obsolete UI result.
struct GitStall {
    marker: PathBuf,
    armed: PathBuf,
    release: PathBuf,
    env: Vec<(&'static str, String)>,
}
impl GitStall {
    fn new(temp: &Path, argument: &str, fail: bool) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let bin = temp.join("stall-bin");
        fs::create_dir(&bin).unwrap();
        let marker = temp.join("stall-pids.json");
        let armed = temp.join("stall-armed");
        let release = temp.join("stall-release");
        let real = Command::new("which").arg("git").output().unwrap();
        assert!(real.status.success());
        let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
        let shim = bin.join("git");
        let mut script = r#"#!/usr/bin/env python3
import os, sys, subprocess, time, json
from pathlib import Path
armed=Path(os.environ['WEV_STALL_ARMED'])
release=Path(os.environ['WEV_STALL_RELEASE'])
if armed.exists() and os.environ['WEV_STALL_ARGUMENT'] in sys.argv[1:]:
    descendant=subprocess.Popen([sys.executable, '-c', 'import sys,time; from pathlib import Path; p=Path(sys.argv[1]);\nwhile not p.exists(): time.sleep(.02)', str(release)])
    temporary=Path(os.environ.get('TMPDIR',os.environ['HOME']))
    if 'weave-tui-read-' in str(temporary):
        (temporary/'fixture-temporary-data').write_text('discard after cancellation')
    data={'git':os.getpid(),'cli':os.getppid(),'group':os.getpgrp(),'descendant':descendant.pid,'temporary':str(temporary)}
    marker=Path(os.environ['WEV_STALL_MARKER']); pending=marker.with_suffix('.pending')
    pending.write_text(json.dumps(data)); pending.replace(marker)
    while not release.exists(): time.sleep(.02)
    descendant.wait()
    if os.environ['WEV_STALL_FAIL']=='yes': sys.exit(9)
os.execv(os.environ['WEV_REAL_GIT'],[os.environ['WEV_REAL_GIT'],*sys.argv[1:]])
"#.to_owned();
        for (name, value) in [
            ("WEV_STALL_ARMED", armed.to_str().unwrap()),
            ("WEV_STALL_RELEASE", release.to_str().unwrap()),
            ("WEV_STALL_MARKER", marker.to_str().unwrap()),
            ("WEV_STALL_ARGUMENT", argument),
            ("WEV_STALL_FAIL", if fail { "yes" } else { "no" }),
            ("WEV_REAL_GIT", real.as_str()),
        ] {
            script = script.replace(
                &format!("os.environ['{name}']"),
                &serde_json::to_string(value).unwrap(),
            );
        }
        fs::write(&shim, script).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            marker: marker.clone(),
            armed: armed.clone(),
            release: release.clone(),
            env: vec![
                (
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                ),
                ("WEV_STALL_MARKER", marker.to_str().unwrap().into()),
                ("WEV_STALL_ARMED", armed.to_str().unwrap().into()),
                ("WEV_STALL_RELEASE", release.to_str().unwrap().into()),
                ("WEV_STALL_ARGUMENT", argument.into()),
                ("WEV_STALL_FAIL", if fail { "yes" } else { "no" }.into()),
                ("WEV_REAL_GIT", real),
            ],
        }
    }
    fn arm(&self) {
        fs::write(&self.armed, "armed").unwrap();
    }
    fn wait(&self, terminal: &mut Terminal) -> serde_json::Value {
        let start = Instant::now();
        while !self.marker.exists() {
            terminal.read();
            assert!(terminal.child.try_wait().unwrap().is_none());
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "Stall not reached: {}",
                terminal.screen_text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        serde_json::from_slice(&fs::read(&self.marker).unwrap()).unwrap()
    }
    fn release(&self) {
        fs::write(&self.release, "released").unwrap();
    }
}
impl Drop for GitStall {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, "released");
    }
}
fn process_executing(pid: i32) -> bool {
    // PID values are recorded by our disposable fixture, never taken from user data.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        // An orphan can briefly remain a zombie until the host init reaps it.
        // It is terminated, cannot execute, and cannot write fixture data.
        if status
            .rsplit_once(") ")
            .is_some_and(|(_, tail)| tail.starts_with('Z'))
        {
            return false;
        }
    }
    true
}
fn assert_read_processes_stopped(pids: &serde_json::Value) {
    let start = Instant::now();
    loop {
        let running = ["cli", "git", "descendant"]
            .into_iter()
            .filter(|key| process_executing(pids[key].as_i64().unwrap() as i32))
            .collect::<Vec<_>>();
        if running.is_empty() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "Processes still executing after read cancel: {running:?}; {pids}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The browser's direct CLI child must be reaped, not merely a zombie.
    let result = unsafe { libc::kill(pids["cli"].as_i64().unwrap() as i32, 0) };
    assert_eq!(result, -1, "Read CLI child was not reaped");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
#[test]
fn stalled_browser_reads_cancel_children_and_temporary_data_without_source_writes() {
    for (action, argument) in [
        (b'r', "status"),
        (b's', "status"),
        (b'\r', "diff"),
        (b'p', "merge-tree"),
        (b'o', "diff"),
        (b'c', "status"),
        (b'D', "status"),
    ] {
        let (temp, repo, spaces) = fixture();
        let name = if action == b'D' { "--yes" } else { "task" };
        assert!(
            weave(&repo, &spaces, &["new", "--from", "HEAD", "--", name])
                .status
                .success()
        );
        fs::write(spaces.join(name).join("file"), "changed\n").unwrap();
        let before = source_state(&repo, &spaces, &[name]);
        let stall = GitStall::new(temp.path(), argument, false);
        let mut terminal = Terminal::with_env(&repo, &spaces, &["tui"], &stall.env);
        terminal.wait_for("1 workspaces; Enter opens files");
        stall.arm();
        terminal.send(&[action]);
        let pids = stall.wait(&mut terminal);
        assert_eq!(pids["group"], pids["cli"]);
        assert_ne!(pids["group"].as_u64().unwrap(), terminal.child.id() as u64);
        terminal.send(b"\x1b");
        terminal.wait_for("Read cancelled; child process stopped and reaped");
        assert_read_processes_stopped(&pids);
        assert!(
            !Path::new(pids["temporary"].as_str().unwrap()).exists(),
            "Temporary read root leaked"
        );
        assert_eq!(source_state(&repo, &spaces, &[name]), before);
        terminal.send(b"\x03");
        terminal.finish();
    }
}
#[test]
fn stalled_file_diff_and_quit_reap_their_read_children() {
    for quit in [false, true] {
        let (temp, repo, spaces) = fixture();
        assert!(
            weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
                .status
                .success()
        );
        fs::write(spaces.join("task/file"), "changed\n").unwrap();
        let before = source_state(&repo, &spaces, &["task"]);
        let stall = GitStall::new(temp.path(), "--no-renames", false);
        let mut terminal = Terminal::with_env(&repo, &spaces, &["tui"], &stall.env);
        terminal.wait_for("1 workspaces; Enter opens files");
        terminal.send(b"\r");
        terminal.wait_for("Enter opens diff");
        stall.arm();
        terminal.send(b"\r");
        let pids = stall.wait(&mut terminal);
        terminal.send(if quit { b"\x03" } else { b"\x1b" });
        if !quit {
            terminal.wait_for("Read cancelled; child process stopped and reaped");
            terminal.send(b"\x03");
        }
        terminal.finish();
        assert_read_processes_stopped(&pids);
        assert_eq!(source_state(&repo, &spaces, &["task"]), before);
    }
}
#[test]
fn mutation_quit_waits_for_success_or_failure_and_restores_terminal() {
    for fail in [false, true] {
        let (temp, repo, spaces) = fixture();
        let index = fs::read(repo.join(".git/index")).unwrap();
        let stall = GitStall::new(temp.path(), "add", fail);
        let mut terminal = Terminal::with_env(&repo, &spaces, &["tui"], &stall.env);
        terminal.wait_for("0 workspaces; Enter opens files");
        stall.arm();
        terminal.send(b"n");
        terminal.wait_for("New workspace name:");
        terminal.send(b"task\r");
        terminal.wait_for("from ref:");
        terminal.send(b"\r");
        terminal.wait_for("y confirm");
        terminal.send(b"y");
        let pids = stall.wait(&mut terminal);
        terminal.send(b"q");
        terminal.wait_for("Finishing operation, will exit when done");
        terminal.send(b"\x03\x1b");
        assert!(terminal.child.try_wait().unwrap().is_none());
        assert!(process_executing(pids["cli"].as_i64().unwrap() as i32));
        assert!(process_executing(pids["git"].as_i64().unwrap() as i32));
        let record = repo.join(".git/weave/task.json");
        let pending: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert_eq!(pending["state"], "pending");
        stall.release();
        terminal.finish_with_success(!fail);
        let after: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert_eq!(after["state"], if fail { "pending" } else { "ready" });
        assert_eq!(spaces.join("task").exists(), !fail);
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
        assert_eq!(fs::read(repo.join("file")).unwrap(), b"original\n");
        if fail {
            assert!(
                String::from_utf8_lossy(&terminal.seen).contains("Operation failed before exit")
            );
        }
    }
}

#[test]
fn vineweaver_capture_validation_and_export_cancel_owned_git_children() {
    for phase in ["capture", "validate", "export"] {
        let (temp, repo, spaces) = fixture();
        assert!(
            weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
                .status
                .success()
        );
        fs::write(spaces.join("task/file"), "changed\n").unwrap();
        let before = source_state(&repo, &spaces, &["task"]);
        let argument = match phase {
            "capture" => "diff",
            "validate" => "read-tree",
            _ => "bundle",
        };
        let stall = GitStall::new(temp.path(), argument, false);
        if phase == "capture" {
            stall.arm();
        }
        let mut terminal = Terminal::with_env(
            &repo,
            &spaces,
            &["untangle", "task", "--review"],
            &stall.env,
        );
        let destination = temp.path().join("cancelled-export");
        if phase != "capture" {
            terminal.wait_for("UNASSIGNED");
            terminal.send(b"gchange\r ");
            if phase == "export" {
                terminal.send(b"v");
                terminal.wait_for("Validation ready");
                stall.arm();
                terminal.send(format!("c{}\rexport\r", destination.display()).as_bytes());
            } else {
                stall.arm();
                terminal.send(b"v");
            }
        }
        let pids = stall.wait(&mut terminal);
        assert_eq!(pids["group"], pids["git"]);
        assert_eq!(pids["cli"].as_u64().unwrap(), terminal.child.id() as u64);
        terminal.send(b"\x1b");
        terminal.wait_for("Read cancelled; child process terminated and reaped");
        for name in ["git", "descendant"] {
            let pid = pids[name].as_i64().unwrap() as i32;
            let start = Instant::now();
            while process_executing(pid) {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "{phase}: {name} still executing"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(
            unsafe { libc::kill(pids["git"].as_i64().unwrap() as i32, 0) },
            -1
        );
        assert!(!destination.exists());
        assert!(!fs::read_dir(temp.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".vineweaver-export-")
        }));
        fs::rename(&stall.armed, stall.armed.with_extension("disarmed")).unwrap();
        if phase == "capture" {
            terminal.send(b"r");
            terminal.wait_for("UNASSIGNED");
        } else {
            terminal.send(b"v");
            terminal.wait_for("Validation ready");
            if phase == "export" {
                let recovered = temp.path().join("recovered-export");
                terminal.send(format!("c{}\rexport\r", recovered.display()).as_bytes());
                terminal.wait_for("Export published");
                assert!(recovered.join("commits.bundle").exists());
            }
        }
        terminal.send(b"q");
        terminal.finish();
        assert_eq!(source_state(&repo, &spaces, &["task"]), before);
    }
}

#[test]
fn removal_quit_finishes_confirmed_deletion_and_restores_terminal() {
    let (temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    let before = source_state(&repo, &spaces, &["task"]);
    let stall = GitStall::new(temp.path(), "remove", false);
    let mut terminal = Terminal::with_env(&repo, &spaces, &["tui"], &stall.env);
    terminal.wait_for("1 workspaces; Enter opens files");
    stall.arm();
    terminal.send(b"d");
    terminal.wait_for("branch is retained");
    terminal.send(b"y");
    let pids = stall.wait(&mut terminal);
    terminal.send(b"\x03");
    terminal.wait_for("Finishing operation, will exit when done");
    assert!(spaces.join("task").exists());
    assert!(process_executing(pids["git"].as_i64().unwrap() as i32));
    stall.release();
    terminal.finish();
    assert!(!spaces.join("task").exists());
    assert!(!repo.join(".git/weave/task.json").exists());
    assert_eq!(source_state(&repo, &spaces, &[])[0..3], before[0..3]);
}

#[test]
fn integration_validation_child_survives_quit_until_completion_then_exits() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, repo, spaces) = fixture();
    assert!(
        weave(&repo, &spaces, &["new", "task", "--from", "HEAD"])
            .status
            .success()
    );
    fs::write(spaces.join("task/file"), "changed\n").unwrap();
    let before = source_state(&repo, &spaces, &["task"]);
    let marker = temp.path().join("validation-pid");
    let release = temp.path().join("validation-release");
    let validator = temp.path().join("validate");
    let script = format!(
        "#!/bin/sh\necho $$ > '{}'\nwhile [ ! -f '{}' ]; do /bin/sleep 0.02; done\n",
        marker.display(),
        release.display()
    );
    fs::write(&validator, script).unwrap();
    fs::set_permissions(&validator, fs::Permissions::from_mode(0o755)).unwrap();
    // Release the fixture even if a later assertion fails.
    let release_guard = GitStall {
        marker: marker.clone(),
        armed: temp.path().join("unused"),
        release: release.clone(),
        env: vec![],
    };
    let mut terminal = Terminal::new(&repo, &spaces);
    terminal.wait_for("1 workspaces; Enter opens files");
    terminal.send(b"i");
    terminal.wait_for("Integration branch:");
    terminal.send(b"review/after-quit\r");
    terminal.wait_for("Next argument:");
    terminal.send(format!("{}\r\r", validator.display()).as_bytes());
    terminal.wait_for("y confirm");
    terminal.send(b"y");
    let start = Instant::now();
    while !marker.exists() {
        terminal.read();
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    terminal.send(b"\x03");
    terminal.wait_for("Finishing operation, will exit when done");
    assert!(process_executing(pid));
    assert!(terminal.child.try_wait().unwrap().is_none());
    release_guard.release();
    terminal.finish();
    assert!(!process_executing(pid));
    let after = source_state(&repo, &spaces, &["task"]);
    assert_eq!(after[1..], before[1..]);
    assert!(String::from_utf8_lossy(&after[0]).contains("refs/heads/review/after-quit"));
}
