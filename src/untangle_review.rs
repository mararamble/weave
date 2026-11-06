//! Vineweaver keyboard review. Git work belongs to one worker; edits carry revisions.
use crate::{
    tui::{safe, window},
    untangle::{Engine, Options},
    untangle_export::{self as export, Kind, Plan},
};
use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
fn bail_result(message: &str) -> Result<()> {
    Err(anyhow::anyhow!("{message}"))
}

struct UnitView {
    id: String,
    label: String,
    preview: String,
}
struct Capture {
    id: String,
    base: String,
    units: Vec<UnitView>,
    plan: Plan,
}
enum Job {
    Capture(String, bool, bool, Arc<AtomicBool>),
    Validate(u64, Plan, Arc<AtomicBool>),
    Export(u64, Plan, PathBuf, Kind, bool, Arc<AtomicBool>),
}
enum Message {
    Captured(Result<Box<Capture>>),
    Validated(u64, Result<export::Validation>),
    Exported(u64, Result<String>),
}
fn captured(engine: &Engine) -> Capture {
    let mut remaining = 2 * 1024 * 1024usize;
    let units = engine
        .snapshot
        .units
        .iter()
        .map(|u| {
            let keep = u.patch.len().min(64 * 1024).min(remaining);
            remaining -= keep;
            let mut preview = String::from_utf8_lossy(&u.patch[..keep])
                .lines()
                .map(safe)
                .collect::<Vec<_>>()
                .join("\n");
            if keep < u.patch.len() {
                preview.push_str("\n[Display truncated; export preserves exact bytes]");
            }
            let range = u
                .hunk
                .as_ref()
                .map(|h| {
                    format!(
                        " -{},{} +{},{}",
                        h.old_start, h.old_lines, h.new_start, h.new_lines
                    )
                })
                .unwrap_or_else(|| " [whole file]".into());
            UnitView {
                id: u.id.clone(),
                label: format!("{}{}", safe(&u.path), range),
                preview,
            }
        })
        .collect();
    Capture {
        id: engine.snapshot.id.clone(),
        base: engine.snapshot.base_commit.clone(),
        units,
        plan: engine.edit_plan(),
    }
}
fn worker(rx: Receiver<Job>, tx: Sender<Message>) {
    let mut engine: Option<Engine> = None;
    while let Ok(job) = rx.recv() {
        match job {
            Job::Capture(name, include_untracked_content, provenance, flag) => {
                let result = crate::git_process::cancellable(flag, || {
                    let mut next = Engine::capture(&name, include_untracked_content)?;
                    if provenance {
                        next.enable_evidence()?;
                    }
                    let data = captured(&next);
                    engine = Some(next);
                    Ok(data)
                });
                if tx.send(Message::Captured(result.map(Box::new))).is_err() {
                    break;
                }
            }
            Job::Validate(revision, plan, flag) => {
                let result = crate::git_process::cancellable(flag.clone(), || {
                    let engine = engine.as_mut().context("Capture a workspace first")?;
                    engine.set_plan(&plan)?;
                    export::validate(engine, &flag)
                });
                if tx.send(Message::Validated(revision, result)).is_err() {
                    break;
                }
            }
            Job::Export(revision, plan, path, kind, partial, flag) => {
                let result = crate::git_process::cancellable(flag.clone(), || {
                    let engine = engine.as_mut().context("Capture a workspace first")?;
                    engine.set_plan(&plan)?;
                    export::export(
                        engine,
                        &path,
                        kind,
                        &engine.snapshot.base_commit,
                        partial,
                        &flag,
                    )?;
                    Ok(format!("Export published: {}", path.display()))
                });
                if tx.send(Message::Exported(revision, result)).is_err() {
                    break;
                }
            }
        }
    }
}
fn copy_plan(plan: &Plan) -> Plan {
    Plan {
        schema_version: 1,
        snapshot_id: plan.snapshot_id.clone(),
        groups: plan.groups.clone(),
        assignments: plan.assignments.clone(),
        evidence: plan.evidence.clone(),
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Units,
    Groups,
    Patch,
}
enum Prompt {
    OverrideAgent,
    EvidenceReason(Option<String>),
    AcknowledgeEvidence,
    Group,
    Rename,
    PatchExport,
    CommitExport,
    Confirm(PathBuf, Kind),
    Partial(PathBuf, Kind),
}
struct App {
    capture: Option<Capture>,
    plan: Option<Plan>,
    history: VecDeque<Plan>,
    revision: u64,
    unit: usize,
    group: usize,
    focus: Focus,
    scroll: usize,
    detail: String,
    validation: String,
    validated: Option<(u64, bool)>,
    message: String,
    help: bool,
    prompt: Option<Prompt>,
    input: String,
    busy: bool,
    started: Option<Instant>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Job>,
    rx: Receiver<Message>,
    name: String,
    include_untracked_content: bool,
    provenance: bool,
}
impl App {
    fn new(name: String, tx: Sender<Job>, rx: Receiver<Message>) -> Self {
        Self {
            capture: None,
            plan: None,
            history: VecDeque::new(),
            revision: 0,
            unit: 0,
            group: 0,
            focus: Focus::Units,
            scroll: 0,
            detail: String::new(),
            validation: "Not validated. Ownership unknown; source checks are non-atomic.".into(),
            validated: None,
            message: "Capturing… Esc cancels; q quits".into(),
            help: false,
            prompt: None,
            input: String::new(),
            busy: false,
            started: None,
            cancel: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
            name,
            include_untracked_content: false,
            provenance: false,
        }
    }
    fn status_message(&self) -> String {
        if let Some(started) = self.started {
            format!(
                "{} · {}s elapsed",
                self.message,
                started.elapsed().as_secs()
            )
        } else {
            self.message.clone()
        }
    }
    fn token(&mut self) -> Arc<AtomicBool> {
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = true;
        self.started = Some(Instant::now());
        self.cancel.clone()
    }
    fn capture(&mut self) {
        self.capture = None;
        self.plan = None;
        self.history.clear();
        self.validated = None;
        self.revision += 1;
        self.detail.clear();
        self.validation = "Capture pending/incomplete".into();
        self.message = "Capturing… Esc cancels; q quits".into();
        let flag = self.token();
        let _ = self.tx.send(Job::Capture(
            self.name.clone(),
            self.include_untracked_content,
            self.provenance,
            flag,
        ));
    }
    fn checkpoint(&mut self) {
        if let Some(plan) = &self.plan {
            // Evidence and its append-only audit are independent of undoable
            // grouping state; never duplicate whole replay histories per edit.
            self.history.push_back(Plan {
                schema_version: plan.schema_version,
                snapshot_id: plan.snapshot_id.clone(),
                groups: plan.groups.clone(),
                assignments: plan.assignments.clone(),
                evidence: None,
            });
            while self.history.len() > 32
                || self
                    .history
                    .iter()
                    .map(|p| {
                        p.assignments
                            .iter()
                            .flatten()
                            .map(String::len)
                            .sum::<usize>()
                            + p.assignments.len() * 32
                            + p.groups.iter().map(String::len).sum::<usize>()
                            + p.evidence
                                .as_ref()
                                .and_then(|e| serde_json::to_vec(e).ok())
                                .map(|b| b.len())
                                .unwrap_or(0)
                    })
                    .sum::<usize>()
                    > 8 * 1024 * 1024
            {
                self.history.pop_front();
            }
        }
        self.revision += 1;
        self.validated = None;
        self.validation = "Edits changed: validation is stale. Press v to validate.".into();
    }
    fn edit(&mut self, action: impl FnOnce(&mut Plan, usize, usize)) {
        if self.busy {
            self.message = "Wait for work or Esc to cancel before editing".into();
            return;
        }
        if self.plan.is_none() {
            return;
        }
        self.checkpoint();
        action(self.plan.as_mut().unwrap(), self.unit, self.group);
        if let Some(e) = self.plan.as_mut().and_then(|p| p.evidence.as_mut()) {
            e.record(
                "grouping_edit",
                "Assignment, group or export order changed; original patches preserved",
            );
        }
        self.group = self
            .group
            .min(self.plan.as_ref().unwrap().groups.len().saturating_sub(1));
        self.update_detail();
    }
    fn update_detail(&mut self) {
        self.scroll = 0;
        let (Some(c), Some(plan)) = (&self.capture, &self.plan) else {
            return;
        };
        self.detail = if self.focus == Focus::Groups {
            if let Some(group) = plan.groups.get(self.group) {
                let mut text = format!(
                    "Group: {}\nOwnership unknown. Preview uses captured units.\n",
                    safe(group)
                );
                for (i, assignment) in plan.assignments.iter().enumerate() {
                    if assignment.as_ref() == Some(group) && text.len() < 128 * 1024 {
                        if let Some(e) = &plan.evidence {
                            text.push_str(&e.unit_preview(&c.units[i].id));
                        }
                        text.push_str(&c.units[i].preview);
                        text.push('\n');
                    }
                }
                if text.len() >= 128 * 1024 {
                    text.truncate(text.floor_char_boundary(128 * 1024));
                    text.push_str("\n[Group display truncated]");
                }
                text
            } else {
                "Press g to create a group".into()
            }
        } else {
            c.units
                .get(self.unit)
                .map(|u| match &plan.evidence {Some(e)=>format!("{}\nORIGINAL CAPTURED PATCH\n{}\nCURRENT PATCH: same immutable bytes; assignment/order edits only.\n",e.unit_preview(&u.id),u.preview),None=>u.preview.clone()})
                .unwrap_or_else(|| "No captured changes".into())
        };
        if self.detail.len() > 128 * 1024 {
            self.detail
                .truncate(self.detail.floor_char_boundary(128 * 1024));
            self.detail
                .push_str("\n[Evidence/patch display truncated; export retains complete evidence]");
        }
    }
    fn receive(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            self.busy = false;
            self.started = None;
            match message {
                Message::Captured(result) => match result {
                    Ok(c) => {
                        self.plan = Some(copy_plan(&c.plan));
                        self.capture = Some(*c);
                        self.history.clear();
                        self.revision += 1;
                        self.validated = None;
                        self.unit = 0;
                        self.group = 0;
                        self.message =
                            "Capture ready; g creates group, Space assigns, v validates".into();
                        self.validation =
                            "Not validated. Ownership unknown; source checks are non-atomic."
                                .into();
                        self.update_detail();
                    }
                    Err(e) => {
                        self.validation = "Capture failed; no snapshot is available.".into();
                        self.message = format!("Capture failed: {e:#}");
                    }
                },
                Message::Validated(revision, result) if revision == self.revision => match result {
                    Ok(v) => {
                        let uncertain = v.engine_preview["applications"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|a| {
                                a["independently_applicable"] != true
                                    || a["sequence_applicable"] != true
                            })
                            .count();
                        let mut lines = vec![format!(
                            "Validation: complete={} sequence={} matches captured tree={} · dependency/applicability warnings in {} groups",
                            v.selection_complete,
                            v.sequence_applicable,
                            v.matches_capture,
                            uncertain
                        )];
                        for a in v.engine_preview["applications"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            lines.push(format!(
                                "{}: independent={} sequence={}",
                                a["group"], a["independently_applicable"], a["sequence_applicable"]
                            ));
                            for w in a["warnings"].as_array().into_iter().flatten() {
                                lines.push(format!("WARNING {}: {}", w["code"], w["message"]));
                            }
                        }
                        for w in v.engine_preview["warnings"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            lines.push(format!("WARNING: {}", w["message"]));
                        }
                        self.validation = lines.join("\n");
                        self.validated = Some((revision, v.selection_complete));
                        self.message="Validation ready. p patches, c independent commits; export asks for destination and confirmation.".into();
                    }
                    Err(e) => {
                        self.validated = None;
                        self.validation = format!("Validation failed: {e:#}")
                            .lines()
                            .map(safe)
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.message="Validation failed; source may be stale. r recaptures and discards session edits.".into();
                    }
                },
                Message::Exported(revision, result) => {
                    self.message = match result {
                        Ok(s) => s,
                        Err(e) => format!("Export failed: {e:#}"),
                    };
                    if revision != self.revision {
                        self.validated = None;
                    }
                }
                _ => self.message = "Discarded stale worker result".into(),
            }
        }
    }
    fn prompt_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(15),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
            KeyCode::Esc => {
                self.prompt = None;
                self.input.clear();
                self.message = "Prompt cancelled; no export started".into();
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) if !c.is_control() => self.input.push(c),
            KeyCode::Enter => {
                let Some(prompt) = self.prompt.take() else {
                    return;
                };
                let input = std::mem::take(&mut self.input);
                match prompt {
                    Prompt::OverrideAgent => {
                        if input.trim().is_empty()
                            || input.len() > 256
                            || input.chars().any(char::is_control)
                        {
                            self.message = "Manual agent ID must be nonempty and bounded".into();
                            return;
                        }
                        self.prompt = Some(Prompt::EvidenceReason(Some(input)));
                    }
                    Prompt::EvidenceReason(agent) => {
                        let unit = self
                            .capture
                            .as_ref()
                            .and_then(|c| c.units.get(self.unit))
                            .map(|u| u.id.clone());
                        let result =
                            match (unit, self.plan.as_mut().and_then(|p| p.evidence.as_mut())) {
                                (Some(unit), Some(e)) => e.resolve(&unit, agent.as_deref(), &input),
                                _ => bail_result("Enable --provenance first"),
                            };
                        self.evidence_edit_result(result);
                    }
                    Prompt::AcknowledgeEvidence => {
                        let result = match self.plan.as_mut().and_then(|p| p.evidence.as_mut()) {
                            Some(e) => e.acknowledge(&input),
                            None => bail_result("Enable --provenance first"),
                        };
                        self.evidence_edit_result(result);
                    }
                    Prompt::Group | Prompt::Rename => {
                        if input.trim().is_empty()
                            || input.chars().any(char::is_control)
                            || self
                                .plan
                                .as_ref()
                                .is_some_and(|p| p.groups.contains(&input))
                        {
                            self.message = "Group name must be nonempty and unique".into();
                            return;
                        }
                        let rename = matches!(prompt, Prompt::Rename);
                        self.edit(|p, _, g| {
                            if rename {
                                if let Some(old) = p.groups.get_mut(g) {
                                    let previous = old.clone();
                                    *old = input.clone();
                                    for value in &mut p.assignments {
                                        if value.as_ref() == Some(&previous) {
                                            *value = Some(input.clone());
                                        }
                                    }
                                }
                            } else {
                                p.groups.push(input);
                            }
                        });
                        if !rename {
                            self.group = self
                                .plan
                                .as_ref()
                                .map(|p| p.groups.len().saturating_sub(1))
                                .unwrap_or(0);
                            self.update_detail();
                        }
                    }
                    Prompt::PatchExport | Prompt::CommitExport => {
                        if input.is_empty() {
                            self.message = "Export destination is required".into();
                            return;
                        }
                        if let (Some(c), Some(plan)) = (&self.capture, &self.plan) {
                            self.detail = format!(
                                "EXPORT REVIEW\nDestination: {}\nFormat: {}\nBase: {}\nGroup order: {}\nUnassigned: {}\nExisting destination refused; source refs untouched.\nType export to proceed.",
                                safe(&input),
                                if matches!(prompt, Prompt::PatchExport) {
                                    "ordered patches"
                                } else {
                                    "independent commit bundle"
                                },
                                c.base,
                                plan.groups
                                    .iter()
                                    .map(|g| safe(g))
                                    .collect::<Vec<_>>()
                                    .join(" -> "),
                                plan.assignments.iter().filter(|g| g.is_none()).count()
                            );
                            self.scroll = 0;
                        }
                        self.prompt = Some(Prompt::Confirm(
                            PathBuf::from(input),
                            if matches!(prompt, Prompt::PatchExport) {
                                Kind::Patches
                            } else {
                                Kind::Commits
                            },
                        ));
                    }
                    Prompt::Confirm(path, kind) => {
                        if input != "export" {
                            self.message = "Export cancelled: confirmation must be export".into();
                            return;
                        }
                        if self
                            .plan
                            .as_ref()
                            .is_some_and(|p| p.assignments.iter().any(Option::is_none))
                        {
                            self.prompt = Some(Prompt::Partial(path, kind));
                        } else {
                            self.start_export(path, kind, false);
                        }
                    }
                    Prompt::Partial(path, kind) => {
                        if input == "partial" {
                            self.start_export(path, kind, true);
                        } else {
                            self.message = "Partial export cancelled".into();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn evidence_edit_result(&mut self, result: Result<()>) {
        match result {
            Ok(()) => {
                self.revision += 1;
                self.validated = None;
                self.validation = "Evidence review changed; v validates before export".into();
                self.message =
                    "Evidence decision appended to audit; original evidence retained".into();
                self.update_detail();
            }
            Err(e) => self.message = format!("Evidence review refused: {e:#}"),
        }
    }
    fn start_export(&mut self, path: PathBuf, kind: Kind, partial: bool) {
        if let Some(plan) = self.plan.as_ref().map(copy_plan) {
            let flag = self.token();
            let _ = self
                .tx
                .send(Job::Export(self.revision, plan, path, kind, partial, flag));
            self.message =
                "Exporting… Esc requests cancellation; publication is the final atomic step".into();
        }
    }
    fn key(&mut self, key: KeyCode) -> bool {
        if self.prompt.is_some() {
            self.prompt_key(key);
            return false;
        }
        match key {
            KeyCode::Char('o'|'z'|'a') if !self.busy && self.plan.as_ref().is_some_and(|p|p.evidence.is_some())=>{
                self.prompt=Some(match key {KeyCode::Char('o')=>Prompt::OverrideAgent,KeyCode::Char('z')=>Prompt::EvidenceReason(None),_=>Prompt::AcknowledgeEvidence});self.input.clear();
            }
            KeyCode::Char('q')=>{self.cancel.store(true,Ordering::Release);return true;}
            KeyCode::Esc=>{self.cancel.store(true,Ordering::Release);self.message="Cancellation requested; waiting for child termination and cleanup".into();}
            KeyCode::Char('?')=>self.help= !self.help,
            KeyCode::Char('r') if !self.busy=>{self.capture();}
            KeyCode::Char('v') if !self.busy=>{if let Some(plan)=self.plan.as_ref().map(copy_plan) {let flag=self.token();let _=self.tx.send(Job::Validate(self.revision,plan,flag));self.message="Validating captured units and aggregate reconstruction…".into();}}
            KeyCode::Char('p'|'c') if !self.busy=>{
                if self.validated.is_some_and(|(r,_)|r==self.revision) {self.prompt=Some(if key==KeyCode::Char('p'){Prompt::PatchExport}else{Prompt::CommitExport});self.input.clear();} else {self.message="Validate this revision with v before exporting".into();}
            }
            KeyCode::Char('g'|'n') if !self.busy && self.plan.is_some()=>{self.prompt=Some(if key==KeyCode::Char('g'){Prompt::Group}else{Prompt::Rename});self.input.clear();}
            KeyCode::Char('d')=>self.edit(|p,_,g|{if g<p.groups.len(){let old=p.groups.remove(g);for a in &mut p.assignments{if a.as_ref()==Some(&old){*a=None;}}}}),
            KeyCode::Char(' ')=>self.edit(|p,u,g|{if let (Some(a),Some(group))=(p.assignments.get_mut(u),p.groups.get(g)){*a=Some(group.clone());}}),
            KeyCode::Char('x')=>self.edit(|p,u,_|{if let Some(a)=p.assignments.get_mut(u){*a=None;}}),
            KeyCode::Char('['|']')=>{let up=key==KeyCode::Char('[');let g=self.group;self.edit(|p,_,_|{if up&&g>0&&g<p.groups.len(){p.groups.swap(g,g-1);}else if !up&&g+1<p.groups.len(){p.groups.swap(g,g+1);}});if !self.busy {let len=self.plan.as_ref().map(|p|p.groups.len()).unwrap_or(0);self.group=if up{g.saturating_sub(1)}else{(g+1).min(len.saturating_sub(1))};self.update_detail();}}
            KeyCode::Char('u') if !self.busy=>{if let Some(mut plan)=self.history.pop_back(){plan.evidence=self.plan.as_ref().and_then(|p|p.evidence.clone());if let Some(e)=&mut plan.evidence{e.record("undo_grouping","Grouping/order edit undone; evidence audit history retained");}self.plan=Some(plan);self.revision+=1;self.validated=None;self.validation="Undo changed assignments/order; validate again".into();self.group=self.group.min(self.plan.as_ref().unwrap().groups.len().saturating_sub(1));self.update_detail();}}
            KeyCode::Char('s')=>self.message="Safe split: capture already separates existing Git hunks. Contiguous/overlapping edits and whole-file units remain indivisible.".into(),
            KeyCode::Tab=>{self.focus=match self.focus{Focus::Units=>Focus::Groups,Focus::Groups=>Focus::Patch,Focus::Patch=>Focus::Units};if self.focus!=Focus::Patch{self.update_detail();}}
            KeyCode::Down|KeyCode::Char('j')=>{match self.focus{Focus::Units=>self.unit=(self.unit+1).min(self.capture.as_ref().map(|c|c.units.len()).unwrap_or(0).saturating_sub(1)),Focus::Groups=>self.group=(self.group+1).min(self.plan.as_ref().map(|p|p.groups.len()).unwrap_or(0).saturating_sub(1)),Focus::Patch=>self.scroll=self.scroll.saturating_add(1)}if self.focus!=Focus::Patch{self.update_detail();}}
            KeyCode::Up|KeyCode::Char('k')=>{match self.focus{Focus::Units=>self.unit=self.unit.saturating_sub(1),Focus::Groups=>self.group=self.group.saturating_sub(1),Focus::Patch=>self.scroll=self.scroll.saturating_sub(1)}if self.focus!=Focus::Patch{self.update_detail();}}
            KeyCode::PageDown=>self.scroll=self.scroll.saturating_add(15),KeyCode::PageUp=>self.scroll=self.scroll.saturating_sub(15),
            _=>{}
        }
        false
    }
}
fn draw(f: &mut Frame, app: &App) {
    let rows = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(f.area());
    let title = if let (Some(c), Some(p)) = (&app.capture, &app.plan) {
        format!(
            "Vineweaver · {} · revision {} · unassigned {} / {}\n{}\nSnapshot {}\nBase {} · ownership UNKNOWN · checks non-atomic",
            safe(&app.name),
            app.revision,
            p.assignments.iter().filter(|g| g.is_none()).count(),
            p.assignments.len(),
            app.validation.lines().next().unwrap_or("Not validated"),
            c.id,
            c.base
        )
    } else {
        format!(
            "Vineweaver · {} · {}",
            safe(&app.name),
            app.validation
                .lines()
                .next()
                .unwrap_or("Capture pending/incomplete")
        )
    };
    f.render_widget(
        Paragraph::new(crate::terminal_text(&title)).wrap(Wrap { trim: false }),
        rows[0],
    );
    let cols = if rows[1].width < 90 {
        Layout::vertical([
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Min(1),
        ])
        .split(rows[1])
    } else {
        Layout::horizontal([
            Constraint::Percentage(30),
            Constraint::Percentage(20),
            Constraint::Min(1),
        ])
        .split(rows[1])
    };
    let units: Vec<_> = app
        .capture
        .as_ref()
        .map(|c| {
            window(app.unit, cols[0].height, c.units.len())
                .map(|i| {
                    let assignment = app
                        .plan
                        .as_ref()
                        .and_then(|p| p.assignments[i].as_deref())
                        .unwrap_or("UNASSIGNED");
                    ListItem::new(crate::terminal_text(&format!(
                        "{} -> {}",
                        c.units[i].label,
                        safe(assignment)
                    )))
                    .style(selected(app.focus == Focus::Units && i == app.unit))
                })
                .collect()
        })
        .unwrap_or_default();
    f.render_widget(
        List::new(units).block(
            Block::default()
                .borders(Borders::ALL)
                .title("File / hunk units"),
        ),
        cols[0],
    );
    let groups: Vec<_> = app
        .plan
        .as_ref()
        .map(|p| {
            window(app.group, cols[1].height, p.groups.len())
                .map(|i| {
                    ListItem::new(format!(
                        "{} {} {}{}",
                        if i == app.group { ">" } else { " " },
                        i + 1,
                        safe(&p.groups[i]),
                        p.evidence
                            .as_ref()
                            .map(|e| {
                                let sources = p
                                    .assignments
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, g)| g.as_ref() == Some(&p.groups[i]))
                                    .filter_map(|(n, _)| {
                                        app.capture
                                            .as_ref()
                                            .and_then(|c| c.units.get(n))
                                            .and_then(|u| e.units.get(&u.id))
                                    })
                                    .map(|u| format!("{:?}", u.attribution.source))
                                    .collect::<std::collections::BTreeSet<_>>();
                                format!(" [{}]", sources.into_iter().collect::<Vec<_>>().join("/"))
                            })
                            .unwrap_or_default()
                    ))
                    .style(selected(app.focus == Focus::Groups && i == app.group))
                })
                .collect()
        })
        .unwrap_or_default();
    f.render_widget(
        List::new(groups).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Groups (ordered)"),
        ),
        cols[1],
    );
    let text = if matches!(app.prompt, Some(Prompt::Confirm(..) | Prompt::Partial(..))) {
        app.detail.clone()
    } else if app.help {
        "Tab panes · j/k or arrows move · PgUp/PgDn scroll\ng new group · n rename · d delete group (unassigns)\nSpace assign/move unit to selected group · x unassign\n[ / ] reorder active group · u undo (32 edits / 8 MiB)\ns safe split explanation (existing Git hunks already separated)\nv validate · p patch export · c independent-commit bundle\nExports require a NEW destination and typing export.\nPartial selection also requires typing partial.\nEsc cancel task/prompt · r recapture (discards edits) · q/Ctrl-C quit\nProvenance mode: o manual override · z leave unknown · a acknowledge\nEvidence decisions require a reason; revalidate with v after review.\nNo authorship inference. Applicability is not semantic compatibility.\nPatch display is bounded; exported bytes are exact.".into()
    } else {
        format!(
            "{}\n{}\n\n{}",
            app.validation,
            app.plan
                .as_ref()
                .and_then(|p| p.evidence.as_ref())
                .map(|e| e.summary())
                .unwrap_or_default(),
            app.detail
        )
    };
    f.render_widget(
        Paragraph::new(crate::terminal_text(&text))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Validation / patch preview"),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.scroll.min(u16::MAX as usize) as u16, 0)),
        cols[2],
    );
    let prompt = app.prompt.as_ref().map(|p| match p {
        Prompt::OverrideAgent => "Manual agent ID (assertion only)",
        Prompt::EvidenceReason(_) => "Reason for manual override / leaving unknown",
        Prompt::AcknowledgeEvidence => {
            "Reason acknowledging evidence uncertainty and semantic dependency review"
        }
        Prompt::Group => "New group",
        Prompt::Rename => "Rename active group",
        Prompt::PatchExport => "New patch export directory",
        Prompt::CommitExport => "New commit bundle directory",
        Prompt::Confirm(_, _) => "Type export to confirm destination/base/order shown above",
        Prompt::Partial(_, _) => "PARTIAL: unassigned changes omitted. Type partial to acknowledge",
    });
    let footer = if let Some(prompt) = prompt {
        format!(
            "{}\n{}\nEnter confirms · Esc cancels",
            prompt,
            safe(&app.input)
        )
    } else {
        format!(
            "{}\n? help · g group · Space assign · v validate · p/c export · q quit",
            safe(&app.status_message())
        )
    };
    f.render_widget(
        Paragraph::new(crate::terminal_text(&footer)).wrap(Wrap { trim: false }),
        rows[2],
    );
}
fn selected(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    }
}
pub(crate) fn run(options: Options) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("Vineweaver review requires an interactive terminal");
    }
    if !options.groups.is_empty()
        || !options.assignments.is_empty()
        || !options.unassign.is_empty()
        || !options.order.is_empty()
        || options.snapshot.is_some()
        || options.base.is_some()
        || options.allow_partial
        || !options.overrides.is_empty()
        || !options.resolutions.is_empty()
        || options.reason.is_some()
        || options.acknowledge_evidence
    {
        bail!("Start --review without CLI grouping arguments; edit in the session");
    }
    crate::workspace::get(&options.name)?;
    let (tx, rx_worker) = mpsc::channel();
    let (tx_worker, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || worker(rx_worker, tx_worker));
    let mut app = App::new(options.name, tx, rx);
    app.include_untracked_content = options.include_untracked_content;
    app.provenance = options.provenance;
    let result = (|| -> Result<()> {
        enable_raw_mode()?;
        let _guard = crate::tui::TerminalGuard;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
        app.capture();
        loop {
            app.receive();
            terminal.draw(|f| draw(f, &app))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind != KeyEventKind::Release
            {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if app.key(key.code) {
                    break;
                }
            }
        }
        Ok(())
    })();
    // Restore terminal before waiting for cooperative worker cleanup.
    app.cancel.store(true, Ordering::Release);
    drop(app);
    handle.join().map_err(|_| {
        anyhow::anyhow!(
            "Vineweaver worker failed; inspect OS temporary directories if cleanup was interrupted"
        )
    })?;
    result
}

