#![cfg(unix)]
use std::process::Command;

#[test]
fn remote_metadata_preservation_and_refusals() {
    let output = Command::new("python3")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["-I", "-B", "tests/remote_probe_tests.py"])
        .output()
        .expect("Python 3.9+ is required to exercise the remote helper");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn profile_cli_preserves_registry_and_runs_outside_git() {
    let temporary = tempfile::tempdir().unwrap();
    let registry = temporary.path().join("private/remotes.json");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_weave"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir(temporary.path())
            .args(["remote", "--registry"])
            .arg(&registry)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(
        !run(&["add", "bad", "-option", "--root=/safe"])
            .status
            .success()
    );
    assert!(!registry.exists());
    assert!(
        run(&["add", "first", "alias", "--root=/safe/./projects/"])
            .status
            .success()
    );
    let before = std::fs::read(&registry).unwrap();
    assert!(
        !run(&["add", "first", "other", "--root=/elsewhere"])
            .status
            .success()
    );
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    let show = run(&["show", "first"]);
    assert!(show.status.success());
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["profile"]["roots"][0], "/safe/projects");
    assert!(
        run(&["add", "second", "alias", "--root=/safe"])
            .status
            .success()
    );
    assert!(run(&["remove", "first"]).status.success());
    let list = run(&["list"]);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["profiles"].as_object().unwrap().len(), 1);
    assert!(value["profiles"].get("second").is_some());
}
