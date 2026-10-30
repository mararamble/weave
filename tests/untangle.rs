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
        common::git(&repo, &["add", "."]);
        common::git(&repo, &["commit", "-m", "base"]);
        let f = Self {
            work: root.join("mixed"),
            _temp: temp,
            repo,
            root,
        };
        assert!(f.run(&["new", "mixed", "--from", "main"]).status.success());
        f
    }
    fn run(&self, args: &[&str]) -> Output {
        common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&self.repo)
            .env("WEAVE_WORKSPACE_DIR", &self.root)
            .args(args)
            .fixture_output()
            .unwrap()
    }
    fn capture(&self) -> Value {
        json(self.run(&["untangle", "mixed", "--json"]))
    }
    fn edit(&self) {
        let data = fs::read_to_string(self.work.join("file"))
            .unwrap()
            .replace("line 2\n", "first\n")
            .replace("line 30\n", "second\n");
        fs::write(self.work.join("file"), data).unwrap();
    }
}
fn json(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn git_out(path: &Path, args: &[&str]) -> Vec<u8> {
    let out = common::isolate(&mut Command::new("git"))
        .current_dir(path)
        .args(args)
        .fixture_output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}
fn source(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let index_dir = git_out(&f.work, &["rev-parse", "--absolute-git-dir"]);
    let index = PathBuf::from(String::from_utf8(index_dir).unwrap().trim()).join("index");
    (
        fs::read(f.work.join("file")).unwrap(),
        fs::read(index).unwrap(),
        git_out(&f.repo, &["show-ref"]),
        git_out(&f.work, &["status", "--porcelain=v1", "-z"]),
    )
}
#[test]
fn same_file_manual_groups_stale_and_conflicting_assignments_preserve_source() {
    let f = Fixture::new();
    f.edit();
    let before = source(&f);
    let v = f.capture();
    assert_eq!(v["snapshot"]["units"].as_array().unwrap().len(), 2);
    assert_eq!(v["unassigned"].as_array().unwrap().len(), 2);
    assert_eq!(v["selection_complete"], false);
    common::assert_fields(
        &v,
        &[
            "schema_version",
            "snapshot",
            "groups",
            "unassigned",
            "selection_complete",
            "sequence_applicable",
            "applications",
            "warnings",
        ],
    );
    let id = v["snapshot"]["id"].as_str().unwrap();
    let u = &v["snapshot"]["units"];
    let a = format!("{}=first", u[0]["id"].as_str().unwrap());
    let b = format!("{}=second", u[1]["id"].as_str().unwrap());
    let args = [
        "untangle",
        "mixed",
        "--json",
        "--snapshot",
        id,
        "--group",
        "first",
        "--group",
        "second",
        "--assign",
        &a,
        "--assign",
        &b,
    ];
    let grouped = json(f.run(&args));
    assert_eq!(grouped["selection_complete"], true);
    assert_eq!(grouped["sequence_applicable"], true);
    assert!(
        grouped["applications"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["independently_applicable"] == true)
    );
    assert_eq!(u[0]["ownership"], "unknown");
    let mut conflicting = args.to_vec();
    conflicting.extend(["--assign", &a]);
    assert!(!f.run(&conflicting).status.success());
    let mut cleared = args.to_vec();
    cleared.extend(["--unassign", u[0]["id"].as_str().unwrap()]);
    assert_eq!(
        json(f.run(&cleared))["unassigned"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(source(&f), before);
    fs::write(f.work.join("extra"), "changed").unwrap();
    assert!(!f.run(&args).status.success());
}
#[test]
fn indivisible_special_changes_and_byte_paths() {
    let f = Fixture::new();
    fs::write(f.work.join("file"), "all replaced\n").unwrap();
    fs::write(f.work.join("binary"), b"\0\xff\n").unwrap();
    fs::write(f.work.join("untracked\nname"), b"new").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        #[cfg(not(target_os = "macos"))]
        {
            use std::os::unix::ffi::OsStrExt;
            fs::write(
                f.work.join(std::ffi::OsStr::from_bytes(b"bad\xff")),
                b"bytes",
            )
            .unwrap();
        }
        fs::set_permissions(f.work.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let v = f.capture();
    assert!(
        v["snapshot"]["units"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["kind"] == "whole_file")
    );
    assert_eq!(v["snapshot"]["complete"], true);
    #[cfg(all(unix, not(target_os = "macos")))]
    assert!(
        v["snapshot"]["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["path_hex"] == "626164ff")
    );
    common::git(&f.work, &["add", "."]);
    common::git(&f.work, &["commit", "-m", "mixed"]);
    common::git(&f.work, &["mv", "file", "moved"]);
    let v = f.capture();
    assert!(!v["snapshot"]["units"].as_array().unwrap().is_empty());
}
#[test]
fn capture_rejects_filters_large_sensitive_and_index_flags_without_mutation() {
    let f = Fixture::new();
    f.edit();
    let before = source(&f);
    common::git(
        &f.work,
        &["config", "filter.bad.clean", "touch SHOULD_NOT_RUN"],
    );
    assert!(!f.run(&["untangle", "mixed"]).status.success());
    assert!(!f.work.join("SHOULD_NOT_RUN").exists());
    common::git(&f.work, &["config", "--unset", "filter.bad.clean"]);
    assert_eq!(source(&f), before);
    fs::write(f.work.join("big"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(!f.run(&["untangle", "mixed"]).status.success());
    fs::remove_file(f.work.join("big")).unwrap();
    fs::write(f.work.join(".env"), "SECRET").unwrap();
    assert!(!f.run(&["untangle", "mixed"]).status.success());
    fs::remove_file(f.work.join(".env")).unwrap();
    common::git(&f.work, &["update-index", "--assume-unchanged", "file"]);
    assert!(!f.run(&["untangle", "mixed"]).status.success());
}
#[test]
fn adjacent_edits_remain_single_hunk_and_deleted_files_survive() {
    let f = Fixture::new();
    let data = fs::read_to_string(f.work.join("file"))
        .unwrap()
        .replace("line 2\n", "a\n")
        .replace("line 3\n", "b\n");
    fs::write(f.work.join("file"), data).unwrap();
    assert_eq!(
        f.capture()["snapshot"]["units"].as_array().unwrap().len(),
        1
    );
    fs::remove_file(f.work.join("file")).unwrap();
    let v = f.capture();
    assert_eq!(v["snapshot"]["units"][0]["kind"], "whole_file");
}

#[test]
fn file_to_directory_replacement_warns_and_observes_ordered_dependency() {
    let f = Fixture::new();
    fs::remove_file(f.work.join("file")).unwrap();
    fs::create_dir(f.work.join("file")).unwrap();
    fs::write(f.work.join("file/child"), "nested\n").unwrap();
    let v = f.capture();
    let units = v["snapshot"]["units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    let delete = units.iter().find(|u| u["path"] == "file").unwrap()["id"]
        .as_str()
        .unwrap();
    let add = units.iter().find(|u| u["path"] == "file/child").unwrap()["id"]
        .as_str()
        .unwrap();
    let a = format!("{delete}=delete");
    let b = format!("{add}=add");
    let args = [
        "untangle",
        "mixed",
        "--json",
        "--snapshot",
        v["snapshot"]["id"].as_str().unwrap(),
        "--group",
        "delete",
        "--group",
        "add",
        "--assign",
        &a,
        "--assign",
        &b,
    ];
    let ordered = json(f.run(&args));
    assert_eq!(ordered["applications"][0]["independently_applicable"], true);
    assert_eq!(
        ordered["applications"][1]["independently_applicable"],
        false
    );
    assert_eq!(ordered["applications"][1]["sequence_applicable"], true);
    assert_eq!(
        ordered["applications"][1]["applicable_after_prefix"][0],
        "delete"
    );
    assert_eq!(ordered["sequence_applicable"], true);
    let mut reverse = args.to_vec();
    reverse.extend(["--order", "add", "--order", "delete"]);
    let reversed = json(f.run(&reverse));
    assert_eq!(reversed["sequence_applicable"], false);
    assert_eq!(
        reversed["applications"][1]["sequence_applicable"],
        Value::Null
    );
    assert_eq!(fs::read(f.work.join("file/child")).unwrap(), b"nested\n");
}

#[cfg(unix)]
#[test]
fn detected_capture_race_and_interrupted_git_clean_private_state() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit();
    let real = Command::new("which").arg("git").output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let bin = f._temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let marker = f._temp.path().join("once");
    let log = f._temp.path().join("private-dir");
    fn quote(p: &Path) -> String {
        format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"))
    }
    for interrupt in [false, true] {
        let before = source(&f);
        let script = format!(
            r#"#!/bin/sh
private=''
prev=''
for arg in "$@"; do
  if [ "$prev" = --git-dir ]; then private="$arg"; fi
  prev="$arg"
done
case " $* " in
 *' commit-tree '*)
   printf '%s' "$private" > {log}
   if [ {interrupt} = true ]; then kill -TERM $$; fi
   if [ ! -e {marker} ]; then printf changed > {target}; : > {marker}; fi
 ;;
esac
exec {real} "$@"
"#,
            log = quote(&log),
            marker = quote(&marker),
            target = quote(&f.work.join("file")),
            real = quote(Path::new(&real))
        );
        let wrapper = bin.join("git");
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&f.repo)
            .env("WEAVE_WORKSPACE_DIR", &f.root)
            .env("PATH", path)
            .args(["untangle", "mixed", "--json"])
            .fixture_output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        if !interrupt {
            assert!(String::from_utf8_lossy(&out.stderr).contains("Source changed"));
        }
        let private = fs::read_to_string(&log).unwrap();
        assert!(!Path::new(&private).exists(), "temporary repository leaked");
        let after = source(&f);
        assert_eq!(before.1, after.1);
        assert_eq!(before.2, after.2);
        if interrupt {
            assert_eq!(before, after);
        } else {
            assert_eq!(after.0, b"changed");
        }
    }
}

#[test]
fn tracked_output_limit_fails_closed() {
    let f = Fixture::new();
    fs::write(f.work.join("file"), vec![b'x'; 17 * 1024 * 1024]).unwrap();
    let before = source(&f);
    let out = f.run(&["untangle", "mixed", "--json"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("exceeds"));
    assert_eq!(source(&f), before);
}

fn export_args<'a>(
    v: &'a Value,
    a: &'a str,
    b: &'a str,
    kind: &'a str,
    destination: &'a str,
) -> Vec<&'a str> {
    vec![
        "untangle",
        "mixed",
        "--json",
        "--snapshot",
        v["snapshot"]["id"].as_str().unwrap(),
        "--base",
        v["snapshot"]["base_commit"].as_str().unwrap(),
        "--group",
        "one",
        "--group",
        "two",
        "--assign",
        a,
        "--assign",
        b,
        kind,
        destination,
    ]
}
#[test]
fn vineweaver_patch_and_independent_commit_exports_are_portable_and_preserve_source() {
    let f = Fixture::new();
    f.edit();
    let before = source(&f);
    let v = f.capture();
    let a = format!("{}=one", v["snapshot"]["units"][0]["id"].as_str().unwrap());
    let b = format!("{}=two", v["snapshot"]["units"][1]["id"].as_str().unwrap());
    for kind in ["--export-patches", "--export-commits"] {
        let destination = f._temp.path().join(if kind == "--export-patches" {
            "patches"
        } else {
            "commits"
        });
        let path = destination.to_str().unwrap();
        let args = export_args(&v, &a, &b, kind, path);
        let manifest = json(f.run(&args));
        assert_eq!(manifest["engine"], "vineweaver");
        assert_eq!(manifest["complete_reconstruction"], true);
        assert_eq!(manifest["captured_tree"], manifest["reconstructed_tree"]);
        let reject = f.run(&args);
        assert!(!reject.status.success());
        assert!(String::from_utf8_lossy(&reject.stderr).contains("already exists"));
        let clone = f._temp.path().join(if kind == "--export-patches" {
            "apply-patches"
        } else {
            "apply-commits"
        });
        common::git(
            f._temp.path(),
            &["clone", f.repo.to_str().unwrap(), clone.to_str().unwrap()],
        );
        for file in ["0001.patch", "0002.patch"] {
            common::git(
                &clone,
                &["apply", "--index", destination.join(file).to_str().unwrap()],
            );
        }
        assert_eq!(
            git_out(&clone, &["write-tree"]),
            format!("{}\n", manifest["reconstructed_tree"].as_str().unwrap()).as_bytes()
        );
        if kind == "--export-commits" {
            let portable = f._temp.path().join("portable");
            fs::create_dir(&portable).unwrap();
            common::init(&portable, "main");
            common::git(
                &portable,
                &[
                    "fetch",
                    destination.join("commits.bundle").to_str().unwrap(),
                    "refs/heads/vineweaver-0001:refs/heads/one",
                    "refs/heads/vineweaver-0002:refs/heads/two",
                ],
            );
            let base = v["snapshot"]["base_commit"].as_str().unwrap();
            for group in ["one", "two"] {
                assert_eq!(
                    git_out(&portable, &["rev-parse", &format!("{group}^")]),
                    format!("{base}\n").as_bytes()
                );
            }
            common::git(&portable, &["checkout", "one"]);
            assert!(
                fs::read_to_string(portable.join("file"))
                    .unwrap()
                    .contains("first")
            );
        }
        assert_eq!(source(&f), before);
    }
}
#[test]
fn partial_export_requires_acknowledgement_and_wrong_base_or_source_destination_is_refused() {
    let f = Fixture::new();
    f.edit();
    let v = f.capture();
    let dest = f._temp.path().join("partial");
    let a = format!("{}=one", v["snapshot"]["units"][0]["id"].as_str().unwrap());
    let mut args = vec![
        "untangle",
        "mixed",
        "--json",
        "--snapshot",
        v["snapshot"]["id"].as_str().unwrap(),
        "--base",
        v["snapshot"]["base_commit"].as_str().unwrap(),
        "--group",
        "one",
        "--assign",
        &a,
        "--export-patches",
        dest.to_str().unwrap(),
    ];
    assert!(!f.run(&args).status.success());
    assert!(!dest.exists());
    args.push("--allow-partial");
    let report = json(f.run(&args));
    assert_eq!(report["complete_reconstruction"], false);
    let protected = f.work.join("export");
    let pos = args.iter().position(|s| *s == "--export-patches").unwrap() + 1;
    args[pos] = protected.to_str().unwrap();
    assert!(!f.run(&args).status.success());
    assert!(!protected.exists());
    args[pos] = f.repo.to_str().unwrap();
    let pos = args.iter().position(|s| *s == "--base").unwrap() + 1;
    args[pos] = "wrong";
    assert!(!f.run(&args).status.success());
}

#[test]
fn dependent_groups_export_as_ordered_patches_but_not_independent_commits() {
    let f = Fixture::new();
    fs::remove_file(f.work.join("file")).unwrap();
    fs::create_dir(f.work.join("file")).unwrap();
    fs::write(f.work.join("file/child"), "child").unwrap();
    let v = f.capture();
    let units = v["snapshot"]["units"].as_array().unwrap();
    let a = format!(
        "{}=one",
        units.iter().find(|u| u["path"] == "file").unwrap()["id"]
            .as_str()
            .unwrap()
    );
    let b = format!(
        "{}=two",
        units.iter().find(|u| u["path"] == "file/child").unwrap()["id"]
            .as_str()
            .unwrap()
    );
    let destination = f._temp.path().join("dependent");
    let args = export_args(
        &v,
        &a,
        &b,
        "--export-patches",
        destination.to_str().unwrap(),
    );
    let manifest = json(f.run(&args));
    assert_eq!(manifest["entries"][1]["independent"], false);
    assert_eq!(manifest["complete_reconstruction"], true);
    let clone = f._temp.path().join("reconstruct");
    common::git(
        f._temp.path(),
        &["clone", f.repo.to_str().unwrap(), clone.to_str().unwrap()],
    );
    for file in ["0001.patch", "0002.patch"] {
        common::git(
            &clone,
            &["apply", "--index", destination.join(file).to_str().unwrap()],
        );
    }
    assert_eq!(fs::read(clone.join("file/child")).unwrap(), b"child");
    let dest = f._temp.path().join("refused");
    let args = export_args(&v, &a, &b, "--export-commits", dest.to_str().unwrap());
    let out = f.run(&args);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("independently"));
    assert!(!dest.exists());
}
#[cfg(unix)]
#[test]
fn binary_mode_unusual_names_reconstruct_through_export() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit();
    fs::set_permissions(f.work.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(f.work.join("binary\n[odd]"), b"\0\xffdata").unwrap();
    let v = f.capture();
    let units = v["snapshot"]["units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    let a = format!("{}=one", units[0]["id"].as_str().unwrap());
    let b = format!("{}=two", units[1]["id"].as_str().unwrap());
    let dest = f._temp.path().join("special");
    let report = json(f.run(&export_args(
        &v,
        &a,
        &b,
        "--export-patches",
        dest.to_str().unwrap(),
    )));
    let clone = f._temp.path().join("apply-special");
    common::git(
        f._temp.path(),
        &["clone", f.repo.to_str().unwrap(), clone.to_str().unwrap()],
    );
    common::git(
        &clone,
        &[
            "apply",
            "--index",
            dest.join("aggregate.patch").to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(clone.join("binary\n[odd]")).unwrap(),
        b"\0\xffdata"
    );
    assert_eq!(
        git_out(&clone, &["write-tree"]),
        format!("{}\n", report["captured_tree"].as_str().unwrap()).as_bytes()
    );
}
#[cfg(unix)]
#[test]
fn interrupted_bundle_and_source_drift_before_publication_leave_no_export_or_staging() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit();
    let real = Command::new("which").arg("git").output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let bin = f._temp.path().join("bin-export");
    fs::create_dir(&bin).unwrap();
    fn q(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
    }
    for mode in ["interrupted", "stale", "destination-race"] {
        fs::copy(f.repo.join("file"), f.work.join("file")).unwrap();
        f.edit();
        let v = f.capture();
        let before = source(&f);
        let destination = f._temp.path().join(mode);
        let action = match mode {
            "interrupted" => "kill -TERM $$".into(),
            "stale" => format!(
                "printf changed-before-publication > {}",
                q(&f.work.join("file"))
            ),
            _ => format!(
                "mkdir {}; printf existing > {}",
                q(&destination),
                q(&destination.join("keep"))
            ),
        };
        let a = format!("{}=one", v["snapshot"]["units"][0]["id"].as_str().unwrap());
        let b = format!("{}=two", v["snapshot"]["units"][1]["id"].as_str().unwrap());
        let wrapper = bin.join("git");
        fs::write(
            &wrapper,
            format!(
                r#"#!/bin/sh
case " $* " in
 *' bundle create '*)
  {action}
 ;;
esac
exec {real} "$@"
"#,
                real = q(Path::new(&real))
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let out = common::isolate(&mut Command::new(env!("CARGO_BIN_EXE_weave")))
            .current_dir(&f.repo)
            .env("WEAVE_WORKSPACE_DIR", &f.root)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .args(export_args(
                &v,
                &a,
                &b,
                "--export-commits",
                destination.to_str().unwrap(),
            ))
            .fixture_output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        if mode == "destination-race" {
            assert_eq!(fs::read(destination.join("keep")).unwrap(), b"existing");
        } else {
            assert!(!destination.exists());
        }
        assert!(!fs::read_dir(f._temp.path()).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".vineweaver-export-")
        }));
        let after = source(&f);
        assert_eq!(before.1, after.1);
        assert_eq!(before.2, after.2);
        if mode != "stale" {
            assert_eq!(before, after);
        } else {
            assert!(String::from_utf8_lossy(&out.stderr).contains("Source changed"));
        }
    }
}

#[cfg(unix)]
#[test]
fn vineweaver_pty_review_restores_terminal_on_quit_and_cancellation() {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    f.edit();
    let before = source(&f);
    for mode in ["review", "cancel", "cancel-export"] {
        let cancel_immediately = mode == "cancel";
        let export_cancel = mode == "cancel-export";
        let (mut master_fd, mut slave_fd) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 35,
            ws_col: 140,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master_fd,
                    &mut slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0
        );
        let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
        let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
        let mut initial = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), initial.as_mut_ptr()) },
            0
        );
        let initial = unsafe { initial.assume_init() };
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        let home = tempfile::tempdir().unwrap();
        let scratch = f._temp.path().join(format!("tmp-{mode}"));
        let destination = f._temp.path().join("cancelled-export");
        let marker = f._temp.path().join("bundle-entered");
        fs::create_dir(&scratch).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_weave"));
        common::isolate(&mut command)
            .current_dir(&f.repo)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("WEAVE_WORKSPACE_DIR", &f.root)
            .env("TMPDIR", &scratch)
            .env("TERM", "xterm-256color")
            .args(["untangle", "mixed", "--review"])
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        if export_cancel {
            use std::os::unix::fs::PermissionsExt;
            let real = Command::new("which").arg("git").output().unwrap();
            let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
            let bin = f._temp.path().join("bin-cancel-export");
            fs::create_dir(&bin).unwrap();
            fn q(p: &Path) -> String {
                format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"))
            }
            let wrapper = bin.join("git");
            fs::write(&wrapper,format!("#!/bin/sh\ncase \" $* \" in *' bundle create '*) : > {}; sleep 1 ;; esac\nexec {} \"$@\"\n",q(&marker),q(Path::new(&real)))).unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
            command.env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            );
        }
        let mut child = command.spawn().unwrap();
        let start = Instant::now();
        let mut output = Vec::new();
        let mut sent = false;
        let mut quit_sent = false;
        let mut export_started = false;
        loop {
            let mut buffer = [0; 16384];
            if let Ok(n) = master.read(&mut buffer) {
                output.extend_from_slice(&buffer[..n]);
            }
            let text = String::from_utf8_lossy(&output);
            if !sent
                && ((cancel_immediately && text.contains("Vineweaver"))
                    || text.contains("UNASSIGNED"))
            {
                if cancel_immediately {
                    master.write_all(b"\x03").unwrap();
                } else if export_cancel {
                    master.write_all(b"gfirst\r j v").unwrap();
                } else {
                    master.write_all(b"gfirst\r v").unwrap();
                }
                sent = true;
            }
            if sent && !cancel_immediately && !quit_sent && text.contains("complete=") {
                if export_cancel && !export_started {
                    master
                        .write_all(format!("c{}\rexport\r", destination.display()).as_bytes())
                        .unwrap();
                    export_started = true;
                } else if !export_cancel {
                    quit_sent = true;
                    master.write_all(b"q").unwrap();
                }
            }
            if export_cancel && marker.exists() && !quit_sent {
                master.write_all(b"\x03").unwrap();
                quit_sent = true;
            }
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{}", String::from_utf8_lossy(&output));
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "PTY review timed out: {}",
                    String::from_utf8_lossy(&output[output.len().saturating_sub(2000)..])
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sent);
        if export_cancel {
            assert!(export_started && marker.exists());
            assert!(!destination.exists());
            assert!(!fs::read_dir(f._temp.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".vineweaver-export-")
            }));
        }
        let mut after = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), after.as_mut_ptr()) },
            0
        );
        let after = unsafe { after.assume_init() };
        assert_eq!(
            (
                initial.c_iflag,
                initial.c_oflag,
                initial.c_cflag,
                initial.c_lflag,
                initial.c_cc
            ),
            (
                after.c_iflag,
                after.c_oflag,
                after.c_cflag,
                after.c_lflag,
                after.c_cc
            )
        );
        assert!(
            fs::read_dir(&scratch).unwrap().next().is_none(),
            "worker temporary repository leaked"
        );
        assert_eq!(source(&f), before);
    }
}