pub(crate) fn run_name(name: String, include_untracked_content: bool) -> Result<()> {
    run(Options {
        name,
        include_untracked_content,
        review: true,
        json: false,
        snapshot: None,
        groups: vec![],
        assignments: vec![],
        unassign: vec![],
        order: vec![],
        export_patches: None,
        export_commits: None,
        base: None,
        allow_partial: false,
        provenance: false,
        evidence_json: false,
        overrides: vec![],
        resolutions: vec![],
        reason: None,
        acknowledge_evidence: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        let (tx, _) = mpsc::channel();
        let (_, rx) = mpsc::channel();
        let mut app = App::new("mixed".into(), tx, rx);
        let plan = Plan {
            schema_version: 1,
            snapshot_id: "capture".into(),
            groups: vec!["a".into(), "b".into()],
            assignments: vec![None, None],
            evidence: None,
        };
        app.capture = Some(Capture {
            id: "capture".into(),
            base: "base".into(),
            units: vec![
                UnitView {
                    id: "u0".into(),
                    label: "file -1,3 +1,3".into(),
                    preview: "@@ first\n-old\n+new".into(),
                },
                UnitView {
                    id: "u1".into(),
                    label: "file -20,3 +20,3".into(),
                    preview: "@@ second".into(),
                },
            ],
            plan: copy_plan(&plan),
        });
        app.plan = Some(plan);
        app
    }
    #[test]
    fn capture_failure_replaces_pending_state_and_message() {
        let (tx_job, _rx_job) = mpsc::channel();
        let (tx_message, rx_message) = mpsc::channel();
        let mut app = App::new("mixed".into(), tx_job, rx_message);
        app.capture();
        tx_message
            .send(Message::Captured(Err(anyhow::anyhow!(
                "filters prevent inspection"
            ))))
            .unwrap();

        app.receive();

        assert_eq!(app.validation, "Capture failed; no snapshot is available.");
        assert!(
            app.message
                .starts_with("Capture failed: filters prevent inspection")
        );
    }
    #[test]
    fn keyboard_assign_move_reorder_rename_undo_and_split_boundaries() {
        let mut app = app();
        app.key(KeyCode::Char(' '));
        assert_eq!(
            app.plan.as_ref().unwrap().assignments[0].as_deref(),
            Some("a")
        );
        app.group = 1;
        app.key(KeyCode::Char(' '));
        assert_eq!(
            app.plan.as_ref().unwrap().assignments[0].as_deref(),
            Some("b")
        );
        app.key(KeyCode::Char('['));
        assert_eq!(app.plan.as_ref().unwrap().groups, vec!["b", "a"]);
        app.key(KeyCode::Char('n'));
        for c in "renamed".chars() {
            app.key(KeyCode::Char(c));
        }
        app.key(KeyCode::Enter);
        assert_eq!(
            app.plan.as_ref().unwrap().assignments[0].as_deref(),
            Some("renamed")
        );
        app.key(KeyCode::Char('u'));
        assert_eq!(
            app.plan.as_ref().unwrap().assignments[0].as_deref(),
            Some("b")
        );
        app.key(KeyCode::Char('x'));
        assert!(app.plan.as_ref().unwrap().assignments[0].is_none());
        app.key(KeyCode::Char('u'));
        assert_eq!(
            app.plan.as_ref().unwrap().assignments[0].as_deref(),
            Some("b")
        );
        app.key(KeyCode::Char('s'));
        assert!(app.message.contains("already separates"));
        app.focus = Focus::Groups;
        app.update_detail();
        assert!(app.detail.contains("@@ first"));
        assert!(!app.detail.contains("@@ second"));
        let group_preview = app.detail.clone();
        app.key(KeyCode::Tab);
        assert!(app.focus == Focus::Patch);
        assert_eq!(app.detail, group_preview);
    }
    #[test]
    fn export_needs_current_validation_destination_confirmation_and_partial_acknowledgement() {
        let mut app = app();
        app.key(KeyCode::Char('p'));
        assert!(app.prompt.is_none());
        app.validated = Some((app.revision, false));
        app.key(KeyCode::Char('p'));
        assert!(app.prompt.is_some());
        for c in "/tmp/new-output".chars() {
            app.key(KeyCode::Char(c));
        }
        app.key(KeyCode::Enter);
        assert!(matches!(app.prompt, Some(Prompt::Confirm(..))));
        assert!(app.detail.contains("/tmp/new-output"));
        for c in "export".chars() {
            app.key(KeyCode::Char(c));
        }
        app.key(KeyCode::Enter);
        assert!(matches!(app.prompt, Some(Prompt::Partial(..))));
        assert!(!app.busy);
        app.key(KeyCode::Esc);
        assert!(!app.busy);
        assert!(app.prompt.is_none());
        app.key(KeyCode::Char(' '));
        assert!(app.validated.is_none());
    }
    #[test]
    fn narrow_and_resized_terminal_safe_previews_and_stale_results() {
        let mut app = app();
        app.update_detail();
        app.validation = "WARNING: unassigned changes".into();
        for (w, h) in [(140, 40), (40, 16), (1, 1), (80, 24)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
        }
        let (tx, rx) = mpsc::channel();
        app.rx = rx;
        app.revision = 10;
        tx.send(Message::Validated(9, Err(anyhow::anyhow!("old failure"))))
            .unwrap();
        app.receive();
        assert_eq!(app.message, "Discarded stale worker result");
        assert!(!app.validation.contains("old failure"));
        app.busy = true;
        app.key(KeyCode::Esc);
        assert!(app.cancel.load(Ordering::Acquire));
        app.key(KeyCode::Char('q'));
    }
    #[test]
    fn every_review_panel_escapes_terminal_controls() {
        let mut app = app();
        let bad = "ESC\x1b]777;WEV40\x07";
        app.name = bad.into();
        app.validation = bad.into();
        app.detail = bad.into();
        app.message = bad.into();
        app.capture.as_mut().unwrap().units[0].label = bad.into();
        app.plan.as_mut().unwrap().groups[0] = bad.into();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains('\x1b'), "{text:?}");
        assert!(text.contains("\\u{1b}"), "{text:?}");
    }
    #[test]
    fn evidence_decisions_survive_grouping_undo_and_acknowledgment_becomes_stale() {
        use crate::untangle_evidence::{ReviewEvidence, UnitEvidence};
        use weave::provenance_replay::{Attribution, Source};
        let mut app = app();
        let units = app
            .capture
            .as_ref()
            .unwrap()
            .units
            .iter()
            .map(|u| {
                (
                    u.id.clone(),
                    UnitEvidence {
                        path_hex: "66696c65".into(),
                        attribution: Attribution {
                            source: Source::Unknown,
                            source_worktree: None,
                            owner_task: None,
                            author_agent: None,
                            event_ids: vec![],
                            snapshot_hash: "snapshot".into(),
                            diagnostics: vec!["unobserved".into()],
                        },
                        original_patch_hex: "00".into(),
                        current_patch_hex: "00".into(),
                        requires_resolution: true,
                        resolution: None,
                    },
                )
            })
            .collect();
        app.plan.as_mut().unwrap().evidence = Some(ReviewEvidence {
            schema_version: 1,
            snapshot_id: "capture".into(),
            base_commit: "base".into(),
            replay: None,
            units,
            audit: vec![],
            acknowledged: false,
        });
        fn enter(app: &mut App, text: &str) {
            for c in text.chars() {
                app.key(KeyCode::Char(c));
            }
            app.key(KeyCode::Enter);
        }
        app.key(KeyCode::Char('o'));
        enter(&mut app, "asserted-agent");
        enter(&mut app, "Human claim; not authenticated");
        assert_eq!(
            app.plan.as_ref().unwrap().evidence.as_ref().unwrap().units["u0"]
                .attribution
                .source,
            Source::Manual
        );
        app.key(KeyCode::Char('a'));
        enter(
            &mut app,
            "Reviewed attribution uncertainty and dependencies",
        );
        assert!(
            app.plan
                .as_ref()
                .unwrap()
                .evidence
                .as_ref()
                .unwrap()
                .acknowledged
        );
        app.key(KeyCode::Char(' '));
        assert!(
            !app.plan
                .as_ref()
                .unwrap()
                .evidence
                .as_ref()
                .unwrap()
                .acknowledged
        );
        app.key(KeyCode::Char('u'));
        let evidence = app.plan.as_mut().unwrap().evidence.as_mut().unwrap();
        assert_eq!(evidence.units["u0"].attribution.source, Source::Manual);
        assert_eq!(evidence.audit.len(), 4);
        assert!(!evidence.acknowledged);
        evidence
            .units
            .get_mut("u0")
            .unwrap()
            .attribution
            .diagnostics
            .push("bad\x1b]777;AUDIT\x07".into());
        app.update_detail();
        for (w, h) in [(40, 16), (1, 1), (140, 40)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .all(|c| !c.symbol().contains('\x1b'))
            );
        }
    }
}
