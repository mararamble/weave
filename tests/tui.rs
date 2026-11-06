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
