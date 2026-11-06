mod common;
use common::FixtureOutput;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
    work: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let root = temp.path().join("spaces");
        fs::create_dir(&repo).unwrap();
        common::init(&repo, "main");
        fs::write(
            repo.join("file"),
            (0..40).map(|n| format!("line {n}\n")).collect::<String>(),
        )
        .unwrap();
        fs::write(repo.join("other"), "other\n").unwrap();
        common::git(&repo, &["add", "."]);
        common::git(&repo, &["commit", "-m", "base"]);
        let f = Self {
            _temp: temp,
            repo,
            root: root.clone(),
            work: root.join("task"),
        };
        assert!(f.run(&["new", "task", "--from", "main"]).status.success());
        f
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_weave"));
        common::isolate(&mut c)
            .current_dir(&self.repo)
            .env("WEAVE_WORKSPACE_DIR", &self.root);
        c
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).fixture_output().unwrap()
    }
    fn checkpoint(&self, key: &str) -> Value {
        json(self.run(&["provenance", "checkpoint", "task", "--key", key]))
    }
    fn edit(&self) {
        let text = fs::read_to_string(self.work.join("file"))
            .unwrap()
            .replace("line 2\n", "first\n")
            .replace("line 30\n", "second\n");
        fs::write(self.work.join("file"), text).unwrap();
    }
    fn state(&self) -> Vec<Vec<u8>> {
        let git = |p: &Path, args: &[&str]| {
            let o = common::isolate(&mut Command::new("git"))
                .current_dir(p)
                .args(args)
                .fixture_output()
                .unwrap();
            assert!(o.status.success());
            o.stdout
        };
        let gitdir =
            String::from_utf8(git(&self.work, &["rev-parse", "--absolute-git-dir"])).unwrap();
        vec![
            fs::read(self.work.join("file")).unwrap(),
            fs::read(Path::new(gitdir.trim()).join("index")).unwrap(),
            git(&self.repo, &["show-ref"]),
            git(&self.work, &["status", "--porcelain=v1", "-z"]),
        ]
    }
    fn store(&self) -> PathBuf {
        self.repo.join(".git/weave/provenance")
    }
}
fn json(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn capture_is_opt_in_idempotent_and_preserves_sources_with_manual_identity_separate() {
    let f = Fixture::new();
    assert!(f.run(&["diff", "task", "--json"]).status.success());
    assert!(!f.store().exists());
    f.edit();
    let before = f.state();
    let first = f.checkpoint("one");
    assert_eq!(first["duplicate"], false);
    assert_eq!(f.checkpoint("one")["duplicate"], true);
    let report = json(f.run(&["provenance", "replay", "task", "--json"]));
    common::assert_fields(
        &report,
        &[
            "schema_version",
            "origin",
            "journal_hash",
            "snapshot_hash",
            "baseline",
            "files",
            "paths",
            "timeline",
            "diagnostics",
            "corrections",
        ],
    );
    assert_eq!(report["timeline"].as_array().unwrap().len(), 1);
    assert_eq!(report["paths"]["66696c65"]["source"], "verified_worktree");
    assert!(report["paths"]["66696c65"]["author_agent"].is_null());
    assert_eq!(f.state(), before);
    let bad = f.run(&[
        "provenance",
        "checkpoint",
        "task",
        "--key",
        "one",
        "--task",
        "different",
    ]);
    assert!(!bad.status.success());
    fs::write(f.work.join("other"), "manual\n").unwrap();
    json(f.run(&[
        "provenance",
        "checkpoint",
        "task",
        "--key",
        "two",
        "--agent",
        "agent-claim",
        "--task",
        "owner-task",
        "--owner-agent",
        "owner-agent",
    ]));
    let report = json(f.run(&["provenance", "replay", "task", "--json"]));
    let a = &report["paths"]["6f74686572"];
    assert_eq!(a["source"], "manual");
    assert_eq!(a["author_agent"], "agent-claim");
    assert_eq!(a["owner_task"], "owner-task");
}

#[test]
fn collector_excludes_tracked_sensitive_and_ignored_content_before_persistence() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env"), "TRACKED_PRIVATE_SENTINEL").unwrap();
    fs::write(f.repo.join("ignored"), "IGNORED_PRIVATE_SENTINEL").unwrap();
    fs::write(f.repo.join(".gitignore"), "ignored\n").unwrap();
    common::git(&f.repo, &["add", "-f", ".env", "ignored", ".gitignore"]);
    common::git(&f.repo, &["commit", "-m", "privacy baseline"]);
    assert!(
        f.run(&["new", "private", "--from", "main"])
            .status
            .success()
    );
    let work = f.root.join("private");
    fs::write(work.join("token-file"), "UNTRACKED_PRIVATE_SENTINEL").unwrap();
    json(f.run(&[
        "provenance",
        "checkpoint",
        "private",
        "--key",
        "privacy",
        "--include-untracked-content",
    ]));
    let report = json(f.run(&["provenance", "replay", "private", "--json"]));
    assert!(report["files"]["2e656e76"].is_null());
    assert!(report["files"]["69676e6f726564"].is_null());
    for item in fs::read_dir(f.store().join("blobs")).unwrap() {
        let data = fs::read(item.unwrap().path()).unwrap();
        for secret in [
            b"TRACKED_PRIVATE_SENTINEL".as_slice(),
            b"IGNORED_PRIVATE_SENTINEL",
            b"UNTRACKED_PRIVATE_SENTINEL",
        ] {
            assert!(!data.windows(secret.len()).any(|w| w == secret));
        }
    }
    fs::write(f.work.join("ordinary-new"), "opt-in\n").unwrap();
    assert!(
        !f.run(&["provenance", "checkpoint", "task", "--key", "refuse"])
            .status
            .success()
    );
    json(f.run(&[
        "provenance",
        "checkpoint",
        "task",
        "--key",
        "opt-in",
        "--include-untracked-content",
    ]));
}

