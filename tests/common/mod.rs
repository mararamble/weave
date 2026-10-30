use std::process::Command;

/// Run a test subprocess with a disposable home and configuration directory.
pub trait FixtureOutput {
    fn fixture_output(&mut self) -> std::io::Result<std::process::Output>;
}
impl FixtureOutput for Command {
    fn fixture_output(&mut self) -> std::io::Result<std::process::Output> {
        let home = tempfile::tempdir()?;
        self.env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .output()
    }
}

/// Keep inherited Git routing and config overrides away from disposable fixtures.
pub fn isolate(command: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
}

pub fn git(dir: &std::path::Path, args: &[&str]) {
    let output = isolate(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .fixture_output()
        .expect("run Git in disposable repository");
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn init(dir: &std::path::Path, branch: &str) {
    git(dir, &["init", "-b", branch]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "user.email", "test@example.invalid"]);
}

/// Protect the documented version-1 JSON contract against accidental field drift.
pub fn assert_fields(value: &serde_json::Value, expected: &[&str]) {
    let object = value.as_object().expect("JSON object");
    let mut actual: Vec<_> = object.keys().map(String::as_str).collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}
