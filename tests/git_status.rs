mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    common::git(dir, args);
}
fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    common::init(dir.path(), "test-branch");
    dir
}
fn run(dir: &Path) -> std::process::Output {
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(dir)
        .args(["status", "--json"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("HOME", dir)
        .fixture_output()
        .unwrap()
}
fn status(dir: &Path) -> Value {
    let out = run(dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn commit(dir: &Path) {
    fs::write(dir.join("tracked"), "original").unwrap();
    git(dir, &["add", "tracked"]);
    git(dir, &["commit", "-m", "initial"]);
}
#[test]
fn outside_repository() {
    let d = tempfile::tempdir().unwrap();
    let out = run(d.path());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Git repository"));
}
#[test]
fn unborn_clean_nested_and_dirty() {
    let d = repo();
    let p = d.path();
    let s = status(p);
    assert_eq!(s["schema_version"], 1);
    common::assert_fields(
        &s,
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
    assert_eq!(s["branch"], "test-branch");
    assert_eq!(s["detached_head"], Value::Null);
    assert_eq!(s["unborn"], true);
    assert_eq!(s["dirty"], false);
    commit(p);
    fs::create_dir(p.join("nested space")).unwrap();
    let root = status(p);
    let nested = status(&p.join("nested space"));
    assert_eq!(root["repository_root"], nested["repository_root"]);
    assert_eq!(root["dirty"], false);
    fs::write(p.join("tracked"), "staged").unwrap();
    git(p, &["add", "tracked"]);
    fs::write(p.join("tracked"), "unstaged").unwrap();
    fs::write(p.join("odd\nname"), "new").unwrap();
    let index_before = fs::read(p.join(".git/index")).unwrap();
    let head_before = fs::read(p.join(".git/HEAD")).unwrap();
    let refs_before = common::isolate(&mut Command::new("git"))
        .current_dir(p)
        .args(["show-ref"])
        .fixture_output()
        .unwrap();
    assert!(refs_before.status.success());
    let s = status(p);
    assert_eq!(s["staged"], 1);
    assert_eq!(s["unstaged"], 1);
    assert_eq!(s["untracked"], 1);
    assert_eq!(index_before, fs::read(p.join(".git/index")).unwrap());
    assert_eq!(head_before, fs::read(p.join(".git/HEAD")).unwrap());
    let refs_after = common::isolate(&mut Command::new("git"))
        .current_dir(p)
        .args(["show-ref"])
        .fixture_output()
        .unwrap();
    assert!(refs_after.status.success());
    assert_eq!(refs_before.stdout, refs_after.stdout);
    assert_eq!(fs::read(p.join("tracked")).unwrap(), b"unstaged");
    assert_eq!(fs::read(p.join("odd\nname")).unwrap(), b"new");
}
#[test]
fn detached_and_linked_worktree() {
    let d = repo();
    let p = d.path();
    commit(p);
    git(p, &["checkout", "--detach"]);
    let s = status(p);
    assert_eq!(s["branch"], Value::Null);
    assert!(s["detached_head"].as_str().is_some());
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked worktree");
    git(
        p,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let a = status(p);
    let b = status(&linked);
    assert_ne!(a["worktree_path"], b["worktree_path"]);
    assert_ne!(a["git_dir"], b["git_dir"]);
    assert_eq!(a["common_dir"], b["common_dir"]);
    assert_eq!(b["branch"], "linked");
}
#[test]
fn missing_git_is_actionable() {
    let d = tempfile::tempdir().unwrap();
    let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(d.path())
        .args(["status"])
        .env("PATH", "")
        .fixture_output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("install Git"));
}

#[cfg(unix)]
#[test]
fn non_utf8_and_rename_paths_are_counted_without_changing_files() {
    use std::os::unix::ffi::OsStrExt;
    let d = repo();
    let p = d.path();
    commit(p);
    git(p, &["mv", "tracked", "renamed file"]);
    assert_eq!(status(p)["staged"], 1);
    let odd = p.join(std::ffi::OsStr::from_bytes(b"odd-\xff\t\nname"));
    if let Err(error) = fs::write(&odd, b"preserve me") {
        // Some macOS filesystems reject non-UTF-8 filenames.
        if cfg!(target_os = "macos") && matches!(error.raw_os_error(), Some(1 | 22 | 92)) {
            eprintln!("non-UTF-8 filename unsupported by this filesystem: {error}");
            return;
        }
        panic!("could not create non-UTF-8 filename: {error}");
    }
    let index_before = fs::read(p.join(".git/index")).unwrap();
    let s = status(p);
    assert_eq!(s["staged"], 1);
    assert_eq!(s["untracked"], 1);
    assert_eq!(fs::read(&odd).unwrap(), b"preserve me");
    assert_eq!(fs::read(p.join("renamed file")).unwrap(), b"original");
    assert_eq!(fs::read(p.join(".git/index")).unwrap(), index_before);
}

#[cfg(unix)]
#[test]
fn unsupported_git_path_format_has_actionable_diagnostic() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let bin = d.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let fake = bin.join("git");
    fs::write(
        &fake,
        b"#!/bin/sh\nprintf '%s\\n' '--path-format=absolute' '/tmp/repository'\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let output = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(d.path())
        .args(["status"])
        .env("PATH", &bin)
        .fixture_output()
        .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("does not support rev-parse --path-format=absolute"));
    assert!(message.contains("upgrade Git"));
}