#[test]
fn concurrent_checkpoint_retries_publish_once_and_no_hook_review_stays_unknown() {
    let f = Fixture::new();
    f.edit();
    let home = tempfile::tempdir().unwrap();
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            f.command()
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path())
                .args(["provenance", "checkpoint", "task", "--key", "retry"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let results = children
        .into_iter()
        .map(|c| json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|r| r["duplicate"] == false).count(),
        1
    );
    assert_eq!(fs::read_dir(f.store().join("records")).unwrap().count(), 1);
    let other = Fixture::new();
    other.edit();
    let e = json(other.run(&["untangle", "task", "--provenance", "--evidence-json"]));
    assert!(!other.store().exists());
    for u in e["units"].as_object().unwrap().values() {
        assert_eq!(u["attribution"]["source"], "unknown");
    }
}

#[test]
fn mixed_file_same_file_evidence_exports_with_audit_and_exact_original_patches() {
    let f = Fixture::new();
    f.edit();
    fs::write(f.work.join("other"), "edited-other\n").unwrap();
    f.checkpoint("mixed");
    let before = f.state();
    let preview = json(f.run(&["untangle", "task", "--json"]));
    let s = &preview["snapshot"];
    let sid = s["id"].as_str().unwrap();
    let units = s["units"].as_array().unwrap();
    assert_eq!(units.len(), 3);
    let evidence = json(f.run(&["untangle", "task", "--provenance", "--evidence-json"]));
    for u in units {
        let e = &evidence["units"][u["id"].as_str().unwrap()];
        assert_eq!(e["attribution"]["source"], "verified_worktree");
        assert_eq!(e["original_patch_hex"], u["patch_hex"]);
        assert_eq!(e["current_patch_hex"], u["patch_hex"]);
        assert_eq!(u["ownership"], "unknown");
    }
    let assigns = units
        .iter()
        .enumerate()
        .map(|(i, u)| {
            format!(
                "{}={}",
                u["id"].as_str().unwrap(),
                if i == 1 { "two" } else { "one" }
            )
        })
        .collect::<Vec<_>>();
    let out = f._temp.path().join("export");
    let mut args = vec![
        "untangle",
        "task",
        "--provenance",
        "--snapshot",
        sid,
        "--base",
        s["base_commit"].as_str().unwrap(),
        "--group",
        "one",
        "--group",
        "two",
        "--export-patches",
        out.to_str().unwrap(),
        "--json",
    ];
    for a in &assigns {
        args.extend(["--assign", a]);
    }
    let reject = f.run(&args);
    assert!(!reject.status.success());
    assert!(!out.exists());
    args.push("--acknowledge-evidence");
    let manifest = json(f.run(&args));
    assert_eq!(manifest["complete_reconstruction"], true);
    assert_eq!(manifest["ownership"], "unknown");
    assert!(out.join("provenance.json").is_file());
    let audit: Value = serde_json::from_slice(&fs::read(out.join("audit.json")).unwrap()).unwrap();
    assert_eq!(audit["entries"][0]["action"], "export_acknowledgment");
    assert!(!f.run(&args).status.success());
    let bundle = f._temp.path().join("bundle-export");
    let position = args.iter().position(|a| *a == "--export-patches").unwrap();
    args[position] = "--export-commits";
    args[position + 1] = bundle.to_str().unwrap();
    let bundle_manifest = json(f.run(&args));
    assert_eq!(bundle_manifest["complete_reconstruction"], true);
    assert!(
        bundle.join("commits.bundle").is_file()
            && bundle.join("provenance.json").is_file()
            && bundle.join("audit.json").is_file()
    );
    assert_eq!(f.state(), before);
}

