mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
fn status(path: &Path) -> Value {
    let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(path)
        .args(["status", "--json"])
        .fixture_output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn assert_paths(path: &Path, root: &Path, git_dir: &Path, common: &Path) -> Value {
    let value = status(path);
    let display = |p: &Path| Value::from(p.canonicalize().unwrap().to_string_lossy().into_owned());
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["repository_root"], display(root));
    assert_eq!(value["worktree_path"], display(root));
    assert_eq!(value["git_dir"], display(git_dir));
    assert_eq!(value["common_dir"], display(common));
    value
}
#[test]
fn primary_linked_nested_and_unavailable_primary_have_current_worktree_paths() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("primary");
    fs::create_dir(&repo).unwrap();
    let metadata = temp.path().join("metadata");
    common::git(
        &repo,
        &[
            "init",
            "-b",
            "trunk",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
        ],
    );
    common::git(&repo, &["config", "user.name", "Test"]);
    common::git(&repo, &["config", "user.email", "test@example.invalid"]);
    // An unborn primary has the same path contract.
    assert_paths(&repo, &repo, &metadata, &metadata);
    fs::write(repo.join("file"), "base\n").unwrap();
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-m", "base"]);
    let linked = temp.path().join("linked");
    common::git(
        &repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let linked_git = metadata.join("worktrees/linked");
    for (root, git_dir) in [(&repo, &metadata), (&linked, &linked_git)] {
        let nested = root.join("nested");
        fs::create_dir(&nested).unwrap();
        assert_paths(root, root, git_dir, &metadata);
        assert_paths(&nested, root, git_dir, &metadata);
    }
    let before = status(&linked);
    fs::rename(&repo, temp.path().join("primary-unavailable")).unwrap();
    assert_eq!(status(&linked), before);
    assert_ne!(before["repository_root"], before["common_dir"]);
}
#[test]
fn outside_and_bare_status_do_not_emit_fabricated_paths() {
    let temp = tempfile::tempdir().unwrap();
    let bare = temp.path().join("bare");
    common::git(
        temp.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=trunk",
            bare.to_str().unwrap(),
        ],
    );
    for path in [temp.path(), bare.as_path()] {
        let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(path)
            .args(["status", "--json"])
            .fixture_output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}
#[cfg(unix)]
#[test]
fn non_utf8_repository_roots_are_lossy_display_without_new_v1_fields() {
    use std::os::unix::ffi::OsStrExt;
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join(std::ffi::OsStr::from_bytes(b"repo-\xff"));
    if let Err(error) = fs::create_dir(&repo) {
        if cfg!(target_os = "macos") && matches!(error.raw_os_error(), Some(1 | 22 | 92)) {
            eprintln!("non-UTF-8 directory unsupported by this filesystem: {error}");
            return;
        }
        panic!("create non-UTF-8 fixture: {error}");
    }
    common::init(&repo, "trunk");
    let value = assert_paths(&repo, &repo, &repo.join(".git"), &repo.join(".git"));
    assert_eq!(value["paths_lossy"], true);
    assert!(
        value["worktree_path"]
            .as_str()
            .unwrap()
            .contains('\u{fffd}')
    );
    common::assert_fields(
        &value,
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
            "paths_lossy",
            "staged",
            "unstaged",
            "untracked",
        ],
    );
}
