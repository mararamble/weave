mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
fn git(p: &Path, args: &[&str]) {
    common::git(p, args);
}
fn weave(p: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(p)
        .args(args)
        .env("WEAVE_WORKSPACE_DIR", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .fixture_output()
        .unwrap()
}
fn weave_with_global_config(
    p: &Path,
    root: &Path,
    args: &[&str],
    global_config: &Path,
) -> std::process::Output {
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(p)
        .args(args)
        .env("WEAVE_WORKSPACE_DIR", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global_config)
        .fixture_output()
        .unwrap()
}
#[test]
fn lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path().join("repo");
    let root = temp.path().join("work spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
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
    common::assert_fields(&v, &["schema_version", "workspaces"]);
    for workspace in v["workspaces"].as_array().unwrap() {
        common::assert_fields(
            workspace,
            &[
                "schema_version",
                "name",
                "branch",
                "base_commit",
                "repository",
                "path",
                "state",
                "exists",
                "dirty",
            ],
        );
    }
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
    common::init(&p, "trunk");
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
    let o = common::isolate(&mut Command::new("git"))
        .current_dir(p)
        .args(["rev-parse", "--absolute-git-dir"])
        .fixture_output()
        .unwrap();
    std::path::PathBuf::from(String::from_utf8(o.stdout).unwrap().trim())
}

#[test]
fn diff_binary_rename_delete_and_untracked_safety() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
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
    let out = weave(
        &p,
        &root,
        &["diff", "test", "--json", "--include-untracked-content"],
    );
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
    common::init(&p, "trunk");
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
    let real_git = Command::new("which").arg("git").fixture_output().unwrap();
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
    let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["diff", "task", "--json", "--include-untracked-content"])
        .env("PATH", &bin)
        .env("WEAVE_RACE_MARKER", t.path().join("marker"))
        .env("WEAVE_RACE_FILE", w.join("new"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .fixture_output()
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
    common::init(&p, "trunk");
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
    common::init(&p, "trunk");
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

#[test]
fn plan_commits_detects_conflicts_and_preserves_sources() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "original\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for name in ["one", "two"] {
        assert!(
            weave(&p, &root, &["new", name, "--from", "trunk"])
                .status
                .success()
        );
        let w = root.join(name);
        fs::write(w.join("file"), format!("{name}\n")).unwrap();
        git(&w, &["add", "file"]);
        git(&w, &["commit", "-m", name]);
    }
    let one = root.join("one");
    let two = root.join("two");
    let indexes = [
        fs::read(git_dir(&one).join("index")).unwrap(),
        fs::read(git_dir(&two).join("index")).unwrap(),
    ];
    let refs_before = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["show-ref"])
        .fixture_output()
        .unwrap()
        .stdout;
    let result = weave(
        &p,
        &root,
        &[
            "plan",
            "one",
            "two",
            "--json",
            "--include-untracked-content",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["status"], "conflict");
    assert_eq!(value["steps"][0]["status"], "clean");
    assert_eq!(value["steps"][1]["status"], "conflict");
    assert_eq!(fs::read(one.join("file")).unwrap(), b"one\n");
    assert_eq!(fs::read(two.join("file")).unwrap(), b"two\n");
    assert_eq!(indexes[0], fs::read(git_dir(&one).join("index")).unwrap());
    assert_eq!(indexes[1], fs::read(git_dir(&two).join("index")).unwrap());
    assert_eq!(
        refs_before,
        common::isolate(&mut Command::new("git"))
            .current_dir(&p)
            .args(["show-ref"])
            .fixture_output()
            .unwrap()
            .stdout
    );
    fs::write(two.join("dirty"), "untracked").unwrap();
    let dirty = weave(
        &p,
        &root,
        &[
            "plan",
            "one",
            "two",
            "--json",
            "--include-untracked-content",
        ],
    );
    assert!(
        dirty.status.success(),
        "{}",
        String::from_utf8_lossy(&dirty.stderr)
    );
    let dirty_value: Value = serde_json::from_slice(&dirty.stdout).unwrap();
    assert_eq!(dirty_value["steps"][1]["untracked"], 1);
    assert_eq!(fs::read(two.join("dirty")).unwrap(), b"untracked");
}

