#![cfg(any(target_os = "macos", target_os = "linux"))]
mod common;

use common::FixtureOutput;
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    io::Read,
    os::{fd::FromRawFd, unix::ffi::OsStringExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const ESC: &str = "\x1b]777;WEV40\x07";

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    spaces: PathBuf,
    file: String,
    invalid: Option<OsString>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join(format!("repo-{ESC}-é"));
        let spaces = temp.path().join(format!("spaces-{ESC}"));
        fs::create_dir(&repo).unwrap();
        common::init(&repo, "main");
        let file = format!("file-{ESC}-é");
        let invalid_name = OsString::from_vec(b"bad-\x1b-\xff".to_vec());
        fs::write(repo.join(&file), "base\n").unwrap();
        let invalid = match fs::write(repo.join(&invalid_name), "non-UTF-8 path\n") {
            Ok(()) => Some(invalid_name),
            Err(error)
                if cfg!(target_os = "macos")
                    && matches!(error.raw_os_error(), Some(1 | 22 | 92)) =>
            {
                None
            }
            Err(error) => panic!("cannot create non-UTF-8 fixture path: {error}"),
        };
        common::git(&repo, &["add", "."]);
        common::git(&repo, &["commit", "-m", "base"]);
        let fixture = Self {
            _temp: temp,
            repo,
            spaces,
            file,
            invalid,
        };
        for name in ["one", "two"] {
            let out = fixture.weave(&["new", name, "--from", "main"]);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            safe("create", &out);
            let work = fixture.spaces.join(name);
            fs::write(
                work.join(&fixture.file),
                format!("{name} {ESC}é\u{85}\t\r\n"),
            )
            .unwrap();
            if let Some(invalid) = &fixture.invalid {
                fs::write(work.join(invalid), format!("{name}\n")).unwrap();
            }
            common::git(&work, &["add", "."]);
            common::git(&work, &["commit", "-m", name]);
        }
        fixture
    }

    fn weave(&self, args: &[&str]) -> Output {
        common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&self.repo)
            .env("WEAVE_WORKSPACE_DIR", &self.spaces)
            .args(args)
            .fixture_output()
            .unwrap()
    }

    fn git(&self, work: &Path, args: &[&str]) -> Output {
        common::isolate(&mut Command::new("git"))
            .current_dir(work)
            .args(args)
            .fixture_output()
            .unwrap()
    }
}

