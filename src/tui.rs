//! Keyboard-driven terminal workspace review. Git operations run off the UI thread.
use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use serde_json::Value;
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

const DIFF_PAGE_SIZE: u64 = 64 * 1024;

// Never pass terminal control characters from repository data to the terminal.
pub(crate) fn safe(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<String>()
            } else {
                c.to_string()
            }
            .chars()
            .collect::<Vec<_>>()
        })
        .collect()
}
fn value_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn command(args: &[String]) -> Result<Value> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(args);
    let out = crate::git_process::bounded_output(&mut cmd, 32 * 1024 * 1024)?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}
#[derive(Clone)]
struct File {
    label: String,
    hex: String,
    previous_hex: Option<String>,
    untracked: bool,
    detail: String,
}
enum Message {
    List(u64, Result<Value>),
    Files(u64, Result<Value>),
    Review(u64, Result<Value>),
    Inspect(u64, Result<Value>),
    Text(u64, String, Result<String>),
    Diff(u64, Result<(std::fs::File, u64)>),
    Operation(u64, Result<String>),
}
#[derive(PartialEq, Eq)]
enum Focus {
    Workspaces,
    Files,
    Review,
}
enum Modal {
    NewName(String),
    NewBase {
        name: String,
        base: String,
    },
    IntegrateBranch(String),
    IntegrateValidation {
        branch: String,
        args: Vec<String>,
        value: String,
    },
    Palette(String),
    Confirm {
        description: String,
        args: Vec<String>,
    },
}