#[test]
fn plan_clean_order_and_missing_workspace_failure() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("base"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for name in ["one", "two"] {
        assert!(
            weave(&p, &root, &["new", name, "--from", "trunk"])
                .status
                .success()
        );
        let w = root.join(name);
        fs::write(w.join(name), name).unwrap();
        git(&w, &["add", name]);
        git(&w, &["commit", "-m", name]);
    }
    let result = weave(&p, &root, &["plan", "two", "one", "--json"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(v["status"], "clean");
    assert_eq!(v["steps"][0]["workspace"], "two");
    assert_eq!(v["steps"][1]["workspace"], "one");
    let failed = weave(&p, &root, &["plan", "one", "missing", "--json"]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert_eq!(fs::read(root.join("one/one")).unwrap(), b"one");
}

#[test]
fn plan_includes_dirty_layers_and_rejects_incomplete_capture() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for name in ["one", "two"] {
        assert!(
            weave(&p, &root, &["new", name, "--from", "trunk"])
                .status
                .success()
        );
    }
    let one = root.join("one");
    let two = root.join("two");
    fs::write(one.join("file"), "staged\n").unwrap();
    git(&one, &["add", "file"]);
    fs::write(one.join("file"), "final one\n").unwrap();
    fs::write(one.join("odd\nname"), b"new\0binary").unwrap();
    fs::write(two.join("file"), "final two\n").unwrap();
    let index_before = fs::read(git_dir(&one).join("index")).unwrap();
    let result = weave(
        &p,
        &root,
        &[
            "plan",
            "one",
            "two",
            "--json",
            "--include-untracked-content",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(v["status"], "conflict");
    assert_eq!(v["steps"][0]["staged"], 1);
    assert_eq!(v["steps"][0]["unstaged"], 1);
    assert_eq!(v["steps"][0]["untracked"], 1);
    assert_eq!(v["steps"][1]["unstaged"], 1);
    assert_eq!(fs::read(git_dir(&one).join("index")).unwrap(), index_before);
    assert_eq!(fs::read(one.join("file")).unwrap(), b"final one\n");
    assert_eq!(fs::read(one.join("odd\nname")).unwrap(), b"new\0binary");
    fs::write(two.join(".env"), "secret").unwrap();
    let incomplete = weave(
        &p,
        &root,
        &[
            "plan",
            "one",
            "two",
            "--json",
            "--include-untracked-content",
        ],
    );
    assert!(!incomplete.status.success());
    assert!(incomplete.stdout.is_empty());
    assert!(String::from_utf8_lossy(&incomplete.stderr).contains("sensitive path excluded"));
}

#[test]
fn plan_snapshot_tree_matches_final_dirty_contents_and_blocks_filters() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    fs::write(p.join("gone"), "delete\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    let one = root.join("one");
    fs::write(one.join("file"), "staged\n").unwrap();
    git(&one, &["add", "file"]);
    fs::write(one.join("file"), "final\n").unwrap();
    fs::remove_file(one.join("gone")).unwrap();
    fs::write(one.join("new file"), b"binary\0bytes").unwrap();
    let out = weave(
        &p,
        &root,
        &["plan", "one", "--json", "--include-untracked-content"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "clean");
    // Build the expected tree in the disposable main fixture, independently of the planner.
    fs::write(p.join("file"), "final\n").unwrap();
    fs::remove_file(p.join("gone")).unwrap();
    fs::write(p.join("new file"), b"binary\0bytes").unwrap();
    git(&p, &["add", "-A"]);
    let expected = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["write-tree"])
        .fixture_output()
        .unwrap();
    assert!(expected.status.success());
    assert_eq!(
        v["steps"][0]["tree"].as_str().unwrap(),
        String::from_utf8(expected.stdout).unwrap().trim()
    );
    git(
        &p,
        &[
            "config",
            "filter.danger.clean",
            "touch /tmp/weave-should-not-run",
        ],
    );
    let blocked = weave(
        &p,
        &root,
        &["plan", "one", "--json", "--include-untracked-content"],
    );
    assert!(!blocked.status.success());
    assert!(blocked.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&blocked.stderr);
    assert!(diagnostic.contains("effective Git clean/process filters"));
    assert!(diagnostic.contains("filter.danger.clean"));
    assert!(diagnostic.contains("file:"));
    assert!(diagnostic.contains("git config --show-origin"));
    assert!(!diagnostic.contains("touch /tmp/weave-should-not-run"));
}

#[test]
fn plan_uses_effective_filter_values_from_included_config() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    fs::write(p.join(".gitattributes"), "file filter=danger\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );

    let weave_config = p.join(".git/weave-config");
    fs::write(
        &weave_config,
        "[filter \"danger\"]\n\tclean =\n\tprocess =\n",
    )
    .unwrap();
    let include_path = weave_config.to_string_lossy().into_owned();
    git(&p, &["config", "--local", "include.path", &include_path]);
    let marker = t.path().join("filter-ran");
    let global_config = t.path().join("global.gitconfig");
    fs::write(
        &global_config,
        format!(
            "[filter \"danger\"]\n\tclean = touch {}\n\tprocess = touch {}\n",
            marker.display(),
            marker.display()
        ),
    )
    .unwrap();

    let effective = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["config", "--show-origin", "--get", "filter.danger.clean"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global_config)
        .fixture_output()
        .unwrap();
    assert!(effective.status.success());
    let effective = String::from_utf8_lossy(&effective.stdout);
    assert!(effective.contains(weave_config.to_str().unwrap()));
    assert!(effective.ends_with("\t\n"));

    fs::write(root.join("one/file"), "changed\n").unwrap();
    let planned = weave_with_global_config(&p, &root, &["plan", "one", "--json"], &global_config);
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert!(!marker.exists());

    fs::write(&weave_config, "# no filter overrides\n").unwrap();
    let blocked = weave_with_global_config(&p, &root, &["plan", "one", "--json"], &global_config);
    assert!(!blocked.status.success());
    let diagnostic = String::from_utf8_lossy(&blocked.stderr);
    assert!(diagnostic.contains("effective Git clean/process filters"));
    assert!(diagnostic.contains("filter.danger.clean"));
    assert!(diagnostic.contains("filter.danger.process"));
    assert!(diagnostic.contains(&global_config.to_string_lossy().to_string()));
    assert!(!diagnostic.contains("touch"));
    assert!(!marker.exists());
}

#[test]
fn plan_dirty_add_add_and_hidden_index_entries() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("tracked"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for name in ["one", "two"] {
        assert!(
            weave(&p, &root, &["new", name, "--from", "trunk"])
                .status
                .success()
        );
        fs::write(root.join(name).join("new\nfile"), format!("{name}\n")).unwrap();
    }
    let result = weave(
        &p,
        &root,
        &[
            "plan",
            "one",
            "two",
            "--json",
            "--include-untracked-content",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(v["status"], "conflict");
    assert!(
        v["steps"][1]["conflict_details"]
            .as_str()
            .unwrap()
            .contains("add/add")
    );
    git(
        &root.join("one"),
        &["update-index", "--assume-unchanged", "tracked"],
    );
    let result = weave(
        &p,
        &root,
        &[
            "plan",
            "one",
            "two",
            "--json",
            "--include-untracked-content",
        ],
    );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("assume-unchanged"));
}

#[test]
fn plan_failure_after_first_merge_does_not_publish_partial_result() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("base"), "first\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "first root"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    git(&p, &["checkout", "--orphan", "other"]);
    git(&p, &["rm", "-rf", "."]);
    fs::write(p.join("other"), "second\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "second root"]);
    assert!(
        weave(&p, &root, &["new", "two", "--from", "other"])
            .status
            .success()
    );
    let refs = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["show-ref"])
        .fixture_output()
        .unwrap()
        .stdout;
    let index_one = fs::read(git_dir(&root.join("one")).join("index")).unwrap();
    let index_two = fs::read(git_dir(&root.join("two")).join("index")).unwrap();
    let result = weave(&p, &root, &["plan", "one", "two", "--json"]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("merge-tree failed"));
    assert_eq!(
        refs,
        common::isolate(&mut Command::new("git"))
            .current_dir(&p)
            .args(["show-ref"])
            .fixture_output()
            .unwrap()
            .stdout
    );
    assert_eq!(
        index_one,
        fs::read(git_dir(&root.join("one")).join("index")).unwrap()
    );
    assert_eq!(
        index_two,
        fs::read(git_dir(&root.join("two")).join("index")).unwrap()
    );
    assert_eq!(fs::read(root.join("one/base")).unwrap(), b"first\n");
    assert_eq!(fs::read(root.join("two/other")).unwrap(), b"second\n");
}

#[cfg(unix)]
#[test]
fn plan_rejects_same_summary_tracked_content_change_after_capture() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "file"]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let tracked = root.join("task/file");
    fs::write(&tracked, "alpha\n").unwrap();
    let git_path = String::from_utf8(
        Command::new("which")
            .arg("git")
            .fixture_output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    let quote = |s: &str| s.replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in *' merge-tree '*) printf 'bravo\\n' > '{}' ;; esac\nexec '{}' \"$@\"\n",
        quote(tracked.to_str().unwrap()),
        quote(git_path.trim())
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let result = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["plan", "task", "--json"])
        .env("PATH", &bin)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .fixture_output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("changed during planning"));
    assert_eq!(fs::read(&tracked).unwrap(), b"bravo\n");
}

