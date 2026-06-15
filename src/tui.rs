//! opencode-style terminal UI (ratatui).
//!
//! Message area + multiline editor + status bar (agent · model · mode); leader
//! key (ctrl+x) with a which-key popup; agent/model/theme pickers; streaming
//! agent steps; inline approvals; interrupt (Esc) cancels the running agent.

use crate::agent::{run_agent, AgentEvent};
use crate::hooks::Hooks;
use crate::permission::PermissionSet;
use crate::prompts::{self, Agent};
use crate::theme::{themes, Theme};
use crate::tools::Tools;
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use fuzzy_matcher::FuzzyMatcher;
use futures::StreamExt;
use liteforge::AsyncForgeClient;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

enum Kind {
    User,
    Assistant,
    Tool,
    Result,
    Final,
    Error,
    Info,
}

impl Kind {
    fn tag(&self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Assistant => "assistant",
            Kind::Tool => "tool",
            Kind::Result => "result",
            Kind::Final => "final",
            Kind::Error => "error",
            Kind::Info => "info",
        }
    }
    fn from_tag(s: &str) -> Kind {
        match s {
            "user" => Kind::User,
            "assistant" => Kind::Assistant,
            "tool" => Kind::Tool,
            "result" => Kind::Result,
            "final" => Kind::Final,
            "error" => Kind::Error,
            _ => Kind::Info,
        }
    }
}

struct Msg {
    kind: Kind,
    text: String,
    /// Highlighted render cache (built lazily at theme `epoch`); avoids
    /// re-running syntect every frame.
    cache: std::cell::RefCell<Option<(usize, Vec<Line<'static>>)>>,
}

impl Msg {
    fn new(kind: Kind, text: String) -> Self {
        Msg { kind, text, cache: std::cell::RefCell::new(None) }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SidebarView {
    Files,
    Stats,
}

/// Which pane has keyboard focus.
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Editor,
    Sidebar,
}

#[derive(PartialEq)]
enum PickerKind {
    Models,
    Agents,
    Themes,
    Commands,
    Sessions,
    Files,
}

struct Picker {
    kind: PickerKind,
    title: String,
    items: Vec<String>,
    filter: String,
    sel: usize,
}

impl Picker {
    fn filtered(&self) -> Vec<(usize, &String)> {
        if self.filter.is_empty() {
            return self.items.iter().enumerate().collect();
        }
        let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
        let mut scored: Vec<(i64, usize, &String)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, s)| matcher.fuzzy_match(s, &self.filter).map(|sc| (sc, i, s)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i, s)| (i, s)).collect()
    }
}

enum Overlay {
    None,
    WhichKey,
    Help,
    Picker(Picker),
}

pub struct App {
    client: AsyncForgeClient,
    model: String,
    base_url: String,
    root: String,
    root_path: PathBuf,
    tools_yolo: bool,
    hooks: Hooks,

    agents: Vec<Agent>,
    agent_idx: usize,
    models: Vec<String>,
    model_idx: usize,
    themes: Vec<Theme>,
    theme_idx: usize,
    theme_epoch: usize,
    glyphs: crate::glyphs::Glyphs,

    input: Vec<char>,
    cursor: usize,
    input_history: Vec<String>,
    history_pos: Option<usize>,
    draft: Vec<char>,
    lines: Vec<Msg>,
    convo: Vec<(String, String)>,
    last_task: String,
    partial: String,
    usage: crate::usage::Usage,
    trace: crate::trace::Trace,
    stats: crate::stats::Stats,
    session_id: String,
    session_title: String,
    session_metas: Vec<crate::session::Meta>,
    offset_from_bottom: u16,

    sidebar: bool,
    sidebar_view: SidebarView,
    focus: Focus,
    sidebar_sel: usize,
    open_in_editor: Option<PathBuf>,
    think: crate::router::Think,
    // Blocking startup model pick: true until the user chooses from the modal.
    needs_model_pick: bool,
    files: Vec<String>,
    files_checked: Option<std::time::Instant>,
    git_branch: String,
    git_dirty: bool,
    git_checked: Option<std::time::Instant>,
    run_started: Option<std::time::Instant>,
    current_tool: Option<String>,
    mode_flash: Option<std::time::Instant>,
    commands: Vec<crate::commands::Cmd>,
    queued_task: Option<String>,
    queued_compact: bool,
    undo: std::sync::Arc<std::sync::Mutex<crate::undo::UndoStack>>,
    mcp: std::sync::Arc<crate::mcp_tools::McpTools>,
    leader: bool,
    ctrl_c_armed: bool,
    overlay: Overlay,
    running: bool,
    pending: Option<(String, oneshot::Sender<bool>)>,
    task: Option<JoinHandle<()>>,
    spinner: usize,
    quit: bool,
}

