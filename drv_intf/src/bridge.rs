use crate::ui;
use anyhow::{Context, Result, bail, ensure};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as KeyEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    env,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use uke::clean;

mod matrix;

/// The console's view of one PID. The kernel owns the PID; this is what the UI draws.
pub struct Agent {
    pub pid: usize,
    pub parent: usize,
    pub rank: u8,
    pub name: String,
    pub mission: String,
    pub state: String,
    pub model: String,
    pub transcript: String,
    pub reply: String,
    pub success: bool,
    pub needs_captain: bool,
    pub turn_active: bool,
    /// Mail waiting in the kernel for this PID.
    pub queue: usize,
    pub harness: String,
    pub effort: String,
    pub pulse: Instant,
    pub unread: bool,
    /// attached · driven
    pub kind: String,
    /// A console seat (ACP or demo CPU) driven for this console.
    pub console: bool,
    pub nesting: bool,
}
impl Agent {
    fn placeholder(pid: usize) -> Self {
        Self {
            pid,
            parent: 0,
            rank: 1,
            name: format!("PID {pid}"),
            mission: String::new(),
            state: "offline".into(),
            model: "unknown".into(),
            transcript: String::new(),
            reply: String::new(),
            success: false,
            needs_captain: false,
            turn_active: false,
            queue: 0,
            harness: String::new(),
            effort: "unknown".into(),
            pulse: Instant::now(),
            unread: false,
            kind: String::new(),
            console: false,
            nesting: false,
        }
    }
    /// Applies the kernel's view; returns the previous state when it changed.
    fn apply(&mut self, v: &Value) -> Option<String> {
        let s = |k: &str| v[k].as_str().map(str::to_owned);
        self.parent = v["parent"].as_u64().unwrap_or(0) as usize;
        self.rank = v["rank"].as_u64().unwrap_or(1) as u8;
        if let Some(n) = s("name") {
            self.name = n;
        }
        if let Some(m) = s("task") {
            self.mission = m;
        }
        if let Some(h) = s("harness") {
            self.harness = h;
        }
        if let Some(m) = s("model") {
            self.model = m;
        }
        if let Some(e) = s("effort") {
            self.effort = e;
        }
        if let Some(k) = s("kind") {
            self.kind = k;
        }
        self.console = v["console"] == true;
        self.queue = v["queue"].as_u64().unwrap_or(0) as usize;
        self.turn_active = v["turn_active"] == true || v["state"] == "working";
        self.needs_captain = v["needs_captain"] == true;
        self.nesting = v["nesting"] == true;
        if !self.console && self.model == "unknown" {
            self.model = format!("{} · {}", self.kind, self.harness);
        }
        let state = s("state").unwrap_or_else(|| "offline".into());
        if state != self.state {
            let old = std::mem::replace(&mut self.state, state);
            self.pulse = Instant::now();
            return Some(old);
        }
        None
    }
}
impl uke::CrewSeat for Agent {
    fn state(&self) -> &str {
        &self.state
    }
    fn mission(&self) -> &str {
        &self.mission
    }
}
pub struct Ship {
    pub obs: uke::Obs<Agent, drv_hdff::HandoffSummary>,
    pub palette: Option<crate::menu::Palette>,
    pub game: crate::game::Game,
    pub onboard: Option<usize>,
    pub developer: bool,
    /// Read-only view of the universe memory files (resting screen); writes go through
    /// the kernel as the captain.
    pub memory: uke::MemoryIndex,
    pub memory_output: Option<String>,
    pub comms_view: bool,
    quit: bool,
    pub selected: usize,
    pub input: String,
    pub mission_input: bool,
    pub demo: bool,
    pub telemetry: bool,
    pub logs: bool,
    pub log_filter: Option<&'static str>,
    pub log_query: String,
    pub expanded: bool,
    pub slash_selected: usize,
    /// COMMS draft set aside while a slash command is typed over the log search.
    stash: Option<String>,
    pub matrix_view: bool,
    pub scroll: u16,
    pub last_lines: usize,
    pub tick: u64,

    pub notice: String,
    pub motion: bool,
    /// DrvBoot picker backend for every Boot this cockpit starts (`/picker`).
    pub picker: uke::Backend,
    /// The admitted mission new seats nest on (`/admit`, `/missions`).
    pub mission: Option<crate::nest::MissionRow>,
    missions: Vec<crate::nest::MissionRow>,
    pub started: Instant,
    /// Seat recorders written by the kernel.
    dir: PathBuf,
    console: crate::console::Console,
    events: Receiver<Wire>,
    tx: Sender<Wire>,
    journal: File,

    matrix: Option<matrix::HandoffMatrix>,
    /// Run DrvBoot when the kernel asks for a nest (tests drive outcomes by hand).
    nest_auto: bool,
    session: Option<PathBuf>,
    dirty: bool,
    saved: Instant,
    clock_base: u64,
    restart: bool,
}
enum Wire {
    Nest(Box<crate::nest::Outcome>),
    Kernel(Value),
}
fn wire(v: Value) -> Wire {
    Wire::Kernel(v)
}
fn random_tag() -> Result<String> {
    let mut b = [0u8; 4];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}