#[test]
fn integrate_creates_branch_from_dirty_candidate_and_runs_validation() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for name in ["one", "two"] {
        assert!(
            weave(&p, &root, &["new", name, "--from", "trunk"])
                .status
                .success()
        );
    }
    let one = root.join("one");
    let two = root.join("two");
    fs::write(one.join("file"), "committed\n").unwrap();
    git(&one, &["add", "file"]);
    git(&one, &["commit", "-m", "source change"]);
    let source_head = common::isolate(&mut Command::new("git"))
        .current_dir(&one)
        .args(["rev-parse", "HEAD"])
        .fixture_output()
        .unwrap();
    let source_head = String::from_utf8(source_head.stdout)
        .unwrap()
        .trim()
        .to_owned();
    fs::write(one.join("file"), "dirty final\n").unwrap();
    fs::write(two.join("new"), "other workspace\n").unwrap();
    let one_index = fs::read(git_dir(&one).join("index")).unwrap();
    let two_index = fs::read(git_dir(&two).join("index")).unwrap();
    let output = weave(
        &p,
        &root,
        &[
            "integrate",
            "--branch",
            "review/test",
            "--json",
            "--include-untracked-content",
            "one",
            "two",
            "--",
            "true",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["branch"], "review/test");
    assert_eq!(v["validation"]["exit_code"], 0);
    assert!(
        v["sources"][0]["included_commits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == &source_head)
    );
    let ancestry = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args([
            "merge-base",
            "--is-ancestor",
            &source_head,
            "refs/heads/review/test",
        ])
        .status()
        .unwrap();
    assert!(ancestry.success());
    let unchanged_head = common::isolate(&mut Command::new("git"))
        .current_dir(&one)
        .args(["rev-parse", "HEAD"])
        .fixture_output()
        .unwrap();
    assert_eq!(
        String::from_utf8(unchanged_head.stdout).unwrap().trim(),
        source_head
    );
    let contents = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["show", "review/test:file"])
        .fixture_output()
        .unwrap();
    assert!(contents.status.success());
    assert_eq!(contents.stdout, b"dirty final\n");
    let new_file = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["show", "review/test:new"])
        .fixture_output()
        .unwrap();
    assert!(new_file.status.success());
    assert_eq!(new_file.stdout, b"other workspace\n");
    assert_eq!(one_index, fs::read(git_dir(&one).join("index")).unwrap());
    assert_eq!(two_index, fs::read(git_dir(&two).join("index")).unwrap());
    assert_eq!(fs::read(one.join("file")).unwrap(), b"dirty final\n");
    let duplicate = weave(&p, &root, &["integrate", "--branch", "review/test", "one"]);
    assert!(!duplicate.status.success());
}

#[cfg(unix)]
#[test]
fn integrate_validation_uses_a_minimal_environment_and_disposable_home() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let spaces = temp.path().join("spaces");
    let parent_home = temp.path().join("parent-home");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&parent_home).unwrap();
    common::init(&repo, "trunk");
    fs::write(repo.join("file"), "base\n").unwrap();
    git(&repo, &["add", "file"]);
    git(&repo, &["commit", "-m", "base"]);
    assert!(
        weave(&repo, &spaces, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );

    let script = r#"
        test -z "${WEAVE_TEST_SECRET+x}" || exit 1
        test -d "$HOME" && test "$HOME" != "$1" || exit 2
        test -n "$PATH" && test "$LC_ALL" = C || exit 3
        test "$(git rev-parse --show-toplevel)" = "$PWD" || exit 4
        test "$(git config --get core.hooksPath)" = /dev/null || exit 5
    "#;
    let output = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&repo)
        .env("WEAVE_WORKSPACE_DIR", &spaces)
        .env("HOME", &parent_home)
        .env("LC_ALL", "C")
        .env("WEAVE_TEST_SECRET", "parent-secret")
        .args([
            "integrate",
            "--branch",
            "review/environment",
            "--json",
            "one",
            "--",
            "sh",
            "-c",
            script,
            "validation",
            parent_home.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["validation"]["exit_code"], 0);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("parent-secret"));
}