struct App {
    tx: Sender<Message>,
    rx: Receiver<Message>,
    generation: u64,
    workspaces: Vec<Value>,
    files: Vec<File>,
    files_owner: Option<String>,
    selected: usize,
    file_selected: usize,
    // Selection order is integration order; an empty list means all workspaces.
    marked: Vec<String>,
    focus: Focus,
    filter: String,
    editing: bool,
    review: String,
    diff_file: Option<(std::fs::File, u64, u64)>,
    diff_cancel: Arc<AtomicBool>,
    review_scroll: usize,
    message: String,
    help: bool,
    modal: Option<Modal>,
    mutating: bool,
    include_untracked_content: bool,
}
impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            generation: 0,
            workspaces: vec![],
            files: vec![],
            files_owner: None,
            selected: 0,
            file_selected: 0,
            marked: Vec::new(),
            focus: Focus::Workspaces,
            filter: String::new(),
            editing: false,
            review: String::new(),
            diff_file: None,
            diff_cancel: Arc::new(AtomicBool::new(false)),
            review_scroll: 0,
            message: "Loading workspaces…".into(),
            help: false,
            modal: None,
            mutating: false,
            include_untracked_content: false,
        }
    }
    fn visible(&self) -> Vec<usize> {
        self.workspaces
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                value_str(w, "name")
                    .to_lowercase()
                    .contains(&self.filter.to_lowercase())
            })
            .map(|(i, _)| i)
            .collect()
    }
    fn name(&self) -> Option<String> {
        self.visible()
            .get(self.selected)
            .and_then(|i| self.workspaces.get(*i))
            .map(|w| value_str(w, "name").to_owned())
    }
    fn start(&mut self, args: Vec<String>, kind: u8) {
        self.diff_cancel.store(true, Ordering::Release);
        if kind >= 2 {
            self.diff_file = None;
        }
        self.generation += 1;
        let id = self.generation;
        let tx = self.tx.clone();
        self.message = "Loading… (q quits immediately)".into();
        std::thread::spawn(move || {
            let result = command(&args);
            let _ = tx.send(match kind {
                0 => Message::List(id, result),
                1 => Message::Files(id, result),
                2 => Message::Review(id, result),
                _ => Message::Inspect(id, result),
            });
        });
    }
    fn operation(&mut self, args: Vec<String>) {
        self.diff_cancel.store(true, Ordering::Release);
        self.diff_file = None;
        self.generation += 1;
        let id = self.generation;
        let tx = self.tx.clone();
        self.mutating = true;
        self.message = "Applying operation… please wait".into();
        std::thread::spawn(move || {
            let result = (|| -> Result<String> {
                let mut cmd = Command::new(std::env::current_exe()?);
                cmd.args(&args);
                let out = crate::git_process::bounded_output(&mut cmd, 32 * 1024 * 1024)?;
                if !out.status.success() {
                    bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
                }
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
            })();
            let _ = tx.send(Message::Operation(id, result));
        });
    }
    fn removal_preview(&mut self) {
        let Some(name) = self.name() else { return };
        self.text_command(
            vec!["remove".into(), name, "--dry-run".into()],
            "Removal preview",
        );
    }
    fn text_command(&mut self, args: Vec<String>, label: &str) {
        self.diff_cancel.store(true, Ordering::Release);
        self.generation += 1;
        let id = self.generation;
        let tx = self.tx.clone();
        self.focus = Focus::Review;
        self.diff_file = None;
        self.review_scroll = 0;
        self.review = format!("Loading {label}…");
        self.message = format!("Loading {label}…");
        let label = label.to_owned();
        std::thread::spawn(move || {
            let result = (|| -> Result<String> {
                let mut cmd = Command::new(std::env::current_exe()?);
                cmd.args(args);
                let out = crate::git_process::bounded_output(&mut cmd, 32 * 1024 * 1024)?;
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !out.status.success() {
                    bail!("{}{}", stdout, stderr);
                }
                Ok(format!("{}{}", stdout, stderr))
            })();
            let _ = tx.send(Message::Text(id, label, result));
        });
    }
    fn selected_names(&self) -> Vec<String> {
        if self.marked.is_empty() {
            self.workspaces
                .iter()
                .map(|w| value_str(w, "name").to_owned())
                .collect()
        } else {
            self.marked.clone()
        }
    }
    fn toggle_mark(&mut self) {
        if let Some(name) = self.name() {
            if let Some(pos) = self.marked.iter().position(|n| n == &name) {
                self.marked.remove(pos);
            } else {
                self.marked.push(name);
            }
        }
    }
    fn integration_confirmation(
        &mut self,
        branch: String,
        validation: Vec<String>,
    ) -> Option<Modal> {
        let names = self.selected_names();
        if names.is_empty() {
            self.message = "No workspaces to integrate".into();
            return None;
        }
        let description = format!(
            "Create integration branch {} from {}?\nValidation: {}",
            safe(&branch),
            names
                .iter()
                .map(|n| safe(n))
                .collect::<Vec<_>>()
                .join(" → "),
            if validation.is_empty() {
                "none".into()
            } else {
                validation
                    .iter()
                    .map(|arg| format!("{:?}", safe(arg)))
                    .collect::<Vec<_>>()
                    .join(" ")
            },
        );
        let mut args = vec!["integrate".into(), "--branch".into(), branch];
        if self.include_untracked_content {
            args.push("--include-untracked-content".into());
        }
        args.extend(names);
        if !validation.is_empty() {
            args.push("--".into());
            args.extend(validation);
        }
        Some(Modal::Confirm { description, args })
    }
    fn modal_key(&mut self, key: KeyCode) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        if key == KeyCode::Esc {
            return;
        }
        if let Modal::IntegrateValidation {
            branch,
            args,
            value,
        } = &mut modal
        {
            match key {
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::F(2) if value.is_empty() => args.push(String::new()),
                KeyCode::Char(c) if !c.is_control() => value.push(c),
                KeyCode::Enter if value.is_empty() => {
                    self.modal = self.integration_confirmation(branch.clone(), args.clone());
                    return;
                }
                KeyCode::Enter => args.push(std::mem::take(value)),
                _ => {}
            }
            self.modal = Some(modal);
            return;
        }
        match &mut modal {
            Modal::Confirm { args, .. } if key == KeyCode::Char('y') => {
                self.operation(args.clone());
                return;
            }
            Modal::Confirm { .. } if key == KeyCode::Char('n') => return,
            Modal::NewName(value)
            | Modal::IntegrateBranch(value)
            | Modal::Palette(value)
            | Modal::NewBase { base: value, .. } => match key {
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) if !c.is_control() => value.push(c),
                KeyCode::Enter if !value.is_empty() => {
                    self.modal = match modal {
                        Modal::NewName(name) => Some(Modal::NewBase {
                            name,
                            base: "HEAD".into(),
                        }),
                        Modal::NewBase { name, base } => Some(Modal::Confirm {
                            description: format!(
                                "Create workspace {} from {}?",
                                safe(&name),
                                safe(&base)
                            ),
                            args: vec!["new".into(), name, "--from".into(), base],
                        }),
                        Modal::Palette(action) => match action.trim() {
                            "new" => Some(Modal::NewName(String::new())),
                            "remove" => self.name().map(|name| Modal::Confirm {
                                description: format!(
                                    "Remove workspace {}? Its branch is retained.",
                                    safe(&name)
                                ),
                                args: vec!["remove".into(), name, "--yes".into()],
                            }),
                            "integrate" => Some(Modal::IntegrateBranch(String::new())),
                            "removal-preview" => {
                                self.removal_preview();
                                None
                            }
                            "status" => {
                                self.start(vec!["status".into(), "--json".into()], 3);
                                None
                            }
                            "reconcile" => {
                                self.start(vec!["reconcile".into(), "--json".into()], 3);
                                None
                            }
                            "preview" => {
                                self.review(true);
                                None
                            }
                            "overlaps" => {
                                self.review(false);
                                None
                            }
                            "refresh" => {
                                self.refresh();
                                None
                            }
                            _ => {
                                self.message = format!("Unknown action: {}", safe(&action));
                                None
                            }
                        },
                        Modal::IntegrateBranch(branch) => Some(Modal::IntegrateValidation {
                            branch,
                            args: Vec::new(),
                            value: String::new(),
                        }),
                        _ => unreachable!(),
                    };
                    return;
                }
                _ => {}
            },
            _ => {}
        }
        self.modal = Some(modal);
    }
    fn refresh(&mut self) {
        self.start(vec!["list".into(), "--json".into()], 0);
    }
    fn load_files(&mut self) {
        if let Some(name) = self.name() {
            if self.files_owner.as_ref() != Some(&name) {
                self.files.clear();
                self.file_selected = 0;
            }
            self.files_owner = Some(name.clone());
            let mut args = vec!["diff".into(), name, "--json".into()];
            if self.include_untracked_content {
                args.push("--include-untracked-content".into());
            }
            self.start(args, 1);
        }
    }
    fn show_workspace(&mut self) {
        if let Some(item) = self
            .visible()
            .get(self.selected)
            .and_then(|i| self.workspaces.get(*i))
        {
            self.diff_cancel.store(true, Ordering::Release);
            self.generation += 1;
            self.diff_file = None;
            self.focus = Focus::Review;
            self.review_scroll = 0;
            self.review = serde_json::to_string_pretty(item).unwrap_or_default();
            self.message = "Selected workspace details".into();
        }
    }
    fn review(&mut self, plan: bool) {
        let mut names = self.selected_names();
        if names.is_empty() {
            self.message = "No workspaces to review".into();
            return;
        }
        let mut args = vec![
            if plan { "plan" } else { "overlaps" }.into(),
            "--json".into(),
        ];
        if plan && self.include_untracked_content {
            args.push("--include-untracked-content".into());
        }
        args.append(&mut names);
        self.focus = Focus::Review;
        self.diff_file = None;
        self.review = "Preparing review…".into();
        self.review_scroll = 0;
        self.start(args, 2);
    }
    fn diff(&mut self) {
        if self.name() != self.files_owner {
            self.message = "Open the selected workspace before reviewing its files".into();
            return;
        }
        let (Some(name), Some(file)) = (self.name(), self.files.get(self.file_selected).cloned())
        else {
            return;
        };
        self.focus = Focus::Review;
        self.review_scroll = 0;
        self.review.clear();
        self.diff_file = None;
        self.diff_cancel.store(true, Ordering::Release);
        self.diff_cancel = Arc::new(AtomicBool::new(false));
        self.generation += 1;
        let id = self.generation;
        if file.untracked {
            if file.detail.len() as u64 > DIFF_PAGE_SIZE {
                let result = (|| -> Result<std::fs::File> {
                    let mut spool = tempfile::tempfile()?;
                    spool.write_all(file.detail.as_bytes())?;
                    Ok(spool)
                })();
                match result {
                    Ok(spool) => {
                        self.diff_file = Some((spool, file.detail.len() as u64, 0));
                        self.diff_page(false);
                    }
                    Err(error) => {
                        self.review = format!("Cannot prepare untracked content: {error:#}");
                        self.message = "Untracked review failed".into();
                    }
                }
            } else {
                self.review = file.detail;
                self.message = "Untracked file details".into();
            }
            return;
        }
        self.message = "Streaming diff…".into();
        let tx = self.tx.clone();
        let include_untracked_content = self.include_untracked_content;
        let cancel = Arc::clone(&self.diff_cancel);
        std::thread::spawn(move || {
            let result = crate::git_process::cancellable(
                Arc::clone(&cancel),
                || -> Result<(std::fs::File, u64)> {
                    // Repeat the CLI collector's validation immediately before reading a patch.
                    let changeset = crate::changeset::collect(&name, include_untracked_content)?;
                    let record = crate::workspace::get(&name)?;
                    let path = path_from_hex(&file.hex)?;
                    let mut cmd =
                        crate::git_process::command_at(std::path::Path::new(&record.path));
                    cmd.args([
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--no-color",
                        "--binary",
                        "--no-renames",
                        &changeset.base_commit,
                        "--",
                    ])
                    .arg(path)
                    .env("GIT_LITERAL_PATHSPECS", "1")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                    if let Some(previous) = &file.previous_hex {
                        cmd.arg(path_from_hex(previous)?);
                    }
                    let mut spool =
                        tempfile::tempfile().context("Cannot create temporary diff file")?;
                    let mut child = crate::git_process::spawn(&mut cmd)?;
                    let mut out = child.stdout.take().context("Missing diff output")?;
                    let mut stderr = child
                        .stderr
                        .take()
                        .context("Missing Git diff error output")?;
                    let stderr_reader = std::thread::spawn(move || -> io::Result<Vec<u8>> {
                        let mut saved = Vec::new();
                        let mut buffer = [0; 8192];
                        loop {
                            let n = stderr.read(&mut buffer)?;
                            if n == 0 {
                                break;
                            }
                            let keep = n.min(65536usize.saturating_sub(saved.len()));
                            saved.extend_from_slice(&buffer[..keep]);
                        }
                        Ok(saved)
                    });
                    let mut total = 0_u64;
                    let mut buf = [0; 8192];
                    loop {
                        if cancel.load(Ordering::Acquire) {
                            let _ = child.kill();
                            let _ = child.wait();
                            bail!("Diff cancelled");
                        }
                        let n = match out.read(&mut buf) {
                            Ok(n) => n,
                            Err(error) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Err(error).context("Cannot read Git diff");
                            }
                        };
                        if n == 0 {
                            break;
                        }
                        if let Err(error) = spool.write_all(&buf[..n]) {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(error).context("Cannot write temporary diff file");
                        }
                        total += n as u64;
                    }
                    let status = child.wait()?;
                    let stderr = stderr_reader
                        .join()
                        .map_err(|_| anyhow::anyhow!("Git diff error reader failed"))??;
                    if !status.success() {
                        bail!(
                            "Git diff failed: {}",
                            String::from_utf8_lossy(&stderr).trim()
                        );
                    }
                    Ok((spool, total))
                },
            );
            let _ = tx.send(Message::Diff(id, result));
        });
    }
    fn diff_page(&mut self, next: bool) {
        let Some((file, len, offset)) = &mut self.diff_file else {
            return;
        };
        let next_offset = if next {
            offset
                .saturating_add(DIFF_PAGE_SIZE)
                .min(len.saturating_sub(1) / DIFF_PAGE_SIZE * DIFF_PAGE_SIZE)
        } else {
            offset.saturating_sub(DIFF_PAGE_SIZE)
        };
        let result = (|| -> Result<String> {
            // Extend the end of a page through UTF-8 continuation bytes and skip
            // those same bytes at the start of the next page, avoiding split glyphs.
            let end = next_offset.saturating_add(DIFF_PAGE_SIZE).min(*len);
            let mut start = next_offset;
            let mut byte = [0; 1];
            while start > 0 && start < end && start < next_offset.saturating_add(3) {
                file.seek(SeekFrom::Start(start))?;
                file.read_exact(&mut byte)?;
                if byte[0] & 0xc0 != 0x80 {
                    break;
                }
                start += 1;
            }
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            (&mut *file).take(end - start).read_to_end(&mut bytes)?;
            let mut extra = end;
            while extra < *len && extra < end.saturating_add(3) {
                file.seek(SeekFrom::Start(extra))?;
                file.read_exact(&mut byte)?;
                if byte[0] & 0xc0 != 0x80 {
                    break;
                }
                bytes.push(byte[0]);
                extra += 1;
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })();
        match result {
            Ok(text) => {
                *offset = next_offset;
                self.review = if text.is_empty() {
                    "No tracked patch for this file.".into()
                } else {
                    text
                };
                self.review_scroll = 0;
                self.message = format!(
                    "Diff bytes {}–{} of {}; [ / ] previous / next page",
                    next_offset.saturating_add(1).min(*len),
                    (next_offset + DIFF_PAGE_SIZE).min(*len),
                    len
                );
            }
            Err(error) => self.message = format!("Cannot read diff page: {error:#}"),
        }
    }
    fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Message::List(id, result) if id == self.generation => match result {
                    Ok(v) => {
                        let selected_name = self.name();
                        self.workspaces = v["workspaces"].as_array().cloned().unwrap_or_default();
                        self.marked.retain(|name| {
                            self.workspaces.iter().any(|w| value_str(w, "name") == name)
                        });
                        self.selected = selected_name
                            .and_then(|name| {
                                self.visible()
                                    .iter()
                                    .position(|&i| value_str(&self.workspaces[i], "name") == name)
                            })
                            .unwrap_or_else(|| {
                                self.selected.min(self.visible().len().saturating_sub(1))
                            });
                        if self.files_owner.as_ref().is_some_and(|name| {
                            !self.workspaces.iter().any(|w| value_str(w, "name") == name)
                        }) {
                            self.files.clear();
                            self.files_owner = None;
                            self.file_selected = 0;
                        }
                        self.message =
                            format!("{} workspaces; Enter opens files", self.workspaces.len());
                    }
                    Err(e) => {
                        self.focus = Focus::Review;
                        self.review_scroll = 0;
                        self.review = format!("Workspace listing failed:\n{e:#}");
                        self.message = "Workspace listing failed; inspect review".into();
                    }
                },
                Message::Files(id, result) if id == self.generation => match result {
                    Ok(v) => {
                        let selected = self
                            .files
                            .get(self.file_selected)
                            .map(|f| (f.hex.clone(), f.untracked));
                        self.files = make_files(&v);
                        self.file_selected = selected
                            .and_then(|(hex, untracked)| {
                                self.files
                                    .iter()
                                    .position(|f| f.hex == hex && f.untracked == untracked)
                            })
                            .unwrap_or_else(|| {
                                self.file_selected.min(self.files.len().saturating_sub(1))
                            });
                        self.focus = Focus::Files;
                        self.message =
                            format!("{} changed files; Enter opens diff", self.files.len());
                    }
                    Err(e) => {
                        self.focus = Focus::Review;
                        self.review_scroll = 0;
                        self.review = format!("Workspace changes failed:\n{e:#}");
                        self.message = "Workspace changes failed; inspect review".into();
                    }
                },
                Message::Inspect(id, result) if id == self.generation => {
                    self.focus = Focus::Review;
                    self.review_scroll = 0;
                    match result {
                        Ok(v) => {
                            self.review = serde_json::to_string_pretty(&v).unwrap_or_default();
                            self.message = "Inspection complete".into();
                        }
                        Err(e) => {
                            self.review = format!("{e:#}");
                            self.message = "Inspection failed".into();
                        }
                    }
                }
                Message::Text(id, label, result) if id == self.generation => match result {
                    Ok(output) => {
                        self.review = output;
                        self.message = format!("{label} complete");
                    }
                    Err(e) => {
                        self.review = format!("{e:#}");
                        self.message = format!("{label} failed");
                    }
                },
                Message::Review(id, result) if id == self.generation => match result {
                    Ok(v) => {
                        self.review = format_review(&v);
                        self.message = "Review complete".into();
                    }
                    Err(e) => {
                        self.review = format!("{e:#}");
                        self.message = "Review failed".into();
                    }
                },
                Message::Operation(id, result) if id == self.generation => {
                    self.mutating = false;
                    self.focus = Focus::Review;
                    match result {
                        Ok(output) => {
                            self.review = format!("Operation complete.\n{output}");
                            self.refresh();
                        }
                        Err(e) => {
                            self.review = format!("Operation failed:\n{e:#}");
                            self.message = "Operation failed; inspect review".into();
                        }
                    }
                }
                Message::Diff(id, result) if id == self.generation => match result {
                    Ok((file, len)) => {
                        self.diff_file = Some((file, len, 0));
                        self.diff_page(false);
                    }
                    Err(error) => {
                        self.review = format!("Diff failed: {error:#}");
                        self.message = "Diff failed".into();
                    }
                },
                _ => {}
            }
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.diff_cancel.store(true, Ordering::Release);
    }
}
fn make_files(v: &Value) -> Vec<File> {
    let mut files = Vec::new();
    for (key, label) in [
        ("aggregate_changes", "aggregate"),
        ("staged_changes", "staged"),
        ("unstaged_changes", "unstaged"),
        ("untracked_changes", "untracked"),
    ] {
        if let Some(changes) = v[key].as_array() {
            for c in changes {
                // The collector appends untracked records to aggregate_changes as well.
                // Give those their own row so a tracked deletion at the same path stays browsable.
                if key == "aggregate_changes"
                    && v["untracked_changes"]
                        .as_array()
                        .is_some_and(|items| items.contains(c))
                {
                    continue;
                }
                let hex = value_str(c, "path_hex").to_owned();
                if let Some(f) = files.iter_mut().find(|f: &&mut File| {
                    f.hex == hex && f.untracked == (key == "untracked_changes")
                }) {
                    if f.previous_hex.is_none() {
                        f.previous_hex = c["previous_path_hex"].as_str().map(str::to_owned);
                    }
                    if let Some(end) = f.label.rfind(']') {
                        f.label.insert_str(end, &format!(" / {label}"));
                    }
                    continue;
                }
                let detail = format!(
                    "Untracked: {}\nKind: {}\n{}",
                    safe(value_str(c, "path")),
                    value_str(c, "kind"),
                    untracked_content(c)
                );
                files.push(File {
                    label: format!(
                        "{}  {}  [{}]{}",
                        safe(value_str(c, "path")),
                        value_str(c, "kind"),
                        label,
                        if c["binary"] == true { " binary" } else { "" }
                    ),
                    hex,
                    previous_hex: c["previous_path_hex"].as_str().map(str::to_owned),
                    untracked: key == "untracked_changes",
                    detail,
                });
            }
        }
    }
    files
}
fn path_from_hex(hex: &str) -> Result<std::ffi::OsString> {
    if !hex.is_ascii() || !hex.len().is_multiple_of(2) {
        bail!("Invalid path encoding");
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        Ok(std::ffi::OsString::from(String::from_utf8(bytes)?))
    }
}
fn untracked_content(change: &Value) -> String {
    if let Some(reason) = change.get("incomplete").and_then(Value::as_str) {
        return format!("Content unavailable: {reason}");
    }
    let Some(hex) = change.get("content_hex").and_then(Value::as_str) else {
        return "Content unavailable".into();
    };
    if hex.len() % 2 != 0 || !hex.is_ascii() {
        return "Invalid captured content".into();
    }
    let decoded = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>();
    match decoded {
        Ok(bytes) if !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok() => {
            format!("Content:\n{}", String::from_utf8_lossy(&bytes))
        }
        Ok(_) => format!("Binary content (hex):\n{hex}"),
        Err(_) => "Invalid captured content".into(),
    }
}
fn format_review(v: &Value) -> String {
    if let Some(steps) = v["steps"].as_array() {
        let mut s = format!(
            "Integration preview: {}\nTarget base: {}\n",
            value_str(v, "status"),
            value_str(v, "target_base")
        );
        for step in steps {
            s.push_str(&format!(
                "\n{}: {}\nBase: {}\nHEAD: {}\nTree: {}\nChanges: {} staged, {} unstaged, {} untracked\n{}\n",
                value_str(step, "workspace"),
                value_str(step, "status"),
                value_str(step, "base_commit"),
                value_str(step, "head_commit"),
                value_str(step, "tree"),
                step["staged"].as_u64().unwrap_or_default(),
                step["unstaged"].as_u64().unwrap_or_default(),
                step["untracked"].as_u64().unwrap_or_default(),
                value_str(step, "conflict_details")
            ));
        }
        if let Some(w) = v["warnings"].as_array() {
            for x in w {
                s.push_str(&format!("\nWarning: {}", x.as_str().unwrap_or("")));
            }
        }
        s
    } else {
        let mut s =
            String::from("Overlap observations (not proof of a conflict or compatibility)\n");
        if let Some(items) = v["overlaps"].as_array() {
            if items.is_empty() {
                s.push_str("\nNo shared changed files.\n");
            }
            for o in items {
                s.push_str(&format!(
                    "\n{}: {} — {}{}\n",
                    value_str(o, "path"),
                    o["workspaces"]
                        .as_array()
                        .map(|a| a
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" / "))
                        .unwrap_or_default(),
                    value_str(o, "classification"),
                    o["incomplete"]
                        .as_str()
                        .map(|s| format!(" (incomplete: {s})"))
                        .unwrap_or_default()
                ));
            }
        }
        s
    }
}
pub(crate) struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show, LeaveAlternateScreen);
    }
}
pub(crate) fn run(include_untracked_content: bool) -> Result<()> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("The TUI requires an interactive terminal on stdin and stdout");
    }
    // Validate repository before changing the terminal.
    crate::git_path("--show-toplevel").context("Run the TUI inside a Git worktree")?;
    let mut app = App::new();
    app.include_untracked_content = include_untracked_content;
    app.refresh();
    loop {
        enable_raw_mode()?;
        let guard = TerminalGuard;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
        let mut vineweaver = None;
        loop {
            app.receive();
            terminal.draw(|f| draw(f, &app))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(k) = event::read()?
                && k.kind != KeyEventKind::Release
            {
                if app.mutating {
                    continue;
                }
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if app.modal.is_some() {
                    if !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    {
                        app.modal_key(k.code);
                    }
                    continue;
                }
                if app.editing {
                    if k.modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    {
                        continue;
                    }
                    match k.code {
                        KeyCode::Esc | KeyCode::Enter => app.editing = false,
                        KeyCode::Backspace => {
                            app.filter.pop();
                            app.selected = 0;
                        }
                        KeyCode::Char(c) if !c.is_control() => {
                            app.filter.push(c);
                            app.selected = 0;
                        }
                        _ => {}
                    }
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') => {
                        if app.help {
                            app.help = false;
                        } else if app.focus == Focus::Workspaces {
                            break;
                        } else {
                            app.focus = Focus::Workspaces;
                        }
                    }
                    KeyCode::Char(':') => app.modal = Some(Modal::Palette(String::new())),
                    KeyCode::Char('n') => app.modal = Some(Modal::NewName(String::new())),
                    KeyCode::Char('d') => {
                        if let Some(name) = app.name() {
                            app.modal = Some(Modal::Confirm {
                                description: format!(
                                    "Remove workspace {}? Its branch is retained.",
                                    safe(&name)
                                ),
                                args: vec!["remove".into(), name, "--yes".into()],
                            });
                        }
                    }
                    KeyCode::Char('D') => app.removal_preview(),
                    KeyCode::Char('t') => {
                        if let Some(name) = app.name() {
                            app.text_command(
                                {
                                    let mut args = vec!["diff".into(), name, "--stat".into()];
                                    if app.include_untracked_content {
                                        args.push("--include-untracked-content".into());
                                    }
                                    args
                                },
                                "Diff statistics",
                            );
                        }
                    }
                    KeyCode::Char('J') => {
                        if let Some(name) = app.name() {
                            let mut args = vec!["diff".into(), name, "--json".into()];
                            if app.include_untracked_content {
                                args.push("--include-untracked-content".into());
                            }
                            app.start(args, 3);
                        }
                    }
                    KeyCode::Char('i') => app.modal = Some(Modal::IntegrateBranch(String::new())),
                    KeyCode::Char('u') => {
                        if let Some(name) = app.name() {
                            vineweaver = Some(name);
                            break;
                        }
                    }
                    KeyCode::Char('?') => app.help = !app.help,
                    KeyCode::Char('/') => {
                        app.editing = true;
                        app.focus = Focus::Workspaces;
                    }
                    KeyCode::Char('r') => app.refresh(),
                    KeyCode::Char('s') => app.start(vec!["status".into(), "--json".into()], 3),
                    KeyCode::Char('c') => app.start(vec!["reconcile".into(), "--json".into()], 3),
                    KeyCode::Char('w') => app.show_workspace(),
                    KeyCode::Char('o') => app.review(false),
                    KeyCode::Char('p') => app.review(true),
                    KeyCode::Char(' ') if app.focus == Focus::Workspaces => app.toggle_mark(),
                    KeyCode::Tab | KeyCode::Char('l') => {
                        app.focus = match app.focus {
                            Focus::Workspaces => Focus::Files,
                            Focus::Files => Focus::Review,
                            Focus::Review => Focus::Workspaces,
                        };
                    }
                    KeyCode::Char('h') | KeyCode::BackTab => {
                        app.focus = match app.focus {
                            Focus::Workspaces => Focus::Review,
                            Focus::Files => Focus::Workspaces,
                            Focus::Review => Focus::Files,
                        };
                    }
                    KeyCode::Esc => {
                        app.help = false;
                        app.focus = Focus::Workspaces;
                    }
                    KeyCode::Enter => match app.focus {
                        Focus::Workspaces => app.load_files(),
                        Focus::Files => app.diff(),
                        Focus::Review => {}
                    },
                    KeyCode::Down | KeyCode::Char('j') => match app.focus {
                        Focus::Workspaces => {
                            app.selected =
                                (app.selected + 1).min(app.visible().len().saturating_sub(1))
                        }
                        Focus::Files => {
                            app.file_selected =
                                (app.file_selected + 1).min(app.files.len().saturating_sub(1))
                        }
                        Focus::Review => app.review_scroll = app.review_scroll.saturating_add(1),
                    },
                    KeyCode::Up | KeyCode::Char('k') => match app.focus {
                        Focus::Workspaces => app.selected = app.selected.saturating_sub(1),
                        Focus::Files => app.file_selected = app.file_selected.saturating_sub(1),
                        Focus::Review => app.review_scroll = app.review_scroll.saturating_sub(1),
                    },
                    KeyCode::PageDown if app.focus == Focus::Review => {
                        app.review_scroll = app.review_scroll.saturating_add(20)
                    }
                    KeyCode::PageUp if app.focus == Focus::Review => {
                        app.review_scroll = app.review_scroll.saturating_sub(20)
                    }
                    KeyCode::Char('[') if app.focus == Focus::Review => app.diff_page(false),
                    KeyCode::Char(']') if app.focus == Focus::Review => app.diff_page(true),
                    _ => {}
                }
            }
        }
        drop(terminal);
        drop(guard);
        if let Some(name) = vineweaver {
            if let Err(error) = crate::untangle_review::run_name(name, include_untracked_content) {
                app.focus = Focus::Review;
                app.review = format!("Vineweaver failed: {error:#}");
                app.review_scroll = 0;
                app.message = "Vineweaver failed; inspect review".into();
            }
            continue;
        }
        break;
    }
    Ok(())
}
pub(crate) fn window(selected: usize, height: u16, len: usize) -> std::ops::Range<usize> {
    let h = height.saturating_sub(2) as usize;
    let start = selected.saturating_sub(h.saturating_sub(1)).min(len);
    start..(start + h).min(len)
}
fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
    let cols = if area.width < 80 {
        let mut cols = [Rect::default(); 3];
        let active = if app.modal.is_some() || app.help {
            2
        } else {
            match app.focus {
                Focus::Workspaces => 0,
                Focus::Files => 1,
                Focus::Review => 2,
            }
        };
        cols[active] = rows[0];
        cols
    } else {
        let layout = Layout::horizontal([
            Constraint::Percentage(28),
            Constraint::Percentage(30),
            Constraint::Percentage(42),
        ])
        .split(rows[0]);
        [layout[0], layout[1], layout[2]]
    };
    let visible = app.visible();
    let ws: Vec<ListItem> = window(app.selected, cols[0].height, visible.len())
        .map(|pos| {
            let w = &app.workspaces[visible[pos]];
            let name = value_str(w, "name");
            let state = if w["exists"] != true {
                "missing"
            } else if w["dirty"] == true {
                "dirty"
            } else {
                "clean"
            };
            let text = format!(
                "{} {} [{}] {}",
                app.marked
                    .iter()
                    .position(|n| n == name)
                    .map(|i| format!("{}", i + 1))
                    .unwrap_or_else(|| " ".into()),
                safe(name),
                state,
                safe(value_str(w, "state"))
            );
            ListItem::new(text).style(if app.focus == Focus::Workspaces && pos == app.selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            })
        })
        .collect();
    f.render_widget(
        List::new(ws).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if app.focus == Focus::Workspaces {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(format!(
                    " Workspaces /{}{} ",
                    safe(&app.filter),
                    if app.include_untracked_content {
                        " [untracked content enabled]"
                    } else {
                        ""
                    }
                )),
        ),
        cols[0],
    );
    let files: Vec<ListItem> = window(app.file_selected, cols[1].height, app.files.len())
        .map(|i| {
            ListItem::new(app.files[i].label.clone()).style(
                if app.focus == Focus::Files && i == app.file_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default()
                },
            )
        })
        .collect();
    f.render_widget(
        List::new(files).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if app.focus == Focus::Files {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(format!(
                    "Files: {}",
                    safe(app.files_owner.as_deref().unwrap_or("none"))
                )),
        ),
        cols[1],
    );
    let content = if let Some(modal) = &app.modal {
        match modal {
            Modal::NewName(value) => format!(
                "New workspace name: {}\nEnter next · Esc cancel",
                safe(value)
            ),
            Modal::NewBase { name, base } => format!(
                "Create {} from ref: {}\nEnter next · Esc cancel",
                safe(name),
                safe(base)
            ),
            Modal::Palette(value) => format!(
                "Action: {}\nnew · remove · removal-preview · integrate · status · reconcile · preview · overlaps · refresh\nEnter run · Esc cancel",
                safe(value)
            ),
            Modal::IntegrateBranch(value) => format!(
                "Integration branch: {}\nEnter next · Esc cancel",
                safe(value)
            ),
            Modal::IntegrateValidation {
                branch,
                args,
                value,
            } => format!(
                "Integration branch: {}\nValidation argv: {}\nNext argument: {}\nEnter adds an argument; empty Enter continues. F2 adds an empty argument. Esc cancels.\nThe first argument is the executable; it runs without a shell.",
                safe(branch),
                args.iter()
                    .map(|arg| format!("{:?}", safe(arg)))
                    .collect::<Vec<_>>()
                    .join(" "),
                safe(value)
            ),
            Modal::Confirm { description, .. } => {
                format!("{}\n\ny confirm · n/Esc cancel", description)
            }
        }
    } else if app.help {
        "q back/quit · Ctrl-C quit · ↑/↓ or j/k move · h/l or Tab switch pane\nEnter open workspace/file · Esc workspace pane\n/ filter workspaces · Space mark in integration order\nn new workspace · D removal preview · d remove · i integrate\ns repository status · w workspace details · c reconcile · : action palette\no overlap review · p integration preview · r refresh\nt diff statistics · J diff JSON · [ / ] diff page\nu open selected workspace in Vineweaver Untangle review\nPageUp/PageDown scroll review\nReviews and integration use marks in order, or all if none marked.".to_string()
    } else {
        app.review.lines().map(safe).collect::<Vec<_>>().join("\n")
    };
    f.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if app.modal.is_some() {
                        "Action"
                    } else if app.help {
                        "Help"
                    } else {
                        "Review"
                    }),
            )
            .wrap(Wrap { trim: false })
            .scroll((
                if app.help || app.modal.is_some() {
                    0
                } else {
                    app.review_scroll.min(u16::MAX as usize) as u16
                },
                0,
            )),
        cols[2],
    );
    f.render_widget(
        Paragraph::new(format!(
            "{}\n{}",
            safe(&app.message),
            if app.editing {
                "Filter: type, Enter to finish"
            } else {
                "? help  n new  d remove  i integrate  s status  c reconcile  / filter  o overlaps  p preview  u Untangle  q back/quit"
            }
        )),
        rows[1],
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marks_preserve_user_order_and_confirmation_uses_exact_argv() {
        let mut app = App::new();
        app.workspaces = vec![
            serde_json::json!({"name":"first"}),
            serde_json::json!({"name":"second"}),
        ];
        app.selected = 1;
        app.toggle_mark();
        app.selected = 0;
        app.toggle_mark();
        assert_eq!(app.selected_names(), ["second", "first"]);
        app.modal = Some(Modal::IntegrateBranch("review".into()));
        app.modal_key(KeyCode::Enter);
        for c in "cargo".chars() {
            app.modal_key(KeyCode::Char(c));
        }
        app.modal_key(KeyCode::Enter);
        for c in "argument with spaces".chars() {
            app.modal_key(KeyCode::Char(c));
        }
        app.modal_key(KeyCode::Enter);
        app.modal_key(KeyCode::F(2));
        app.modal_key(KeyCode::Enter);
        assert!(
            matches!(app.modal, Some(Modal::Confirm { ref args, .. }) if args == &["integrate", "--branch", "review", "second", "first", "--", "cargo", "argument with spaces", ""])
        );
        app.modal_key(KeyCode::Esc);
        assert!(!app.mutating);
    }
    #[test]
    fn captured_untracked_content_is_available_without_running_a_filter() {
        assert_eq!(
            untracked_content(&serde_json::json!({"content_hex":"610a62"})),
            "Content:\na\nb"
        );
        assert_eq!(
            untracked_content(&serde_json::json!({"content_hex":"00ff"})),
            "Binary content (hex):\n00ff"
        );
        assert_eq!(
            untracked_content(&serde_json::json!({"incomplete":"sensitive path excluded"})),
            "Content unavailable: sensitive path excluded"
        );
        assert_eq!(
            untracked_content(&serde_json::json!({"content_hex":"0"})),
            "Invalid captured content"
        );
    }
    #[test]
    fn aggregate_untracked_entries_keep_captured_content() {
        let untracked = serde_json::json!({"path":"new", "path_hex":"6e6577", "kind":"added", "content_hex":"6162"});
        let tracked = serde_json::json!({"path":"new", "path_hex":"6e6577", "kind":"deleted"});
        let files = make_files(
            &serde_json::json!({"aggregate_changes":[tracked, untracked.clone()], "untracked_changes":[untracked]}),
        );
        assert_eq!(files.len(), 2);
        assert!(!files[0].untracked);
        assert!(files[1].untracked);
        assert!(files[1].detail.contains("Content:\nab"));
    }
    #[test]
    fn narrow_layout_displays_modal_and_focused_pane() {
        let mut app = App::new();
        app.workspaces = vec![serde_json::json!({"name":"example", "exists":true})];
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("example"));
        app.modal = Some(Modal::NewName("task".into()));
        app.review_scroll = 50;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("New workspace name: task"));
    }
    #[test]
    fn large_diff_pages_are_read_without_truncating_the_capture() {
        let mut file = tempfile::tempfile().unwrap();
        let mut data = vec![b'a'; (DIFF_PAGE_SIZE * 33) as usize];
        data.extend_from_slice(b"last page");
        file.write_all(&data).unwrap();
        let mut app = App::new();
        app.diff_file = Some((file, data.len() as u64, 0));
        app.diff_page(false);
        assert_eq!(app.review.len(), DIFF_PAGE_SIZE as usize);
        for _ in 0..33 {
            app.diff_page(true);
        }
        assert_eq!(app.review, "last page");
        app.diff_page(true);
        assert_eq!(app.review, "last page");
        app.diff_page(false);
        assert_eq!(app.review.len(), DIFF_PAGE_SIZE as usize);
    }
    #[test]
    fn diff_pages_preserve_utf8_across_boundaries() {
        let mut file = tempfile::tempfile().unwrap();
        let mut bytes = vec![b'a'; DIFF_PAGE_SIZE as usize - 1];
        bytes.extend_from_slice("€end".as_bytes());
        file.write_all(&bytes).unwrap();
        let mut app = App::new();
        app.diff_file = Some((file, bytes.len() as u64, 0));
        app.diff_page(false);
        assert!(app.review.ends_with('€'));
        app.diff_page(true);
        assert_eq!(app.review, "end");
    }
    #[test]
    fn refresh_preserves_selection_by_name_and_removes_stale_marks() {
        let mut app = App::new();
        app.workspaces = vec![
            serde_json::json!({"name":"a"}),
            serde_json::json!({"name":"b"}),
        ];
        app.selected = 1;
        app.marked = vec!["a".into(), "b".into()];
        app.tx
            .send(Message::List(
                0,
                Ok(serde_json::json!({"workspaces":[{"name":"b"},{"name":"c"}]})),
            ))
            .unwrap();
        app.receive();
        assert_eq!(app.name().as_deref(), Some("b"));
        assert_eq!(app.marked, ["b"]);
    }
    #[test]
    fn files_refresh_preserves_selected_path_and_errors_remain_readable() {
        let mut app = App::new();
        app.files = make_files(
            &serde_json::json!({"aggregate_changes":[{"path":"a", "path_hex":"61"}, {"path":"b", "path_hex":"62"}]}),
        );
        app.file_selected = 1;
        app.tx.send(Message::Files(0, Ok(serde_json::json!({"aggregate_changes":[{"path":"b", "path_hex":"62"}, {"path":"c", "path_hex":"63"}]})))).unwrap();
        app.receive();
        assert_eq!(app.files[app.file_selected].hex, "62");
        app.tx
            .send(Message::Files(0, Err(anyhow::anyhow!("detailed cause"))))
            .unwrap();
        app.receive();
        assert!(app.focus == Focus::Review);
        assert!(app.review.contains("detailed cause"));
    }
    #[test]
    fn creation_modal_requires_confirmation_and_cancel_is_recoverable() {
        let mut app = App::new();
        app.modal = Some(Modal::NewName(String::new()));
        for c in "task".chars() {
            app.modal_key(KeyCode::Char(c));
        }
        app.modal_key(KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::NewBase { .. })));
        app.modal_key(KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
        app.modal_key(KeyCode::Esc);
        assert!(app.modal.is_none());
        assert!(!app.mutating);
    }
    #[test]
    fn palette_opens_requested_action() {
        let mut app = App::new();
        app.modal = Some(Modal::Palette(String::new()));
        for c in "new".chars() {
            app.modal_key(KeyCode::Char(c));
        }
        app.modal_key(KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::NewName(ref value)) if value.is_empty()));
    }
    #[test]
    fn input_keys_are_text_inside_modal() {
        let mut app = App::new();
        app.modal = Some(Modal::NewName(String::new()));
        for c in "qndip".chars() {
            app.modal_key(KeyCode::Char(c));
        }
        assert!(matches!(app.modal, Some(Modal::NewName(ref s)) if s == "qndip"));
    }
    #[test]
    fn review_wraps_and_scrolls_by_displayed_lines() {
        let mut app = App::new();
        app.review =
            "start 123456789012345678901234567890123456789012345678901234567890 end".into();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("start"));
        assert!(!screen.contains("end"));
        app.review_scroll = 1;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("end"));
        assert!(!screen.contains("start"));
    }
    #[test]
    fn escapes_controls_and_windows_large_lists() {
        assert_eq!(safe("x\x1b\ny"), "x\\u{1b}\\ny");
        assert_eq!(window(9999, 10, 10000), 9992..10000);
    }
    #[test]
    fn file_statuses_are_combined() {
        let v = serde_json::json!({"aggregate_changes":[{"path":"a\nb","path_hex":"610a62","kind":"modified"}], "staged_changes":[{"path_hex":"610a62"}]});
        let files = make_files(&v);
        assert_eq!(files.len(), 1);
        assert!(files[0].label.contains("aggregate / staged"));
        assert!(files[0].label.contains("a\\nb"));
    }
}
