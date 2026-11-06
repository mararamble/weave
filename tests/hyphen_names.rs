mod common;
use common::FixtureOutput;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const NAMES: [&str; 6] = ["-odd", "--help", "--yes", "-", "--", "ordinary"];

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    spaces: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let spaces = temp.path().join("spaces");
        fs::create_dir(&repo).unwrap();
        common::init(&repo, "trunk");
        fs::write(repo.join("file"), "base\n").unwrap();
        common::git(&repo, &["add", "file"]);
        common::git(&repo, &["commit", "-m", "base"]);
        Self {
            _temp: temp,
            repo,
            spaces,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&self.repo)
            .env("WEAVE_WORKSPACE_DIR", &self.spaces)
            .args(args)
            .fixture_output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        value
    }
    fn git(&self, dir: &Path, args: &[&str]) -> Vec<u8> {
        let out = common::isolate(&mut Command::new("git"))
            .current_dir(dir)
            .args(args)
            .fixture_output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }
    fn refs(&self) -> Vec<u8> {
        self.git(&self.repo, &["show-ref"])
    }
    fn existing(&self, name: &str) {
        // Construct a valid pre-existing record without exercising the new CLI.
        fs::create_dir_all(&self.spaces).unwrap();
        let path = self.spaces.join(name);
        let branch = format!("weave/{name}");
        common::git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str().unwrap(),
                "trunk",
            ],
        );
        let base = String::from_utf8(self.git(&self.repo, &["rev-parse", "trunk"])).unwrap();
        let meta = self.repo.join(".git/weave");
        fs::create_dir_all(&meta).unwrap();
        fs::write(
            meta.join(format!("{name}.json")),
            serde_json::to_vec(&json!({
                "schema_version":1, "name":name, "branch":branch, "base_commit":base.trim(),
                "repository":self.repo.join(".git").canonicalize().unwrap(), "path":path.canonicalize().unwrap(), "state":"ready"
            }))
            .unwrap(),
        )
        .unwrap();
    }
    fn source_state(&self, names: &[&str]) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(path: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut children: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            children.sort();
            for child in children {
                if child.is_dir() {
                    walk(&child, entries);
                } else {
                    entries.push((child.clone(), fs::read(child).unwrap()));
                }
            }
        }
        let mut entries = vec![
            (
                self.repo.join("file"),
                fs::read(self.repo.join("file")).unwrap(),
            ),
            (
                self.repo.join(".git/index"),
                fs::read(self.repo.join(".git/index")).unwrap(),
            ),
        ];
        for name in names {
            let path = self.spaces.join(name);
            walk(&path, &mut entries);
            let raw = self.git(
                &path,
                &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            );
            let index = PathBuf::from(String::from_utf8(raw).unwrap().trim());
            entries.push((index.clone(), fs::read(index).unwrap()));
            let branch = format!("refs/heads/weave/{name}");
            entries.push((
                PathBuf::from(&branch),
                self.git(&self.repo, &["rev-parse", "--verify", &branch]),
            ));
            let record = self.repo.join(".git/weave").join(format!("{name}.json"));
            entries.push((record.clone(), fs::read(record).unwrap()));
        }
        entries
    }
}