impl App {
    fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }
    fn agent(&self) -> &Agent {
        &self.agents[self.agent_idx]
    }

    fn push(&mut self, kind: Kind, text: String) {
        self.lines.push(Msg::new(kind, text));
        self.offset_from_bottom = 0;
    }

    fn input_string(&self) -> String {
        self.input.iter().collect()
    }

    /// Toggle keyboard focus between the editor and the file-tree sidebar.
    /// Focusing the tree forces it visible + the Files view and clamps the
    /// selection into range.
    fn focus_sidebar(&mut self) {
        if self.focus == Focus::Sidebar {
            self.focus = Focus::Editor;
            return;
        }
        self.sidebar = true;
        self.sidebar_view = SidebarView::Files;
        self.focus = Focus::Sidebar;
        if self.sidebar_sel >= self.files.len() {
            self.sidebar_sel = self.files.len().saturating_sub(1);
        }
    }

    /// A status word for the animated thinking line: the active tool if one is
    /// running, else a playful verb that rotates ~every 3s so it feels alive.
    fn thinking_word(&self, elapsed_secs: u64) -> String {
        if let Some(tool) = &self.current_tool {
            return format!("Running {tool}");
        }
        const WORDS: &[&str] = &[
            "Thinking", "Pondering", "Noodling", "Cooking", "Crunching", "Conjuring",
            "Tinkering", "Computing",
        ];
        WORDS[((elapsed_secs / 3) as usize) % WORDS.len()].to_string()
    }

    /// The model a run will actually use: the bigger "thinking" model when an
    /// elevated think level is set (resolved against the endpoint's model list),
    /// else the user's selected model. This is what the status bar/header show.
    // --- curated model picker (Auto-first, <=32B, specialty fine-tunes collapsed) ---
    fn size_label(s: f32) -> String {
        if s.fract() == 0.0 { format!("{}B", s as u32) } else { format!("{s}B") }
    }
    fn size_tag(s: f32) -> String {
        if s.fract() == 0.0 { format!("{}b", s as u32) } else { format!("{s}b") }
    }

    /// Distinct served specialist sizes (<=32B), smallest first.
    fn specialist_sizes(&self) -> Vec<f32> {
        let mut sizes: Vec<f32> = Vec::new();
        for m in &self.models {
            if crate::router::is_specialty_model(m) {
                let s = crate::router::parse_size_b_f(m);
                if s > 0.0 && s <= 32.0 && !sizes.iter().any(|x| (*x - s).abs() < 1e-6) {
                    sizes.push(s);
                }
            }
        }
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sizes
    }

    /// (label, model, think) for every picker row: Auto, Auto·think, Auto·<size>,
    /// then generic concrete models filtered to <=32B with specialty fine-tunes hidden.
    fn model_entries(&self) -> Vec<(String, String, crate::router::Think)> {
        use crate::router::Think;
        let mut e: Vec<(String, String, Think)> = vec![
            ("Auto".into(), "auto".into(), Think::Off),
            ("Auto · think low".into(), "auto".into(), Think::Low),
            ("Auto · think high".into(), "auto".into(), Think::High),
            ("Auto · think xtra".into(), "auto".into(), Think::Xtra),
        ];
        for s in self.specialist_sizes() {
            e.push((format!("Auto · {}", Self::size_label(s)),
                    format!("auto:{}", Self::size_tag(s)), Think::Off));
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for m in &self.models {
            if crate::router::is_specialty_model(m) || crate::router::parse_size_b_f(m) > 32.0 {
                continue;
            }
            if seen.insert(m.clone()) {
                e.push((m.clone(), m.clone(), Think::Off));
            }
        }
        e
    }

    fn model_picker_items(&self) -> Vec<String> {
        self.model_entries().into_iter().map(|(l, _, _)| l).collect()
    }

    /// Friendly label for the current (self.model, self.think) selection.
    fn model_label(&self) -> String {
        let e = self.model_entries();
        if let Some((l, _, _)) = e.iter().find(|(_, m, t)| *m == self.model && *t == self.think) {
            return l.clone();
        }
        if let Some((l, _, _)) = e.iter().find(|(_, m, _)| *m == self.model) {
            return l.clone();
        }
        self.model.clone()
    }

    /// Resolve the run's (start model, escalation ladder, think) from the selection.
    /// Auto -> classify specialty, build that ladder, start at the complexity tier;
    /// Auto·<size> -> same but start pinned to that size; concrete -> 1-rung ladder
    /// (pinned, no escalation). Think: an explicit /think wins, else derived.
    fn resolve_run(&self, task: &str) -> (String, crate::router::Ladder, crate::router::Think) {
        use crate::router::{self, Think, Tier};
        let derive = |t: Tier| match t {
            Tier::Small => Think::Off,
            Tier::Medium => Think::Low,
            Tier::Large => Think::High,
        };
        let tier = router::classify_start(task);
        let think = if self.think != Think::Off { self.think } else { derive(tier) };
        let sel = self.model.clone();
        if sel == "auto" || sel.starts_with("auto:") {
            // Learned classifier picks the specialty when confident, else regex.
            let spec = router::classify_specialty_smart(task);
            let ladder = router::specialty_ladder(&spec, &self.models);
            let start = if self.think.forces_top() {
                // User explicitly forced think high/xtra -> start on the MOST capable
                // model in the ladder (the big generic model), not a tiny specialist.
                ladder.models.last().cloned().unwrap_or_else(|| ladder.model_for(tier))
            } else if let Some(rest) = sel.strip_prefix("auto:") {
                let floor = router::parse_size_b_f(rest);
                ladder.models.iter()
                    .find(|m| router::parse_size_b_f(m) >= floor)
                    .cloned()
                    .unwrap_or_else(|| ladder.model_for(tier))
            } else {
                ladder.model_for(tier)
            };
            (start, ladder, think)
        } else {
            // concrete pin: single-rung ladder -> no escalation.
            (sel.clone(), router::Ladder { models: vec![sel] }, think)
        }
    }

    /// Idle terminal-window title: `smolcode — <project dir basename>`.
    fn idle_title(&self) -> String {
        let base = std::path::Path::new(&self.root)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.root.clone());
        format!("smolcode — {base}")
    }

    fn submit(&mut self, agent_rx: &mut Option<mpsc::Receiver<AgentEvent>>) {
        let task = self.input_string();
        let task = task.trim().to_string();
        if task.is_empty() {
            return;
        }
        // Blocking startup model pick: open the modal and keep the typed task.
        if self.needs_model_pick {
            self.open_picker(PickerKind::Models);
            self.push(Kind::Info, "pick a model first — Auto is recommended".into());
            return;
        }
        self.input.clear();
        self.cursor = 0;
        self.remember_history(&task);
        self.push(Kind::User, task.clone());
        self.stats.on_task();
        self.trace.record(&crate::trace::TraceEvent::Task { text: task.clone() });
        self.last_task = task.clone();
        if self.session_title.is_empty() {
            self.session_title = crate::agent::clip(&task, 36);
        }
        set_terminal_title(&format!("● {} — smolcode", crate::agent::clip(&task, 40)));

        // expand @file references into the task sent to the model
        let mut attach = String::new();
        for tok in task.split_whitespace() {
            if let Some(p) = tok.strip_prefix('@') {
                if let Ok(content) = std::fs::read_to_string(self.root_path.join(p)) {
                    attach.push_str(&format!("\n\n--- {p} ---\n{}", crate::agent::clip(&content, 4000)));
                } else {
                    // not a file: try resolving @name as a symbol definition
                    let found = crate::symbols::find(&self.root_path, p);
                    if !found.starts_with('(') {
                        attach.push_str(&format!("\n\n--- symbol {p} ---\n{found}"));
                    }
                }
            }
        }
        let effective = if attach.is_empty() {
            task.clone()
        } else {
            format!("{task}\n\nReferenced files:{attach}")
        };

        let system = prompts::resolve_system(self.agent(), &self.root_path, &task);
        // refresh the context-usage meter: system + prior turns + this task
        {
            let mut texts: Vec<&str> = vec![system.as_str()];
            for (u, a) in &self.convo {
                texts.push(u.as_str());
                texts.push(a.as_str());
            }
            texts.push(effective.as_str());
            self.usage.set_from_texts(&texts);
        }
        let read_only = self.agent().read_only;
        let perms = PermissionSet::for_agent(read_only, self.tools_yolo);
        let tools = Tools::new(self.root_path.clone(), self.tools_yolo).with_undo(self.undo.clone());
        // keep the last few turns for multi-turn memory (bounded for small ctx)
        let history: Vec<(String, String)> = self.convo.iter().rev().take(6).rev().cloned().collect();
        let (tx, rx) = mpsc::channel::<AgentEvent>(64);
        *agent_rx = Some(rx);
        self.running = true;
        self.run_started = Some(std::time::Instant::now());
        self.current_tool = None;
        let (run_model, ladder, think) = self.resolve_run(&task);
        self.task = Some(tokio::spawn(run_agent(
            self.client.clone(),
            run_model,
            ladder,
            tools,
            effective,
            system,
            read_only,
            history,
            perms,
            self.hooks.clone(),
            self.mcp.clone(),
            think,
            tx,
        )));
    }

    fn interrupt(&mut self, agent_rx: &mut Option<mpsc::Receiver<AgentEvent>>) {
        if let Some(h) = self.task.take() {
            h.abort();
        }
        if let Some((_, resp)) = self.pending.take() {
            let _ = resp.send(false);
        }
        *agent_rx = None;
        self.running = false;
        self.run_started = None;
        self.current_tool = None;
        set_terminal_title(&self.idle_title());
        self.push(Kind::Error, "interrupted".into());
    }

    fn cycle_agent(&mut self, back: bool) {
        let n = self.agents.len();
        self.agent_idx = if back {
            (self.agent_idx + n - 1) % n
        } else {
            (self.agent_idx + 1) % n
        };
    }

    /// Shift+Tab cycles the working mode: normal (edits ask) -> auto
    /// (auto-approve) -> plan (read-only) -> normal. Sets the agent + yolo.
    fn cycle_mode(&mut self) {
        let cur = if self.agent().read_only {
            2
        } else if self.tools_yolo {
            1
        } else {
            0
        };
        let next = (cur + 1) % 3;
        let (agent_name, yolo, label) = match next {
            0 => ("build", false, "normal (edits ask)"),
            1 => ("build", true, "auto (auto-approve edits + shell)"),
            _ => ("plan", false, "plan (read-only)"),
        };
        if let Some(i) = self.agents.iter().position(|a| a.name == agent_name) {
            self.agent_idx = i;
        }
        self.tools_yolo = yolo;
        self.mode_flash = Some(std::time::Instant::now());
        self.push(Kind::Info, format!("mode → {label}"));
    }

    /// Rescan the workspace file tree for the sidebar (throttled ~1.5s) so
    /// files the agent creates/deletes show up live.
    fn refresh_files(&mut self) {
        let fresh = self.files_checked.map(|i| i.elapsed().as_millis() < 1500).unwrap_or(false);
        if fresh {
            return;
        }
        self.files_checked = Some(std::time::Instant::now());
        self.files = scan_files(&self.root_path);
    }

    /// Refresh the cached git branch/dirty for the header (throttled ~2s; git
    /// shells out, so never call it per frame).
    fn refresh_git(&mut self) {
        let fresh = self.git_checked.map(|i| i.elapsed().as_secs() < 2).unwrap_or(false);
        if fresh {
            return;
        }
        self.git_checked = Some(std::time::Instant::now());
        if !crate::git::is_repo(&self.root_path) {
            self.git_branch.clear();
            self.git_dirty = false;
            return;
        }
        let status = crate::git::status(&self.root_path);
        let branch = status
            .lines()
            .find(|l| l.trim_start().starts_with("## "))
            .map(|l| {
                l.trim().trim_start_matches("## ").split("...").next().unwrap_or("").trim().to_string()
            })
            .unwrap_or_default();
        let dirty = status.contains("change(s)")
            || status.lines().any(|l| {
                let l = l.trim_start();
                l.len() >= 2 && !l.starts_with("##") && !l.starts_with("clean")
                    && matches!(l.chars().next(), Some('M' | 'A' | 'D' | 'R' | 'C' | '?' | 'U'))
            });
        self.git_branch = branch;
        self.git_dirty = dirty;
    }
    fn cycle_model(&mut self, back: bool) {
        let items = self.model_picker_items();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        let cur = self.model_label();
        let i = items.iter().position(|l| *l == cur).unwrap_or(0);
        let j = if back { (i + n - 1) % n } else { (i + 1) % n };
        self.model_idx = j;
        let label = items[j].clone();
        if let Some((_, m, t)) = self.model_entries().into_iter().find(|(l, _, _)| *l == label) {
            self.model = m;
            self.think = t;
        }
        self.needs_model_pick = false;
        self.usage.context_window = crate::usage::model_context_window(&self.model);
    }

    fn open_picker(&mut self, kind: PickerKind) {
        let (title, items) = match kind {
            PickerKind::Models => ("models".into(), self.model_picker_items()),
            PickerKind::Agents => (
                "agents".into(),
                self.agents.iter().map(|a| a.name.clone()).collect(),
            ),
            PickerKind::Themes => (
                "themes".into(),
                self.themes.iter().map(|t| t.name.to_string()).collect(),
            ),
            PickerKind::Sessions => ("sessions".into(), Vec::new()),
            PickerKind::Files => ("@ file".into(), scan_files(&self.root_path)),
            PickerKind::Commands => {
                let mut v: Vec<String> = vec![
                    "/help".into(),
                    "/mcp".into(),
                    "/rules".into(),
                    "/skills".into(),
                    "/new".into(),
                    "/sessions".into(),
                    "/agents".into(),
                    "/models".into(),
                    "/themes".into(),
                    "/files".into(),
                    "/clear".into(),
                    "/quit".into(),
                ];
                for c in &self.commands {
                    v.push(format!("/{}", c.name));
                }
                ("commands".into(), v)
            }
        };
        self.overlay = Overlay::Picker(Picker {
            kind,
            title,
            items,
            filter: String::new(),
            sel: 0,
        });
    }

    fn picker_accept(&mut self) {
        if let Overlay::Picker(p) = &self.overlay {
            let filtered = p.filtered();
            if let Some((real_idx, value)) = filtered.get(p.sel).map(|(i, s)| (*i, (*s).clone())) {
                match p.kind {
                    PickerKind::Models => {
                        // `value` is a curated label; map it to (model, think).
                        self.model_idx = real_idx;
                        if let Some((_, m, t)) =
                            self.model_entries().into_iter().find(|(l, _, _)| *l == value)
                        {
                            self.model = m;
                            self.think = t;
                        }
                        self.needs_model_pick = false;
                        self.usage.context_window = crate::usage::model_context_window(&self.model);
                    }
                    PickerKind::Agents => self.agent_idx = real_idx,
                    PickerKind::Themes => {
                        self.theme_idx = real_idx;
                        self.theme_epoch += 1;
                    }
                    PickerKind::Sessions => {
                        if let Some(meta) = self.session_metas.get(real_idx) {
                            let id = meta.id.clone();
                            self.overlay = Overlay::None;
                            self.load_session(&id);
                            return;
                        }
                    }
                    PickerKind::Files => {
                        self.overlay = Overlay::None;
                        for c in format!("{value} ").chars() {
                            self.input.insert(self.cursor, c);
                            self.cursor += 1;
                        }
                        return;
                    }
                    PickerKind::Commands => {
                        let cmd = value.clone();
                        self.overlay = Overlay::None;
                        self.run_command(&cmd);
                        return;
                    }
                }
            }
        }
        self.overlay = Overlay::None;
    }

    fn run_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        let (head, rest) = match cmd.split_once(char::is_whitespace) {
            Some((h, r)) => (h, r.trim()),
            None => (cmd, ""),
        };
        match head {
            "/help" => self.overlay = Overlay::Help,
            "/new" => self.new_session(),
            "/sessions" => self.open_sessions(),
            "/agents" => self.open_picker(PickerKind::Agents),
            "/models" => self.open_picker(PickerKind::Models),
            "/themes" => self.open_picker(PickerKind::Themes),
            "/files" => self.sidebar = !self.sidebar,
            "/bg" => {
                let out = if let Some(id) = rest.strip_prefix("stop ").map(|s| s.trim()) {
                    crate::bgproc::stop(id)
                } else if rest.trim() == "stop" {
                    crate::bgproc::stop_all();
                    "stopped all background jobs".to_string()
                } else {
                    crate::bgproc::list()
                };
                for line in out.lines() {
                    self.push(Kind::Info, line.to_string());
                }
            }
            "/mcp" => {
                let servers = self.mcp.list_by_server();
                if servers.is_empty() {
                    self.push(Kind::Info, "no MCP servers connected — add [[mcp]] entries to ~/.config/smolcode/config.toml or .smolcode/config.toml".into());
                } else {
                    self.push(Kind::Info, format!("MCP servers ({}):", servers.len()));
                    for (server, tools) in servers {
                        let list = if tools.is_empty() {
                            "(no tools)".to_string()
                        } else {
                            crate::agent::clip(&tools.join(", "), 200)
                        };
                        self.push(Kind::Info, format!("  • {server} ({}): {list}", tools.len()));
                    }
                }
            }
            "/rules" => {
                let rules = crate::rules::load(&self.root_path);
                if rules.is_empty() {
                    self.push(Kind::Info, "no rules — add *.md to .smolcode/rules/ or ~/.config/smolcode/rules/".into());
                } else {
                    self.push(Kind::Info, format!("active rules ({}):", rules.len()));
                    for r in rules {
                        let desc = r.description.unwrap_or_default();
                        let tail = if desc.is_empty() { String::new() } else { format!(" — {desc}") };
                        self.push(Kind::Info, format!("  • {} [{}]{}", r.name, r.scope, tail));
                    }
                }
            }
            "/skills" => {
                let skills = crate::skills::load(&self.root_path);
                if skills.is_empty() {
                    self.push(Kind::Info, "no skills — add <name>/SKILL.md to .smolcode/skills/ or ~/.config/smolcode/skills/".into());
                } else {
                    self.push(Kind::Info, format!("skills ({}) — run with /skill <name>:", skills.len()));
                    for s in skills {
                        let tail = if s.description.is_empty() { String::new() } else { format!(" — {}", s.description) };
                        self.push(Kind::Info, format!("  • {}{}", s.name, tail));
                    }
                }
            }
            "/skill" => {
                let (sname, sargs) = match rest.split_once(char::is_whitespace) {
                    Some((n, a)) => (n.trim(), a.trim()),
                    None => (rest, ""),
                };
                if sname.is_empty() {
                    self.push(Kind::Info, "usage: /skill <name> [args]  (see /skills)".into());
                } else if let Some(skill) = crate::skills::find(&self.root_path, sname) {
                    self.queued_task = Some(crate::commands::expand(&skill.body, sargs));
                } else {
                    self.push(Kind::Info, format!("no skill named '{sname}' (see /skills)"));
                }
            }
            "/mode" => self.cycle_mode(),
            "/think" => {
                self.think = crate::router::Think::parse_or_cycle(rest, self.think);
                let note = if self.think.forces_top() {
                    " (forces the top-tier model)"
                } else {
                    ""
                };
                self.push(Kind::Info, format!("thinking effort: {}{}", self.think.label(), note));
            }
            "/init" => match crate::agents_init::write(&self.root_path) {
                Ok(p) => self.push(Kind::Info, format!("wrote {p} (project guide for agents)")),
                Err(e) => self.push(Kind::Info, format!("/init: {e}")),
            },
            "/commit" => {
                let msg = if rest.is_empty() {
                    "Review the staged/unstaged changes with git_diff, then commit them with git_commit using a concise, descriptive message.".to_string()
                } else {
                    format!("Commit all current changes with git_commit using this message: {rest}")
                };
                self.queued_task = Some(msg);
            }
            "/rename" => {
                if !rest.is_empty() {
                    self.save_session();
                    if crate::session_ops::rename(&self.session_id, rest) {
                        self.session_title = rest.to_string();
                        self.push(Kind::Info, format!("renamed session → {rest}"));
                    }
                } else {
                    self.push(Kind::Info, "usage: /rename <new title>".into());
                }
            }
            "/fork" => {
                self.save_session();
                if let Some(new) = crate::session_ops::fork(&self.session_id) {
                    self.session_id = new.id.clone();
                    self.session_title = new.title.clone();
                    self.push(Kind::Info, format!("forked → {} ({})", new.title, new.id));
                } else {
                    self.push(Kind::Info, "fork failed: no saved session yet".into());
                }
            }
            "/delete" => {
                let old = self.session_id.clone();
                let removed = crate::session_ops::delete(&old);
                self.new_session();
                self.push(
                    Kind::Info,
                    if removed { format!("deleted session {old}; started a new one") }
                    else { "no saved session to delete; started a new one".into() },
                );
            }
            "/stats" => {
                self.push(Kind::Info, format!("session {}", self.session_id));
                for line in self.stats.summary().lines() {
                    self.push(Kind::Info, line.to_string());
                }
                self.push(Kind::Info, format!("trace: {}", self.trace.path().display()));
            }
            "/config" => {
                let ro = self.agent().read_only;
                let rendered = {
                    let tool_names: Vec<&str> = crate::tools::tool_names();
                    let servers = self.mcp.server_names();
                    let view = crate::config_view::ConfigView {
                        model: &self.model,
                        base_url: &self.base_url,
                        agent: self.agent().name.as_str(),
                        read_only: ro,
                        yolo: self.tools_yolo,
                        root: &self.root_path,
                        perm_read: perm_label(ro, self.tools_yolo, "read"),
                        perm_edit: perm_label(ro, self.tools_yolo, "edit"),
                        perm_shell: perm_label(ro, self.tools_yolo, "shell"),
                        hooks_count: self.hooks.hooks.len(),
                        mcp_servers: &servers,
                        tool_names: &tool_names,
                    };
                    crate::config_view::render(&view)
                };
                for line in rendered.lines() {
                    self.push(Kind::Info, line.to_string());
                }
            }
            "/search" => {
                if rest.is_empty() {
                    self.push(Kind::Info, "usage: /search <text>".into());
                } else {
                    let texts: Vec<String> = self.lines.iter().map(|m| m.text.clone()).collect();
                    let matches = crate::transcript_search::find_all(&texts, rest);
                    if matches.is_empty() {
                        self.push(Kind::Info, format!("no matches for '{rest}'"));
                    } else {
                        self.push(Kind::Info, format!("{} match(es) for '{rest}':", matches.len()));
                        for m in matches.iter().take(20) {
                            let line = &texts[m.msg];
                            let start = line[..m.offset].char_indices().rev().take(24).last().map(|(i, _)| i).unwrap_or(m.offset);
                            self.push(Kind::Info, format!("  · …{}", crate::agent::clip(&line[start..], 70)));
                        }
                    }
                }
            }
            "/export" => {
                let events = crate::trace::read(&self.session_id);
                if events.is_empty() {
                    self.push(Kind::Info, "nothing to export yet (run a task first)".into());
                } else {
                    let md = crate::trace::to_markdown(&events);
                    let name = if rest.is_empty() { format!("smolcode-{}.md", self.session_id) } else { rest.to_string() };
                    let path = self.root_path.join(&name);
                    match std::fs::write(&path, md) {
                        Ok(_) => self.push(Kind::Info, format!("exported transcript to {}", path.display())),
                        Err(e) => self.push(Kind::Info, format!("/export failed: {e}")),
                    }
                }
            }
            "/timeline" => {
                self.save_session();
                if let Some(s) = crate::session::load(&self.session_id) {
                    self.push(Kind::Info, format!("— timeline: {} —", s.title));
                    for line in crate::session_ops::timeline(&s) {
                        self.push(Kind::Info, line);
                    }
                } else {
                    self.push(Kind::Info, "no saved session yet".into());
                }
            }
            "/clear" => { self.lines.clear(); self.convo.clear(); }
            "/quit" => {
                self.save_session();
                self.quit = true;
            }
            _ => {
                // custom Markdown command?
                let name = head.trim_start_matches('/');
                if let Some(c) = self.commands.iter().find(|c| c.name == name) {
                    self.queued_task = Some(crate::commands::expand(&c.body, rest));
                }
            }
        }
    }

    // -------- sessions --------
    fn save_session(&self) {
        if !self.lines.iter().any(|m| matches!(m.kind, Kind::User)) {
            return; // nothing meaningful to persist
        }
        let lines = self
            .lines
            .iter()
            .map(|m| crate::session::StoredMsg { role: m.kind.tag().to_string(), text: m.text.clone() })
            .collect();
        crate::session::save(&crate::session::Session {
            id: self.session_id.clone(),
            title: if self.session_title.is_empty() { "(untitled)".into() } else { self.session_title.clone() },
            created: crate::session::now(),
            updated: crate::session::now(),
            lines,
            convo: self.convo.clone(),
        });
    }

    fn new_session(&mut self) {
        self.save_session();
        self.lines.clear();
        self.convo.clear();
        self.session_id = crate::session::new_id();
        self.session_title.clear();
        self.push(Kind::Info, "new session".into());
    }

    fn open_sessions(&mut self) {
        self.save_session();
        self.session_metas = crate::session::list();
        let items = self
            .session_metas
            .iter()
            .map(|m| {
                format!(
                    "{}   ·   {}",
                    if m.title.is_empty() { "(untitled)" } else { &m.title },
                    crate::session::rel_time(m.updated)
                )
            })
            .collect();
        self.overlay = Overlay::Picker(Picker {
            kind: PickerKind::Sessions,
            title: "sessions".into(),
            items,
            filter: String::new(),
            sel: 0,
        });
    }

    fn load_session(&mut self, id: &str) {
        if let Some(s) = crate::session::load(id) {
            self.lines = s
                .lines
                .into_iter()
                .map(|m| Msg::new(Kind::from_tag(&m.role), m.text))
                .collect();
            self.convo = s.convo;
            self.session_id = s.id;
            self.session_title = s.title;
            self.offset_from_bottom = 0;
        }
    }

    /// Copy the most recent assistant/tool message to the system clipboard.
    fn yank_last(&mut self) {
        let text = self
            .lines
            .iter()
            .rev()
            .find(|m| matches!(m.kind, Kind::Assistant | Kind::Tool | Kind::Final))
            .map(|m| m.text.clone())
            .or_else(|| self.lines.last().map(|m| m.text.clone()));
        match text {
            Some(t) if !t.trim().is_empty() => match crate::clipboard::copy(&t) {
                Ok(backend) => self.push(Kind::Info, format!("copied to clipboard ({backend})")),
                Err(e) => self.push(Kind::Info, format!("copy failed: {e}")),
            },
            _ => self.push(Kind::Info, "nothing to copy".into()),
        }
    }

    // -------- key handling --------
    fn on_term_event(&mut self, ev: Event, agent_rx: &mut Option<mpsc::Receiver<AgentEvent>>) {
        // Pasted text arrives as one event: insert it literally (newlines kept)
        // so multi-line pastes don't submit line-by-line.
        if let Event::Paste(s) = ev {
            if !self.running && self.pending.is_none() && matches!(self.overlay, Overlay::None) {
                for ch in s.chars() {
                    self.input.insert(self.cursor, ch);
                    self.cursor += 1;
                }
                self.history_pos = None;
            }
            return;
        }
        let key = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => k,
            _ => return,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Ctrl-C: interrupt a run, else clear the input, else press twice to quit.
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            if self.running {
                self.interrupt(agent_rx);
                self.ctrl_c_armed = false;
            } else if !self.input.is_empty() {
                self.input.clear();
                self.cursor = 0;
                self.history_pos = None;
                self.ctrl_c_armed = false;
            } else if self.ctrl_c_armed {
                self.save_session();
                self.quit = true;
            } else {
                self.ctrl_c_armed = true;
                self.push(Kind::Info, "press Ctrl-C again to quit (or Ctrl-D)".into());
            }
            return;
        }
        // any other key disarms the quit confirmation
        self.ctrl_c_armed = false;
        // Ctrl-D on an empty prompt quits immediately
        if ctrl && matches!(key.code, KeyCode::Char('d')) && self.input.is_empty() && !self.running {
            self.save_session();
            self.quit = true;
            return;
        }
        // Ctrl-L clears the transcript (keeps the conversation memory)
        if ctrl && matches!(key.code, KeyCode::Char('l')) {
            self.lines.clear();
            self.offset_from_bottom = 0;
            return;
        }
        // undo / redo of agent file edits
        if ctrl && matches!(key.code, KeyCode::Char('z')) && !self.running {
            let (undo, root) = (self.undo.clone(), self.root_path.clone());
            let msg = match undo.lock() {
                Ok(mut u) => u.undo(&root).map(|d| format!("undo: {d}")).unwrap_or_else(|| "nothing to undo".into()),
                Err(_) => return,
            };
            self.push(Kind::Info, msg);
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Char('y')) && !self.running {
            let (undo, root) = (self.undo.clone(), self.root_path.clone());
            let msg = match undo.lock() {
                Ok(mut u) => u.redo(&root).map(|d| format!("redo: {d}")).unwrap_or_else(|| "nothing to redo".into()),
                Err(_) => return,
            };
            self.push(Kind::Info, msg);
            return;
        }

        // approval intercept
        if self.pending.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.answer(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.answer(false),
                _ => {}
            }
            return;
        }

        // leader sequence (ctrl+x then key)
        if self.leader {
            self.leader = false;
            self.overlay = Overlay::None;
            match key.code {
                KeyCode::Char('m') => self.open_picker(PickerKind::Models),
                KeyCode::Char('a') => self.open_picker(PickerKind::Agents),
                KeyCode::Char('t') => self.open_picker(PickerKind::Themes),
                KeyCode::Char('l') => self.open_sessions(),
                KeyCode::Char('n') => self.new_session(),
                KeyCode::Char('b') => self.sidebar = !self.sidebar,
                KeyCode::Char('s') => {
                    self.sidebar = true;
                    self.sidebar_view = match self.sidebar_view {
                        SidebarView::Files => SidebarView::Stats,
                        SidebarView::Stats => SidebarView::Files,
                    };
                }
                KeyCode::Char('f') => self.focus_sidebar(),
                KeyCode::Char('h') => self.overlay = Overlay::Help,
                KeyCode::Char('o') => self.cycle_mode(),
                KeyCode::Char('e') => {
                    self.think = self.think.next();
                    self.push(Kind::Info, format!("thinking effort: {}", self.think.label()));
                }
                KeyCode::Char('y') => self.yank_last(),
                KeyCode::Char('c') => {
                    if !self.convo.is_empty() {
                        self.queued_compact = true;
                    }
                }
                KeyCode::Char('q') => {
                    self.save_session();
                    self.quit = true;
                }
                _ => {}
            }
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Char('x')) {
            self.leader = true;
            self.overlay = Overlay::WhichKey;
            return;
        }

        // overlay navigation
        if let Overlay::Picker(p) = &mut self.overlay {
            match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Enter => self.picker_accept(),
                KeyCode::Up => p.sel = p.sel.saturating_sub(1),
                KeyCode::Down => {
                    let max = p.filtered().len().saturating_sub(1);
                    p.sel = (p.sel + 1).min(max);
                }
                KeyCode::Backspace => {
                    p.filter.pop();
                    p.sel = 0;
                }
                KeyCode::Char(c) => {
                    p.filter.push(c);
                    p.sel = 0;
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Overlay::WhichKey | Overlay::Help) {
            self.overlay = Overlay::None;
            return;
        }

        // file-tree navigation when the sidebar has focus
        if self.focus == Focus::Sidebar {
            match key.code {
                KeyCode::Esc => self.focus = Focus::Editor,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.sidebar_sel = self.sidebar_sel.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let max = self.files.len().saturating_sub(1);
                    self.sidebar_sel = (self.sidebar_sel + 1).min(max);
                }
                KeyCode::Enter => {
                    if self.running {
                        self.push(Kind::Info, "finish the current run before opening a file".into());
                    } else if let Some(rel) = self.files.get(self.sidebar_sel) {
                        self.open_in_editor = Some(self.root_path.join(rel));
                        self.focus = Focus::Editor;
                    }
                }
                _ => {}
            }
            return;
        }

        // esc interrupts a running agent
        if matches!(key.code, KeyCode::Esc) {
            if self.running {
                self.interrupt(agent_rx);
            }
            return;
        }

        // scrolling
        match key.code {
            KeyCode::PageUp => {
                self.offset_from_bottom = self.offset_from_bottom.saturating_add(8);
                return;
            }
            KeyCode::PageDown => {
                self.offset_from_bottom = self.offset_from_bottom.saturating_sub(8);
                return;
            }
            _ => {}
        }

        // model / agent cycling
        if matches!(key.code, KeyCode::F(2)) {
            self.cycle_model(shift);
            return;
        }
        // Shift+Tab cycles the working mode (normal -> auto -> plan). Some
        // terminals deliver this as BackTab, others as Tab+Shift.
        if matches!(key.code, KeyCode::BackTab) || (matches!(key.code, KeyCode::Tab) && shift) {
            self.cycle_mode();
            return;
        }
        if matches!(key.code, KeyCode::Tab) {
            self.cycle_agent(false);
            return;
        }

        if self.running {
            return; // editor locked while the agent runs
        }

        // editor
        match key.code {
            KeyCode::Enter if alt || shift => {
                self.input.insert(self.cursor, '\n');
                self.cursor += 1;
            }
            KeyCode::Enter => {
                let s = self.input_string();
                // `!cmd` — run a shell command directly, no LLM (Claude Code's `!`).
                if let Some(cmd) = s.trim_start().strip_prefix('!') {
                    let cmd = cmd.trim().to_string();
                    let raw = s.trim().to_string();
                    self.input.clear();
                    self.cursor = 0;
                    self.remember_history(&raw);
                    if cmd.is_empty() {
                        self.push(Kind::Info, "usage: !<shell command>  (runs locally, no LLM)".into());
                    } else {
                        self.push(Kind::User, format!("! {cmd}"));
                        let out = crate::tools::run_shell_sync(&self.root_path, &cmd);
                        let safe = crate::redact::redact(&out);
                        self.push(Kind::Result, cap_lines(&safe, 60));
                        self.files_checked = None; // the command may have changed files
                    }
                } else if s.trim_start().starts_with('/') && !s.contains('\n') {
                    // slash command at start of a single-line input
                    let cmd = s.trim().to_string();
                    self.input.clear();
                    self.cursor = 0;
                    self.remember_history(&cmd);
                    self.run_command(&cmd);
                } else {
                    self.submit(agent_rx);
                }
            }
            KeyCode::Char('@') => {
                self.input.insert(self.cursor, '@');
                self.cursor += 1;
                self.open_picker(PickerKind::Files);
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.input.remove(self.cursor - 1);
                    self.cursor -= 1;
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
                self.history_pos = None;
            }
            _ => {}
        }
    }

    /// Recall an earlier submitted prompt (shell-style Up). Stashes the current
    /// draft the first time so Down can restore it.
    fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.history_pos {
            None => {
                self.draft = self.input.clone();
                self.input_history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_pos = Some(next);
        self.set_input(self.input_history[next].clone());
    }

    /// Move toward more recent prompts; past the newest restores the draft.
    fn history_next(&mut self) {
        match self.history_pos {
            None => {}
            Some(i) if i + 1 < self.input_history.len() => {
                self.history_pos = Some(i + 1);
                self.set_input(self.input_history[i + 1].clone());
            }
            Some(_) => {
                self.history_pos = None;
                let draft = std::mem::take(&mut self.draft);
                self.input = draft;
                self.cursor = self.input.len();
            }
        }
    }

    fn set_input(&mut self, s: String) {
        self.input = s.chars().collect();
        self.cursor = self.input.len();
    }

    /// Append a submitted line to the recall history (skip blanks + immediate
    /// duplicates) and reset the recall cursor.
    fn remember_history(&mut self, s: &str) {
        let s = s.trim();
        if !s.is_empty() && self.input_history.last().map(|l| l != s).unwrap_or(true) {
            self.input_history.push(s.to_string());
        }
        self.history_pos = None;
        self.draft.clear();
    }

    fn answer(&mut self, ok: bool) {
        if let Some((_, resp)) = self.pending.take() {
            let _ = resp.send(ok);
            self.push(Kind::Info, format!("approval: {}", if ok { "yes" } else { "no" }));
        }
    }

    fn on_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::Token(s) => {
                self.partial.push_str(&s);
                self.offset_from_bottom = 0;
            }
            AgentEvent::Assistant(s) => {
                self.partial.clear();
                self.stats.on_step();
                self.stats.add_tokens(&s);
                if !s.trim().is_empty() {
                    self.push(Kind::Assistant, crate::redact::redact(s.trim()));
                }
            }
            AgentEvent::ToolCall { name, args } => {
                self.partial.clear();
                self.current_tool = Some(name.clone());
                self.stats.on_tool(&name);
                self.trace.record(&crate::trace::TraceEvent::ToolCall { name: name.clone(), args: args.clone() });
                self.push(Kind::Tool, summarize_call(&name, &args));
            }
            AgentEvent::ToolResult { name, text } => {
                // scrub secrets from what we persist (trace) and show (screen);
                // the model's own context keeps the original (agent.rs).
                let safe = crate::redact::redact(&text);
                self.trace.record(&crate::trace::TraceEvent::ToolResult { name, text: safe.clone() });
                self.push(Kind::Result, cap_lines(&safe, 60));
                self.files_checked = None; // a tool may have created/removed files
            }
            AgentEvent::Approval { desc, resp } => self.pending = Some((desc, resp)),
            AgentEvent::Final(s) => {
                self.partial.clear();
                self.stats.add_tokens(&s);
                let safe = crate::redact::redact(s.trim());
                self.trace.record(&crate::trace::TraceEvent::Final { text: safe.clone() });
                self.push(Kind::Final, safe);
                if !self.last_task.is_empty() {
                    self.convo.push((std::mem::take(&mut self.last_task), s.trim().to_string()));
                }
                self.running = false;
                self.save_session();
                crate::notify::task_done(&crate::agent::clip(&s, 80), true);
                // auto-compact when the context window is filling up
                let used = crate::autocompact::estimate_context("", &self.convo);
                if !self.queued_compact
                    && crate::autocompact::should_compact(used, self.usage.context_window, 0.8, self.convo.len(), 6)
                {
                    self.queued_compact = true;
                    self.push(Kind::Info, "context is filling up: auto-compacting…".into());
                }
            }
            AgentEvent::Error(s) => {
                if s.contains("escalating") {
                    self.stats.on_escalation();
                } else {
                    self.stats.on_error();
                }
                self.trace.record(&crate::trace::TraceEvent::Error { text: s.clone() });
                self.push(Kind::Error, s);
                self.running = false;
            }
            AgentEvent::Done => {
                self.running = false;
                self.run_started = None;
                self.current_tool = None;
                self.git_checked = None; // force a git + file refresh after the run
                self.files_checked = None;
                set_terminal_title(&self.idle_title());
            }
        }
    }

    // -------- rendering --------
    fn render(&self, f: &mut Frame) {
        let area = f.area();
        let main_area = if self.sidebar {
            let cols = Layout::horizontal([Constraint::Length(32), Constraint::Min(1)]).split(area);
            self.render_sidebar(f, cols[0]);
            cols[1]
        } else {
            area
        };
        let editor_inner_w = main_area.width.saturating_sub(2);
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(self.editor_height(editor_inner_w)),
            Constraint::Length(1),
        ])
        .split(main_area);

        self.render_header(f, chunks[0]);
        self.render_messages(f, chunks[1]);
        self.render_editor(f, chunks[2]);
        self.render_status(f, chunks[3]);

        match &self.overlay {
            Overlay::WhichKey => self.render_whichkey(f, area),
            Overlay::Help => self.render_help(f, area),
            Overlay::Picker(p) => self.render_picker(f, area, p),
            Overlay::None => {}
        }
        if let Some((desc, _)) = &self.pending {
            self.render_approval(f, area, desc);
        }
        if matches!(self.overlay, Overlay::None) && self.pending.is_none() {
            let s = self.input_string();
            if s.starts_with('/') && !s.contains(char::is_whitespace) {
                self.render_slash(f, chunks[2], &s);
            }
        }
    }

    /// Top header strip: wordmark, git branch, model + endpoint, theme.
    fn render_header(&self, f: &mut Frame, area: Rect) {
        use ratatui::style::Color;
        let t = self.theme();
        let g = self.glyphs;
        let cols = Layout::horizontal([Constraint::Min(1), Constraint::Length(18)]).split(area);
        let mut spans = vec![Span::styled(
            " ◆ smolcode ",
            Style::default().fg(Color::Black).bg(t.accent).add_modifier(Modifier::BOLD),
        )];
        if !self.git_branch.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format!("{} {}", g.branch, self.git_branch), Style::default().fg(t.ok)));
            if self.git_dirty {
                spans.push(Span::styled(format!(" {}", g.dirty), Style::default().fg(t.warn)));
            }
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(self.model_label(), Style::default().fg(t.tool).add_modifier(Modifier::BOLD)));
        let host = self
            .base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or(&self.base_url);
        spans.push(Span::styled(format!(" @ {host}"), Style::default().fg(t.dim)));
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(t.bg_alt)),
            cols[0],
        );
        let theme_name = self.themes[self.theme_idx].name;
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("{theme_name} "), Style::default().fg(t.dim))))
                .style(Style::default().bg(t.bg_alt))
                .right_aligned(),
            cols[1],
        );
    }

    fn render_slash(&self, f: &mut Frame, editor: Rect, prefix: &str) {
        let t = self.theme();
        let mut all: Vec<String> = ["/help", "/mode", "/think", "/mcp", "/rules", "/skills", "/skill", "/bg", "/init", "/new", "/sessions", "/rename", "/fork", "/delete", "/timeline", "/stats", "/export", "/search", "/config", "/commit", "/agents", "/models", "/themes", "/files", "/clear", "/quit"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for c in &self.commands {
            all.push(format!("/{}", c.name));
        }
        let matches: Vec<String> = all.into_iter().filter(|c| c.starts_with(prefix)).collect();
        if matches.is_empty() {
            return;
        }
        let h = (matches.len() as u16 + 2).min(10);
        let w = 44.min(editor.width);
        let r = Rect {
            x: editor.x,
            y: editor.y.saturating_sub(h),
            width: w,
            height: h,
        };
        f.render_widget(Clear, r);
        let lines: Vec<Line> = matches
            .iter()
            .map(|c| {
                let custom = self.commands.iter().any(|x| format!("/{}", x.name) == *c);
                Line::from(vec![
                    Span::styled(c.clone(), Style::default().fg(t.ok)),
                    Span::styled(if custom { "  (command)" } else { "" }, Style::default().fg(t.dim)),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.accent))
                    .title(" commands "),
            ),
            r,
        );
    }

    fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let t = self.theme();
        let g = self.glyphs;
        let cap = area.height.saturating_sub(2) as usize;
        let (title, lines): (&str, Vec<Line>) = match self.sidebar_view {
            SidebarView::Files => {
                let focused = self.focus == Focus::Sidebar;
                let sel = self.sidebar_sel.min(self.files.len().saturating_sub(1));
                // Build every row (dir headers + files), remembering which row is
                // the selected file, then window the rows so the selection stays
                // visible (the headers make a pure file-window imprecise).
                let mut rows: Vec<Line> = Vec::new();
                let mut sel_row: Option<usize> = None;
                let mut last_dir = String::new();
                for (i, path) in self.files.iter().enumerate() {
                    let (dir, file) = match path.rfind('/') {
                        Some(j) => (&path[..j], &path[j + 1..]),
                        None => ("", path.as_str()),
                    };
                    if dir != last_dir {
                        last_dir = dir.to_string();
                        let label = if dir.is_empty() { ".".to_string() } else { format!("{dir}/") };
                        rows.push(Line::from(Span::styled(
                            format!("{} {}", g.dir, label),
                            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                        )));
                    }
                    let is_sel = focused && i == sel;
                    if i == sel {
                        sel_row = Some(rows.len());
                    }
                    let style = if is_sel {
                        Style::default().fg(t.ok).add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    } else {
                        Style::default().fg(t.fg)
                    };
                    rows.push(Line::from(vec![
                        Span::styled(if is_sel { "❯ " } else { "  " }, style),
                        Span::styled(format!("{} {file}", g.file), style),
                    ]));
                }
                let total = rows.len();
                let start = if total <= cap {
                    0
                } else {
                    let s = sel_row.map(|r| r.saturating_sub(cap.saturating_sub(1))).unwrap_or(0);
                    s.min(total - cap)
                };
                let mut lines: Vec<Line> = rows.into_iter().skip(start).take(cap).collect();
                if total > cap && start + cap < total {
                    if let Some(last) = lines.last_mut() {
                        *last = Line::from(Span::styled(
                            format!("  … +{} more", total - (start + cap) + 1),
                            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                        ));
                    }
                }
                (if focused { "files ▸" } else { "files" }, lines)
            }
            SidebarView::Stats => {
                let mut lines: Vec<Line> = vec![
                    Line::from(Span::styled(crate::agent::clip(&self.session_id, 26), Style::default().fg(t.dim))),
                    Line::from(""),
                ];
                for l in self.stats.summary().lines() {
                    lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(t.fg))));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(format!("ctx {}", self.usage.label()), Style::default().fg(self.usage.color()))));
                ("stats", lines)
            }
        };
        let title_line = Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
        let border = if self.focus == Focus::Sidebar { t.accent } else { t.border };
        f.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(border))
                        .title(title_line),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    /// Visible content rows the editor caps at (excluding borders).
    const EDITOR_MAX_ROWS: u16 = 6;

    /// Char-wrap the input buffer at `width` columns into display rows, and
    /// report where the cursor lands (row, col) within them. This is the same
    /// wrapping the editor renders with, so the cursor always tracks the text.
    fn wrap_input(&self, width: u16) -> (Vec<String>, u16, u16) {
        wrap_chars(&self.input, self.cursor, width)
    }

    /// Editor box height (borders included), grown to fit wrapped input up to a
    /// cap. `inner_width` is the available text width (box width minus borders).
    fn editor_height(&self, inner_width: u16) -> u16 {
        let (rows, _, _) = self.wrap_input(inner_width);
        (rows.len() as u16).clamp(1, Self::EDITOR_MAX_ROWS) + 2
    }

    fn render_messages(&self, f: &mut Frame, area: Rect) {
        let t = self.theme();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(" conversation ", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)));

        // first-run splash: empty transcript and no prior turns
        if self.lines.is_empty() && self.convo.is_empty() && self.partial.is_empty() {
            f.render_widget(self.splash().block(block), area);
            return;
        }

        let mut text: Vec<Line> = Vec::new();
        for m in &self.lines {
            text.extend(self.message_lines(m));
        }
        // live streaming tokens (current, not-yet-finalized assistant text)
        if !self.partial.is_empty() {
            let blink = (self.spinner / 4) % 2 == 0;
            let cursor = if blink { "▏" } else { " " };
            let lines: Vec<&str> = self.partial.split('\n').collect();
            for (i, raw) in lines.iter().enumerate() {
                let last = i + 1 == lines.len();
                let mut spans = vec![Span::styled(format!("{} ", self.glyphs.assistant), Style::default().fg(t.assistant))];
                spans.push(Span::styled(raw.to_string(), Style::default().fg(t.assistant)));
                if last {
                    spans.push(Span::styled(cursor.to_string(), Style::default().fg(t.accent)));
                }
                text.push(Line::from(spans));
            }
        }

        // animated "thinking" indicator while the agent works (no approval modal up)
        if self.running && self.pending.is_none() {
            let star = self.glyphs.thinking[self.spinner % self.glyphs.thinking.len()];
            let elapsed = self.run_started.map(|i| i.elapsed().as_secs()).unwrap_or(0);
            let word = self.thinking_word(elapsed);
            let toks = self.stats.est_tokens;
            let tok_s = if toks >= 1000 {
                format!("~{:.1}k tok", toks as f64 / 1000.0)
            } else {
                format!("~{toks} tok")
            };
            text.push(Line::from(vec![
                Span::styled(format!("{star} "), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{word}… "), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(format!("({elapsed}s · {tok_s} · esc to interrupt)"), Style::default().fg(t.dim)),
            ]));
        }

        // Count rows AFTER word-wrapping at the inner width, not logical lines,
        // so the auto-scroll actually reaches the bottom when messages wrap.
        let inner_w = area.width.saturating_sub(2);
        let view_h = area.height.saturating_sub(2);
        let p = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
        let total = p.line_count(inner_w) as u16;
        let max_scroll = total.saturating_sub(view_h);
        let scroll = max_scroll.saturating_sub(self.offset_from_bottom);
        f.render_widget(p.scroll((scroll, 0)), area);

        // scrollbar (only when there's overflow)
        if max_scroll > 0 {
            let mut sb_state = ScrollbarState::new(max_scroll as usize).position(scroll as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .thumb_style(Style::default().fg(t.accent))
                    .track_style(Style::default().fg(t.border))
                    .begin_symbol(None)
                    .end_symbol(None),
                area,
                &mut sb_state,
            );
        }
    }

    /// Cached, role-decorated render lines for one message (rebuilt when the
    /// theme changes). Avoids re-running syntect every frame.
    fn message_lines(&self, m: &Msg) -> Vec<Line<'static>> {
        {
            let c = m.cache.borrow();
            if let Some((epoch, lines)) = c.as_ref() {
                if *epoch == self.theme_epoch {
                    return lines.clone();
                }
            }
        }
        let built = self.build_message_lines(m);
        *m.cache.borrow_mut() = Some((self.theme_epoch, built.clone()));
        built
    }

    fn build_message_lines(&self, m: &Msg) -> Vec<Line<'static>> {
        use ratatui::style::Color;
        let t = self.theme();
        let g = self.glyphs;
        let mut out: Vec<Line<'static>> = Vec::new();

        // Tool call: a compact card — icon + colored verb chip + dim args.
        if matches!(m.kind, Kind::Tool) {
            let (verb, rest) = m.text.split_once(' ').unwrap_or((m.text.as_str(), ""));
            let vc = match verb {
                "read" | "list" | "search" | "outline" | "tree" | "find_symbol" | "find_context"
                | "repo_map" | "project_info" => t.tool,
                "write" | "edit" => t.warn,
                "shell" | "run" => t.accent,
                v if v.starts_with("git") => t.ok,
                _ => t.tool,
            };
            out.push(Line::from(vec![
                Span::styled(format!("{} ", g.tool), Style::default().fg(vc)),
                Span::styled(format!(" {verb} "), Style::default().fg(Color::Black).bg(vc).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {}", rest.trim()), Style::default().fg(t.dim)),
            ]));
            return out;
        }

        let (icon, bar_color, body_style): (&str, Color, Style) = match m.kind {
            Kind::User => (g.user, t.user, Style::default().fg(t.fg).add_modifier(Modifier::BOLD)),
            Kind::Assistant => (g.assistant, t.assistant, Style::default().fg(t.assistant)),
            Kind::Result => (g.result, t.dim, Style::default().fg(t.dim)),
            Kind::Final => (g.final_, t.ok, Style::default().fg(t.ok).add_modifier(Modifier::BOLD)),
            Kind::Error => (g.error, t.warn, Style::default().fg(t.warn)),
            Kind::Info => (g.info, t.dim, Style::default().fg(t.dim).add_modifier(Modifier::ITALIC)),
            Kind::Tool => unreachable!(),
        };
        let result_bg = matches!(m.kind, Kind::Result);

        let mut in_code = false;
        let mut code_buf: Vec<String> = Vec::new();
        let mut code_lang = String::new();
        let mut first = true;
        for raw in m.text.split('\n') {
            if raw.trim_start().starts_with("```") {
                if !in_code {
                    in_code = true;
                    code_lang = raw.trim_start().trim_start_matches('`').trim().to_string();
                    code_buf.clear();
                    let label = if code_lang.is_empty() { "code".to_string() } else { code_lang.clone() };
                    out.push(Line::from(Span::styled(format!("    ╭─ {label} "), Style::default().fg(t.dim))));
                } else {
                    for segs in crate::syntax::highlight(&code_buf.join("\n"), &code_lang) {
                        let mut spans = vec![Span::styled("    │ ".to_string(), Style::default().fg(t.border))];
                        for (col, s) in segs {
                            spans.push(Span::styled(s, Style::default().fg(col)));
                        }
                        out.push(Line::from(spans));
                    }
                    out.push(Line::from(Span::styled("    ╰─".to_string(), Style::default().fg(t.dim))));
                    in_code = false;
                    code_buf.clear();
                }
                continue;
            }
            if in_code {
                code_buf.push(raw.to_string());
                continue;
            }
            // gutter: icon on the first line, accent bar after.
            let (glyph, mut gutter_style) = if first {
                (format!("{} ", icon), Style::default().fg(bar_color).add_modifier(Modifier::BOLD))
            } else {
                (format!("{} ", g.bar), Style::default().fg(bar_color))
            };
            first = false;
            // diff tinting for result blocks
            let mut bstyle = match raw.chars().next() {
                Some('+') if result_bg => Style::default().fg(t.user),
                Some('-') if result_bg => Style::default().fg(t.warn),
                _ => body_style,
            };
            if result_bg {
                bstyle = bstyle.bg(t.bg_alt);
                gutter_style = gutter_style.bg(t.bg_alt);
            }
            out.push(Line::from(vec![
                Span::styled(glyph, gutter_style),
                Span::styled(raw.to_string(), bstyle),
            ]));
        }
        if in_code && !code_buf.is_empty() {
            for segs in crate::syntax::highlight(&code_buf.join("\n"), &code_lang) {
                let mut spans = vec![Span::styled("    │ ".to_string(), Style::default().fg(t.border))];
                for (col, s) in segs {
                    spans.push(Span::styled(s, Style::default().fg(col)));
                }
                out.push(Line::from(spans));
            }
        }
        out
    }

    /// First-run welcome splash.
    fn splash(&self) -> Paragraph<'static> {
        let t = self.theme();
        // HF yellow (#FFD21E) for the wordmark — matches the web UI branding.
        let hf = Style::default()
            .fg(ratatui::style::Color::Rgb(0xFF, 0xD2, 0x1E))
            .add_modifier(Modifier::BOLD);
        let d = Style::default().fg(t.dim);
        let mode = crate::theme::Theme::mode_label(self.agent().read_only, self.tools_yolo);
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled("                 _            _", hf)),
            Line::from(Span::styled("    ____ __  ___| |__ ___  __| |___", hf)),
            Line::from(Span::styled("   (_-< '  \\/ _ \\ / _/ _ \\/ _` / -_)", hf)),
            Line::from(Span::styled("   /__/_|_|_\\___/_\\__\\___/\\__,_\\___|", hf)),
            Line::from(""),
            Line::from(Span::styled("   an SLM-optimized coding agent  ·  Rust + LiteForge", d)),
            Line::from(""),
            Line::from(vec![
                Span::styled("   model ", d),
                Span::styled(self.model_label(), Style::default().fg(t.tool).add_modifier(Modifier::BOLD)),
                Span::styled(format!("   mode {mode}"), d),
            ]),
            Line::from(""),
            Line::from(Span::styled("   Type a task and press Enter.", Style::default().fg(t.fg))),
            Line::from(Span::styled("   Shift+Tab cycle mode  ·  / commands  ·  @ files  ·  ctrl+x leader  ·  ctrl+x h help", d)),
        ];
        Paragraph::new(lines)
    }

    fn render_editor(&self, f: &mut Frame, area: Rect) {
        let t = self.theme();
        let title = if self.running {
            let sp = self.glyphs.spinner[self.spinner % self.glyphs.spinner.len()];
            let secs = self.run_started.map(|i| i.elapsed().as_secs()).unwrap_or(0);
            let tool = self.current_tool.clone().unwrap_or_else(|| "thinking".into());
            let toks = self.stats.est_tokens;
            let tok_s = if toks >= 1000 { format!("~{:.1}k tok", toks as f64 / 1000.0) } else { format!("~{toks} tok") };
            format!(" {sp} {secs}s · {tool} · {tok_s} · Esc interrupt ")
        } else {
            format!(" {}  (Enter run · ↑ history · Shift+Tab mode · / commands · ctrl+x leader) ", self.agent().name)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if self.running { t.warn } else { t.accent }))
            .title(Span::styled(title, Style::default().fg(if self.running { t.warn } else { t.dim })));

        let inner_w = area.width.saturating_sub(2);
        let max_rows = area.height.saturating_sub(2).max(1);
        if self.input.is_empty() && !self.running {
            let p = Paragraph::new(Line::from(Span::styled("type a task…", Style::default().fg(t.dim)))).block(block);
            f.render_widget(p, area);
            if matches!(self.overlay, Overlay::None) && self.pending.is_none() {
                f.set_cursor_position(Position::new(area.x + 1, area.y + 1));
            }
            return;
        }

        // Char-wrap exactly as wrap_input measured, then scroll so the cursor row
        // is always on screen (long lines wrap and stay visible while typing).
        let (rows, cur_row, cur_col) = self.wrap_input(inner_w);
        let offset = cur_row.saturating_sub(max_rows.saturating_sub(1));
        let visible: Vec<Line> = rows
            .iter()
            .skip(offset as usize)
            .take(max_rows as usize)
            .map(|r| Line::from(Span::styled(r.clone(), Style::default().fg(t.fg))))
            .collect();
        f.render_widget(Paragraph::new(visible).block(block), area);
        if !self.running && matches!(self.overlay, Overlay::None) && self.pending.is_none() {
            f.set_cursor_position(Position::new(
                area.x + 1 + cur_col.min(inner_w.saturating_sub(1)),
                area.y + 1 + (cur_row - offset),
            ));
        }
    }

    fn render_status(&self, f: &mut Frame, area: Rect) {
        use ratatui::style::Color;
        let t = self.theme();
        let cols = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
        let sess = if self.session_title.is_empty() {
            "new session".to_string()
        } else {
            self.session_title.clone()
        };
        // left: brand badge + session chip + cwd
        let left = Line::from(vec![
            Span::styled(" smolcode ", Style::default().fg(Color::Black).bg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {} ", crate::agent::clip(&sess, 32)), Style::default().fg(t.fg).bg(t.bg_alt)),
            Span::styled(format!(" {}", crate::agent::clip(&self.root, 36)), Style::default().fg(t.dim)),
        ]);
        f.render_widget(Paragraph::new(left), cols[0]);

        // right: agent chip · MODE badge · model · usage gauge · theme
        let ro = self.agent().read_only;
        let mode = crate::theme::Theme::mode_label(ro, self.tools_yolo);
        let mode_bg = t.mode_color(ro, self.tools_yolo);
        let flashing = self.mode_flash.map(|i| i.elapsed().as_millis() < 500).unwrap_or(false);
        let mut mode_style = Style::default().fg(Color::Black).bg(mode_bg).add_modifier(Modifier::BOLD);
        if flashing {
            mode_style = mode_style.add_modifier(Modifier::REVERSED);
        }
        let mut spans = vec![
            Span::styled(format!(" {} ", self.agent().name), Style::default().fg(t.fg).bg(t.bg_alt).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(format!(" {} ", mode), mode_style),
        ];
        // thinking-effort chip (only when elevated, to keep the bar uncluttered)
        if self.think != crate::router::Think::Off {
            let col = if self.think.forces_top() { t.accent } else { t.warn };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!(" think:{} ", self.think.label()),
                Style::default().fg(Color::Black).bg(col).add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend([
            Span::raw("  "),
            Span::styled(format!("{} ", self.model_label()), Style::default().fg(t.tool)),
            Span::styled(self.usage.label(), Style::default().fg(self.usage.color()).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {} ", self.themes[self.theme_idx].name), Style::default().fg(t.dim)),
        ]);
        let right = Line::from(spans).right_aligned();
        f.render_widget(Paragraph::new(right), cols[1]);
    }

    fn render_approval(&self, f: &mut Frame, area: Rect, desc: &str) {
        let t = self.theme();
        let r = centered_rect(70, 20, area);
        f.render_widget(Clear, r);
        let body = Paragraph::new(vec![
            Line::from(Span::styled("Approve this action?", Style::default().fg(t.warn).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(Span::styled(desc.to_string(), Style::default().fg(t.fg))),
            Line::from(""),
            Line::from(vec![
                Span::styled("[y]", Style::default().fg(t.user).add_modifier(Modifier::BOLD)),
                Span::raw(" approve   "),
                Span::styled("[n]", Style::default().fg(t.warn).add_modifier(Modifier::BOLD)),
                Span::raw(" deny"),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(t.warn)).title(" approval "));
        f.render_widget(body, r);
    }

    fn render_whichkey(&self, f: &mut Frame, area: Rect) {
        let t = self.theme();
        let r = centered_rect(50, 40, area);
        f.render_widget(Clear, r);
        let items = [
            ("n", "new session"),
            ("l", "sessions"),
            ("b", "toggle sidebar"),
            ("s", "sidebar files/stats"),
            ("f", "focus file tree"),
            ("m", "models"),
            ("a", "agents"),
            ("t", "themes"),
            ("o", "cycle mode"),
            ("e", "thinking effort"),
            ("h", "help"),
            ("y", "copy last reply"),
            ("c", "compact"),
            ("q", "quit"),
        ];
        let mut lines = vec![Line::from(Span::styled("ctrl+x →", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)))];
        for (k, d) in items {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k}  "), Style::default().fg(t.ok).add_modifier(Modifier::BOLD)),
                Span::styled(d, Style::default().fg(t.fg)),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(t.accent)).title(" which-key ")),
            r,
        );
    }

    fn render_help(&self, f: &mut Frame, area: Rect) {
        let t = self.theme();
        let r = centered_rect(64, 60, area);
        f.render_widget(Clear, r);
        let lines = vec![
            Line::from(Span::styled("smolcode — keys", Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from("Enter            run task"),
            Line::from("Alt/Shift+Enter  newline"),
            Line::from("Up / Down        recall previous prompts"),
            Line::from("Shift+Tab / ^x o cycle mode (edit / auto / plan); also /mode"),
            Line::from("Tab              cycle agent (build/plan/explore/review)"),
            Line::from("F2               cycle model"),
            Line::from("@ / /            file picker / command palette"),
            Line::from("! <cmd>          run a shell command (no LLM)"),
            Line::from("/mcp             list connected MCP servers + tools"),
            Line::from("/rules /skills   list active rules / skills (/skill <name> to run)"),
            Line::from("ctrl+x e /think  reasoning effort (high/xtra → top-tier model)"),
            Line::from("ctrl+x f         focus file tree (↑/↓ move, Enter open in $EDITOR)"),
            Line::from("ctrl+x           leader (then m/a/t/b/h/y/c/q)"),
            Line::from("ctrl+z / ctrl+y  undo / redo file edits"),
            Line::from("ctrl+l           clear the transcript"),
            Line::from("PgUp / PgDn      scroll"),
            Line::from("Esc              interrupt run / close popup"),
            Line::from("ctrl+c           interrupt, clear input, or quit (x2)"),
            Line::from("ctrl+d           quit (empty prompt)"),
            Line::from(""),
            Line::from(Span::styled("press any key to close", Style::default().fg(t.dim))),
        ];
        f.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(t.accent)).title(" help ")),
            r,
        );
    }

    fn render_picker(&self, f: &mut Frame, area: Rect, p: &Picker) {
        let t = self.theme();
        let r = centered_rect(60, 60, area);
        f.render_widget(Clear, r);
        let inner = Layout::vertical([Constraint::Length(1), Constraint::Min(1)])
            .margin(1)
            .split(r);
        f.render_widget(
            Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(t.accent)).title(format!(" {} ", p.title)),
            r,
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("› ", Style::default().fg(t.accent)),
                Span::styled(p.filter.clone(), Style::default().fg(t.fg)),
                Span::styled("▏", Style::default().fg(t.dim)),
            ])),
            inner[0],
        );
        let mut lines: Vec<Line> = Vec::new();
        for (vis_i, (_real, item)) in p.filtered().into_iter().enumerate() {
            let selected = vis_i == p.sel;
            let style = if selected {
                Style::default().fg(t.ok).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            let marker = if selected { "❯ " } else { "  " };
            lines.push(Line::from(vec![Span::styled(marker, style), Span::styled(item.clone(), style)]));
        }
        f.render_widget(Paragraph::new(lines), inner[1]);
    }
}