#[test]
fn conflicting_hunk_evidence_requires_resolution_and_manual_override_never_verifies() {
    let f = Fixture::new();
    f.edit();
    f.checkpoint("one");
    let text = fs::read_to_string(f.work.join("file"))
        .unwrap()
        .replace("first\n", "again\n");
    fs::write(f.work.join("file"), text).unwrap();
    f.checkpoint("two");
    let before = f.state();
    let v = json(f.run(&["untangle", "task", "--json"]));
    let s = &v["snapshot"];
    let units = s["units"].as_array().unwrap();
    let out = f._temp.path().join("resolved");
    let assignments = units
        .iter()
        .map(|u| format!("{}=one", u["id"].as_str().unwrap()))
        .collect::<Vec<_>>();
    let mut args = vec![
        "untangle",
        "task",
        "--provenance",
        "--snapshot",
        s["id"].as_str().unwrap(),
        "--base",
        s["base_commit"].as_str().unwrap(),
        "--group",
        "one",
        "--export-patches",
        out.to_str().unwrap(),
        "--acknowledge-evidence",
        "--json",
    ];
    for a in &assignments {
        args.extend(["--assign", a]);
    }
    assert!(!f.run(&args).status.success());
    assert!(!out.exists());
    for u in units {
        args.extend(["--resolve-unknown", u["id"].as_str().unwrap()]);
    }
    args.extend([
        "--reason",
        "Overlapping checkpoint evidence reviewed; keep authorship unknown",
    ]);
    json(f.run(&args));
    let evidence: Value =
        serde_json::from_slice(&fs::read(out.join("provenance.json")).unwrap()).unwrap();
    assert!(
        evidence["units"]
            .as_object()
            .unwrap()
            .values()
            .all(|u| u["attribution"]["source"] == "unknown" && u["resolution"].is_string())
    );
    assert_eq!(f.state(), before);
    let manual = format!("{}=claimed-agent", units[0]["id"].as_str().unwrap());
    let e = json(f.run(&[
        "untangle",
        "task",
        "--provenance",
        "--evidence-json",
        "--snapshot",
        s["id"].as_str().unwrap(),
        "--override-author",
        &manual,
        "--reason",
        "External assertion",
    ]));
    assert_eq!(
        e["units"][units[0]["id"].as_str().unwrap()]["attribution"]["source"],
        "manual"
    );
}