#[test]
fn integrate_validation_failure_and_conflicts_leave_no_branch() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    for name in ["one", "two"] {
        assert!(
            weave(&p, &root, &["new", name, "--from", "trunk"])
                .status
                .success()
        );
    }
    fs::write(root.join("one/file"), "one\n").unwrap();
    let fail = weave(
        &p,
        &root,
        &["integrate", "--branch", "test-failed", "one", "--", "false"],
    );
    assert!(!fail.status.success());
    assert!(String::from_utf8_lossy(&fail.stderr).contains("Validation failed"));
    assert!(
        !common::isolate(&mut Command::new("git"))
            .current_dir(&p)
            .args(["show-ref", "--verify", "--quiet", "refs/heads/test-failed"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join("two/file"), "two\n").unwrap();
    let conflict = weave(
        &p,
        &root,
        &["integrate", "--branch", "test-conflict", "one", "two"],
    );
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("conflicts"));
    assert!(
        !common::isolate(&mut Command::new("git"))
            .current_dir(&p)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/test-conflict"
            ])
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
#[test]
fn integrate_reports_validation_mutation_without_including_it() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    fs::write(root.join("one/file"), "candidate\n").unwrap();
    let result = weave(
        &p,
        &root,
        &[
            "integrate",
            "--branch",
            "tested",
            "--json",
            "one",
            "--",
            "sh",
            "-c",
            "git rev-parse --verify HEAD >/dev/null && printf 'modified by test\\n' > file",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(v["validation"]["changed_tracked_files"], true);
    let content = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["show", "tested:file"])
        .fixture_output()
        .unwrap();
    assert_eq!(content.stdout, b"candidate\n");
    assert_eq!(fs::read(root.join("one/file")).unwrap(), b"candidate\n");
}

#[cfg(unix)]
#[test]
fn integrate_recovery_journal_survives_ref_update_interruption() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    fs::write(root.join("one/file"), "changed\n").unwrap();
    let real_git = String::from_utf8(
        Command::new("which")
            .arg("git")
            .fixture_output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in *' update-ref refs/heads/recover '*) '{}' \"$@\" || exit $?; exit 75 ;; esac\nexec '{}' \"$@\"\n",
        real_git.trim(),
        real_git.trim()
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let result = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["integrate", "--branch", "recover", "one"])
        .env("PATH", &bin)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .fixture_output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("recovery journal"));
    let journals: Vec<_> = fs::read_dir(p.join(".git/weave/integrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(journals.len(), 1);
    let record: Value = serde_json::from_slice(&fs::read(&journals[0]).unwrap()).unwrap();
    let actual = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["rev-parse", "refs/heads/recover"])
        .fixture_output()
        .unwrap();
    assert!(actual.status.success());
    assert_eq!(
        record["candidate"].as_str().unwrap(),
        String::from_utf8(actual.stdout).unwrap().trim()
    );
    assert_eq!(fs::read(root.join("one/file")).unwrap(), b"changed\n");
}

#[cfg(unix)]
#[test]
fn integrate_detects_source_change_during_validation() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    let source_file = root.join("one/file");
    fs::write(&source_file, "alpha\n").unwrap();
    let command = format!(
        "printf 'bravo\\n' > '{}'",
        source_file.to_str().unwrap().replace('\'', "'\\''")
    );
    let result = weave(
        &p,
        &root,
        &[
            "integrate",
            "--branch",
            "race",
            "one",
            "--",
            "sh",
            "-c",
            &command,
        ],
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("changed during planning"));
    assert!(
        !common::isolate(&mut Command::new("git"))
            .current_dir(&p)
            .args(["show-ref", "--verify", "--quiet", "refs/heads/race"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(source_file).unwrap(), b"bravo\n");
}

#[cfg(unix)]
#[test]
fn integrate_does_not_overwrite_concurrently_created_ref() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    fs::write(root.join("one/file"), "changed\n").unwrap();
    let base = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["rev-parse", "HEAD"])
        .fixture_output()
        .unwrap();
    let base = String::from_utf8(base.stdout).unwrap().trim().to_owned();
    let real = String::from_utf8(
        Command::new("which")
            .arg("git")
            .fixture_output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    let quote = |s: &str| s.replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in *' update-ref refs/heads/race '*) '{}' -C '{}' update-ref refs/heads/race '{}' ;; esac\nexec '{}' \"$@\"\n",
        quote(real.trim()),
        quote(p.to_str().unwrap()),
        base,
        quote(real.trim())
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let result = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["integrate", "--branch", "race", "one"])
        .env("PATH", &bin)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .fixture_output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("No existing branch was overwritten"));
    let actual = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["rev-parse", "refs/heads/race"])
        .fixture_output()
        .unwrap();
    assert_eq!(String::from_utf8(actual.stdout).unwrap().trim(), base);
}