/// Friendly one-line summary of a tool call (instead of dumping JSON args).
/// Spawn the background task that reads terminal events and forwards them on
/// `tx`. Returned handle can be `.abort()`ed to release stdin (e.g. while a
/// child `$EDITOR` runs), then re-spawned to resume input.
fn spawn_event_reader(tx: mpsc::Sender<Event>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut es = EventStream::new();
        while let Some(Ok(ev)) = es.next().await {
            if tx.send(ev).await.is_err() {
                break;
            }
        }
    })
}

/// Set the terminal window/tab title via the OSC sequence (crossterm `SetTitle`).
fn set_terminal_title(s: &str) {
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(s));
}

/// Char-wrap `input` at `width` columns into display rows and locate the cursor
/// (row, col) within them. Honours hard `\n` breaks and soft-wraps long runs so
/// the editor can grow/scroll and keep the cursor visible while typing.
fn wrap_chars(input: &[char], cursor: usize, width: u16) -> (Vec<String>, u16, u16) {
    let width = width.max(1) as usize;
    let mut rows: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut col = 0usize;
    let mut cur_row = 0u16;
    let mut cur_col = 0u16;
    for (i, &c) in input.iter().enumerate() {
        if i == cursor {
            cur_row = rows.len() as u16;
            cur_col = col as u16;
        }
        if c == '\n' {
            rows.push(std::mem::take(&mut line));
            col = 0;
        } else {
            line.push(c);
            col += 1;
            if col == width {
                rows.push(std::mem::take(&mut line));
                col = 0;
            }
        }
    }
    if cursor >= input.len() {
        cur_row = rows.len() as u16;
        cur_col = col as u16;
    }
    rows.push(line);
    (rows, cur_row, cur_col)
}

