mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(repo: &Path, spaces: &Path, args: &[&str], config: &Path) -> Output {
    common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
        .current_dir(repo)
        .args(args)
        .env("WEAVE_WORKSPACE_DIR", spaces)
        .env("GIT_CONFIG_GLOBAL", config)
        .fixture_output()
        .unwrap()
}

#[test]
fn filter_refusals_explain_each_operation_and_preserve_v1_and_sources() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let spaces = temp.path().join("spaces");
    fs::create_dir(&repo).unwrap();
    common::init(&repo, "trunk");
    fs::write(repo.join("file"), "base\n").unwrap();
    fs::write(repo.join(".gitattributes"), "file filter=unsafe\n").unwrap();
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-m", "base"]);
    let config = temp.path().join("global-\x1b[31m-config");
    fs::write(&config, "").unwrap();
    assert!(
        run(&repo, &spaces, &["new", "task", "--from", "trunk"], &config)
            .status
            .success()
    );
    let work = spaces.join("task");
    fs::write(work.join("file"), "changed\n").unwrap();
    let index_path = repo.join(".git/worktrees/task/index");
    let index = fs::read(&index_path).unwrap();
    let record_path = repo.join(".git/weave/task.json");
    let record = fs::read(&record_path).unwrap();
    let refs = || {
        common::isolate(&mut Command::new("git"))
            .current_dir(&repo)
            .args(["show-ref"])
            .fixture_output()
            .unwrap()
            .stdout
    };
    let before_refs = refs();
    let marker = temp.path().join("filter-ran");
    for key in ["clean", "process"] {
        // Exercise effective global and local definitions separately.
        for local in [false, true] {
            let command = format!("touch '{}'; cat", marker.display());
            if local {
                common::git(
                    &repo,
                    &["config", &format!("filter.unsafe.{key}"), &command],
                );
            } else {
                fs::write(&config, format!("[filter \"unsafe\"]\n{key} = {command}\n")).unwrap();
            }
            for (args, context) in [
                (vec!["plan", "task", "--json"], "Planning"),
                (
                    vec!["integrate", "task", "--branch", "review", "--json"],
                    "Integration",
                ),
                (vec!["untangle", "task", "--json"], "Untangle"),
                (vec!["remove", "task", "--dry-run"], "Removal"),
                (vec!["remove", "task", "--yes"], "Removal"),
            ] {
                let out = run(&repo, &spaces, &args, &config);
                assert!(!out.status.success(), "{args:?}");
                let text = String::from_utf8_lossy(&out.stderr);
                assert!(text.contains(context), "{text}");
                assert!(!text.contains('\x1b'));
                assert!(text.contains("git config --show-origin --get"), "{text}");
                assert!(text.contains("repository-specific"));
                assert!(text.contains(&format!("filter.unsafe.{key}")));
                assert!(!text.contains("touch"));
            }
            let human = run(&repo, &spaces, &["reconcile"], &config);
            assert!(human.status.success());
            let text = String::from_utf8_lossy(&human.stdout);
            assert!(text.contains("Reconciliation cannot inspect workspace task"));
            assert!(!text.contains('\x1b'));
            assert!(text.contains("git config --show-origin"));
            let json = run(&repo, &spaces, &["reconcile", "--json"], &config);
            assert!(json.status.success());
            let json: Value = serde_json::from_slice(&json.stdout).unwrap();
            let item = &json["managed"][0];
            common::assert_fields(
                item,
                &[
                    "name",
                    "path",
                    "branch",
                    "record",
                    "registration",
                    "states",
                    "moved_candidates",
                    "staged",
                    "unstaged",
                    "untracked",
                    "ignored",
                ],
            );
            assert_eq!(
                item["states"],
                serde_json::json!(["inspection_unavailable"])
            );
            assert_eq!(item["staged"], Value::Null);
            assert!(!marker.exists());
            assert_eq!(fs::read(&index_path).unwrap(), index);
            assert_eq!(fs::read(&record_path).unwrap(), record);
            assert_eq!(fs::read(work.join("file")).unwrap(), b"changed\n");
            assert_eq!(fs::read(repo.join("file")).unwrap(), b"base\n");
            assert_eq!(refs(), before_refs);
            if local {
                common::git(
                    &repo,
                    &["config", "--unset", &format!("filter.unsafe.{key}")],
                );
            }
            fs::write(&config, "").unwrap();
        }
    }
    // Definitions that cannot run clean/process commands remain accepted.
    common::git(&repo, &["config", "filter.unsafe.clean", ""]);
    common::git(&repo, &["config", "filter.unsafe.process", ""]);
    common::git(&repo, &["config", "filter.unsafe.smudge", "false"]);
    common::git(&repo, &["config", "filter.unsafe.required", "false"]);
    assert!(
        run(&repo, &spaces, &["plan", "task", "--json"], &config)
            .status
            .success()
    );
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn unreadable_filter_configuration_has_actionable_refusal() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let spaces = temp.path().join("spaces");
    fs::create_dir(&repo).unwrap();
    common::init(&repo, "trunk");
    fs::write(repo.join("file"), "base").unwrap();
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-m", "base"]);
    let config = temp.path().join("global");
    fs::write(&config, "").unwrap();
    assert!(
        run(&repo, &spaces, &["new", "task", "--from", "trunk"], &config)
            .status
            .success()
    );
    let real = Command::new("which").arg("git").output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let shim = bin.join("git");
    fs::write(&shim, format!("#!/bin/sh\nfor arg do\n if [ \"$arg\" = '--name-only' ]; then exit 2; fi\ndone\nexec '{}' \"$@\"\n", real.trim().replace('\'', "'\\''"))).unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        vec!["plan", "task"],
        vec!["reconcile"],
        vec!["remove", "task", "--yes"],
        vec!["untangle", "task", "--json"],
    ] {
        let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&repo)
            .args(&args)
            .env("WEAVE_WORKSPACE_DIR", &spaces)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .fixture_output()
            .unwrap();
        assert_eq!(out.status.success(), args[0] == "reconcile");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("Cannot inspect Git filter configuration"),
            "{text}"
        );
        assert!(text.contains("git config --show-origin"), "{text}");
    }
    assert_eq!(fs::read(spaces.join("task/file")).unwrap(), b"base");
}