fn safe(label: &str, out: &Output) {
    assert!(
        !out.stdout.contains(&0x1b),
        "{label}: unsafe stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.stderr.contains(&0x1b),
        "{label}: unsafe stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn json(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn human_cli_routes_escape_fixture_values_and_errors() {
    let f = Fixture::new();
    let cases: &[(&str, &[&str])] = &[
        ("status", &["status"]),
        ("list", &["list"]),
        ("reconcile", &["reconcile"]),
        ("remove preview", &["remove", "one", "--dry-run"]),
        ("overlaps", &["overlaps", "one", "two"]),
        ("conflicting plan", &["plan", "one", "two"]),
        ("untangle preview", &["untangle", "one"]),
        (
            "invalid workspace error",
            &["diff", "bad\x1b]777;WEV40\x07"],
        ),
        ("clap error", &["--bad\x1b]777;WEV40\x07"]),
    ];
    for (label, args) in cases {
        let out = f.weave(args);
        safe(label, &out);
        if !label.contains("error") {
            assert!(
                out.status.success(),
                "{label}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("\\u{1b}"),
                "{label}: fixture was not rendered: {:?}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
    let status = f.weave(&["status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("\\u{1b}"));
    let plan = f.weave(&["plan", "one", "two"]);
    assert!(String::from_utf8_lossy(&plan.stdout).contains("\\u{1b}"));
    let preview = json(f.weave(&["untangle", "one", "--json"]));
    let destination = f._temp.path().join(format!("export-{ESC}"));
    let mut export_args = vec![
        "untangle".to_string(),
        "one".to_string(),
        "--snapshot".into(),
        preview["snapshot"]["id"].as_str().unwrap().into(),
        "--base".into(),
        preview["snapshot"]["base_commit"].as_str().unwrap().into(),
        "--group".into(),
        "all".into(),
    ];
    for unit in preview["snapshot"]["units"].as_array().unwrap() {
        export_args.push("--assign".into());
        export_args.push(format!("{}=all", unit["id"].as_str().unwrap()));
    }
    export_args.push("--export-patches".into());
    export_args.push(destination.to_string_lossy().into_owned());
    let export_refs: Vec<_> = export_args.iter().map(String::as_str).collect();
    let export = f.weave(&export_refs);
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    safe("untangle export", &export);
    assert!(String::from_utf8_lossy(&export.stdout).contains("\\u{1b}"));
    let create = f.weave(&["new", "three", "--from", "main"]);
    assert!(create.status.success());
    safe("create", &create);
    assert!(String::from_utf8_lossy(&create.stdout).contains("\\u{1b}"));
    let removed = f.weave(&["remove", "three", "--yes"]);
    assert!(removed.status.success());
    safe("confirmed remove", &removed);
    let integration = f.weave(&[
        "integrate",
        "--branch",
        "review",
        "one",
        "--",
        "/usr/bin/printf",
        ESC,
    ]);
    assert!(
        integration.status.success(),
        "{}",
        String::from_utf8_lossy(&integration.stderr)
    );
    safe("integration and validation output", &integration);
    assert!(String::from_utf8_lossy(&integration.stdout).contains("\\u{1b}"));
}

#[test]
fn human_refusals_unknown_worktrees_and_untracked_summaries_are_safe() {
    let f = Fixture::new();
    let invalid_ref = format!("missing-{ESC}");
    let failed_new = f.weave(&["new", "badref", "--from", &invalid_ref]);
    assert!(!failed_new.status.success());
    safe("invalid Git ref error", &failed_new);

    let conflict = f.weave(&["integrate", "--branch", "conflict", "one", "two"]);
    assert!(!conflict.status.success());
    safe("integration conflict error", &conflict);
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("\\u{1b}"));

    let script = format!("printf '%s' '{ESC}' >&2; exit 7");
    let failed_validation = f.weave(&[
        "integrate",
        "--branch",
        "validation-failure",
        "one",
        "--",
        "/bin/sh",
        "-c",
        &script,
    ]);
    assert!(!failed_validation.status.success());
    safe("validation error", &failed_validation);
    assert!(String::from_utf8_lossy(&failed_validation.stderr).contains("\\u{1b}"));

    let unknown = f._temp.path().join(format!("unknown-{ESC}"));
    let out = f.git(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            unknown.to_str().unwrap(),
            "main",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let inventory = f.weave(&["reconcile"]);
    assert!(inventory.status.success());
    safe("unregistered worktree", &inventory);
    assert!(String::from_utf8_lossy(&inventory.stdout).contains("\\u{1b}"));

    fs::write(
        f.spaces.join("one").join(format!("untracked-{ESC}")),
        b"content",
    )
    .unwrap();
    let summary = f.weave(&["diff", "one", "--stat"]);
    assert!(summary.status.success());
    safe("untracked diff summary", &summary);
    assert!(String::from_utf8_lossy(&summary.stdout).contains("\\u{1b}"));
    let blocked = f.weave(&["remove", "one", "--dry-run"]);
    assert!(!blocked.status.success());
    safe("blocked removal preview", &blocked);
    assert!(String::from_utf8_lossy(&blocked.stdout).contains("\\u{1b}"));
}

#[test]
fn json_and_path_bytes_are_unchanged_and_redirected_diff_stays_exact() {
    let f = Fixture::new();
    let work = f.spaces.join("one");
    let refs_before = f.git(&f.repo, &["show-ref"]).stdout;
    let git_dir = f.git(&work, &["rev-parse", "--absolute-git-dir"]);
    assert!(git_dir.status.success());
    let index_path = PathBuf::from(String::from_utf8(git_dir.stdout).unwrap().trim()).join("index");
    let index_before = fs::read(&index_path).unwrap();
    let source_before = fs::read(work.join(&f.file)).unwrap();
    let status = json(f.weave(&["status", "--json"]));
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
    assert!(status["repository_root"].as_str().unwrap().contains(ESC));
    let list = json(f.weave(&["list", "--json"]));
    assert!(
        list["workspaces"][0]["path"]
            .as_str()
            .unwrap()
            .contains(ESC)
    );
    let changes = json(f.weave(&["diff", "one", "--json"]));
    let files = changes["aggregate_changes"].as_array().unwrap();
    let hex = |s: &[u8]| s.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert!(
        files
            .iter()
            .any(|v| v["path_hex"] == hex(f.file.as_bytes()) && v["path"] == f.file)
    );
    if f.invalid.is_some() {
        assert!(files.iter().any(|v| v["path_hex"] == hex(b"bad-\x1b-\xff")));
    }
    let preview = json(f.weave(&["untangle", "one", "--json"]));
    assert!(
        preview["snapshot"]["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path_hex"] == hex(f.file.as_bytes()))
    );
    let conflict = json(f.weave(&["plan", "one", "two", "--json"]));
    assert_eq!(conflict["status"], "conflict");
    assert!(
        conflict["steps"][1]["conflict_details"]
            .as_str()
            .unwrap()
            .contains(ESC)
    );
    let diff = f.weave(&["diff", "one"]);
    assert!(diff.status.success());
    assert!(diff.stdout.contains(&0x1b));
    let work = f.spaces.join("one");
    let expected = f.git(
        &work,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "main",
            "--",
        ],
    );
    assert!(expected.status.success());
    assert_eq!(diff.stdout, expected.stdout);
    assert_eq!(f.git(&f.repo, &["show-ref"]).stdout, refs_before);
    assert_eq!(fs::read(index_path).unwrap(), index_before);
    assert_eq!(fs::read(work.join(&f.file)).unwrap(), source_before);
    assert!(work.join(&f.file).is_file());
    if let Some(invalid) = &f.invalid {
        assert!(work.join(invalid).is_file());
    }
}

#[test]
fn integration_json_preserves_validation_control_characters() {
    let f = Fixture::new();
    let payload = format!("{ESC}é\u{85}");
    let result = json(f.weave(&[
        "integrate",
        "--branch",
        "json-review",
        "--json",
        "one",
        "--",
        "/usr/bin/printf",
        &payload,
    ]));
    assert_eq!(result["validation"]["stdout"], payload);
    assert_eq!(result["schema_version"], 1);
}

#[test]
fn terminal_diff_escapes_content_while_piped_output_does_not() {
    let f = Fixture::new();
    let (mut master_fd, mut slave_fd) = (-1, -1);
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "uclibc")))]
    let mut size = libc::winsize {
        ws_row: 40,
        ws_col: 160,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    #[cfg(all(target_os = "linux", not(target_env = "uclibc")))]
    let size = libc::winsize {
        ws_row: 40,
        ws_col: 160,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "uclibc")))]
    let size_ptr = &raw mut size;
    #[cfg(all(target_os = "linux", not(target_env = "uclibc")))]
    let size_ptr = &raw const size;
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
    let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
    assert_eq!(
        unsafe { libc::fcntl(master_fd, libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let home = f._temp.path();
    let child = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&f.repo)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("WEAVE_WORKSPACE_DIR", &f.spaces)
        .args(["diff", "one"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let mut shown = Vec::new();
    let mut buffer = [0; 4096];
    while let Ok(n) = master.read(&mut buffer) {
        if n == 0 {
            break;
        }
        shown.extend_from_slice(&buffer[..n]);
    }
    assert!(!shown.contains(&0x1b));
    assert!(
        String::from_utf8_lossy(&shown).contains("\\u{1b}"),
        "terminal output: {:?}",
        String::from_utf8_lossy(&shown)
    );
    assert!(String::from_utf8_lossy(&shown).contains("\\u{85}"));
    assert!(String::from_utf8_lossy(&shown).contains("\\t"));
    assert!(String::from_utf8_lossy(&shown).contains("\\r"));
    assert!(String::from_utf8_lossy(&shown).contains('é'));
    assert!(f.weave(&["diff", "one"]).stdout.contains(&0x1b));
}
