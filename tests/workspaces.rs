use serde_json::Value;
use std::{fs, path::Path, process::Command};
fn git(p: &Path, args: &[&str]) {
    let o = Command::new("git")
        .current_dir(p)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn weave(p: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_weave"))
        .current_dir(p)
        .args(args)
        .env("WEAVE_WORKSPACE_DIR", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
}
#[test]
fn lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path().join("repo");
    let root = temp.path().join("work spaces");
    fs::create_dir(&p).unwrap();
    git(&p, &["init", "-b", "trunk"]);
    git(&p, &["config", "user.name", "Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    fs::write(p.join("file"), "original").unwrap();
    git(&p, &["add", "file"]);
    git(&p, &["commit", "-m", "initial"]);
    fs::create_dir(p.join("nested")).unwrap();
    let a = weave(
        &p.join("nested"),
        &root,
        &["new", "auth", "--from", "trunk"],
    );
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    let b = weave(
        &root.join("auth"),
        &root,
        &["new", "parser", "--from", "HEAD"],
    );
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    fs::write(root.join("auth/file"), "auth").unwrap();
    fs::write(root.join("parser/file"), "parser").unwrap();
    assert_eq!(fs::read(p.join("file")).unwrap(), b"original");
    assert_eq!(fs::read(root.join("auth/file")).unwrap(), b"auth");
    assert!(
        !weave(&p, &root, &["new", "auth", "--from", "trunk"])
            .status
            .success()
    );
    fs::create_dir(root.join("occupied")).unwrap();
    fs::write(root.join("occupied/keep"), "safe").unwrap();
    assert!(
        !weave(&p, &root, &["new", "occupied", "--from", "trunk"])
            .status
            .success()
    );
    assert_eq!(fs::read(root.join("occupied/keep")).unwrap(), b"safe");
    assert!(
        !weave(&p, &root, &["new", "unknown", "--from", "no-such-ref"])
            .status
            .success()
    );
    git(&p, &["checkout", "--detach"]);
    assert!(
        weave(&p, &root, &["new", "detached", "--from", "HEAD"])
            .status
            .success()
    );
    let list = weave(&p, &root, &["list", "--json"]);
    let v: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["workspaces"].as_array().unwrap().len(), 3);
    assert_eq!(v["workspaces"][0]["dirty"], true);
    git(
        &p,
        &[
            "worktree",
            "remove",
            "--force",
            root.join("parser").to_str().unwrap(),
        ],
    );
    let list = weave(&p, &root, &["list", "--json"]);
    let v: Value = serde_json::from_slice(&list.stdout).unwrap();
    let parser = v["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "parser")
        .unwrap();
    assert_eq!(parser["exists"], false);
    assert_eq!(parser["dirty"], Value::Null);
}

#[test]
fn diff_collects_distinct_layers_without_modifying_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path().join("repo");
    let root = temp.path().join("spaces");
    fs::create_dir(&p).unwrap();
    git(&p, &["init", "-b", "trunk"]);
    git(&p, &["config", "user.name", "Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "file"]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    fs::write(w.join("file"), "committed\n").unwrap();
    git(&w, &["add", "file"]);
    git(&w, &["commit", "-m", "change"]);
    fs::write(w.join("file"), "staged\n").unwrap();
    git(&w, &["add", "file"]);
    fs::write(w.join("file"), "unstaged\n").unwrap();
    fs::write(w.join("odd\nname"), "new").unwrap();
    let index = fs::read(git_dir(&w).join("index")).unwrap();
    let result = weave(&p, &root, &["diff", "task", "--json"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v: Value = serde_json::from_slice(&result.stdout).unwrap();
    for key in [
        "committed_changes",
        "staged_changes",
        "unstaged_changes",
        "aggregate_changes",
        "untracked_changes",
    ] {
        assert_eq!(
            v[key].as_array().unwrap().len(),
            if key == "aggregate_changes" { 2 } else { 1 },
            "{key}"
        );
    }
    assert_eq!(v["untracked_changes"][0]["path"], "odd\nname");
    assert_eq!(index, fs::read(git_dir(&w).join("index")).unwrap());
    assert_eq!(fs::read(w.join("file")).unwrap(), b"unstaged\n");
}
fn git_dir(p: &Path) -> std::path::PathBuf {
    let o = Command::new("git")
        .current_dir(p)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .unwrap();
    std::path::PathBuf::from(String::from_utf8(o.stdout).unwrap().trim())
}

#[test]
fn diff_binary_rename_delete_and_untracked_safety() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    git(&p, &["init", "-b", "trunk"]);
    git(&p, &["config", "user.name", "Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    fs::write(p.join("old name"), "rename me\n").unwrap();
    fs::write(p.join("delete"), "gone\n").unwrap();
    fs::write(p.join("binary"), b"before\0data").unwrap();
    fs::write(p.join(".gitignore"), "ignored\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "test", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("test");
    git(&w, &["mv", "old name", "new name"]);
    fs::remove_file(w.join("delete")).unwrap();
    fs::write(w.join("binary"), b"after\0data").unwrap();
    fs::write(w.join("text"), "one\ntwo\n").unwrap();
    fs::write(w.join(".env"), "SECRET=do-not-expose").unwrap();
    fs::write(w.join("ignored"), "ignored-content").unwrap();
    fs::write(w.join("big"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let out = weave(&p, &root, &["diff", "test", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let changes = v["aggregate_changes"].as_array().unwrap();
    let find = |name: &str| changes.iter().find(|c| c["path"] == name).unwrap();
    assert_eq!(find("new name")["kind"], "renamed");
    assert_eq!(find("new name")["previous_path"], "old name");
    assert_eq!(find("delete")["kind"], "deleted");
    assert_eq!(find("binary")["binary"], true);
    assert_eq!(find("binary")["added"], Value::Null);
    assert_eq!(find("text")["added"], 2);
    assert_eq!(find("text")["content_hex"], "6f6e650a74776f0a");
    assert_eq!(find("text")["new_mode"], "100644");
    assert!(find(".env")["content_hex"].is_null());
    assert!(find("big")["content_hex"].is_null());
    assert!(!changes.iter().any(|c| c["path"] == "ignored"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SECRET"));
    assert_eq!(fs::read(w.join("binary")).unwrap(), b"after\0data");
}

#[cfg(unix)]
#[test]
fn detects_untracked_file_changed_during_collection() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    git(&p, &["init", "-b", "trunk"]);
    git(&p, &["config", "user.name", "Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "file"]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    fs::write(w.join("new"), "original").unwrap();
    let real_git = Command::new("which").arg("git").output().unwrap();
    let real_git = String::from_utf8(real_git.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in *' ls-files '*) if [ -e \"$WEAVE_RACE_MARKER\" ]; then printf changed > \"$WEAVE_RACE_FILE\"; else : > \"$WEAVE_RACE_MARKER\"; fi ;; esac\nexec '{}' \"$@\"\n",
        real_git.replace('\'', "'\\''")
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_weave"))
        .current_dir(&p)
        .args(["diff", "task", "--json"])
        .env("PATH", &bin)
        .env("WEAVE_RACE_MARKER", t.path().join("marker"))
        .env("WEAVE_RACE_FILE", w.join("new"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("changed during collection"));
}

#[test]
fn overlaps_regions_and_ambiguous_changes() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    git(&p, &["init", "-b", "trunk"]);
    git(&p, &["config", "user.name", "Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    let original = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
    fs::write(p.join("file"), &original).unwrap();
    fs::write(p.join("remove"), "base\n").unwrap();
    fs::write(p.join("rename"), "rename content\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for n in ["a", "b"] {
        let out = weave(&p, &root, &["new", n, "--from", "trunk"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let a = root.join("a");
    let b = root.join("b");
    fs::write(a.join("file"), original.replace("line 2\n", "alpha\n")).unwrap();
    fs::write(b.join("file"), original.replace("line 18\n", "beta\n")).unwrap();
    let inspect = || -> Value {
        let out = weave(&p, &root, &["overlaps", "a", "b", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let out = inspect();
    assert_eq!(out["overlaps"][0]["classification"], "separate_regions");
    assert_eq!(out["overlaps"][0]["region_overlap"], false);
    fs::write(b.join("file"), original.replace("line 2\n", "other\n")).unwrap();
    fs::write(a.join("added"), "one").unwrap();
    fs::write(b.join("added"), "two").unwrap();
    fs::remove_file(a.join("remove")).unwrap();
    fs::write(b.join("remove"), "changed\n").unwrap();
    git(&a, &["mv", "rename", "renamed"]);
    fs::write(b.join("rename"), "edited\n").unwrap();
    let out = inspect();
    let entries = out["overlaps"].as_array().unwrap();
    let file = entries.iter().find(|x| x["path"] == "file").unwrap();
    assert_eq!(file["region_overlap"], true);
    assert!(entries.iter().any(|x| x["classification"] == "add_add"));
    assert!(
        entries
            .iter()
            .any(|x| x["classification"] == "delete_modify")
    );
    assert!(
        entries
            .iter()
            .any(|x| x["incomplete"].as_str().unwrap_or("").contains("rename"))
    );
    assert_eq!(
        fs::read_to_string(a.join("file")).unwrap(),
        original.replace("line 2\n", "alpha\n")
    );
}

#[test]
fn overlaps_different_bases_report_incomplete() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    git(&p, &["init", "-b", "trunk"]);
    git(&p, &["config", "user.name", "Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    fs::write(p.join("file"), "first\nsecond\n").unwrap();
    git(&p, &["add", "file"]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "old", "--from", "trunk"])
            .status
            .success()
    );
    fs::write(p.join("file"), "first\nsecond\nthird\n").unwrap();
    git(&p, &["add", "file"]);
    git(&p, &["commit", "-m", "later"]);
    assert!(
        weave(&p, &root, &["new", "new", "--from", "trunk"])
            .status
            .success()
    );
    fs::write(root.join("old/file"), "old\nsecond\n").unwrap();
    fs::write(root.join("new/file"), "new\nsecond\nthird\n").unwrap();
    let out = weave(&p, &root, &["overlaps", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["overlaps"][0]["region_overlap"], Value::Null);
    assert_eq!(v["overlaps"][0]["incomplete"], "different base commits");
}