impl Ship {
    /// Fresh demo flight in a throwaway universe with an in-process kernel (tests).
    #[cfg(test)]
    fn new(demo: bool) -> Result<Self> {
        let root = tests::universe();
        Self::launch_at(&root, demo, true, None)
    }
    /// Cockpit flight: attaches to the universe kernel (starting it if needed), restores
    /// the console's own view preferences, and keeps them saved.
    fn open(demo: bool, fresh: bool) -> Result<Self> {
        let root = env::current_dir()?;
        Self::launch_at(
            &root,
            demo,
            fresh,
            Some(root.join(crate::session::path(demo))),
        )
    }
    fn launch_at(root: &Path, demo: bool, fresh: bool, session: Option<PathBuf>) -> Result<Self> {
        let (tx, events) = mpsc::channel();
        let exe = env::current_exe()?;
        let (console, hello) = crate::console::Console::attach(root, &exe, tx.clone(), wire)?;
        let root = console.universe.root().to_path_buf();
        let dir = root.join(".unvrs/kernel/seats");
        fs::create_dir_all(&dir)?;
        let tag = format!("{}-{}", std::process::id(), random_tag()?);
        let journal = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(format!(".unvrs/flight-{tag}.jsonl")))?;
        let mut s = Self {
            obs: uke::Obs::default(),
            palette: None,
            quit: false,
            game: crate::game::Game::default(),
            onboard: None,
            developer: false,
            memory: uke::MemoryIndex::open(&root)?,
            memory_output: None,
            comms_view: false,
            selected: 0,
            input: String::new(),
            mission_input: false,
            demo,
            telemetry: false,
            logs: false,
            log_filter: None,
            log_query: String::new(),
            expanded: false,
            slash_selected: 0,
            stash: None,
            matrix_view: false,
            scroll: 0,
            last_lines: 0,
            tick: 0,
            notice: String::new(),
            motion: true,
            picker: uke::Backend::Rules,
            mission: None,
            missions: vec![],
            started: Instant::now(),
            dir,
            console,
            events,
            tx,
            journal,
            matrix: None,
            nest_auto: true,
            session,
            dirty: true,
            saved: Instant::now(),
            clock_base: 0,
            restart: false,
        };
        let saved = if fresh {
            if let Some(path) = &s.session {
                crate::session::archive(path, "archived");
            }
            s.console.cmd("reset", json!({}))?;
            None
        } else {
            s.session.as_deref().and_then(crate::session::load)
        };
        s.adopt(&hello, saved.as_ref());
        if fresh {
            for a in &mut s.obs.crew {
                if a.console {
                    a.state = "ended".into();
                }
            }
        }
        let crewed = s
            .obs
            .crew
            .iter()
            .any(|a| a.rank == 1 && !matches!(a.state.as_str(), "ended" | "handed-off"));
        if let Some(saved) = saved {
            s.restore(saved);
        } else {
            s.record("BRIDGE ONLINE / Captain has the helm");
        }
        if !crewed {
            let first = s.spawn(0, 1, "COPILOT", "Awaiting your course")?;
            s.selected = first - 1;
            if demo {
                s.spawn(first, 2, "SCOUT", "Survey the nearby star system")?;
                s.spawn(first, 2, "ENGINEER", "Inspect propulsion systems")?;
            }
        } else if s.selected >= s.obs.crew.len() || s.obs.crew[s.selected].state == "ended" {
            s.selected = s
                .obs
                .crew
                .iter()
                .position(|a| a.state != "ended")
                .unwrap_or(0);
        }
        Ok(s)
    }
    /// The kernel's hello: every PID, console transcripts, handoffs so far.
    fn adopt(&mut self, hello: &Value, saved: Option<&crate::session::Session>) {
        for v in hello["pids"].as_array().into_iter().flatten() {
            let pid = v["pid"].as_u64().unwrap_or(0) as usize;
            if pid == 0 {
                continue;
            }
            self.seat(pid);
            let a = &mut self.obs.crew[pid - 1];
            a.apply(v);
            a.transcript = hello["transcripts"][pid.to_string()]
                .as_str()
                .unwrap_or("")
                .to_owned();
            if a.transcript.is_empty()
                && let Some(old) = saved
                    .and_then(|s| s.crew.get(pid - 1))
                    .filter(|old| old.name == a.name)
            {
                a.transcript = old.transcript.clone();
            }
        }
        self.obs.handoffs = hello["handoffs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
    }
    /// The crew entry for `pid`, created as a placeholder when the view is new.
    fn seat(&mut self, pid: usize) -> &mut Agent {
        while self.obs.crew.len() < pid {
            let next = self.obs.crew.len() + 1;
            self.obs.crew.push(Agent::placeholder(next));
        }
        &mut self.obs.crew[pid - 1]
    }
    /// Flight seconds, continuous across quit/reopen.
    pub fn clock(&self) -> u64 {
        self.clock_base + self.started.elapsed().as_secs()
    }
    /// The console's own preferences; the crew itself comes from the kernel.
    fn restore(&mut self, saved: crate::session::Session) {
        self.clock_base = saved.clock;
        self.obs.tail = saved.tail.into();
        self.input = saved.input;
        self.mission_input = saved.mission_input;
        self.motion = saved.motion;
        self.expanded = saved.expanded;
        self.telemetry = saved.telemetry;
        self.logs = saved.logs;
        self.log_filter = match saved.log_filter.as_deref() {
            Some("ERROR") => Some("ERROR"),
            Some("WARN") => Some("WARN"),
            _ => None,
        };
        self.log_query = saved.log_query;
        self.picker = uke::Backend::parse(&saved.picker).unwrap_or(self.picker);
        self.mission = saved
            .mission
            .filter(|path| path.is_file())
            .map(|path| crate::nest::describe(&path));
        let seats = self.obs.crew.len();
        self.selected = saved.selected.min(seats.saturating_sub(1));
        self.record(&format!(
            "RESTORE / {seats} seats / flight resumed from {}",
            self.session
                .as_deref()
                .unwrap_or(Path::new("session"))
                .display()
        ));
        self.notice =
            format!("FLIGHT RESUMED / {seats} seats restored · /new starts a fresh flight");
    }
    fn snapshot(&self) -> crate::session::Session {
        crate::session::Session {
            version: crate::session::VERSION,
            saved_at: 0,
            clock: self.clock(),
            selected: self.selected,
            input: self.input.clone(),
            mission_input: self.mission_input,
            motion: self.motion,
            expanded: self.expanded,
            telemetry: self.telemetry,
            logs: self.logs,
            log_filter: self.log_filter.map(str::to_owned),
            log_query: self.log_query.clone(),
            picker: self.picker.as_str().into(),
            mission: self.mission.as_ref().map(|m| m.path.clone()),
            crew: self
                .obs
                .crew
                .iter()
                .map(|a| crate::session::Seat {
                    parent: a.parent,
                    rank: a.rank,
                    name: a.name.clone(),
                    mission: a.mission.clone(),
                    state: a.state.clone(),
                    model: a.model.clone(),
                    harness: a.harness.clone(),
                    effort: a.effort.clone(),
                    transcript: a.transcript.clone(),
                    queue: vec![],
                    unread: a.unread,
                    needs_captain: a.needs_captain,
                    turn_active: a.turn_active,
                })
                .collect(),
            tail: self.obs.tail.iter().cloned().collect(),
            handoffs: self.obs.handoffs.clone(),
        }
    }
    fn save(&mut self) {
        let Some(path) = self.session.clone() else {
            return;
        };
        self.dirty = false;
        self.saved = Instant::now();
        if let Err(e) = crate::session::save(&path, &mut self.snapshot()) {
            self.notice = format!("Session save failed: {e}");
        }
    }
    /// Bounded autosave: a killed terminal loses at most a few seconds of flight.
    fn autosave(&mut self) {
        if self.dirty && self.saved.elapsed() > Duration::from_secs(3) {
            self.save();
        }
    }
    /// Ends this flight's console seats in the kernel and asks the run loop for a fresh one.
    fn fresh_flight(&mut self) -> Result<()> {
        self.ensure_matrix_idle()?;
        if let Some(path) = self.session.take() {
            crate::session::save(&path, &mut self.snapshot())?;
            crate::session::archive(&path, "archived");
        }
        self.console.cmd("reset", json!({}))?;
        self.restart = true;
        Ok(())
    }
    pub fn visible_events(&self) -> VecDeque<String> {
        self.obs
            .tail
            .iter()
            .filter(|line| self.developer || !line.contains("SYSTEM / "))
            .cloned()
            .collect()
    }
    fn record(&mut self, text: &str) {
        self.dirty = true;
        self.obs
            .tail
            .push_back(format!("{:04}  {}", self.clock(), clean(text)));
        if self.obs.tail.len() > 200 {
            self.obs.tail.pop_front();
        }
        if let Err(e) = writeln!(
            self.journal,
            "{}",
            json!({"seconds":self.clock(),"event":text})
        ) {
            self.notice = format!("Flight log write failed: {e}");
        }
    }
    fn spawn(&mut self, parent: usize, rank: u8, name: &str, mission: &str) -> Result<usize> {
        self.spawn_profile(parent, rank, name, mission, "pi")
    }
    /// A console seat in the kernel (ACP CPU, or the demo simulator in `--demo`).
    fn spawn_profile(
        &mut self,
        parent: usize,
        rank: u8,
        name: &str,
        mission: &str,
        harness: &str,
    ) -> Result<usize> {
        let r = self.console.cmd(
            "spawn",
            json!({"parent": parent, "rank": rank, "name": name, "mission": mission, "harness": harness, "demo": self.demo}),
        )?;
        let pid = r["pid"].as_u64().context("kernel gave no PID")? as usize;
        self.drain_until(pid);
        Ok(pid)
    }
    /// Applies queued kernel events until `pid` is in the crew view.
    fn drain_until(&mut self, pid: usize) {
        let until = Instant::now() + Duration::from_secs(2);
        while self.obs.crew.len() < pid && Instant::now() < until {
            if let Ok(w) = self.events.recv_timeout(Duration::from_millis(50)) {
                self.dirty = true;
                match w {
                    Wire::Nest(o) => self.apply_nest(*o),
                    Wire::Kernel(v) => self.kernel_event(v),
                }
            }
        }
        self.seat(pid);
    }
    fn enqueue(&mut self, to: usize, text: String) -> Result<()> {
        self.console.cmd("send", json!({"pid": to, "text": text}))?;
        Ok(())
    }
    fn incoming(&mut self) {
        while let Ok(w) = self.events.try_recv() {
            self.dirty = true;
            match w {
                Wire::Nest(outcome) => self.apply_nest(*outcome),
                Wire::Kernel(v) => self.kernel_event(v),
            }
        }
    }
    fn kernel_event(&mut self, v: Value) {
        let pid = v["pid"].as_u64().unwrap_or(0) as usize;
        let text = clean(v["text"].as_str().unwrap_or(""));
        match v["ev"].as_str().unwrap_or("") {
            "pid" if pid > 0 => {
                let selected = self.selected;
                let a = self.seat(pid);
                if let Some(_old) = a.apply(&v["view"]) {
                    let state = a.state.clone();
                    a.unread |= pid - 1 != selected && state == "blocked";
                    self.record(&format!("PID {pid} / {state}"));
                }
            }
            "seat" if pid > 0 => {
                let kind = v["kind"].as_str().unwrap_or("");
                let selected = self.selected;
                let a = self.seat(pid);
                match kind {
                    "chunk" => {
                        a.transcript.push_str(&text);
                        a.reply.push_str(&text);
                        a.unread = pid - 1 != selected;
                    }
                    "tool" => {
                        a.transcript.push_str(&format!("\n[ {text} ]\n"));
                        a.unread = pid - 1 != selected;
                    }
                    "name" => {
                        a.transcript.push_str(&text);
                        a.reply.clear();
                        a.success = false;
                        a.turn_active = true;
                    }
                    "done" => {
                        a.transcript.push_str(&text);
                        a.turn_active = false;
                    }
                    _ => {
                        a.transcript.push_str(&text);
                        if matches!(kind, "nest" | "error" | "turn_error" | "needs_captain") {
                            a.unread = pid - 1 != selected;
                        }
                    }
                }
                if a.reply.contains("HANDOFF_OK") {
                    a.success = true;
                }
                if a.transcript.len() > 131072 {
                    let mut cut = a.transcript.len() - 100000;
                    while !a.transcript.is_char_boundary(cut) {
                        cut += 1;
                    }
                    a.transcript.drain(..cut);
                }
                if kind == "chunk" {
                    let _ = writeln!(
                        self.journal,
                        "{}",
                        json!({"seconds":self.started.elapsed().as_secs(),"pid":pid,"event":kind,"text":text})
                    );
                }
            }
            "log" => {
                if text.starts_with("SUCCESS / PID") {
                    self.notice = text.clone();
                }
                self.record(&text);
            }
            "attention" if pid > 0 => {
                self.seat(pid);
                self.attention(pid - 1, &text);
            }
            "notice" => self.notice = text,
            "nest" if pid > 0 => {
                let goal = v["goal"].as_str().unwrap_or("").to_owned();
                if self.nest_auto {
                    self.nest_job(pid, &goal);
                } else {
                    self.seat(pid).nesting = true;
                }
            }
            "handoff" => {
                if let Ok(h) =
                    serde_json::from_value::<drv_hdff::HandoffSummary>(v["summary"].clone())
                {
                    self.notice = format!(
                        "HANDOFF / {} → {} / {} / owner PID {}",
                        h.from_harness,
                        h.to_harness.as_deref().unwrap_or("unknown"),
                        h.work_id,
                        h.to_pid.unwrap_or(0)
                    );
                    self.obs.handoffs.push(h);
                }
            }
            "closed" => {
                self.notice =
                    "ATTENTION / kernel connection closed; relaunch unvrs to reattach".into();
                self.record("SYSTEM / kernel connection closed");
            }
            _ => {}
        }
    }
    fn captain_memory(&mut self, request: Value) -> Result<()> {
        let result = match self.console.cmd(
            "memory",
            json!({"pid": self.selected + 1, "request": request}),
        ) {
            Ok(v) => v["result"].clone(),
            Err(e) => {
                self.memory_output = Some(format!("Memory refused\n\n{e:#}"));
                return Err(e);
            }
        };
        self.memory_output = Some(if request["op"] == "remember" {
            let note: uke::MemoryNote = serde_json::from_value(result)?;
            format!(
                "Saved: {}\n{} · {}{}{}\n\n{}",
                note.title,
                note.id,
                note.scope,
                if note.pinned { " · pinned" } else { "" },
                if note.open { " · open" } else { "" },
                note.body
            )
        } else if request["op"] == "recall" {
            let hits: Vec<uke::MemoryHit> = serde_json::from_value(result)?;
            if hits.is_empty() {
                "No matching memories.".into()
            } else {
                hits.into_iter()
                    .map(|h| {
                        format!(
                            "{} · {}\n{}\n{} · {} · {}\n{}",
                            h.title,
                            h.kind,
                            h.snippet,
                            h.scope,
                            h.source,
                            h.time,
                            h.pointer.display()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        } else {
            serde_json::to_string_pretty(&result)?
        });
        self.record(&format!(
            "SYSTEM / memory {} / PID {}",
            request["op"].as_str().unwrap_or("operation"),
            self.selected + 1
        ));
        self.notice = "Memory updated · /comms returns to conversation".into();
        Ok(())
    }
    fn submit(&mut self) -> Result<()> {
        let text = self.input.trim().to_owned();
        if text.is_empty() {
            return Ok(());
        }
        if text.starts_with('/') && !self.mission_input {
            let word = text.split_whitespace().next().unwrap_or("");
            if let Some((id, args)) = crate::slash::parse(&text) {
                let args = args.to_owned();
                self.input = self.stash.take().unwrap_or_default();
                self.slash_selected = 0;
                return self.run_slash(id, &args);
            }
            // A leading path such as /usr/bin is a message; a bare /word is a mistyped command.
            ensure!(
                word[1..].contains('/'),
                "Unknown command {word} · type / to list commands"
            );
        }
        self.ensure_matrix_idle()?;
        if self.mission_input {
            let pid = self.launch_worker(1, &text, &text)?;
            self.selected = pid - 1;
            self.mission_input = false;
        } else {
            self.send_captain(self.selected + 1, text)?;
        }
        self.input.clear();
        self.scroll = 0;
        self.last_lines = 0;
        Ok(())
    }
    pub fn slash_items(&self) -> Vec<&'static crate::slash::Cmd> {
        if self.mission_input || self.palette.is_some() || self.game.active {
            return vec![];
        }
        crate::slash::suggestions(&self.input)
    }
    fn run_slash(&mut self, id: crate::slash::Id, args: &str) -> Result<()> {
        use crate::slash::Id;
        match id {
            Id::Remember => {
                let mut rest = args;
                let mut pin = false;
                let mut open = false;
                let mut scope = None;
                loop {
                    if let Some(t) = rest.strip_prefix("--pin ") {
                        pin = true;
                        rest = t;
                    } else if let Some(t) = rest.strip_prefix("--open ") {
                        open = true;
                        rest = t;
                    } else if let Some(t) = rest.strip_prefix("--scope ") {
                        let (value, t) = t
                            .split_once(' ')
                            .context("usage: --scope area:name title | body")?;
                        scope = Some(value);
                        rest = t;
                    } else {
                        break;
                    }
                }
                let (title, body) = rest.split_once('|').unwrap_or((rest, ""));
                self.captain_memory(json!({"op":"remember","title":title.trim(),"body":body.trim(),"pinned":pin,"open":open,"scope":scope}))?;
            }
            Id::Recall => {
                self.captain_memory(json!({"op":"recall","query":args}))?;
                self.notice = "Recall · /comms returns to conversation".into();
            }
            Id::Forget => self.captain_memory(json!({"op":"forget","id":args}))?,
            Id::DeleteNote => self.captain_memory(json!({"op":"delete-note","id":args}))?,
            Id::Scope => {
                let mut scope = self.memory.session(self.selected + 1)?.active_scope;
                if args == "universe" {
                    scope = uke::ActiveScope::default();
                } else {
                    for word in args.split_whitespace() {
                        let (key, value) = word.split_once(':').context(
                            "usage: /scope area:name path:relative mission:id · '-' clears a field",
                        )?;
                        let value = (value != "-").then(|| value.to_owned());
                        match key {
                            "area" => scope.area = value,
                            "path" => scope.path = value,
                            "mission" => scope.mission = value,
                            _ => bail!("Unknown scope field"),
                        }
                    }
                }
                self.captain_memory(json!({"op":"scope","scope":scope}))?;
            }
            Id::Developer => {
                self.developer = !self.developer;
                self.logs = self.developer;
                self.notice = format!(
                    "Developer mode {}",
                    if self.developer { "on" } else { "off" }
                );
            }
            Id::Onboard => {
                self.comms_view = false;
                self.memory_output = None;
                self.onboard = Some(0);
                self.game.active = false;
                self.palette = None;
            }
            Id::More => {
                self.input = "/more".into();
                self.slash_selected = 0;
            }
            Id::Mission if args.is_empty() => self.mission_input = true,
            Id::Mission => {
                self.ensure_matrix_idle()?;
                let pid = self.launch_worker(1, args, args)?;
                self.select(pid - 1);
            }
            Id::Spawn => {
                self.ensure_matrix_idle()?;
                ensure!(
                    !args.is_empty(),
                    "usage: /spawn <goal> · launches a worker under the selected seat"
                );
                let pid = self.launch_worker(self.selected + 1, args, args)?;
                self.select(pid - 1);
            }
            Id::Launch => {
                self.ensure_matrix_idle()?;
                let brief = self.mission_brief()?;
                let pid = self.launch_worker(1, &brief, "")?;
                self.select(pid - 1);
            }
            Id::Missions => self.open_missions(),
            Id::Admit if args.is_empty() => self.open_missions(),
            Id::Admit if args.starts_with("note-") => {
                let fields = args.splitn(3, '|').map(str::trim).collect::<Vec<_>>();
                if fields.len() != 3 {
                    self.input = format!("/admit {} | ", fields[0]);
                    self.notice = "Write the outcome, then | and the done check".into();
                    return Ok(());
                }
                let r = self.console.cmd(
                    "memory",
                    json!({"pid": self.selected + 1, "request": {"op": "admit-note", "id": fields[0], "outcome": fields[1], "done": fields[2]}}),
                )?;
                let path = PathBuf::from(
                    r["result"]["mission"]
                        .as_str()
                        .context("kernel returned no mission path")?,
                );
                self.admit(&path);
            }
            Id::Admit => self.admit(&crate::nest::resolve(args)?),
            Id::Picker => self.set_picker(args)?,
            Id::Boot => {
                let mut pid = self.selected + 1;
                for word in args.split_whitespace() {
                    match word.parse::<usize>() {
                        Ok(n) => pid = n,
                        Err(_) => self.set_picker(word)?,
                    }
                }
                self.boot_seat(pid)?;
            }
            Id::Comms => {
                self.comms_view = true;
                self.memory_output = None;
                self.select(self.selected);
            }
            Id::Logs | Id::Errors | Id::Search => {
                self.logs = true;
                self.telemetry = false;
                self.matrix_view = false;
                self.log_filter = (id == Id::Errors).then_some("ERROR");
                if id != Id::Errors {
                    self.log_query = args.into();
                }
                self.scroll = 0;
                self.last_lines = 0;
            }
            Id::Obs => {
                self.telemetry = true;
                self.logs = false;
                self.matrix_view = false;
                self.scroll = 0;
                self.last_lines = 0;
            }
            Id::TestHandoffs => self.start_handoff_matrix()?,
            Id::Galaga => self.toggle_game(),
            Id::Motion => self.motion = !self.motion,
            Id::Expand => self.toggle_expanded(),
            Id::Cancel => self.cancel_turn(self.selected + 1)?,
            Id::Menu => self.palette = Some(crate::menu::Palette::default()),
            Id::Help => {
                let mut menu = crate::menu::Palette::default();
                menu.open(crate::menu::Page::Help);
                self.palette = Some(menu);
            }
            Id::New => self.fresh_flight()?,
            Id::Quit => self.quit = true,
        }
        Ok(())
    }
    /// New worker under `parent` with `text` in its mailbox. The kernel holds the mail until
    /// the console's Boot nest for it lands, so the seat is never ready before its outcome.
    fn launch_worker(&mut self, parent: usize, text: &str, goal: &str) -> Result<usize> {
        let rank = self
            .obs
            .crew
            .get(parent.checked_sub(1).context("PID starts at 1")?)
            .context("Unknown parent")?
            .rank;
        ensure!(rank < 3, "L3 cannot create workers");
        let name = format!(
            "WORKER {:02}",
            self.obs.crew.iter().filter(|a| a.console).count()
        );
        let r = self.console.cmd(
            "spawn",
            json!({"parent": parent, "rank": rank + 1, "name": name, "mission": text, "harness": "pi", "demo": self.demo, "mail": text, "nest": true, "goal": goal}),
        )?;
        let pid = r["pid"].as_u64().context("kernel gave no PID")? as usize;
        self.drain_until(pid);
        Ok(pid)
    }
    /// DrvBoot for `pid`, off the UI thread; `apply_nest` reports the outcome to the
    /// kernel. Focus is read now: an L2 has a captain channel only while the captain is
    /// looking at it.
    fn nest_job(&mut self, pid: usize, goal: &str) {
        if let Some(mission) = &self.mission {
            let mut scope = self
                .memory
                .session(pid)
                .map(|s| s.active_scope)
                .unwrap_or_default();
            scope.mission = Some(mission.id.clone());
            if let Err(e) = self.console.cmd(
                "memory",
                json!({"pid": pid, "request": {"op": "scope", "scope": scope}}),
            ) {
                self.notice = format!("Scope: {e}");
                return;
            }
        }
        let focused = self.selected == pid - 1;
        let a = self.seat(pid);
        a.nesting = true;
        a.pulse = Instant::now();
        let job = crate::nest::Job {
            pid,
            seat: uke::Seat {
                rank: a.rank,
                intf_focused: focused,
            },
            profile: a.harness.clone(),
            picker: self.picker,
            mission: self.mission.as_ref().map(|m| m.path.clone()),
            goal: goal.into(),
        };
        self.record(&format!(
            "NEST / PID {pid} / {} / picker {} / started",
            self.mission.as_ref().map_or("adhoc", |m| m.id.as_str()),
            self.picker.as_str()
        ));
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Wire::Nest(Box::new(crate::nest::run(&job))));
        });
    }
    fn apply_nest(&mut self, outcome: crate::nest::Outcome) {
        self.comms_view = true;
        self.record("SYSTEM / skill load / Boot result");
        let Some(i) = outcome
            .pid
            .checked_sub(1)
            .filter(|i| *i < self.obs.crew.len())
        else {
            return;
        };
        let focused = i == self.selected;
        {
            let a = &mut self.obs.crew[i];
            a.nesting = false;
            a.unread = !focused;
            a.pulse = Instant::now();
        }
        self.record(&outcome.event());
        if let Err(e) = self.console.cmd(
            "nest_done",
            json!({"pid": outcome.pid, "ready": outcome.ready(), "activation": outcome.activation(), "card": outcome.card()}),
        ) {
            self.notice = format!("Boot result not delivered: {e}");
        }
        if let Some(mission) = &mut self.mission
            && mission.id == outcome.mission
        {
            *mission = crate::nest::describe(&mission.path);
        }
        match &outcome.refused {
            Some((stage, reason)) => self.attention(
                i,
                &format!("PID {} / boot refused at {stage}: {reason}", i + 1),
            ),
            None => {
                self.notice = format!(
                    "BOOT / PID {} ready · nest: {}",
                    i + 1,
                    outcome
                        .record
                        .as_ref()
                        .filter(|r| !r.selected.is_empty())
                        .map_or("empty (no skill needed)".into(), |r| r.selected.join(", "))
                )
            }
        }
    }
    /// `/boot`: nest an existing seat on the admitted mission.
    fn boot_seat(&mut self, pid: usize) -> Result<()> {
        self.ensure_matrix_idle()?;
        ensure!(
            self.mission.is_some(),
            "Admit a mission first · /missions lists them"
        );
        let a = self
            .obs
            .crew
            .get(pid.checked_sub(1).context("PID starts at 1")?)
            .context("Unknown PID")?;
        ensure!(a.console, "PID {pid} is not a console seat");
        ensure!(!a.nesting, "PID {pid} is already nesting");
        ensure!(
            matches!(a.state.as_str(), "idle" | "blocked"),
            "PID {pid} is {}; Boot needs an idle seat",
            a.state
        );
        self.console.cmd("nest", json!({"pid": pid, "goal": ""}))?;
        self.notice = format!(
            "BOOT / PID {pid} nesting with {} · result lands in its COMMS",
            self.picker.as_str()
        );
        Ok(())
    }
    fn set_picker(&mut self, arg: &str) -> Result<()> {
        use uke::Backend;
        self.picker = match arg.trim() {
            "" if self.picker == Backend::Rules => Backend::Jev,
            "" => Backend::Rules,
            name => {
                Backend::parse(name).map_err(|_| anyhow::anyhow!("usage: /picker rules|jev"))?
            }
        };
        self.record(&format!("PICKER / {}", self.picker.as_str()));
        self.notice = format!(
            "PICKER / {} · {}",
            self.picker.as_str(),
            if self.picker == Backend::Jev {
                "TypeSafe Jev; falls back to rules without TYPESAFE_API_KEY"
            } else {
                "local intent rules; no network"
            }
        );
        Ok(())
    }
    fn open_missions(&mut self) {
        self.missions = crate::nest::list();
        let mut menu = crate::menu::Palette::default();
        menu.open(crate::menu::Page::Missions);
        self.palette = Some(menu);
    }
    /// Admit and select. A refusal is loud in COMMS and the notice, never only in a log.
    fn admit(&mut self, path: &Path) {
        self.comms_view = true;
        let result = crate::nest::admit(path);
        self.missions = crate::nest::list();
        let line = match result {
            Ok(mission) => {
                let line = format!(
                    "MISSION ADMITTED / {} / {} / {}",
                    mission.id,
                    mission.status,
                    crate::theme::one_line(&mission.outcome, 120)
                );
                self.notice = format!(
                    "MISSION ADMITTED / {} · /boot nests this seat · /launch starts a worker",
                    mission.id
                );
                self.mission = Some(mission);
                line
            }
            Err(e) => {
                let line = format!("ADMIT REFUSED / {} / {e:#}", path.display());
                self.notice = format!("ATTENTION / {line}");
                line
            }
        };
        self.record(&line);
        let a = &mut self.obs.crew[self.selected];
        a.transcript.push_str(&format!("\n{line}\n"));
        self.scroll = 0;
        self.last_lines = 0;
    }
    /// The admitted mission as a worker's first mail.
    fn mission_brief(&self) -> Result<String> {
        let mission = self
            .mission
            .as_ref()
            .context("Admit a mission first · /missions lists them")?;
        Ok(uke::Mission::load(&mission.path)?.brief())
    }
    fn toggle_expanded(&mut self) {
        self.comms_view = true;
        self.expanded = !self.expanded;
        self.scroll = 0;
        self.last_lines = 0;
        self.notice = if self.expanded {
            "COMMS detail shown · Ctrl+O or /expand collapses tool and ctl output"
        } else {
            "COMMS detail collapsed · Ctrl+O or /expand shows tool and ctl output"
        }
        .into();
    }
    /// Keys claimed by the slash popup while it is open. Returns true when handled.
    fn slash_key(&mut self, code: KeyCode) -> Result<bool> {
        let items = self.slash_items();
        if items.is_empty() {
            return Ok(false);
        }
        let selected = self.slash_selected.min(items.len() - 1);
        match code {
            KeyCode::Up => self.slash_selected = (selected + items.len() - 1) % items.len(),
            KeyCode::Down => self.slash_selected = (selected + 1) % items.len(),
            KeyCode::Tab => {
                self.input = crate::slash::complete(items[selected]);
                self.slash_selected = 0;
            }
            KeyCode::Enter => {
                self.input = crate::slash::complete(items[selected]);
                self.slash_selected = 0;
                // `[optional]` args run straight away; `<required>` ones wait for typing.
                if !items[selected].args.starts_with('<') {
                    self.submit()?;
                }
            }
            KeyCode::Esc => {
                self.input = self.stash.take().unwrap_or_default();
                self.slash_selected = 0;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    /// In the log view plain typing edits the live search; `/` still opens commands.
    fn log_search_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        if !self.logs || self.input.starts_with('/') {
            return false;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('/') if !ctrl => {
                self.stash = Some(std::mem::replace(&mut self.input, "/".into()));
                self.slash_selected = 0;
            }
            KeyCode::Char('l' | 'u') if ctrl => self.log_query.clear(),
            KeyCode::Char(c) if !ctrl && self.log_query.len() < 256 => self.log_query.push(c),
            KeyCode::Backspace => {
                self.log_query.pop();
            }
            KeyCode::Enter => {}
            _ => return false,
        }
        self.scroll = 0;
        self.last_lines = 0;
        true
    }
    fn send_captain(&mut self, pid: usize, text: String) -> Result<()> {
        self.comms_view = true;
        self.memory_output = None;
        self.console
            .cmd("send", json!({"pid": pid, "text": text}))?;
        if let Some(a) = self.obs.crew.get_mut(pid - 1)
            && a.needs_captain
        {
            a.needs_captain = false;
            self.notice.clear();
        }
        Ok(())
    }
    /// Captain-initiated handoff (palette): routed by the kernel's handoff illusion.
    fn handoff(&mut self, from: usize, mut summary: drv_hdff::HandoffSummary) -> Result<usize> {
        summary.validate()?;
        summary.from_pid = from;
        let r = self.console.cmd(
            "handoff",
            json!({"from": from, "summary": serde_json::to_value(&summary)?}),
        )?;
        let to = r["pid"].as_u64().context("kernel gave no PID")? as usize;
        self.drain_until(to);
        Ok(to)
    }
    fn attention(&mut self, index: usize, text: &str) {
        self.comms_view = true;
        self.game.active = false;
        self.onboard = None;
        self.palette = None;
        self.selected = index;
        self.telemetry = false;
        self.scroll = 0;
        self.last_lines = 0;
        self.notice = format!("ATTENTION / {text}");
    }
    pub fn palette_items(&self) -> Vec<crate::menu::Entry> {
        use crate::menu::{Action as A, Entry as E, Form, Page as P};
        let Some(menu) = &self.palette else {
            return vec![];
        };
        let current = self.selected + 1;
        let items = match menu.page {
            P::Root => vec![
                E::new(
                    format!("Current seat · {} ›", self.obs.crew[self.selected].name),
                    "Message, handoff, cancel turn, recorder",
                    A::Open(P::Seat(current)),
                ),
                E::new("Crew ›", "Choose a seat, then an action", A::Open(P::Crew)),
                E::new(
                    "New mission…",
                    "Assign a goal to a new L2 worker · F2",
                    A::Open(P::Compose(Form::Mission)),
                ),
                E::new("Observation", "Ship health and kernel bus · F3", A::Obs),
                E::new(
                    "Flight logs ›",
                    "Read this flight's events; filter errors or warnings",
                    A::Open(P::Logs),
                ),
                E::new(
                    "Test handoffs",
                    "Run the live directed harness matrix",
                    A::TestHandoffs,
                ),
                E::new(
                    format!(
                        "Missions › {}",
                        self.mission.as_ref().map_or("none admitted", |m| &m.id)
                    ),
                    "Admit a mission file; new seats nest on it · /missions",
                    A::Open(P::Missions),
                ),
                E::new(
                    format!("Boot this seat · {}", self.obs.crew[self.selected].name),
                    "Pick skills for the admitted mission and install them · /boot",
                    A::Boot(current),
                ),
                E::new(
                    "Launch mission worker",
                    "New L2 on the admitted mission, nested before ready · /launch",
                    A::Launch,
                ),
                E::new(
                    format!("Skill picker · {}", self.picker.as_str()),
                    "Switch DrvBoot between rules and jev · /picker",
                    A::Picker,
                ),
                E::new(
                    "Galaga ›",
                    "Play, resume, and controls · F8",
                    A::Open(P::Galaga),
                ),
                E::new(
                    "Display ›",
                    "Motion and animation preferences",
                    A::Open(P::Display),
                ),
                E::new(
                    "Controls ›",
                    "Keyboard and mouse reference",
                    A::Open(P::Help),
                ),
                E::new(
                    "New flight",
                    "Archive this session and start fresh · /new",
                    A::NewFlight,
                ),
                E::new(
                    "Dock and exit",
                    "Stop seats and exit; relaunch resumes this flight · F10",
                    A::Quit,
                ),
            ],
            P::Crew => self
                .obs
                .crew
                .iter()
                .map(|a| {
                    E::new(
                        format!("PID {} · {} ›", a.pid, a.name),
                        format!("{} · {} · {} · {}", a.state, a.harness, a.model, a.effort),
                        A::Open(P::Seat(a.pid)),
                    )
                })
                .collect(),
            P::Seat(pid) => {
                let a = &self.obs.crew[pid - 1];
                let mut cancel = E::new("Cancel turn", "Request cancellation · F4", A::Cancel(pid));
                cancel.enabled = a.turn_active && !self.demo;
                if !cancel.enabled {
                    cancel.detail = "Unavailable: no live turn running".into();
                }
                vec![
                    E::new(
                        "Message…",
                        "Write a message to this seat",
                        A::Open(P::Compose(Form::Message(pid))),
                    ),
                    E::new(
                        "Open conversation",
                        "Read this seat's COMMS history",
                        A::Comms(pid),
                    ),
                    E::new(
                        "Handoff…",
                        "Choose a harness, then describe the next focus",
                        A::Open(P::Handoff(pid)),
                    ),
                    cancel,
                    E::new(
                        "Seat recorder",
                        "Show the local log path · F5",
                        A::Recorder(pid),
                    ),
                ]
            }
            P::Missions => self
                .missions
                .iter()
                .enumerate()
                .map(|(index, m)| {
                    let selected = self.mission.as_ref().is_some_and(|s| s.path == m.path);
                    E::new(
                        format!(
                            "{}{} · {}",
                            if selected { "● " } else { "" },
                            m.id,
                            m.status
                        ),
                        match &m.problem {
                            Some(problem) => format!("will refuse: {problem}"),
                            None => m.outcome.clone(),
                        },
                        A::Admit(index),
                    )
                })
                .collect(),
            P::Handoff(pid) => vec![
                E::new(
                    "Pi ›",
                    "Continue in a new Pi L2 seat",
                    A::Open(P::Compose(Form::Handoff(pid, "pi"))),
                ),
                E::new(
                    "Codex ›",
                    "Continue in a new Codex L2 seat",
                    A::Open(P::Compose(Form::Handoff(pid, "codex"))),
                ),
                E::new(
                    "Claude ›",
                    "Continue in a new Claude L2 seat",
                    A::Open(P::Compose(Form::Handoff(pid, "claude"))),
                ),
                E::new(
                    "Cursor ›",
                    "Continue in a new Cursor L2 seat",
                    A::Open(P::Compose(Form::Handoff(pid, "cursor"))),
                ),
            ],
            P::Logs => vec![
                E::new(
                    "All events",
                    "Show every recorded flight event",
                    A::LogFilter(None),
                ),
                E::new(
                    "Errors only",
                    "Show ERROR, FAILED, offline, and blocked events",
                    A::LogFilter(Some("ERROR")),
                ),
                E::new(
                    "Warnings only",
                    "Show attention and cancellation events",
                    A::LogFilter(Some("WARN")),
                ),
            ],
            P::Galaga => {
                let mut play = E::new(
                    "Play / resume",
                    "Available while idle or working; attention pauses play",
                    A::Play,
                );
                play.enabled = !self
                    .obs
                    .crew
                    .iter()
                    .any(|a| matches!(a.state.as_str(), "blocked" | "offline"));
                if !play.enabled {
                    play.detail = "Unavailable: resolve crew attention first".into();
                }
                vec![play]
            }
            P::Display => vec![E::new(
                if self.motion {
                    "Reduce motion"
                } else {
                    "Enable motion"
                },
                "Cosmetic animation only · F7",
                A::Motion,
            )],
            P::Help | P::Compose(_) => vec![],
        };
        items
            .into_iter()
            .filter(|entry| menu.matches(entry))
            .collect()
    }
    fn palette_choose(&mut self, index: usize) -> Result<()> {
        use crate::menu::{Action, Page};
        let Some(entry) = self.palette_items().into_iter().nth(index) else {
            return Ok(());
        };
        ensure!(entry.enabled, "{}", entry.detail);
        match entry.action {
            Action::Open(page) => {
                if let Page::Seat(pid) = page {
                    self.select(pid - 1);
                }
                self.palette.as_mut().unwrap().open(page);
                return Ok(());
            }
            Action::Comms(pid) => self.select(pid - 1),
            Action::Obs => {
                self.telemetry = true;
                self.logs = false;
                self.matrix_view = false;
                self.scroll = 0;
                self.last_lines = 0;
            }
            Action::LogFilter(filter) => {
                self.logs = true;
                self.log_filter = filter;
                self.scroll = 0;
                self.last_lines = 0;
            }
            Action::Play => self.toggle_game(),
            Action::Motion => {
                self.motion = !self.motion;
                self.palette.as_mut().unwrap().query.clear();
                return Ok(());
            }
            Action::Admit(index) => {
                let path = self
                    .missions
                    .get(index)
                    .context("Mission list changed; reopen /missions")?
                    .path
                    .clone();
                self.admit(&path);
                self.select(self.selected);
            }
            Action::Boot(pid) => self.boot_seat(pid)?,
            Action::Launch => {
                self.palette = None;
                return self.run_slash(crate::slash::Id::Launch, "");
            }
            Action::Picker => {
                self.set_picker("")?;
                self.palette.as_mut().unwrap().query.clear();
                return Ok(());
            }
            Action::TestHandoffs => self.start_handoff_matrix()?,
            Action::NewFlight => self.fresh_flight()?,
            Action::Cancel(pid) => self.cancel_turn(pid)?,
            Action::Recorder(pid) => {
                self.notice = format!("Seat recorder: {}/seat-{pid}.log", self.dir.display())
            }
            Action::Quit => self.quit = true,
        }
        self.palette = None;
        Ok(())
    }
    fn cancel_turn(&mut self, pid: usize) -> Result<()> {
        self.ensure_matrix_idle()?;
        ensure!(!self.demo, "Training turns complete automatically");
        let a = &self.obs.crew[pid - 1];
        ensure!(a.turn_active, "No turn is running");
        self.console.cmd("cancel", json!({"pid": pid}))?;
        self.notice = "Stop requested; waiting for ACP acknowledgement".into();
        Ok(())
    }
    fn palette_submit(&mut self) -> Result<()> {
        use crate::menu::{Form, Page};
        let menu = self.palette.as_ref().unwrap();
        let Page::Compose(form) = menu.page else {
            return self.palette_choose(menu.selected);
        };
        self.ensure_matrix_idle()?;
        let text = menu.query.trim().to_owned();
        ensure!(
            !text.is_empty(),
            "Enter a message or goal before submitting"
        );
        let pid = match form {
            Form::Message(pid) => {
                self.send_captain(pid, text)?;
                pid
            }
            Form::Mission => self.launch_worker(1, &text, &text)?,
            Form::Handoff(from, harness) => {
                self.handoff(from, drv_hdff::HandoffSummary::new(from, 2, harness, &text))?
            }
        };
        self.select(pid - 1);
        self.palette = None;
        Ok(())
    }
    fn palette_key(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        let count = self.palette_items().len();
        let menu = self.palette.as_mut().unwrap();
        if menu.page == crate::menu::Page::Help && key.code != KeyCode::Esc {
            return Ok(());
        }
        match key.code {
            KeyCode::Esc => {
                if !menu.back() {
                    self.palette = None;
                }
            }
            KeyCode::Up if count > 0 => menu.selected = (menu.selected + count - 1) % count,
            KeyCode::Down | KeyCode::Tab if count > 0 => {
                menu.selected = (menu.selected + 1) % count
            }
            KeyCode::Enter => self.palette_submit()?,
            KeyCode::Backspace => {
                menu.query.pop();
                menu.selected = 0;
                menu.error.clear();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                menu.query.clear();
                menu.selected = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                    && menu.query.len() + c.len_utf8() <= 32768 =>
            {
                menu.query.push(c);
                menu.selected = 0;
                menu.error.clear();
            }
            _ => {}
        }
        Ok(())
    }
    fn select(&mut self, index: usize) {
        self.selected = index;
        self.obs.crew[index].unread = false;
        self.telemetry = false;
        self.logs = false;
        self.matrix_view = false;
        self.scroll = 0;
        self.last_lines = 0;
    }
    fn toggle_game(&mut self) {
        if self.game.active {
            self.game.active = false;
        } else if self
            .obs
            .crew
            .iter()
            .any(|a| matches!(a.state.as_str(), "blocked" | "offline"))
        {
            self.notice = "Resolve crew attention before Galaga".into();
        } else {
            self.game.active = true;
        }
    }
}
impl Drop for Ship {
    fn drop(&mut self) {
        // The console only saves its view; closing the connection lets the kernel dock
        // this flight's ACP seats (their processes stop) until a console attaches again.
        self.save();
    }
}
pub fn run(demo: bool, fresh: bool) -> Result<()> {
    let mut ship = Ship::open(demo, fresh)?;
    // The CRT is a colored application surface, even when a launcher sets NO_COLOR for CLI logs.
    crossterm::style::force_color_output(true);
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    let result = (|| -> Result<()> {
        loop {
            ship.incoming();
            ship.tick_handoff_matrix();
            ship.game.step();
            terminal.draw(|f| ui::draw(f, &mut ship))?;
            if event::poll(Duration::from_millis(80))? {
                ship.dirty = true;
                match event::read()? {
                    KeyEvent::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => {
                            if let Some(menu) = &mut ship.palette {
                                menu.selected = menu.selected.saturating_sub(1);
                            } else if !ship.game.active {
                                ship.scroll = ship.scroll.saturating_add(3);
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            let count = ship.palette_items().len();
                            if let Some(menu) = &mut ship.palette {
                                menu.selected = (menu.selected + 1).min(count.saturating_sub(1));
                            } else if !ship.game.active {
                                ship.scroll = ship.scroll.saturating_sub(3);
                            }
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            let area = terminal.get_frame().area();
                            if let Some(index) = ui::hit_palette(area, &ship, m.column, m.row) {
                                if let Err(e) = ship.palette_choose(index)
                                    && let Some(menu) = &mut ship.palette
                                {
                                    menu.error = e.to_string();
                                }
                            } else if ship.palette.is_none()
                                && let Some(index) = ui::hit_crew(area, &ship, m.column, m.row)
                            {
                                ship.select(index);
                            }
                        }
                        _ => {}
                    },
                    KeyEvent::Paste(text) => {
                        let text = clean(&text).replace("\r\n", "\n").replace('\r', "\n");
                        if let Some(menu) = &mut ship.palette {
                            let text = text.replace('\n', " ");
                            if menu.query.len() + text.len() <= 32768 {
                                menu.query.push_str(&text);
                                menu.selected = 0;
                            }
                        } else if !ship.game.active
                            && ship.onboard.is_none()
                            && ship.input.len() + text.len() <= 32768
                        {
                            ship.input.push_str(&text);
                        }
                    }
                    KeyEvent::Key(k)
                        if k.kind == KeyEventKind::Press
                            || ship.game.active && k.kind == KeyEventKind::Repeat =>
                    {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        if k.code == KeyCode::F(10)
                            || ctrl && matches!(k.code, KeyCode::Char('q' | 'c'))
                        {
                            break;
                        }
                        let action = (|| -> Result<()> {
                            if k.code == KeyCode::Char('k')
                                && (ctrl || k.modifiers.contains(KeyModifiers::SUPER))
                            {
                                ship.palette = if ship.palette.is_some() {
                                    None
                                } else {
                                    Some(crate::menu::Palette::default())
                                };
                                ship.game.active = false;
                                return Ok(());
                            }
                            if k.code == KeyCode::F(7) {
                                ship.motion = !ship.motion;
                                return Ok(());
                            }
                            if let Some(scene) = ship.onboard {
                                if scene < 3 {
                                    match k.code {
                                        KeyCode::Esc | KeyCode::Char('s') => ship.onboard = None,
                                        KeyCode::Enter | KeyCode::Char(' ') => {
                                            ship.onboard = Some(scene + 1);
                                            if scene == 2 {
                                                ship.notice="Your universe is ready · /remember keeps a note".into();
                                            }
                                        }
                                        _ => {}
                                    }
                                    return Ok(());
                                }
                                ship.onboard = None;
                            }
                            if ship.palette.is_some() {
                                return ship.palette_key(k);
                            }
                            if ship.game.active {
                                match k.code {
                                    KeyCode::Left | KeyCode::Char('a' | 'A') => ship.game.left(),
                                    KeyCode::Right | KeyCode::Char('d' | 'D') => ship.game.right(),
                                    KeyCode::Char(' ') => ship.game.fire(),
                                    KeyCode::Esc | KeyCode::F(8) => ship.game.active = false,
                                    _ => {}
                                }
                                return Ok(());
                            }
                            if k.code == KeyCode::Char('o') && ctrl {
                                ship.toggle_expanded();
                                return Ok(());
                            }
                            if ship.slash_key(k.code)? || ship.log_search_key(k) {
                                return Ok(());
                            }
                            match k.code {
                                KeyCode::Tab | KeyCode::BackTab => {
                                    let next = (ship.selected
                                        + if k.code == KeyCode::BackTab {
                                            ship.obs.crew.len() - 1
                                        } else {
                                            1
                                        })
                                        % ship.obs.crew.len();
                                    ship.select(next);
                                }
                                KeyCode::F(2) => {
                                    ship.mission_input = !ship.mission_input;
                                }
                                KeyCode::F(3) => {
                                    ship.telemetry = !ship.telemetry;
                                    ship.logs = false;
                                    ship.matrix_view = false;
                                    ship.scroll = 0;
                                    ship.last_lines = 0;
                                }
                                KeyCode::F(4) => ship.cancel_turn(ship.selected + 1)?,
                                KeyCode::F(5) => {
                                    ship.notice = format!(
                                        "Seat recorder: {}/seat-{}.log",
                                        ship.dir.display(),
                                        ship.selected + 1
                                    )
                                }
                                KeyCode::F(8) => ship.toggle_game(),
                                KeyCode::F(6) => ship.select(0),
                                KeyCode::End => {
                                    ship.scroll = 0;
                                    ship.last_lines = 0;
                                }
                                KeyCode::Home => ship.scroll = u16::MAX,
                                KeyCode::PageUp => ship.scroll = ship.scroll.saturating_add(8),
                                KeyCode::PageDown => ship.scroll = ship.scroll.saturating_sub(8),
                                KeyCode::Esc => {
                                    ship.mission_input = false;
                                    ship.telemetry = false;
                                    ship.logs = false;
                                    ship.log_query.clear();
                                    ship.matrix_view = false;
                                    ship.notice.clear();
                                    ship.comms_view = false;
                                    ship.memory_output = None;
                                }
                                KeyCode::Enter
                                    if !k
                                        .modifiers
                                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                                {
                                    ship.submit()?
                                }
                                KeyCode::Enter | KeyCode::Char('j')
                                    if k.modifiers.intersects(
                                        KeyModifiers::SHIFT
                                            | KeyModifiers::ALT
                                            | KeyModifiers::CONTROL,
                                    ) && ship.input.len() < 32768 =>
                                {
                                    ship.input.push('\n')
                                }
                                KeyCode::Backspace => {
                                    ship.input.pop();
                                    ship.slash_selected = 0;
                                    if ship.input.is_empty()
                                        && let Some(draft) = ship.stash.take()
                                    {
                                        ship.input = draft;
                                    }
                                }
                                KeyCode::Char('l') if ctrl => ship.input.clear(),
                                KeyCode::Char(c) if !ctrl && ship.input.len() < 32768 => {
                                    ship.input.push(c);
                                    ship.slash_selected = 0;
                                }
                                _ => {}
                            }
                            Ok(())
                        })();
                        if let Err(e) = action {
                            if let Some(menu) = &mut ship.palette {
                                menu.error = e.to_string();
                            } else {
                                ship.notice = e.to_string();
                            }
                        }
                    }
                    _ => {}
                }
            }
            if ship.quit {
                break;
            }
            if ship.restart {
                // Drop stops the old seats before the fresh flight binds its own bus.
                drop(std::mem::replace(&mut ship, Ship::open(demo, false)?));
                ship.notice = "FRESH FLIGHT / previous session archived in .unvrs/".into();
            }
            ship.autosave();
            if ship.motion {
                ship.tick = ship.tick.wrapping_add(1);
            }
        }
        Ok(())
    })();
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
    println!(
        "Flight docked. Relaunch unvrs here to resume it; /new starts fresh. Journal: .unvrs/flight-*.jsonl."
    );
    result
}

/// Live acceptance path through the kernel: the same console, mailboxes and ACP seats as
/// the cockpit, in the current directory's universe.
pub fn handoff_smoke() -> Result<()> {
    let root = env::current_dir()?;
    let mut ship = Ship::launch_at(&root, false, true, None)?;
    println!("smoke: HANDOFF_SMOKE\nrecorders: {}", ship.dir.display());
    let copilot = ship.selected + 1;
    wait_idle(&mut ship, copilot)?;
    println!(
        "source: pi / {} / {}",
        ship.obs.crew[copilot - 1].model,
        ship.obs.crew[copilot - 1].effort
    );
    for harness in ["pi", "codex"] {
        let mut summary = drv_hdff::HandoffSummary::new(
            copilot,
            2,
            harness,
            "Reply with exactly HANDOFF_OK. Do not call tools or delegate.",
        );
        summary.goal = "Verify summary delivery and ownership transfer".into();
        summary.done = vec!["L1 Pi connected".into()];
        summary.artifacts = vec!["docs/design/voyages/IMPLEMENTATION-0.2.0-eval.1.md".into()];
        let path = ship.dir.join(format!("handoff-{harness}.json"));
        fs::write(&path, serde_json::to_vec(&summary)?)?;
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let command = format!(
            "{} ctl handoff {}",
            quote(&env::current_exe()?.to_string_lossy()),
            quote(&path.to_string_lossy())
        );
        ship.enqueue(copilot, format!("Run exactly this command using your shell tool: {command}. Then reply REQUESTED. Do not send any other messages, inspect files, or delegate separately."))?;
        thread::sleep(Duration::from_millis(300));
        wait_idle(&mut ship, copilot)?;
        let delivered = ship
            .obs
            .handoffs
            .iter()
            .find(|s| s.work_id == summary.work_id)
            .context("L1 did not invoke ctl handoff")?;
        let pid = delivered.to_pid.context("Handoff has no resolved PID")?;
        wait_idle(&mut ship, pid)?;
        let a = &ship.obs.crew[pid - 1];
        ensure!(a.harness == harness, "Wrong harness");
        ensure!(
            a.reply.contains("HANDOFF_OK"),
            "PID {pid} did not reply HANDOFF_OK"
        );
        ensure!(
            a.transcript.contains("OPEN:") && a.transcript.contains("ARTIFACTS:"),
            "Missing handoff card"
        );
        println!(
            "transfer: pi → {harness}\npid: {pid}\nmodel: {}\neffort: {}\nsummary: delivered\nreply: HANDOFF_OK",
            a.model, a.effort
        );
        ship.record(&format!(
            "HANDOFF_SMOKE / pi → {harness} / PASS / HANDOFF_OK"
        ));
    }
    let status = uke::kernel_request(
        &ship.console.universe,
        &json!({"op": "snapshot"}),
        Duration::from_secs(5),
    )?;
    let groups: Vec<u32> = status["pids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["driven"]["os_pid"].as_u64().map(|g| g as u32))
        .collect();
    drop(ship);
    let deadline = Instant::now() + Duration::from_secs(8);
    for group in groups {
        while uke::signals::probe_group(i64::from(group)).is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        ensure!(
            uke::signals::probe_group(i64::from(group)).is_err(),
            "Orphan ACP process group {group}"
        );
    }
    println!(
        "cleanup: PASS (the kernel docked the seats; ACP process groups exited)\nresult: PASS\nhelp[1]:\n  unvrs"
    );
    Ok(())
}
fn wait_idle(ship: &mut Ship, pid: usize) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(150);
    loop {
        ship.incoming();
        let a = &ship.obs.crew[pid - 1];
        ensure!(
            !matches!(a.state.as_str(), "offline" | "blocked"),
            "PID {pid} {}: {}",
            a.state,
            a.transcript
        );
        if a.state == "idle" && a.queue == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < until,
            "HANDOFF_SMOKE timeout for PID {pid}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    struct NoCpu;
    impl uke::Drivers for NoCpu {
        fn fold(&self, _: &uke::FoldJob) -> Result<uke::BriefFold> {
            bail!("no fold worker in console tests")
        }
        fn turn(&self, _: &uke::TurnRequest, _: &dyn Fn(u32)) -> Result<uke::TurnResult> {
            bail!("no headless CPU in console tests")
        }
    }
    /// A throwaway universe with the real kernel running in-process.
    pub fn universe() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "unvrs-console-{}-{}",
            std::process::id(),
            random_tag().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let u = uke::Universe::at(&root).unwrap();
        u.init().unwrap();
        let k = u.clone();
        thread::spawn(move || {
            uke::serve_kernel(
                k,
                std::sync::Arc::new(NoCpu),
                std::sync::Arc::new(uke::SilentMapp),
                None,
            )
        });
        let until = Instant::now() + Duration::from_secs(5);
        while !uke::kernel_running(&u) && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        u.root().to_path_buf()
    }
    /// Applies kernel events until `done` holds (or 10 s pass).
    pub fn until(s: &mut Ship, done: impl Fn(&Ship) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done(s) && Instant::now() < end {
            s.incoming();
            thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn settle(s: &mut Ship, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            s.incoming();
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn handoff_illusion_attention_and_scroll() {
        use drv_hdff::HandoffSummary;
        let mut s = Ship::new(true).unwrap();
        s.nest_auto = false;
        let child = s.spawn(2, 3, "L3", "child").unwrap();
        let peer = s
            .handoff(child, HandoffSummary::new(child, 3, "codex", "continue"))
            .unwrap();
        settle(&mut s, 150);
        assert_eq!(s.obs.crew[peer - 1].parent, 2);
        assert_eq!(s.obs.crew[peer - 1].harness, "codex");
        assert!(s.obs.crew[peer - 1].transcript.contains("HANDOFF /"));
        let before = s.obs.crew.len();
        let up = s
            .handoff(child, HandoffSummary::new(child, 1, "pi", "report"))
            .unwrap();
        assert_eq!(up, 1);
        settle(&mut s, 100);
        assert_eq!(s.obs.crew.len(), before);
        let mut explicit = HandoffSummary::new(2, 2, "pi", "existing");
        explicit.to_pid = Some(3);
        assert_eq!(s.handoff(2, explicit.clone()).unwrap(), 3);
        assert!(s.handoff(2, explicit.clone()).is_err());
        explicit.to_pid = Some(2);
        assert_eq!(s.handoff(3, explicit).unwrap(), 2);
        assert_eq!(s.obs.crew.len(), before);
        assert!(
            s.handoff(1, HandoffSummary::new(1, 1, "pi", "invalid root"))
                .is_err()
        );
        let mut wrong = HandoffSummary::new(2, 3, "pi", "wrong rank");
        wrong.to_pid = Some(3);
        assert!(s.handoff(2, wrong).is_err());
        settle(&mut s, 200);
        assert!(s.obs.handoffs.len() >= 4);
        s.game.active = true;
        s.tx.send(Wire::Kernel(
            json!({"ev":"attention","pid":2,"text":"PID 2 / needs_captain: choose course"}),
        ))
        .unwrap();
        s.incoming();
        assert!(!s.game.active);
        assert_eq!(s.selected, 1);
        assert!(s.notice.contains("ATTENTION"));
        s.obs.crew[1].state = "blocked".into();
        s.toggle_game();
        assert!(!s.game.active);
        s.obs.crew[1].transcript = "history line\n".repeat(120);
        s.scroll = 8;
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 44)).unwrap();
        t.draw(|f| ui::draw(f, &mut s)).unwrap();
        let old_lines = s.last_lines;
        let old_scroll = s.scroll;
        s.obs.crew[1].transcript.push_str("new output\n");
        t.draw(|f| ui::draw(f, &mut s)).unwrap();
        assert_eq!(s.scroll, old_scroll + (s.last_lines - old_lines) as u16);
        s.palette = Some(crate::menu::Palette::default());
        s.palette
            .as_mut()
            .unwrap()
            .open(crate::menu::Page::Handoff(1));
        t.draw(|f| ui::draw(f, &mut s)).unwrap();
        assert!(format!("{:?}", t.backend().buffer()).contains("Codex"));
        s.palette_choose(1).unwrap();
        assert_eq!(
            s.palette.as_ref().unwrap().page,
            crate::menu::Page::Compose(crate::menu::Form::Handoff(1, "codex"))
        );
        s.obs.crew.iter_mut().for_each(|a| a.state = "idle".into());
        assert_eq!(s.obs.focus(), "YOUR INTENT");
        s.obs.crew[0].state = "working".into();
        s.obs.crew[0].mission = "real focus".into();
        assert_eq!(s.obs.focus(), "real focus");
        s.game.active = true;
        s.game.fire();
        assert!(s.game.shot.is_some());
    }
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn command_menu_discloses_actions_and_preserves_drafts() {
        use crate::menu::{Form, Page, Palette};
        use crossterm::event::KeyEvent;
        let mut s = Ship::new(true).unwrap();
        settle(&mut s, 50);
        s.toggle_game();
        assert!(s.game.active, "Idle play must be available");
        s.game.active = false;
        s.input = "existing COMMS draft".into();
        s.palette = Some(Palette::default());
        s.palette.as_mut().unwrap().query = "crew".into();
        assert_eq!(s.palette_items().len(), 1);
        s.palette_submit().unwrap();
        assert_eq!(s.palette.as_ref().unwrap().page, Page::Crew);
        s.palette.as_mut().unwrap().query = "scout".into();
        s.palette_submit().unwrap();
        assert_eq!(s.palette.as_ref().unwrap().page, Page::Seat(2));
        s.palette.as_mut().unwrap().query = "cancel".into();
        assert!(
            s.palette_submit().is_err(),
            "Idle cancel must explain why it is unavailable"
        );
        s.palette.as_mut().unwrap().query = "message".into();
        s.palette_submit().unwrap();
        assert_eq!(
            s.palette.as_ref().unwrap().page,
            Page::Compose(Form::Message(2))
        );
        assert!(s.palette_submit().is_err(), "Empty form must not send");
        s.palette.as_mut().unwrap().query = "message from menu".into();
        s.palette_submit().unwrap();
        assert!(s.palette.is_none());
        settle(&mut s, 150);
        assert!(s.obs.crew[1].transcript.contains("message from menu"));
        assert_eq!(s.input, "existing COMMS draft");
        s.palette = Some(Palette::default());
        s.palette.as_mut().unwrap().open(Page::Display);
        s.palette_choose(0).unwrap();
        assert!(!s.motion);
        assert_eq!(s.palette_items()[0].label, "Enable motion");
        s.palette_choose(0).unwrap();
        assert!(s.motion);
        s.palette = Some(Palette::default());
        s.palette.as_mut().unwrap().open(Page::Handoff(2));
        s.palette_choose(1).unwrap();
        assert_eq!(
            s.palette.as_ref().unwrap().page,
            Page::Compose(Form::Handoff(2, "codex"))
        );
        s.palette_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.palette.as_ref().unwrap().page, Page::Handoff(2));
        for (w, h) in [(140, 44), (65, 23)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            for page in [
                Page::Root,
                Page::Crew,
                Page::Seat(2),
                Page::Handoff(2),
                Page::Compose(Form::Mission),
                Page::Galaga,
                Page::Display,
                Page::Help,
            ] {
                s.palette.as_mut().unwrap().open(page);
                terminal.draw(|f| ui::draw(f, &mut s)).unwrap();
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .any(|cell| cell.fg == ratatui::style::Color::Rgb(99, 235, 233))
                );
            }
        }
    }
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn quit_and_reattach_keeps_the_kernel_flight_and_console_prefs() {
        let root = universe();
        let path = root.join(".unvrs/session-demo.json");
        let mut s = Ship::launch_at(&root, true, true, Some(path.clone())).unwrap();
        s.nest_auto = false;
        let pid = s.spawn(1, 2, "WORKER 03", "chart the nebula").unwrap();
        s.enqueue(2, "hello scout".into()).unwrap();
        until(&mut s, |s| {
            s.obs.crew[1].transcript.contains("Simulated report") && s.obs.crew[1].state == "idle"
        });
        assert!(s.obs.crew[1].transcript.contains("Simulated report"));
        s.selected = 1;
        s.input = "unsent draft".into();
        s.motion = false;
        s.logs = true;
        s.log_query = "boot".into();
        s.clock_base = 90;
        s.picker = uke::Backend::Jev;
        let seats = s.obs.crew.len();
        drop(s);
        let mut s = Ship::launch_at(&root, true, false, Some(path.clone())).unwrap();
        assert_eq!(s.obs.crew.len(), seats, "the kernel kept the flight");
        assert!(s.obs.crew[1].transcript.contains("hello scout"));
        assert_eq!(s.obs.crew[pid - 1].mission, "chart the nebula");
        assert_eq!((s.selected, s.input.as_str()), (1, "unsent draft"));
        assert!(!s.motion && s.logs && s.log_query == "boot");
        assert_eq!(s.picker, uke::Backend::Jev, "picker survives relaunch");
        assert!(s.clock() >= 90 && s.notice.contains("FLIGHT RESUMED"));
        s.fresh_flight().unwrap();
        assert!(s.restart && !path.exists(), "/new archives the session");
        drop(s);
        assert!(!path.exists(), "an archived flight is not saved again");
        let s = Ship::launch_at(&root, true, false, Some(path.clone())).unwrap();
        let live = s.obs.crew.iter().filter(|a| a.state != "ended").count();
        assert_eq!(live, 3, "fresh demo crew after /new");
    }
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn a_worker_is_not_ready_until_its_boot_outcome_lands() {
        use crate::nest::Outcome;
        use uke::Seat;
        let mut s = Ship::new(true).unwrap();
        s.nest_auto = false;
        let pid = s
            .launch_worker(2, "probe the hull", "probe the hull")
            .unwrap();
        settle(&mut s, 100);
        assert_eq!(
            s.obs.crew[pid - 1].rank,
            3,
            "worker sits under the chosen parent"
        );
        assert!(
            s.launch_worker(pid, "too deep", "").is_err(),
            "L3 cannot spawn"
        );
        let a = &s.obs.crew[pid - 1];
        assert_eq!(a.state, "nesting");
        assert_eq!(a.queue, 1, "mail waits for the nest");
        let outcome = |refused| Outcome {
            pid,
            mission: "msn_x".into(),
            profile: "pi".into(),
            seat: Seat {
                rank: 3,
                intf_focused: false,
            },
            record: None,
            install: None,
            refused,
        };
        s.apply_nest(outcome(Some(("install", "no harness dir".into()))));
        settle(&mut s, 100);
        let a = &s.obs.crew[pid - 1];
        assert_eq!(a.state, "blocked");
        assert!(
            a.transcript
                .contains("BOOT / msn_x / PID 4 L3 / pi / REFUSED")
        );
        assert!(a.transcript.contains("REFUSED: install · no harness dir"));
        assert_eq!(s.selected, pid - 1, "refusal takes the captain to the seat");
        assert!(s.notice.contains("boot refused at install: no harness dir"));
        assert!(s.boot_seat(pid).is_err(), "no mission admitted yet");
        s.apply_nest(outcome(None));
        settle(&mut s, 150);
        assert!(
            matches!(s.obs.crew[pid - 1].state.as_str(), "working" | "idle"),
            "a ready Boot clears its own block"
        );
        assert!(
            s.obs.crew[pid - 1].transcript.contains("Simulated report")
                || s.obs.crew[pid - 1].state == "working",
            "mail flows once nested"
        );
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 44)).unwrap();
        t.draw(|f| ui::draw(f, &mut s)).unwrap();
        let screen = format!("{:?}", t.backend().buffer());
        assert!(screen.contains("BOOT msn_x"), "Boot chip renders in COMMS");
        assert!(screen.contains("OPEN"), "open notes render");
        s.set_picker("").unwrap();
        assert_eq!(s.picker, uke::Backend::Jev);
        assert!(s.set_picker("nope").is_err());
    }
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn kernel_mailbox_rank_and_ui() {
        let mut s = Ship::new(true).unwrap();
        assert!(s.spawn(3, 4, "INVALID", "mission").is_err());
        assert!(s.enqueue(0, "invalid".into()).is_err());
        assert!(s.enqueue(1, " ".into()).is_err());
        let refused = (0..40)
            .map(|_| s.enqueue(1, "queued".into()))
            .filter(Result::is_err)
            .count();
        assert!(refused > 0, "the kernel mailbox is bounded");
        for (w, h) in [(140, 44), (90, 30), (40, 15), (1, 1)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            t.draw(|f| ui::draw(f, &mut s)).unwrap();
            if w == 140 {
                let rendered = t
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(rendered.contains("UNVRS"));
            }
        }
    }
}
#[cfg(test)]
mod dump {
    use super::*;
    #[test]
    #[ignore]
    fn render_dump() {
        let mut s = Ship::new(true).unwrap();
        s.obs.crew[1].state = "working".into();
        s.obs.crew[2].state = "blocked".into();
        s.tick = 40;
        s.input = "/".into();
        let mode = env::var("DUMP").unwrap_or_default();
        if mode == "resting" {
            s.input.clear();
        }
        if let Some(scene) = mode.strip_prefix("onboard-").and_then(|s| s.parse().ok()) {
            s.onboard = Some(scene);
        }
        if mode == "game" {
            s.input.clear();
            s.game.active = true;
            for _ in 0..5 {
                s.game.step();
            }
        }
        if mode == "comms" {
            s.comms_view = true;
            s.input.clear();
            s.obs.crew[0].transcript = "\nCAPTAIN / MAILBOX\nshow me the build pipeline\n\nCOPILOT\nChecking the repo first.\n[ read Cargo.toml ]\n[ bash cargo test ]\n[ bash unvrs ctl spawn ]\npid: 3\nowner: 3\nstatus: \"delivered\"\nhelp[1]:\n  unvrs ctl --help\nHere is the pipeline:\n```flow\ngraph LR\nA[Plan] :done\nB[Build] :active\nC[Test] :todo\nD[Ship] :blocked\nA -> B -> C -> D\nB -> D: hotfix\ncoverage: 7/10\n```\nDone.\n\nERROR: Seat disconnected\n".into();
        }
        if mode == "logs" {
            s.input.clear();
            s.logs = true;
            s.log_query = "boot".into();
        }
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 44)).unwrap();
        t.draw(|f| ui::draw(f, &mut s)).unwrap();
        let b = t.backend().buffer();
        if let Ok(path) = env::var("UNVRS_RENDER_PATH") {
            let cells = b
                .content
                .iter()
                .map(
                    |c| json!({"s":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)}),
                )
                .collect::<Vec<_>>();
            fs::write(
                path,
                serde_json::to_string(&json!({"width":140,"height":44,"cells":cells})).unwrap(),
            )
            .unwrap();
        }
        for y in 0..44 {
            let line: String = (0..140).map(|x| b[(x, y)].symbol()).collect();
            println!("{line}");
        }
    }
}