#[test]
fn filter_commands_never_execute_and_raw_git_paths_remain_exact() {
    let f = Fixture::new();
    f.edit();
    let marker = f._temp.path().join("filter-ran");
    fs::write(f.work.join(".gitattributes"), "file filter=unsafe\n").unwrap();
    common::git(
        &f.work,
        &[
            "config",
            "filter.unsafe.clean",
            &format!("touch {}", marker.display()),
        ],
    );
    let before = f.state();
    assert!(
        !f.run(&["provenance", "checkpoint", "task", "--key", "unsafe"])
            .status
            .success()
    );
    assert!(!marker.exists());
    assert_eq!(f.state(), before);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let other = Fixture::new();
        // APFS requires valid Unicode names; byte paths are exercised on Linux,
        // while the schema fixture checks non-UTF-8 serialization on both OSes.
        #[cfg(target_os = "macos")]
        let raw = b"name\n".as_slice();
        #[cfg(not(target_os = "macos"))]
        let raw = b"name\xff\n".as_slice();
        fs::write(
            other.work.join(std::ffi::OsStr::from_bytes(raw)),
            b"bytes\xff",
        )
        .unwrap();
        json(other.run(&[
            "provenance",
            "checkpoint",
            "task",
            "--key",
            "raw",
            "--include-untracked-content",
        ]));
        let r = json(other.run(&["provenance", "replay", "task", "--json"]));
        assert_eq!(
            r["timeline"][0]["event"]["event"]["path_hex"],
            raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
    }
}

#[test]
fn overlapping_two_worktree_sessions_remain_unknown_and_keep_separate_origins() {
    let f = Fixture::new();
    assert!(
        f.run(&["new", "other-task", "--from", "main"])
            .status
            .success()
    );
    f.edit();
    fs::write(f.root.join("other-task/file"), "overlapping change\n").unwrap();
    json(f.run(&[
        "provenance",
        "checkpoint",
        "task",
        "--key",
        "agent-one",
        "--agent",
        "agent-one",
    ]));
    json(f.run(&[
        "provenance",
        "checkpoint",
        "other-task",
        "--key",
        "agent-two",
        "--agent",
        "agent-two",
    ]));
    let before = f.state();
    let a = json(f.run(&["provenance", "replay", "task", "--json"]));
    let b = json(f.run(&["provenance", "replay", "other-task", "--json"]));
    assert_ne!(a["origin"]["worktree_id"], b["origin"]["worktree_id"]);
    assert_eq!(a["paths"]["66696c65"]["source"], "unknown");
    assert!(
        a["paths"]["66696c65"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d == "cross_worktree_overlap_requires_review")
    );
    assert_eq!(a["timeline"].as_array().unwrap().len(), 2);
    assert_eq!(f.state(), before);
}

