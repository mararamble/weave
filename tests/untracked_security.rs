mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn weave(repo: &Path, root: &Path, args: &[&str]) -> Output {
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(repo)
        .env("WEAVE_WORKSPACE_DIR", root)
        .args(args)
        .fixture_output()
        .unwrap()
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let root = temp.path().join("spaces");
    fs::create_dir(&repo).unwrap();
    common::init(&repo, "main");
    fs::write(repo.join("base"), "base\n").unwrap();
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-m", "base"]);
    assert!(
        weave(&repo, &root, &["new", "task", "--from", "main"])
            .status
            .success()
    );
    (temp, repo, root)
}

#[test]
fn default_is_path_only_and_snapshot_commands_fail_closed_until_opted_in() {
    let (temp, repo, root) = fixture();
    let work = root.join("task");
    fs::write(work.join("new.txt"), "private-untracked-data\n").unwrap();
    let diff = weave(&repo, &root, &["diff", "task", "--json"]);
    assert!(diff.status.success());
    let data: Value = serde_json::from_slice(&diff.stdout).unwrap();
    common::assert_fields(
        &data,
        &[
            "schema_version",
            "workspace_id",
            "name",
            "repository",
            "base_commit",
            "head_commit",
            "committed_changes",
            "staged_changes",
            "unstaged_changes",
            "untracked_changes",
            "aggregate_changes",
            "warnings",
        ],
    );
    for section in ["untracked_changes", "aggregate_changes"] {
        assert_eq!(data[section][0]["path"], "new.txt");
        assert!(data[section][0]["content_hex"].is_null());
        assert!(
            data[section][0]["incomplete"]
                .as_str()
                .unwrap()
                .contains("--include-untracked-content")
        );
    }
    let human = weave(&repo, &root, &["diff", "task"]);
    assert!(
        !String::from_utf8_lossy(&human.stdout)
            .contains("707269766174652d756e747261636b65642d64617461")
    );
    let dest = temp.path().join("export");
    let dest_str = dest.to_str().unwrap();
    let bundle_dest = temp.path().join("bundle-export");
    let bundle_dest_str = bundle_dest.to_str().unwrap();
    for args in [
        vec!["plan", "task", "--json"],
        vec!["integrate", "--branch", "safe", "task"],
        vec!["untangle", "task", "--json"],
        vec![
            "untangle",
            "task",
            "--export-patches",
            dest_str,
            "--base",
            "HEAD",
        ],
        vec![
            "untangle",
            "task",
            "--export-commits",
            bundle_dest_str,
            "--base",
            "HEAD",
        ],
    ] {
        let result = weave(&repo, &root, &args);
        assert!(!result.status.success(), "{args:?}");
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("--include-untracked-content"),
            "{:?}",
            result.stderr
        );
    }
    assert!(!dest.exists());
    assert!(!bundle_dest.exists());
    assert!(
        weave(&repo, &root, &["new", "other", "--from", "main"])
            .status
            .success()
    );
    fs::write(root.join("other/new.txt"), "another-private-value\n").unwrap();
    let overlap = weave(&repo, &root, &["overlaps", "task", "other", "--json"]);
    assert!(overlap.status.success());
    let overlap_json: Value = serde_json::from_slice(&overlap.stdout).unwrap();
    assert_eq!(overlap_json["overlaps"][0]["path"], "new.txt");
    assert_eq!(overlap_json["overlaps"][0]["classification"], "add_add");
    let output = String::from_utf8_lossy(&overlap.stdout);
    assert!(!output.contains("private-untracked-data"));
    assert!(!output.contains("another-private-value"));
    assert!(!output.contains("707269766174652d756e747261636b65642d64617461"));
    let captured = weave(
        &repo,
        &root,
        &["diff", "task", "--json", "--include-untracked-content"],
    );
    let v: Value = serde_json::from_slice(&captured.stdout).unwrap();
    assert_eq!(
        v["untracked_changes"][0]["content_hex"],
        "707269766174652d756e747261636b65642d646174610a"
    );
    let untangle = weave(
        &repo,
        &root,
        &["untangle", "task", "--json", "--include-untracked-content"],
    );
    assert!(
        untangle.status.success(),
        "{}",
        String::from_utf8_lossy(&untangle.stderr)
    );
    let v: Value = serde_json::from_slice(&untangle.stdout).unwrap();
    assert_eq!(
        v["snapshot"]["context"]["untracked_changes"][0]["content_hex"],
        "707269766174652d756e747261636b65642d646174610a"
    );
    assert!(
        weave(
            &repo,
            &root,
            &["plan", "task", "--include-untracked-content"]
        )
        .status
        .success()
    );
    let result = weave(
        &repo,
        &root,
        &[
            "integrate",
            "--branch",
            "safe",
            "task",
            "--include-untracked-content",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let show = common::isolate(&mut Command::new("git"))
        .current_dir(&repo)
        .args(["show", "safe:new.txt"])
        .fixture_output()
        .unwrap();
    assert_eq!(show.stdout, b"private-untracked-data\n");
}

#[test]
fn sensitive_names_are_excluded_even_with_opt_in() {
    let (_temp, repo, root) = fixture();
    let work = root.join("task");
    let names = [
        ".npmrc",
        ".pypirc",
        ".git-credentials",
        ".docker/config.json",
        "kubeconfig",
        "key.p8",
        "cert.keystore",
        "cert.jks",
        "prod-service-account-key.json",
        "token-file",
        "nested/TOKEN_FILE",
        "nested/.DOCKER/CONFIG.JSON",
    ];
    for name in names {
        let path = work.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "private-value").unwrap();
    }
    let out = weave(
        &repo,
        &root,
        &["diff", "task", "--json", "--include-untracked-content"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    for name in names {
        let item = v["untracked_changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["path"] == name)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert!(item["content_hex"].is_null(), "{name}");
        assert_eq!(item["incomplete"], "sensitive path excluded", "{name}");
    }
    for args in [
        vec!["plan", "task", "--include-untracked-content"],
        vec![
            "integrate",
            "--branch",
            "secret",
            "task",
            "--include-untracked-content",
        ],
        vec!["untangle", "task", "--json", "--include-untracked-content"],
    ] {
        let result = weave(&repo, &root, &args);
        assert!(!result.status.success(), "{args:?}");
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("sensitive path excluded"));
    }
}
