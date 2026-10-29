use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "test-branch"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    dir
}
fn run(dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_weave"))
        .current_dir(dir)
        .args(["status", "--json"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("HOME", dir)
        .output()
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
    let s = status(p);
    assert_eq!(s["staged"], 1);
    assert_eq!(s["unstaged"], 1);
    assert_eq!(s["untracked"], 1);
    assert_eq!(index_before, fs::read(p.join(".git/index")).unwrap());
    assert_eq!(head_before, fs::read(p.join(".git/HEAD")).unwrap());
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
    let out = Command::new(env!("CARGO_BIN_EXE_weave"))
        .current_dir(d.path())
        .args(["status"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("install Git"));
}