#[test]
fn a_restored_file_cannot_reuse_recorded_deletion_evidence() {
    let f = Fixture::new();
    fs::remove_file(f.work.join("other")).unwrap();
    f.checkpoint("deletion");
    fs::write(f.work.join("other"), "restored and modified\n").unwrap();
    let before = f.state();
    let evidence = json(f.run(&["untangle", "task", "--provenance", "--evidence-json"]));
    let unit = evidence["units"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(unit["attribution"]["source"], "unknown");
    assert_eq!(unit["requires_resolution"], true);
    assert!(
        unit["attribution"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d == "snapshot_or_base_mismatch")
    );
    assert_eq!(f.state(), before);
}

#[cfg(unix)]
#[test]
fn journal_drift_during_export_refuses_publication_and_cleans_staging() {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    f.edit();
    f.checkpoint("before-export");
    let before = f.state();
    let v = json(f.run(&["untangle", "task", "--json"]));
    let s = &v["snapshot"];
    let assignments = s["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| format!("{}=one", u["id"].as_str().unwrap()))
        .collect::<Vec<_>>();
    let bin = f._temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let real = Command::new("which").arg("git").output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let marker = f._temp.path().join("bundle-entered");
    let q = |p: &Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
    let wrapper = bin.join("git");
    fs::write(&wrapper,format!("#!/bin/sh\ncase \" $* \" in *' bundle create '*) : > {}; sleep 1 ;; esac\nexec {} \"$@\"\n",q(&marker),q(Path::new(&real)))).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let out = f._temp.path().join("stale-export");
    let home = tempfile::tempdir().unwrap();
    let mut c = f.command();
    c.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .args([
            "untangle",
            "task",
            "--provenance",
            "--acknowledge-evidence",
            "--snapshot",
            s["id"].as_str().unwrap(),
            "--base",
            s["base_commit"].as_str().unwrap(),
            "--group",
            "one",
            "--export-commits",
            out.to_str().unwrap(),
            "--json",
        ]);
    for a in &assignments {
        c.args(["--assign", a]);
    }
    let child = c
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !marker.exists() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    f.checkpoint("during-export");
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Provenance changed since capture"));
    assert!(!out.exists());
    assert!(!fs::read_dir(f._temp.path()).unwrap().any(|i| {
        i.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".vineweaver-export-")
    }));
    assert_eq!(f.state(), before);
}

#[cfg(unix)]
#[test]
fn source_changes_between_checkpoint_passes_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit();
    let before = f.state();
    let bin = f._temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let real = Command::new("which").arg("git").output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let marker = f._temp.path().join("first-tree");
    let q = |s: &Path| format!("'{}'", s.to_str().unwrap().replace('\'', "'\\''"));
    let wrapper = bin.join("git");
    fs::write(&wrapper,format!("#!/bin/sh\ncase \" $* \" in *' ls-tree '*) if test -e {}; then printf 'concurrent content\\n' > {}; else : > {}; fi ;; esac\nexec {} \"$@\"\n",q(&marker),q(&f.work.join("file")),q(&marker),q(Path::new(&real)))).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let out = f
        .command()
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .args(["provenance", "checkpoint", "task", "--key", "race"])
        .fixture_output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Inconsistent checkpoint capture"));
    assert_eq!(f.state()[1..], before[1..]);
    assert_eq!(fs::read_dir(f.store().join("records")).unwrap().count(), 0);
}

#[test]
fn privacy_policy_drift_is_refused_without_appending_or_reading_newly_excluded_content() {
    let f = Fixture::new();
    f.edit();
    f.checkpoint("before-ignore");
    fs::write(f.work.join(".gitignore"), "file\n").unwrap();
    fs::write(
        f.work.join("file"),
        "NEW_IGNORED_CONTENT_MUST_NOT_BE_STORED",
    )
    .unwrap();
    let before = f.state();
    let result = f.run(&[
        "provenance",
        "checkpoint",
        "task",
        "--key",
        "after-ignore",
        "--include-untracked-content",
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("baseline/privacy selection changed"));
    assert_eq!(fs::read_dir(f.store().join("records")).unwrap().count(), 1);
    json(f.run(&["provenance", "replay", "task", "--json"]));
    for blob in fs::read_dir(f.store().join("blobs")).unwrap() {
        assert!(
            !String::from_utf8_lossy(&fs::read(blob.unwrap().path()).unwrap())
                .contains("NEW_IGNORED_CONTENT_MUST_NOT_BE_STORED")
        );
    }
    assert_eq!(f.state(), before);
}

#[cfg(unix)]
#[test]
fn symlinked_metadata_parent_cannot_redirect_storage_into_source() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let metadata = f.repo.join(".git/weave");
    let redirected = f.work.join("relocated-metadata");
    fs::rename(&metadata, &redirected).unwrap();
    symlink(&redirected, &metadata).unwrap();
    let before = f.state();
    let result = f.run(&[
        "provenance",
        "checkpoint",
        "task",
        "--key",
        "refuse-redirect",
        "--include-untracked-content",
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Unsafe provenance metadata parent"));
    assert!(!redirected.join("provenance").exists());
    assert_eq!(f.state(), before);
}