fn summarize_call(name: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    match name {
        "write_file" => format!("write {}", g("path")),
        "str_replace" => format!("edit {}", g("path")),
        "read_file" => format!("read {}", g("path")),
        "list_dir" => {
            let p = g("path");
            format!("list {}", if p.is_empty() { "." } else { p })
        }
        "run_shell" => format!("shell  {}", crate::agent::clip(g("command"), 90)),
        "run_python" => format!("run {}", g("path")),
        other => format!("{other}  {}", crate::agent::clip(args, 80)),
    }
}

fn cap_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max {
        s.trim_end().to_string()
    } else {
        let mut out = lines[..max].join("\n");
        out.push_str(&format!("\n…(+{} more lines)", lines.len() - max));
        out
    }
}

/// Recursively list project files (relative paths), skipping junk dirs. Capped.
fn scan_files(root: &std::path::Path) -> Vec<String> {
    fn skip_dir(name: &str) -> bool {
        name.starts_with('.')
            || matches!(
                name,
                "target" | "node_modules" | "__pycache__" | "dist" | "build" | "vendor"
            )
    }
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
        if out.len() >= 4000 {
            return;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => {
                    if !skip_dir(&name) {
                        walk(&path, root, out);
                    }
                }
                Ok(ft) if ft.is_file() => {
                    if !name.starts_with('.') {
                        if let Ok(rel) = path.strip_prefix(root) {
                            out.push(rel.to_string_lossy().to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Mirror of PermissionSet::for_agent, as a label for the /config view.
fn perm_label(read_only: bool, yolo: bool, cap: &str) -> &'static str {
    if cap == "read" {
        return "allow";
    }
    if read_only {
        "deny"
    } else if yolo {
        "allow"
    } else {
        "ask"
    }
}

fn centered_rect(px: u16, py: u16, area: Rect) -> Rect {
    let w = area.width * px / 100;
    let h = area.height * py / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w, height: h }
}

async fn fetch_models(client: &AsyncForgeClient, current: &str) -> Vec<String> {
    let mut models: Vec<String> = match client.list_models().await {
        Ok(list) => list.data.into_iter().map(|m| m.id).collect(),
        Err(_) => Vec::new(),
    };
    if models.is_empty() {
        models = vec![current.to_string()];
    }
    if !models.iter().any(|m| m == current) {
        models.insert(0, current.to_string());
    }
    models
}

async fn recv_opt(rx: &mut Option<mpsc::Receiver<AgentEvent>>) -> Option<AgentEvent> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

async fn recv_compact(rx: &mut Option<mpsc::Receiver<String>>) -> Option<String> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: AsyncForgeClient,
    model: String,
    base_url: String,
    root: PathBuf,
    yolo: bool,
    agent_name: String,
    hooks: Hooks,
    mcp: std::sync::Arc<crate::mcp_tools::McpTools>,
    resume: bool,
) -> Result<()> {
    let models = fetch_models(&client, &model).await;
    let model_idx = models.iter().position(|m| m == &model).unwrap_or(0);
    let agents = prompts::builtin();
    let agent_idx = agents.iter().position(|a| a.name == agent_name).unwrap_or(0);
    let mut theme_list = themes();
    theme_list.extend(crate::themes_extra::extra_themes());
    let files = scan_files(&root);
    let commands = crate::commands::load(&root);

    let model_for_usage = model.clone();
    let sid = crate::session::new_id();
    let trace_on = std::env::var("SMOLCODE_TRACE").map(|v| v != "0").unwrap_or(true);
    let trace = crate::trace::Trace::new(&sid, trace_on);
    let mut app = App {
        client,
        model,
        base_url,
        root: root.display().to_string(),
        root_path: root,
        tools_yolo: yolo,
        hooks,
        agents,
        agent_idx,
        models,
        model_idx,
        themes: theme_list,
        theme_idx: 0,
        theme_epoch: 0,
        glyphs: crate::glyphs::Glyphs::from_env(),
        input: Vec::new(),
        cursor: 0,
        input_history: Vec::new(),
        history_pos: None,
        draft: Vec::new(),
        lines: Vec::new(),
        convo: Vec::new(),
        last_task: String::new(),
        partial: String::new(),
        usage: crate::usage::Usage::new(&model_for_usage),
        trace,
        stats: crate::stats::Stats::new(),
        session_id: sid,
        session_title: String::new(),
        session_metas: Vec::new(),
        offset_from_bottom: 0,
        sidebar: true,
        sidebar_view: SidebarView::Files,
        focus: Focus::Editor,
        sidebar_sel: 0,
        open_in_editor: None,
        think: crate::router::Think::Off,
        needs_model_pick: true,
        files,
        files_checked: None,
        git_branch: String::new(),
        git_dirty: false,
        git_checked: None,
        run_started: None,
        current_tool: None,
        mode_flash: None,
        commands,
        queued_task: None,
        queued_compact: false,
        undo: std::sync::Arc::new(std::sync::Mutex::new(crate::undo::UndoStack::new())),
        mcp,
        leader: false,
        ctrl_c_armed: false,
        overlay: Overlay::None,
        running: false,
        pending: None,
        task: None,
        spinner: 0,
        quit: false,
    };
    if resume {
        if let Some(latest) = crate::session::list().into_iter().next() {
            app.load_session(&latest.id);
            app.push(Kind::Info, format!("resumed session: {}", latest.title));
            app.needs_model_pick = false; // continuing a session -> keep its model
        }
        // (fresh start shows the welcome splash instead of info lines)
    }

    app.refresh_git();
    // Blocking startup model pick: default to Auto (router-driven) and open the modal.
    if app.needs_model_pick {
        app.model = "auto".to_string();
        app.open_picker(PickerKind::Models);
    }
    let mut terminal = ratatui::init();
    // bracketed paste: lets the terminal deliver a paste as one Event::Paste
    // instead of a burst of keystrokes (whose embedded newlines would submit).
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    set_terminal_title(&app.idle_title());

    let (in_tx, mut in_rx) = mpsc::channel::<Event>(128);
    let mut reader = spawn_event_reader(in_tx.clone());

    let mut agent_rx: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut compact_rx: Option<mpsc::Receiver<String>> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(110));

    loop {
        terminal.draw(|f| app.render(f))?;
        tokio::select! {
            Some(ev) = in_rx.recv() => {
                app.on_term_event(ev, &mut agent_rx);
                // open a file in $EDITOR: suspend the TUI, run it foreground, resume.
                if let Some(path) = app.open_in_editor.take() {
                    let editor = std::env::var("VISUAL")
                        .or_else(|_| std::env::var("EDITOR"))
                        .unwrap_or_else(|_| "vi".into());
                    reader.abort(); // release stdin so the child editor owns it
                    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
                    ratatui::restore();
                    let _ = std::process::Command::new(&editor).arg(&path).status();
                    terminal = ratatui::init();
                    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
                    set_terminal_title(&app.idle_title());
                    let _ = terminal.clear();
                    app.files_checked = None; // the edit may have changed files
                    reader = spawn_event_reader(in_tx.clone()); // resume input
                }
                if let Some(t) = app.queued_task.take() {
                    app.input = t.chars().collect();
                    app.cursor = app.input.len();
                    app.submit(&mut agent_rx);
                }
                if app.queued_compact {
                    app.queued_compact = false;
                    let (tx, rx) = mpsc::channel::<String>(1);
                    let (c, m, convo) = (app.client.clone(), app.model.clone(), app.convo.clone());
                    tokio::spawn(async move {
                        let s = crate::context::compact(&c, &m, &convo).await;
                        let _ = tx.send(s).await;
                    });
                    compact_rx = Some(rx);
                    app.push(Kind::Info, "compacting prior turns…".into());
                }
            }
            aev = recv_opt(&mut agent_rx) => match aev {
                Some(a) => app.on_agent_event(a),
                None => { agent_rx = None; app.running = false; }
            },
            cs = recv_compact(&mut compact_rx) => {
                compact_rx = None;
                if let Some(summary) = cs {
                    if !summary.trim().is_empty() {
                        app.convo = vec![("(summary of earlier work)".into(), summary.trim().to_string())];
                        app.push(Kind::Info, "compacted prior turns into a summary".into());
                    }
                }
            }
            _ = ticker.tick() => { app.spinner = app.spinner.wrapping_add(1); app.refresh_git(); app.refresh_files(); }
        }
        if app.quit {
            break;
        }
    }

    app.save_session();
    crate::bgproc::stop_all(); // don't leak background dev servers on exit
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wrap_chars;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn short_line_is_one_row_cursor_at_end() {
        let c = chars("hello");
        let (rows, r, col) = wrap_chars(&c, c.len(), 20);
        assert_eq!(rows, vec!["hello".to_string()]);
        assert_eq!((r, col), (0, 5));
    }

    #[test]
    fn long_line_soft_wraps_into_multiple_rows() {
        // 25 chars at width 10 -> rows of 10,10,5
        let c = chars(&"x".repeat(25));
        let (rows, r, col) = wrap_chars(&c, c.len(), 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].chars().count(), 10);
        assert_eq!(rows[2].chars().count(), 5);
        // cursor at end sits on the third row, column 5
        assert_eq!((r, col), (2, 5));
    }

    #[test]
    fn cursor_wraps_to_next_row_at_boundary() {
        // cursor right after filling row 0 (width 10) -> row 1, col 0
        let c = chars(&"y".repeat(15));
        let (_rows, r, col) = wrap_chars(&c, 10, 10);
        assert_eq!((r, col), (1, 0));
    }

    #[test]
    fn hard_newlines_make_rows() {
        let c = chars("a\nbb\nccc");
        let (rows, r, col) = wrap_chars(&c, c.len(), 80);
        assert_eq!(rows, vec!["a".to_string(), "bb".to_string(), "ccc".to_string()]);
        assert_eq!((r, col), (2, 3));
    }

    #[test]
    fn empty_input_is_one_empty_row() {
        let (rows, r, col) = wrap_chars(&[], 0, 40);
        assert_eq!(rows, vec![String::new()]);
        assert_eq!((r, col), (0, 0));
    }
}