fn exercise_names(preexisting: bool) {
    let f = Fixture::new();
    for name in NAMES {
        if preexisting {
            f.existing(name);
        } else {
            f.ok(&["new", "--from", "trunk", "--", name]);
        }
    }
    for (i, name) in NAMES.iter().enumerate() {
        fs::write(
            f.spaces.join(name).join(format!("addition-{i}")),
            format!("{name}\n"),
        )
        .unwrap();
    }
    let sources = f.source_state(&NAMES);
    let refs = f.refs();
    let list = f.json(&["list", "--json"]);
    assert_eq!(list["workspaces"].as_array().unwrap().len(), NAMES.len());
    common::assert_fields(&list, &["schema_version", "workspaces"]);
    f.json(&["status", "--json"]);
    f.json(&["reconcile", "--json"]);
    for name in NAMES {
        let diff = f.json(&["diff", "--json", "--", name]);
        assert_eq!(diff["name"], name);
        assert!(diff["untracked_changes"][0]["content_hex"].is_null());
        let diff = f.json(&["diff", "--json", "--include-untracked-content", "--", name]);
        assert!(diff["untracked_changes"][0]["content_hex"].is_string());
        f.ok(&["diff", "--stat", "--", name]);
        f.ok(&["diff", "--", name]);
        let plan = f.json(&["plan", "--json", "--include-untracked-content", "--", name]);
        assert_eq!(plan["selected_workspaces"], json!([name]));
        f.json(&["overlaps", "--json", "--", name]);
        let untangle = f.json(&[
            "untangle",
            "--json",
            "--include-untracked-content",
            "--",
            name,
        ]);
        assert_eq!(untangle["snapshot"]["context"]["name"], name);
        // Dirty refusal must apply equally to flag-like names, even with --yes.
        assert!(!f.run(&["remove", "--dry-run", "--", name]).status.success());
        assert!(!f.run(&["remove", "--yes", "--", name]).status.success());
    }
    let order = ["ordinary", "--", "-odd", "--yes", "-", "--help"];
    let mut args = vec!["plan", "--json", "--include-untracked-content", "--"];
    args.extend(order);
    assert_eq!(f.json(&args)["selected_workspaces"], json!(order));
    let overlap = f.json(&["overlaps", "--json", "--", "ordinary", "--help", "-odd"]);
    common::assert_fields(&overlap, &["schema_version", "overlaps", "warnings"]);
    assert_eq!(sources, f.source_state(&NAMES));
    assert_eq!(refs, f.refs());

    let named: Vec<_> = order.iter().map(|n| format!("--workspace={n}")).collect();
    let mut args = vec![
        "integrate",
        "--branch",
        "review/named",
        "--json",
        "--include-untracked-content",
    ];
    args.extend(named.iter().map(String::as_str));
    let result = f.json(&args);
    common::assert_fields(
        &result,
        &[
            "schema_version",
            "branch",
            "commit",
            "tree",
            "target_base",
            "sources",
            "validation",
            "warnings",
        ],
    );
    assert_eq!(
        result["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["workspace"].as_str().unwrap())
            .collect::<Vec<_>>(),
        order
    );
    assert!(result["validation"].is_null());
    assert_eq!(sources, f.source_state(&NAMES));

    #[cfg(unix)]
    {
        args[2] = "review/validated";
        let command = [
            "sh",
            "-c",
            "test \"$1\" = --help && test \"$2\" = --yes && test \"$3\" = -odd && test -z \"$4\"",
            "validation",
            "--help",
            "--yes",
            "-odd",
            "",
        ];
        args.push("--");
        args.extend(command);
        let result = f.json(&args);
        assert_eq!(result["validation"]["command"], json!(command));
        assert_eq!(result["validation"]["exit_code"], 0);
        assert_eq!(sources, f.source_state(&NAMES));
    }
    // Refusal during validation must not publish a branch or change any source.
    let refs = f.refs();
    assert!(
        !f.run(&[
            "integrate",
            "--branch",
            "review/refused",
            "--workspace=--help",
            "--include-untracked-content",
            "--",
            "/weave-no-such-validation"
        ])
        .status
        .success()
    );
    assert_eq!(refs, f.refs());
    assert_eq!(sources, f.source_state(&NAMES));
}

#[test]
fn newly_created_flag_like_names_have_all_cli_routes() {
    exercise_names(false);
}
#[test]
fn preexisting_flag_like_names_remain_manageable() {
    exercise_names(true);
}

#[test]
fn clean_removal_requires_confirmation_preserves_branch_and_refuses_uncertainty() {
    let f = Fixture::new();
    for name in NAMES {
        f.ok(&["new", "--from", "trunk", "--", name]);
        let state = f.source_state(&[name]);
        let refs = f.refs();
        let preview = f.ok(&["remove", "--dry-run", "--", name]);
        assert!(String::from_utf8_lossy(&preview.stdout).contains("Preview only"));
        assert!(!f.run(&["remove", "--", name]).status.success());
        assert_eq!(state, f.source_state(&[name]));
        assert_eq!(refs, f.refs());
        common::git(
            &f.spaces.join(name),
            &["update-index", "--assume-unchanged", "file"],
        );
        let uncertain = f.source_state(&[name]);
        assert!(!f.run(&["remove", "--yes", "--", name]).status.success());
        assert_eq!(uncertain, f.source_state(&[name]));
        assert_eq!(refs, f.refs());
        common::git(
            &f.spaces.join(name),
            &["update-index", "--no-assume-unchanged", "file"],
        );
        f.ok(&["remove", "--yes", "--", name]);
        assert!(!f.spaces.join(name).exists());
        assert_eq!(refs, f.refs());
    }
}

#[test]
fn parser_keeps_options_distinct_and_rejects_empty_mixed_or_unknown_arguments() {
    let f = Fixture::new();
    f.ok(&["new", "ordinary", "--from", "trunk"]);
    let refs = f.refs();
    let state = f.source_state(&["ordinary"]);
    for args in [
        vec!["integrate", "--branch", "review/empty", "--workspace="],
        vec![
            "integrate",
            "--branch",
            "review/mixed",
            "ordinary",
            "--workspace=-odd",
        ],
        vec![
            "integrate",
            "--branch",
            "review/unknown",
            "ordinary",
            "--unknown",
        ],
        vec![
            "integrate",
            "--branch",
            "review/ambiguous",
            "--workspace",
            "--help",
        ],
        vec!["integrate", "--branch", "review/missing", "--", "-odd"],
        vec!["integrate", "--branch", "review/missing"],
    ] {
        let out = f.run(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for command in [
        "new",
        "diff",
        "plan",
        "overlaps",
        "remove",
        "untangle",
        "integrate",
    ] {
        let help = f.ok(&[command, "--help"]);
        assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
        assert_eq!(f.run(&[command, "--no-such-option"]).status.code(), Some(2));
    }
    assert_eq!(state, f.source_state(&["ordinary"]));
    assert_eq!(refs, f.refs());
    f.json(&["plan", "ordinary", "--json"]);
    f.json(&["diff", "ordinary", "--json"]);
    assert_eq!(state, f.source_state(&["ordinary"]));
    assert_eq!(refs, f.refs());
    // Existing positional integration syntax remains supported.
    f.json(&[
        "integrate",
        "--branch",
        "review/positional",
        "--json",
        "ordinary",
    ]);
}
