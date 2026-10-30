//! Read-only terminal review. Potentially expensive Git operations run off the UI thread.
use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use serde_json::Value;
use std::{
    collections::HashSet,
    io::{self, Read},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;

// Never pass terminal control characters from repository data to the terminal.
fn safe(s: &str) -> String {
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
    untracked: bool,
    detail: String,
}
enum Message {
    List(u64, Result<Value>),
    Files(u64, Result<Value>),
    Review(u64, Result<Value>),
    Chunk(u64, Vec<u8>),
    Finished(u64, Result<()>),
}
#[derive(PartialEq, Eq)]
enum Focus {
    Workspaces,
    Files,
    Review,
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
    marked: HashSet<String>,
    focus: Focus,
    filter: String,
    editing: bool,
    review: String,
    review_scroll: usize,
    message: String,
    help: bool,
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
            marked: HashSet::new(),
            focus: Focus::Workspaces,
            filter: String::new(),
            editing: false,
            review: String::new(),
            review_scroll: 0,
            message: "Loading workspaces…".into(),
            help: false,
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
        self.generation += 1;
        let id = self.generation;
        let tx = self.tx.clone();
        self.message = "Loading… (q quits immediately)".into();
        std::thread::spawn(move || {
            let result = command(&args);
            let _ = tx.send(match kind {
                0 => Message::List(id, result),
                1 => Message::Files(id, result),
                _ => Message::Review(id, result),
            });
        });
    }
    fn refresh(&mut self) {
        self.start(vec!["list".into(), "--json".into()], 0);
    }
    fn load_files(&mut self) {
        if let Some(name) = self.name() {
            self.files.clear();
            self.file_selected = 0;
            self.files_owner = Some(name.clone());
            self.start(vec!["diff".into(), name, "--json".into()], 1);
        }
    }
    fn review(&mut self, plan: bool) {
        let mut names: Vec<String> = self
            .workspaces
            .iter()
            .map(|w| value_str(w, "name").to_owned())
            .filter(|n| self.marked.is_empty() || self.marked.contains(n))
            .collect();
        if names.is_empty() {
            self.message = "No workspaces to review".into();
            return;
        }
        let mut args = vec![
            if plan { "plan" } else { "overlaps" }.into(),
            "--json".into(),
        ];
        args.append(&mut names);
        self.focus = Focus::Review;
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
        self.generation += 1;
        let id = self.generation;
        if file.untracked {
            self.review = file.detail;
            self.message =
                "Untracked content is summarized; use the CLI for its captured bytes".into();
            return;
        }
        self.message = "Streaming diff…".into();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let record = crate::workspace::get(&name)?;
                let bytes: Vec<u8> = (0..file.hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&file.hex[i..i + 2], 16))
                    .collect::<std::result::Result<_, _>>()?;
                #[cfg(unix)]
                let path = {
                    use std::os::unix::ffi::OsStringExt;
                    std::ffi::OsString::from_vec(bytes)
                };
                #[cfg(not(unix))]
                let path = std::ffi::OsString::from(String::from_utf8(bytes)?);
                let mut cmd = crate::git_process::command_at(std::path::Path::new(&record.path));
                cmd.args([
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--no-renames",
                    &record.base_commit,
                    "--",
                ])
                .arg(path)
                .env("GIT_LITERAL_PATHSPECS", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
                let mut child = crate::git_process::spawn(&mut cmd)?;
                let mut out = child.stdout.take().context("Missing diff output")?;
                let mut total = 0;
                let mut buf = [0; 8192];
                loop {
                    let n = out.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    let keep = n.min(OUTPUT_LIMIT - total);
                    if keep > 0 && tx.send(Message::Chunk(id, buf[..keep].to_vec())).is_err() {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(());
                    }
                    total += keep;
                    if total == OUTPUT_LIMIT {
                        let _ = child.kill();
                        let _ = child.wait();
                        bail!("Diff truncated at 2 MiB; use the CLI for complete output");
                    }
                }
                if !child.wait()?.success() {
                    bail!("Git diff failed; inspect with the CLI");
                }
                Ok(())
            })();
            let _ = tx.send(Message::Finished(id, result));
        });
    }
    fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Message::List(id, result) if id == self.generation => match result {
                    Ok(v) => {
                        self.workspaces = v["workspaces"].as_array().cloned().unwrap_or_default();
                        self.selected = 0;
                        self.files.clear();
                        self.message =
                            format!("{} workspaces; Enter opens files", self.workspaces.len());
                    }
                    Err(e) => self.message = format!("{e:#}"),
                },
                Message::Files(id, result) if id == self.generation => match result {
                    Ok(v) => {
                        self.files = make_files(&v);
                        self.focus = Focus::Files;
                        self.message =
                            format!("{} changed files; Enter opens diff", self.files.len());
                    }
                    Err(e) => self.message = format!("{e:#}"),
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
                Message::Chunk(id, b) if id == self.generation => {
                    self.review.push_str(&String::from_utf8_lossy(&b));
                }
                Message::Finished(id, result) if id == self.generation => {
                    self.message = match result {
                        Ok(()) => "Diff complete".into(),
                        Err(e) => format!("{e:#}"),
                    };
                    if self.review.is_empty() {
                        self.review = "No tracked patch for this file (it may be untracked or unchanged against the base).".into();
                    }
                }
                _ => {}
            }
        }
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
                let hex = value_str(c, "path_hex").to_owned();
                if let Some(f) = files.iter_mut().find(|f: &&mut File| f.hex == hex) {
                    if let Some(end) = f.label.rfind(']') {
                        f.label.insert_str(end, &format!(" / {label}"));
                    }
                    continue;
                }
                let detail = format!(
                    "Untracked: {}\nKind: {}\n{}",
                    safe(value_str(c, "path")),
                    value_str(c, "kind"),
                    c.get("incomplete").and_then(Value::as_str).unwrap_or(
                        "Content captured by the changeset collector; use weave diff for bytes."
                    )
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
                    untracked: key == "untracked_changes",
                    detail,
                });
            }
        }
    }
    files
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
                "\n{}: {}\n{}\n",
                value_str(step, "workspace"),
                value_str(step, "status"),
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
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
pub(crate) fn run() -> Result<()> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("The TUI requires an interactive terminal on stdin and stdout");
    }
    // Validate repository before changing the terminal.
    crate::git_path("--show-toplevel").context("Run the TUI inside a Git worktree")?;
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new();
    app.refresh();
    loop {
        app.receive();
        terminal.draw(|f| draw(f, &app))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(k) = event::read()?
            && k.kind != KeyEventKind::Release
        {
            if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }
            if app.editing {
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
                KeyCode::Char('q') => break,
                KeyCode::Char('?') => app.help = !app.help,
                KeyCode::Char('/') => {
                    app.editing = true;
                    app.focus = Focus::Workspaces;
                }
                KeyCode::Char('r') => app.refresh(),
                KeyCode::Char('o') => app.review(false),
                KeyCode::Char('p') => app.review(true),
                KeyCode::Char(' ') if app.focus == Focus::Workspaces => {
                    if let Some(n) = app.name()
                        && !app.marked.remove(&n)
                    {
                        app.marked.insert(n);
                    }
                }
                KeyCode::Tab => {
                    app.focus = match app.focus {
                        Focus::Workspaces => Focus::Files,
                        Focus::Files => Focus::Review,
                        Focus::Review => Focus::Workspaces,
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
                        app.selected = (app.selected + 1).min(app.visible().len().saturating_sub(1))
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
                _ => {}
            }
        }
    }
    Ok(())
}
fn window(selected: usize, height: u16, len: usize) -> std::ops::Range<usize> {
    let h = height.saturating_sub(2) as usize;
    let start = selected.saturating_sub(h.saturating_sub(1)).min(len);
    start..(start + h).min(len)
}
fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
    let cols = Layout::horizontal([
        Constraint::Percentage(28),
        Constraint::Percentage(30),
        Constraint::Percentage(42),
    ])
    .split(rows[0]);
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
                if app.marked.contains(name) { "*" } else { " " },
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
                .title(format!("Workspaces /{}", safe(&app.filter))),
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
        List::new(files).block(Block::default().borders(Borders::ALL).title(format!(
            "Files: {}",
            safe(app.files_owner.as_deref().unwrap_or("none"))
        ))),
        cols[1],
    );
    let content = if app.help {
        "q/Ctrl-C quit · ↑/↓ or j/k move · Tab switch pane\nEnter open workspace/file · Esc workspace pane\n/ filter workspaces · Space select for review\no overlap review · p integration preview · r refresh\nPageUp/PageDown scroll review\nReviews use selected workspaces, or all if none selected.\nThis TUI is read-only; use the CLI for mutations.".to_string()
    } else {
        app.review
            .lines()
            .skip(app.review_scroll)
            .take(cols[2].height.saturating_sub(2) as usize)
            .map(safe)
            .collect::<Vec<_>>()
            .join("\n")
    };
    f.render_widget(
        Paragraph::new(content).block(Block::default().borders(Borders::ALL).title(if app.help {
            "Help"
        } else {
            "Review"
        })),
        cols[2],
    );
    f.render_widget(
        Paragraph::new(format!(
            "{}\n{}",
            safe(&app.message),
            if app.editing {
                "Filter: type, Enter to finish"
            } else {
                "? help  / filter  Enter open  Space select  o overlaps  p preview  q quit"
            }
        )),
        rows[1],
    );
}
#[cfg(test)]
mod tests {
    use super::*;
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
