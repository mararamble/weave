mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn run(dir: &Path, args: &[&str], root: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_weave"));
    common::isolate(&mut cmd)
        .current_dir(dir)
        .env_remove("WEAVE_WORKSPACE_DIR")
        .args(args);
    if let Some(root) = root {
        cmd.env("WEAVE_WORKSPACE_DIR", root);
    }
    cmd.fixture_output().unwrap()
}
fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn repo(temp: &Path) -> PathBuf {
    let repo = temp.join("repo");
    fs::create_dir(&repo).unwrap();
    common::init(&repo, "main");
    fs::write(repo.join("file"), "base\n").unwrap();
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-m", "base"]);
    repo
}

#[test]
fn default_is_stable_across_primary_nested_and_linked_worktrees() {
    let temp = tempfile::tempdir().unwrap();
    let repo = repo(temp.path());
    let parent = repo
        .canonicalize()
        .unwrap()
        .parent()
        .unwrap()
        .join("repo.weave");
    fs::create_dir(repo.join("nested")).unwrap();
    let one = ok(run(
        &repo.join("nested"),
        &["new", "one", "--from", "main"],
        None,
    ));
    assert_eq!(
        String::from_utf8_lossy(&one.stdout).trim(),
        parent.join("one").to_str().unwrap()
    );
    let two = ok(run(
        &parent.join("one"),
        &["new", "two", "--from", "HEAD"],
        None,
    ));
    assert_eq!(
        String::from_utf8_lossy(&two.stdout).trim(),
        parent.join("two").to_str().unwrap()
    );
    assert!(repo.join(".git/weave/one.json").is_file());
    assert!(!parent.starts_with(repo.join(".git")));
    let list: Value =
        serde_json::from_slice(&ok(run(&repo, &["list", "--json"], None)).stdout).unwrap();
    common::assert_fields(&list, &["schema_version", "workspaces"]);
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 2);
}

#[test]
fn legacy_and_new_workspaces_remain_usable_together() {
    let temp = tempfile::tempdir().unwrap();
    let repo = repo(temp.path());
    let old_root = repo.join(".git/weave-workspaces");
    ok(run(
        &repo,
        &["new", "old", "--from", "main"],
        Some(&old_root),
    ));
    ok(run(&repo, &["new", "new", "--from", "main"], None));
    let list: Value =
        serde_json::from_slice(&ok(run(&repo, &["list", "--json"], None)).stdout).unwrap();
    let items = list["workspaces"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|x| {
        x["name"] == "old"
            && x["exists"] == true
            && x["path"]
                == old_root
                    .join("old")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
    }));
    assert!(
        items
            .iter()
            .any(|x| x["name"] == "new" && x["exists"] == true)
    );
    let diff: Value =
        serde_json::from_slice(&ok(run(&repo, &["diff", "old", "--json"], None)).stdout).unwrap();
    assert_eq!(diff["name"], "old");
    let reconcile: Value =
        serde_json::from_slice(&ok(run(&repo, &["reconcile", "--json"], None)).stdout).unwrap();
    assert_eq!(reconcile["managed"].as_array().unwrap().len(), 2);
    ok(run(&repo, &["remove", "old", "--yes"], None));
    assert!(!old_root.join("old").exists());
    assert!(temp.path().join("repo.weave/new").is_dir());
    assert!(!repo.join(".git/weave/old.json").exists());
}

#[test]
fn separate_git_dir_and_bare_use_stable_common_directory_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let work = temp.path().join("main");
    let common = temp.path().join("separate.git");
    fs::create_dir(&work).unwrap();
    common::git(
        &work,
        &[
            "init",
            "-b",
            "main",
            "--separate-git-dir",
            common.to_str().unwrap(),
        ],
    );
    common::git(&work, &["config", "user.name", "Test"]);
    common::git(&work, &["config", "user.email", "test@example.invalid"]);
    fs::write(work.join("file"), "base\n").unwrap();
    common::git(&work, &["add", "file"]);
    common::git(&work, &["commit", "-m", "base"]);
    let first = temp.path().join("separate.git.weave/one");
    ok(run(&work, &["new", "one", "--from", "main"], None));
    assert!(first.is_dir());
    ok(run(&first, &["new", "two", "--from", "HEAD"], None));
    assert!(temp.path().join("separate.git.weave/two").is_dir());
    assert!(common.join("weave/one.json").is_file());
    let bare = temp.path().join("bare.git");
    common::git(
        temp.path(),
        &[
            "clone",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    ok(run(&bare, &["new", "bare-task", "--from", "main"], None));
    assert!(temp.path().join("bare.git.weave/bare-task").is_dir());
    assert!(bare.join("weave/bare-task.json").is_file());
    ok(run(&bare, &["remove", "bare-task", "--yes"], None));
    assert!(!temp.path().join("bare.git.weave/bare-task").exists());
}

#[cfg(unix)]
#[test]
fn unsafe_default_symlink_is_refused_without_creating_task_or_record() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let repo = repo(temp.path());
    let root = temp.path().join("repo.weave");
    symlink(repo.join(".git"), &root).unwrap();
    let result = run(&repo, &["new", "danger", "--from", "main"], None);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("symlink"));
    assert!(!repo.join(".git/danger").exists());
    assert!(!repo.join(".git/weave/danger.json").exists());
}

#[test]
fn same_named_repositories_have_distinct_roots_and_existing_tasks_are_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let first = repo(&a);
    let second = repo(&b);
    ok(run(&first, &["new", "task", "--from", "main"], None));
    ok(run(&second, &["new", "task", "--from", "main"], None));
    assert!(a.join("repo.weave/task/file").exists());
    assert!(b.join("repo.weave/task/file").exists());
    let occupied = a.join("repo.weave/taken");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "preserve").unwrap();
    let result = run(&first, &["new", "taken", "--from", "main"], None);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Destination already exists"));
    assert_eq!(
        fs::read_to_string(occupied.join("keep")).unwrap(),
        "preserve"
    );
    assert!(!first.join(".git/weave/taken.json").exists());
}

#[cfg(unix)]
#[test]
fn default_root_reports_unwritable_parent() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let repo = repo(&parent);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let result = run(&repo, &["new", "task", "--from", "main"], None);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("Cannot create workspace root"), "{error}");
    assert!(error.contains("Permission denied"), "{error}");
    assert!(!parent.join("repo.weave/task").exists());
    assert!(!repo.join(".git/weave/task.json").exists());
}
