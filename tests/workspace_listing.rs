mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(repo: &Path, spaces: &Path, args: &[&str]) -> Output {
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(repo)
        .env("WEAVE_WORKSPACE_DIR", spaces)
        .args(args)
        .fixture_output()
        .unwrap()
}
fn refs(repo: &Path) -> Vec<u8> {
    common::isolate(&mut Command::new("git"))
        .current_dir(repo)
        .args(["show-ref"])
        .fixture_output()
        .unwrap()
        .stdout
}
#[test]
fn mixed_listing_retains_unavailable_records_and_fails_without_partial_output() {
    for damage in [
        "missing",
        "unregistered",
        "malformed",
        "unsupported",
        "status-failure",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let spaces = temp.path().join("spaces");
        fs::create_dir(&repo).unwrap();
        common::init(&repo, "trunk");
        fs::write(repo.join("file"), "base\n").unwrap();
        common::git(&repo, &["add", "."]);
        common::git(&repo, &["commit", "-m", "base"]);
        for name in ["healthy", "damaged"] {
            assert!(
                run(&repo, &spaces, &["new", name, "--from", "trunk"])
                    .status
                    .success()
            );
        }
        let work = spaces.join("damaged");
        let record_path = repo.join(".git/weave/damaged.json");
        let kept = if damage == "missing" {
            let moved = spaces.join("moved");
            fs::rename(&work, &moved).unwrap();
            moved
        } else {
            work.clone()
        };
        if damage == "unregistered" {
            let other = spaces.join("unregistered");
            fs::create_dir(&other).unwrap();
            fs::write(other.join("keep"), "keep").unwrap();
            let mut record: Value =
                serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
            record["path"] = Value::from(other.to_str().unwrap());
            fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        } else if damage == "malformed" {
            fs::write(&record_path, "{broken").unwrap();
        } else if damage == "unsupported" {
            let mut record: Value =
                serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
            record["schema_version"] = Value::from(99);
            fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        } else if damage == "status-failure" {
            fs::write(work.join(".git"), "gitdir: /nonexistent-wev-listing\n").unwrap();
        }
        let records = fs::read(&record_path).unwrap();
        let index = fs::read(repo.join(".git/index")).unwrap();
        let healthy_index = fs::read(repo.join(".git/worktrees/healthy/index")).unwrap();
        let damaged_index = fs::read(repo.join(".git/worktrees/damaged/index")).unwrap();
        let before_refs = refs(&repo);
        for json in [false, true] {
            let out = run(
                &repo,
                &spaces,
                if json { &["list", "--json"] } else { &["list"] },
            );
            if matches!(damage, "missing" | "unregistered") {
                assert!(
                    out.status.success(),
                    "{damage}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                if json {
                    let data: Value = serde_json::from_slice(&out.stdout).unwrap();
                    common::assert_fields(&data, &["schema_version", "workspaces"]);
                    assert_eq!(data["schema_version"], 1);
                    let entries = data["workspaces"].as_array().unwrap();
                    assert_eq!(entries.len(), 2);
                    let damaged = entries.iter().find(|v| v["name"] == "damaged").unwrap();
                    assert_eq!(damaged["exists"], false);
                    assert_eq!(damaged["dirty"], Value::Null);
                    let healthy = entries.iter().find(|v| v["name"] == "healthy").unwrap();
                    assert_eq!(healthy["exists"], true);
                    assert_eq!(healthy["dirty"], false);
                } else {
                    assert!(String::from_utf8_lossy(&out.stdout).contains("missing"));
                }
            } else {
                assert!(!out.status.success(), "{damage}");
                assert!(out.stdout.is_empty(), "{damage}");
                assert!(!out.stderr.is_empty());
            }
        }
        for args in [
            &["remove", "damaged", "--yes"][..],
            &["remove", "damaged", "--dry-run"][..],
        ] {
            assert!(!run(&repo, &spaces, args).status.success(), "{damage}");
        }
        assert_eq!(fs::read(&record_path).unwrap(), records);
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
        assert_eq!(
            fs::read(repo.join(".git/worktrees/healthy/index")).unwrap(),
            healthy_index
        );
        assert_eq!(
            fs::read(repo.join(".git/worktrees/damaged/index")).unwrap(),
            damaged_index
        );
        assert_eq!(refs(&repo), before_refs);
        for p in [
            repo.join("file"),
            spaces.join("healthy/file"),
            kept.join("file"),
        ] {
            assert_eq!(fs::read(p).unwrap(), b"base\n");
        }
        if damage == "unregistered" {
            assert_eq!(fs::read(spaces.join("unregistered/keep")).unwrap(), b"keep");
        }
    }
}