#[test]
fn reconciliation_reports_unknown_dirty_locked_and_moved_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path().join("repo");
    let root = temp.path().join("managed spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join(".gitignore"), "ignored\n").unwrap();
    fs::write(p.join("file"), "original").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "initial"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    let managed = root.join("one");
    let unknown = temp.path().join("unknown worktree");
    git(
        &p,
        &[
            "worktree",
            "add",
            "-b",
            "unmanaged",
            unknown.to_str().unwrap(),
        ],
    );
    fs::write(managed.join("ignored"), "keep ignored").unwrap();
    git(&p, &["worktree", "lock", managed.to_str().unwrap()]);
    let record = p.join(".git/weave/one.json");
    let before = fs::read(&record).unwrap();
    let out = weave(&p, &root, &["reconcile", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let item = &v["managed"][0];
    assert_eq!(item["registration"], true);
    assert_eq!(item["ignored"], 1);
    assert_eq!(item["staged"], 0);
    assert!(
        item["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("locked"))
    );
    assert!(
        item["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("dirty_or_ignored"))
    );
    assert!(
        v["unknown"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["branch"] == "refs/heads/unmanaged")
    );
    assert_eq!(fs::read(&record).unwrap(), before);
    assert_eq!(fs::read(managed.join("ignored")).unwrap(), b"keep ignored");
    git(&p, &["worktree", "unlock", managed.to_str().unwrap()]);
    let moved = root.join("moved");
    git(
        &p,
        &[
            "worktree",
            "move",
            managed.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let out = weave(&p, &root, &["reconcile", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["managed"][0]["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("missing"))
    );
    assert_eq!(
        v["managed"][0]["moved_candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fs::read(&record).unwrap(), before);
    assert_eq!(fs::read(moved.join("ignored")).unwrap(), b"keep ignored");
}

#[test]
fn removal_preview_confirmation_and_dirty_refusal_preserve_data() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path().join("repo");
    let root = temp.path().join("work spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "original").unwrap();
    fs::create_dir(p.join("tracked-dir")).unwrap();
    fs::write(p.join("tracked-dir/file"), "tracked").unwrap();
    fs::write(p.join(".gitignore"), "ignored\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "one", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("one");
    let record = p.join(".git/weave/one.json");
    let record_before = fs::read(&record).unwrap();
    let preview = weave(&p, &root, &["remove", "one", "--dry-run"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(w.is_dir() && record.is_file());
    let unconfirmed = weave(&p, &root, &["remove", "one"]);
    assert!(!unconfirmed.status.success());
    assert!(w.is_dir() && record.is_file());
    for (filename, bytes) in [
        ("file", "modified"),
        ("untracked", "new"),
        ("ignored", "important"),
    ] {
        fs::write(w.join(filename), bytes).unwrap();
        let preview = weave(&p, &root, &["remove", "one", "--dry-run"]);
        assert!(!preview.status.success());
        assert!(String::from_utf8_lossy(&preview.stdout).contains("Git registration: true"));
        assert!(String::from_utf8_lossy(&preview.stderr).contains("Removal blocked"));
        let result = weave(&p, &root, &["remove", "one", "--yes"]);
        assert!(!result.status.success(), "{filename} should block removal");
        assert_eq!(fs::read(w.join(filename)).unwrap(), bytes.as_bytes());
        assert_eq!(fs::read(&record).unwrap(), record_before);
        if filename == "file" {
            fs::write(w.join(filename), "original").unwrap();
        } else {
            fs::remove_file(w.join(filename)).unwrap();
        }
    }
    fs::write(w.join("file"), "staged").unwrap();
    git(&w, &["add", "file"]);
    assert!(
        !weave(&p, &root, &["remove", "one", "--yes"])
            .status
            .success()
    );
    git(&w, &["reset", "--hard", "HEAD"]);
    git(&p, &["worktree", "lock", w.to_str().unwrap()]);
    assert!(
        !weave(&p, &root, &["remove", "one", "--yes"])
            .status
            .success()
    );
    git(&p, &["worktree", "unlock", w.to_str().unwrap()]);
    let removed = weave(&p, &root, &["remove", "one", "--yes"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!w.exists() && !record.exists());
    assert_eq!(fs::read(p.join("file")).unwrap(), b"original");
    git(&p, &["rev-parse", "--verify", "refs/heads/weave/one"]);
    assert!(
        !weave(&p, &root, &["remove", "one", "--yes"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn removal_injected_failures_preserve_or_report_recoverable_state() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    let real = Command::new("which").arg("git").fixture_output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    let script = format!(
        r#"#!/bin/sh
case " $* " in
  *' status '*) if [ "$MODE" = race ]; then if [ -e "$MARKER" ]; then printf 'external change' > "$TARGET/file"; else : > "$MARKER"; fi; fi ;;
  *' worktree remove '*)
    if [ "$MODE" = before ]; then printf 'injected failure\n' >&2; exit 1; fi
    if [ "$MODE" = after ]; then '{}' "$@" || exit $?; printf 'changed record' > "$RECORD"; exit 0; fi ;;
esac
exec '{}' "$@"
"#,
        real.replace('\'', "'\\''"),
        real.replace('\'', "'\\''")
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    for mode in ["before", "race", "after"] {
        assert!(
            weave(&p, &root, &["new", mode, "--from", "trunk"])
                .status
                .success()
        );
        let target = root.join(mode);
        let record = p.join(format!(".git/weave/{mode}.json"));
        let original = fs::read(&record).unwrap();
        let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&p)
            .args(["remove", mode, "--yes"])
            .env("PATH", &bin)
            .env("MODE", mode)
            .env("TARGET", &target)
            .env("RECORD", &record)
            .env("MARKER", t.path().join(format!("marker-{mode}")))
            .fixture_output()
            .unwrap();
        assert!(!out.status.success(), "{mode}: unexpectedly succeeded");
        if mode == "after" {
            assert!(!target.exists());
            assert_eq!(fs::read(&record).unwrap(), b"changed record");
            assert!(String::from_utf8_lossy(&out.stderr).contains("Git removed"));
        } else {
            assert!(target.is_dir());
            assert_eq!(fs::read(&record).unwrap(), original);
            if mode == "race" {
                assert_eq!(fs::read(target.join("file")).unwrap(), b"external change");
            }
        }
        // A malformed record must not prevent setting up the next scenario.
        if mode == "after" {
            fs::write(&record, original).unwrap();
        }
    }
}

#[test]
fn removal_refuses_drift_missing_filters_and_hidden_index() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    let record = p.join(".git/weave/task.json");
    let original = fs::read(&record).unwrap();
    git(&w, &["update-index", "--skip-worktree", "file"]);
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    git(&w, &["update-index", "--no-skip-worktree", "file"]);
    git(&p, &["config", "filter.test.clean", "false"]);
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    git(&p, &["config", "--unset", "filter.test.clean"]);
    let mut metadata: Value = serde_json::from_slice(&original).unwrap();
    metadata["repository"] = Value::from("/another/repository");
    fs::write(&record, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    fs::write(&record, &original).unwrap();
    git(&w, &["checkout", "-b", "different"]);
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    git(&w, &["checkout", "weave/task"]);
    let moved = root.join("moved");
    git(
        &p,
        &[
            "worktree",
            "move",
            w.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    assert_eq!(fs::read(&record).unwrap(), original);
    assert_eq!(fs::read(moved.join("file")).unwrap(), b"base");
}

#[cfg(unix)]
#[test]
fn removal_refuses_primary_unknown_symlink_and_submodule() {
    use std::os::unix::fs::symlink;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    let record = p.join(".git/weave/task.json");
    let before = fs::read(&record).unwrap();
    assert!(
        !weave(&p, &root, &["remove", "unknown", "--yes"])
            .status
            .success()
    );
    let mut forged: Value = serde_json::from_slice(&before).unwrap();
    forged["path"] = Value::from(p.canonicalize().unwrap().to_str().unwrap());
    fs::write(&record, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    assert_eq!(fs::read(p.join("file")).unwrap(), b"base");
    fs::write(&record, &before).unwrap();
    let moved = root.join("moved");
    fs::rename(&w, &moved).unwrap();
    symlink(&moved, &w).unwrap();
    let report = weave(&p, &root, &["reconcile", "--json"]);
    assert!(report.status.success());
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert!(
        report["managed"][0]["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("path_drift"))
    );
    assert!(
        report["unknown"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["branch"] == "refs/heads/weave/task")
    );
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    assert_eq!(fs::read(moved.join("file")).unwrap(), b"base");
    fs::remove_file(&w).unwrap();
    fs::rename(&moved, &w).unwrap();
    let head = common::isolate(&mut Command::new("git"))
        .current_dir(&p)
        .args(["rev-parse", "HEAD"])
        .fixture_output()
        .unwrap();
    let oid = String::from_utf8(head.stdout).unwrap();
    let cache = format!("160000,{},nested", oid.trim());
    git(&w, &["update-index", "--add", "--cacheinfo", &cache]);
    let out = weave(&p, &root, &["remove", "task", "--yes"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("submodules"));
    assert!(w.is_dir());
    assert_eq!(fs::read(&record).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn removal_reports_metadata_delete_failure_after_git_succeeds() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let meta = p.join(".git/weave");
    let record = meta.join("task.json");
    let before = fs::read(&record).unwrap();
    let real = Command::new("which").arg("git").fixture_output().unwrap();
    let real = String::from_utf8(real.stdout)
        .unwrap()
        .trim()
        .replace('\'', "'\\''");
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!("#!/bin/sh\ncase \" $* \" in *' worktree remove '*) '{real}' \"$@\" || exit $?; /bin/chmod 0555 \"$META\"; exit 0 ;; esac\nexec '{real}' \"$@\"\n")).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let result = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["remove", "task", "--yes"])
        .env("PATH", &bin)
        .env("META", &meta)
        .fixture_output()
        .unwrap();
    fs::set_permissions(&meta, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(!root.join("task").exists());
    assert_eq!(fs::read(&record).unwrap(), before);
    assert!(String::from_utf8_lossy(&result.stderr).contains("metadata remains"));
    let retry = weave(&p, &root, &["remove", "task", "--yes"]);
    assert!(!retry.status.success());
    let reconcile = weave(&p, &root, &["reconcile", "--json"]);
    assert!(reconcile.status.success());
    let report: Value = serde_json::from_slice(&reconcile.stdout).unwrap();
    assert!(
        report["managed"][0]["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("missing"))
    );
    assert!(
        report["managed"][0]["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("unregistered"))
    );
    assert_eq!(fs::read(&record).unwrap(), before);
    git(&p, &["rev-parse", "--verify", "refs/heads/weave/task"]);
}

#[test]
fn reconciliation_and_removal_reject_ambiguous_or_malformed_metadata() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    let record = p.join(".git/weave/task.json");
    let original = fs::read(&record).unwrap();
    let second = p.join(".git/weave/other.json");
    let mut conflicting: Value = serde_json::from_slice(&original).unwrap();
    conflicting["name"] = Value::from("other");
    conflicting["branch"] = Value::from("weave/other");
    fs::write(&second, serde_json::to_vec(&conflicting).unwrap()).unwrap();
    let out = weave(&p, &root, &["reconcile", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["managed"].as_array().unwrap().iter().all(|e| {
        e["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("ambiguous_metadata"))
    }));
    assert!(
        v["unknown"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["branch"] == "refs/heads/weave/task")
    );
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    fs::write(&second, &original).unwrap();
    let preview = weave(&p, &root, &["remove", "task", "--dry-run"]);
    assert!(!preview.status.success());
    assert!(
        String::from_utf8_lossy(&preview.stderr)
            .contains("Multiple metadata records claim workspace task")
    );
    assert!(!String::from_utf8_lossy(&preview.stdout).contains("Target:"));
    assert_eq!(fs::read(&record).unwrap(), original);
    assert_eq!(fs::read(w.join("file")).unwrap(), b"base");
    fs::write(&second, b"{invalid").unwrap();
    assert!(!weave(&p, &root, &["reconcile", "--json"]).status.success());
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    assert_eq!(fs::read(&record).unwrap(), original);
    assert_eq!(fs::read(w.join("file")).unwrap(), b"base");
}

#[test]
fn removal_inspects_actual_worktree_when_git_index_is_overridden() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    let alternate = t.path().join("alternate-index");
    fs::copy(git_dir(&w).join("index"), &alternate).unwrap();
    fs::write(w.join("file"), "unsaved").unwrap();
    let output = common::isolate(&mut Command::new("git"))
        .current_dir(&w)
        .args(["add", "file"])
        .env("GIT_INDEX_FILE", &alternate)
        .fixture_output()
        .unwrap();
    assert!(output.status.success());
    let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["remove", "task", "--yes"])
        .env("GIT_INDEX_FILE", &alternate)
        .fixture_output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(w.join("file")).unwrap(), b"unsaved");
    assert!(p.join(".git/weave/task.json").exists());
    let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["remove", "task", "--yes"])
        .env("GIT_WORK_TREE", &p)
        .fixture_output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(w.join("file")).unwrap(), b"unsaved");
    assert!(p.join(".git/weave/task.json").exists());
    for args in [&["list", "--json"][..], &["diff", "task", "--json"][..]] {
        let output = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&p)
            .args(args)
            .env("GIT_WORK_TREE", &p)
            .fixture_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        if args[0] == "list" {
            assert_eq!(json["workspaces"][0]["dirty"], true);
        } else {
            assert!(!json["unstaged_changes"].as_array().unwrap().is_empty());
        }
    }
}

#[cfg(unix)]
#[test]
fn removal_rejects_malformed_git_inventory() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let record = p.join(".git/weave/task.json");
    let before = fs::read(&record).unwrap();
    let real = Command::new("which").arg("git").fixture_output().unwrap();
    let real = String::from_utf8(real.stdout)
        .unwrap()
        .trim()
        .replace('\'', "'\\''");
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!(r#"#!/bin/sh
case " $* " in
  *' worktree list '*) if [ "$MODE" = list ]; then printf 'worktree /broken\000HEAD broken'; exit 0; fi ;;
  *' ls-files -v '*) if [ "$MODE" = index ]; then exit 0; fi ;;
esac
exec '{real}' "$@"
"#)).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    for (mode, diagnostic) in [("list", "Truncated"), ("index", "listings disagree")] {
        let result = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&p)
            .args(["remove", "task", "--yes"])
            .env("PATH", &bin)
            .env("MODE", mode)
            .fixture_output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(fs::read(&record).unwrap(), before);
    assert_eq!(fs::read(root.join("task/file")).unwrap(), b"base");
}

#[cfg(unix)]
#[test]
fn configured_hooks_and_fsmonitor_are_not_run_by_status_or_worktree_creation() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    let marker = t.path().join("helper-executed");
    let script = b"#!/bin/sh\n/usr/bin/touch \"$WEAVE_TEST_MARKER\"\nexit 0\n";
    let hook = p.join(".git/hooks/post-checkout");
    let monitor = t.path().join("fsmonitor");
    for file in [&hook, &monitor] {
        fs::write(file, script).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&p, &["config", "core.fsmonitor", monitor.to_str().unwrap()]);
    for args in [
        &["status", "--json"][..],
        &["new", "task", "--from", "trunk"][..],
        &["list", "--json"][..],
    ] {
        let output = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&p)
            .args(args)
            .env("WEAVE_TEST_MARKER", &marker)
            .env("WEAVE_WORKSPACE_DIR", &root)
            .fixture_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!marker.exists(), "configured helper ran during {args:?}");
    }
    assert_eq!(fs::read(p.join("file")).unwrap(), b"base");
}

#[test]
fn pending_and_missing_records_remain_untouched_across_retries() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    let record = p.join(".git/weave/task.json");
    let ready = fs::read(&record).unwrap();
    let mut pending: Value = serde_json::from_slice(&ready).unwrap();
    pending["state"] = Value::from("pending");
    let pending = serde_json::to_vec(&pending).unwrap();
    fs::write(&record, &pending).unwrap();
    for _ in 0..2 {
        assert!(
            !weave(&p, &root, &["remove", "task", "--yes"])
                .status
                .success()
        );
        let report = weave(&p, &root, &["reconcile", "--json"]);
        assert!(report.status.success());
        let report: Value = serde_json::from_slice(&report.stdout).unwrap();
        assert!(
            report["managed"][0]["states"]
                .as_array()
                .unwrap()
                .contains(&Value::from("pending_or_invalid_state"))
        );
        assert_eq!(fs::read(&record).unwrap(), pending);
        assert_eq!(fs::read(w.join("file")).unwrap(), b"base");
    }
    fs::write(&record, &ready).unwrap();
    let displaced = root.join("displaced");
    fs::rename(&w, &displaced).unwrap();
    for _ in 0..2 {
        assert!(
            !weave(&p, &root, &["remove", "task", "--yes"])
                .status
                .success()
        );
        let report = weave(&p, &root, &["reconcile", "--json"]);
        assert!(report.status.success());
        let report: Value = serde_json::from_slice(&report.stdout).unwrap();
        assert!(
            report["managed"][0]["states"]
                .as_array()
                .unwrap()
                .contains(&Value::from("missing"))
        );
        assert_eq!(fs::read(&record).unwrap(), ready);
        assert_eq!(fs::read(displaced.join("file")).unwrap(), b"base");
    }
}

#[test]
fn removal_preserves_empty_directories_and_nested_git_data() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let w = root.join("task");
    let record = p.join(".git/weave/task.json");
    let before = fs::read(&record).unwrap();
    fs::create_dir(w.join("empty")).unwrap();
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    assert!(w.join("empty").is_dir());
    fs::remove_dir(w.join("empty")).unwrap();
    fs::create_dir_all(w.join("nested/.git")).unwrap();
    fs::write(w.join("nested/.git/important"), b"preserve").unwrap();
    let status = common::isolate(&mut Command::new("git"))
        .current_dir(&w)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ])
        .fixture_output()
        .unwrap();
    assert!(status.status.success());
    assert!(
        status.stdout.is_empty(),
        "fixture must exercise data invisible to Git status"
    );
    let report = weave(&p, &root, &["reconcile", "--json"]);
    assert!(report.status.success());
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    let states = report["managed"][0]["states"].as_array().unwrap();
    assert!(states.contains(&Value::from("untracked_or_uninspectable_filesystem_data")));
    assert!(!states.contains(&Value::from("clean")));
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    assert_eq!(
        fs::read(w.join("nested/.git/important")).unwrap(),
        b"preserve"
    );
    assert_eq!(fs::read(record).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn interactive_removal_requires_positive_confirmation() {
    use std::{io::Write, os::fd::FromRawFd, process::Stdio};
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    let record = p.join(".git/weave/task.json");
    let original = fs::read(&record).unwrap();
    for (answer, accepted) in [
        (&b"no\n"[..], false),
        (&b"\x04"[..], false),
        (&b"yes\n"[..], true),
    ] {
        let (mut master_fd, mut slave_fd) = (-1, -1);
        // openpty provides owned master and slave descriptors for this isolated command.
        let result = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0, "could not create a pseudo-terminal");
        let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
        let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
        let home = tempfile::tempdir().unwrap();
        let child = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&p)
            .args(["remove", "task"])
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        master.write_all(answer).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Type yes"));
        if !accepted {
            assert_eq!(fs::read(&record).unwrap(), original);
            assert_eq!(fs::read(root.join("task/file")).unwrap(), b"base");
        }
    }
    assert!(!root.join("task").exists());
    assert!(!record.exists());
    git(&p, &["rev-parse", "--verify", "refs/heads/weave/task"]);
}

#[test]
fn unregistered_directory_with_data_is_never_removed() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces \tand\nnewlines");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    assert!(
        weave(&p, &root, &["new", "task", "--from", "trunk"])
            .status
            .success()
    );
    assert!(
        weave(&p, &root, &["new", "clean", "--from", "trunk"])
            .status
            .success()
    );
    let clean = weave(&p, &root, &["remove", "clean", "--yes"]);
    assert!(
        clean.status.success(),
        "{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(!root.join("clean").exists());
    git(&p, &["rev-parse", "--verify", "refs/heads/weave/clean"]);
    let w = root.join("task");
    let record = p.join(".git/weave/task.json");
    let before = fs::read(&record).unwrap();
    git(&p, &["worktree", "remove", w.to_str().unwrap()]);
    fs::create_dir(&w).unwrap();
    fs::write(w.join("do-not-delete"), b"valuable").unwrap();
    let report = weave(&p, &root, &["reconcile", "--json"]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert!(
        report["managed"][0]["states"]
            .as_array()
            .unwrap()
            .contains(&Value::from("unregistered"))
    );
    for _ in 0..2 {
        assert!(
            !weave(&p, &root, &["remove", "task", "--yes"])
                .status
                .success()
        );
    }
    assert_eq!(fs::read(w.join("do-not-delete")).unwrap(), b"valuable");
    assert_eq!(fs::read(&record).unwrap(), before);
    git(&p, &["rev-parse", "--verify", "refs/heads/weave/task"]);
}

#[cfg(unix)]
#[test]
fn creation_metadata_write_failure_preserves_pending_recovery_state() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("repo");
    let root = t.path().join("spaces");
    fs::create_dir(&p).unwrap();
    common::init(&p, "trunk");
    fs::write(p.join("file"), "base").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-m", "base"]);
    let meta = p.join(".git/weave");
    fs::create_dir(&meta).unwrap();
    fs::set_permissions(&meta, fs::Permissions::from_mode(0o555)).unwrap();
    let before = weave(&p, &root, &["new", "before", "--from", "trunk"]);
    fs::set_permissions(&meta, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!before.status.success());
    assert!(!meta.join("before.json").exists());
    assert!(!root.join("before").exists());
    let real = Command::new("which").arg("git").fixture_output().unwrap();
    let real = String::from_utf8(real.stdout)
        .unwrap()
        .trim()
        .replace('\'', "'\\''");
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!("#!/bin/sh\ncase \" $* \" in *' worktree add '*) '{real}' \"$@\" || exit $?; /bin/chmod 0555 \"$META\"; exit 0 ;; esac\nexec '{real}' \"$@\"\n")).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let result = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(&p)
        .args(["new", "task", "--from", "trunk"])
        .env("PATH", &bin)
        .env("META", &meta)
        .env("WEAVE_WORKSPACE_DIR", &root)
        .fixture_output()
        .unwrap();
    fs::set_permissions(&meta, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("Worktree was created but metadata finalization failed")
    );
    let record: Value = serde_json::from_slice(&fs::read(meta.join("task.json")).unwrap()).unwrap();
    assert_eq!(record["state"], "pending");
    assert_eq!(fs::read(root.join("task/file")).unwrap(), b"base");
    assert!(
        !weave(&p, &root, &["remove", "task", "--yes"])
            .status
            .success()
    );
    git(&p, &["rev-parse", "--verify", "refs/heads/weave/task"]);
}
