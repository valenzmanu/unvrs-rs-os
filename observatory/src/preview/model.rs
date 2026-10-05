//! The preview's model: one kernel snapshot plus the host probes, read into what the
//! page shows, in plain Rust so the relevance rules are tested without a browser.
//!
//! Truth rules: every value comes from the snapshot or a probe and names its source; what
//! has no source is "unknown" (or a grey "not built" light); nothing is ever made up.
//! Dark cockpit: normal is one quiet line; only faults get amber or red, and always with a
//! reason.
use crate::probe::{Host, KernelState, PidInfo, USAGE_STALE_MS};
use crate::view::dur;
use serde::Serialize;
use serde_json::Value;

/// A worker running with no output for this long is "stalled".
pub const STALL_S: u64 = 10 * 60;
/// A worker whose output is this fresh is producing output now (its light pulses).
pub const PRODUCING_S: u64 = 30;
/// Kernel heartbeat: older than this is degraded, older than `HB_DOWN_S` is down.
pub const HB_WARN_S: u64 = 5;
pub const HB_DOWN_S: u64 = 30;
/// Agent failures stop affecting health after ten minutes without another failure.
const DRIVER_FAILURE_TTL_MS: u64 = 10 * 60 * 1000;
/// Disk free under this is red (blocking), under `DISK_WARN` amber.
pub const DISK_RED: u64 = 10_000_000_000;
pub const DISK_WARN: u64 = 20_000_000_000;
/// Quota remaining under these percentages is amber / red.
pub const QUOTA_WARN: f64 = 20.0;
pub const QUOTA_RED: f64 = 10.0;
/// How many items a zone shows before "+N more".
pub const ZONE_MAX: usize = 3;
/// Surfaces whose deep links are proven to open the right thread. Nothing is proven yet
/// for T3 Code, Codex Desktop or Claude Desktop, so no item gets an "Open" button.
pub const PROVEN_DEEP_LINKS: &[&str] = &[];

pub const SRC_SNAPSHOT: &str = "/api/snapshot";
pub const SRC_STATE: &str = "kernel/state.json";
pub const SRC_JOURNAL: &str = "kernel/journal.jsonl";

/// The one colour language of the page. `Idle` is quiet and fine (nothing active);
/// `Unknown` means there is no source; `Off` is "planned" (not built in this release;
/// never counted toward health).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Tone {
    Ok,
    Idle,
    Unknown,
    Off,
    Warn,
    Bad,
}

impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Tone::Ok => "t-ok",
            Tone::Idle => "t-idle",
            Tone::Unknown => "t-unknown",
            Tone::Off => "t-off",
            Tone::Warn => "t-warn",
            Tone::Bad => "t-bad",
        }
    }
    pub fn word(self) -> &'static str {
        match self {
            Tone::Ok => "ok",
            Tone::Idle => "idle",
            Tone::Unknown => "unknown",
            Tone::Off => "planned",
            Tone::Warn => "degraded",
            Tone::Bad => "down",
        }
    }
    pub fn is_fault(self) -> bool {
        matches!(self, Tone::Warn | Tone::Bad)
    }
}

/// One line of a drawer: what, its value, and where the value came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
    pub source: String,
}

fn fact(label: &str, value: impl Into<String>, source: &str) -> Fact {
    Fact {
        label: label.into(),
        value: value.into(),
        source: source.into(),
    }
}

/// The drawer's text for one row, light or gauge.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Detail {
    pub title: String,
    pub tone: Tone,
    /// Why it has this colour, in one sentence.
    pub reason: String,
    pub facts: Vec<Fact>,
    /// Longer text: a question and its options, an error, a list.
    pub text: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Light {
    pub key: String,
    pub name: String,
    pub tone: Tone,
    pub summary: String,
    /// Age of the signal behind the light ("3m"), or "age unknown".
    pub age: String,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum NeedKind {
    Fault,
    Decision,
    Proposal,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Need {
    pub key: String,
    pub id: String,
    pub kind: NeedKind,
    pub blocking: bool,
    pub tone: Tone,
    pub title: String,
    pub age: String,
    /// "Answer in: <app> · <seat>"
    pub app: String,
    pub seat: String,
    /// Why this route (raising seat live / L1 / nothing live).
    pub route: String,
    /// What the Copy button puts on the clipboard, and how it reads on screen.
    pub copy: String,
    pub shown: String,
    /// A deep link, only for surfaces in `PROVEN_DEEP_LINKS`.
    pub open: Option<String>,
    /// Evidence that this call is already settled (the kernel journal records hold.close), though the kernel still lists it open. Settled calls leave
    /// the list and wait in their own drawer.
    pub settled: Option<String>,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Worker {
    pub routed: Option<String>,
    pub go_quote: Option<String>,
    pub done_when: Option<String>,
    pub key: String,
    pub pid: u64,
    /// The PID that started it (snapshot workers[].parent, else kernel/state.json).
    pub parent: Option<u64>,
    pub title: String,
    pub project: String,
    /// What it is working on: its brief's `now`, else its first `next` (snapshot).
    pub doing: Option<String>,
    pub harness: String,
    pub model: String,
    pub effort: String,
    pub tone: Tone,
    pub state: String,
    /// The light pulses while recent lifecycle or transcript activity is observed.
    pub pulse: bool,
    pub age: String,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Seat {
    pub routed: Option<String>,
    pub key: String,
    pub pid: u64,
    pub rank: u64,
    pub project: Option<String>,
    pub label: String,
    /// What it is working on: its brief's `now`, else its first `next` (snapshot).
    pub doing: Option<String>,
    pub tone: Tone,
    pub state: String,
    pub harness: String,
    pub app: Option<String>,
    pub model: String,
    pub effort: String,
    /// No thread, no run and no known model: the row draws no model chip.
    pub vacant: bool,
    pub pulse: bool,
    pub age: String,
    pub workers: Vec<Worker>,
    pub idle_workers: usize,
    /// The idle workers themselves (the crew diagram draws them dimmed).
    pub resting: Vec<Worker>,
    /// Its L3 workers that have ended or handed off (kernel/state.json).
    pub finished: usize,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Crew {
    pub l1: Option<Seat>,
    /// Leads with something running: expanded.
    pub active: Vec<Seat>,
    /// Leads with nothing running: one collapsed line.
    pub idle: Vec<Seat>,
    /// Workers whose lead is not in the snapshot.
    pub loose: Vec<Worker>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gauge {
    pub key: String,
    pub label: String,
    /// How full the bar is drawn (0..1), None when unmeasured.
    pub fill: Option<f64>,
    pub value: String,
    pub tone: Tone,
    pub source: String,
    pub age: String,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WsRow {
    pub key: String,
    pub name: String,
    pub owner: String,
    pub merged: String,
    pub size: String,
    pub live: bool,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fuel {
    pub quota: Vec<Gauge>,
    pub disk: Gauge,
    pub ws: Gauge,
    pub ws_rows: Vec<WsRow>,
    pub ws_more: usize,
}

/// One task's context-ownership check (context-ownership.md): what UNVRS holds of its
/// job context and whether its handoff passed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CtxRow {
    pub key: String,
    pub pid: u64,
    pub project: String,
    pub harness: String,
    pub title: String,
    /// passed · passed after n bounces · bounced · blocked
    pub verdict: String,
    pub tone: Tone,
    pub age: String,
    /// The first leak of the latest check, when it failed.
    pub leak: Option<String>,
    /// Times the check sent this task's result back.
    pub bounces: u64,
    pub detail: Detail,
}

/// The Context zone: the latest context-ownership checks.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Context {
    pub rows: Vec<CtxRow>,
    pub passed: usize,
    /// Tasks the check sent back at least once (fixed since or not).
    pub bounced: usize,
    pub blocked: usize,
    /// The probe could read the task folders.
    pub read: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Model {
    pub sim: bool,
    /// The fixture's name in sim mode ("problem", "calm"); None for real data.
    pub fixture: Option<String>,
    pub now_ms: u64,
    pub status_tone: Tone,
    pub status: String,
    /// Open calls and faults, most important first; settled calls are in `settled`.
    pub needs: Vec<Need>,
    /// Calls the kernel lists open but the journal shows settled (hidden from the list).
    pub settled: Vec<Need>,
    /// Kernel activity, one journal-event count per 5 minutes over the last hour, oldest
    /// first; None where the journal window does not reach.
    pub activity: Vec<Option<u32>>,
    pub kernel: Light,
    pub drivers: Vec<Light>,
    pub surfaces: Vec<Light>,
    pub crew: Crew,
    pub fuel: Fuel,
    pub context: Context,
}

// ---------------------------------------------------------------- helpers

fn st(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(x) => x.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
fn arr(v: &Value, k: &str) -> Vec<Value> {
    v[k].as_array().cloned().unwrap_or_default()
}
fn clip(text: &str, n: usize) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= n {
        t
    } else {
        format!("{}…", t.chars().take(n).collect::<String>())
    }
}
fn ago(now: u64, at: u64) -> String {
    if at == 0 {
        "age unknown".into()
    } else {
        format!("{} ago", dur(now.saturating_sub(at) / 1000))
    }
}
pub fn gb(n: u64) -> String {
    let g = n as f64 / 1e9;
    if g >= 100.0 {
        format!("{g:.0} GB")
    } else if g >= 1.0 {
        format!("{g:.1} GB")
    } else {
        format!("{:.0} MB", n as f64 / 1e6)
    }
}
pub fn app_name(app: &str) -> String {
    match app {
        "t3" => "T3 Code".into(),
        "codex-app" => "Codex Desktop".into(),
        "claude-desktop" => "Claude Desktop".into(),
        "claude-cli" => "Claude Code CLI".into(),
        "codex-cli" => "Codex CLI".into(),
        "" | "headless" => "headless".into(),
        other => other.into(),
    }
}
fn seat_label(seat: &Value) -> String {
    match seat["rank"].as_u64() {
        Some(1) => "L1".into(),
        _ => match seat["project"].as_str() {
            Some(p) => format!("L2 {p}"),
            None => format!("PID {}", st(seat, "pid")),
        },
    }
}
/// A seat whose thread is live in an app (a place the captain can type).
fn live_thread(seat: &Value) -> Option<String> {
    let app = seat["occupied_by"]["app"].as_str()?;
    (matches!(seat["state"].as_str(), Some("live" | "running")) && app != "headless")
        .then(|| app.to_owned())
}

/// The command prefix the captain types in `app` (a route app name): Claude's slash
/// form in Claude apps, `$unvrs:` elsewhere (the kernel parses both).
fn command_prefix(app: &str) -> &'static str {
    if app.starts_with("Claude") {
        "/unvrs:"
    } else {
        "$unvrs:"
    }
}

/// The exact text the captain types for a call in `app` (clipboard), and how it reads
/// on screen.
pub fn call_copy(c: &Value, app: &str) -> (String, String) {
    let id = st(c, "id");
    let pre = command_prefix(app);
    if st(c, "kind") == "proposal" {
        let t = format!("{pre}approve {id}");
        (t.clone(), t)
    } else {
        (
            format!("{pre}answer {id} "),
            format!("{pre}answer {id} <your answer>"),
        )
    }
}

/// The captain's apps whose process runs now (ps), or all three when none is known to.
fn open_apps(host: &Host) -> String {
    let all = [
        ("T3 Code", host.procs.as_ref().map(|p| p.t3)),
        ("Codex Desktop", host.procs.as_ref().map(|p| p.codex)),
        (
            "Claude Desktop",
            host.procs.as_ref().map(|p| p.claude_desktop),
        ),
    ];
    let running: Vec<&str> = all
        .iter()
        .filter(|(_, r)| *r == Some(true))
        .map(|(n, _)| *n)
        .collect();
    let names: Vec<&str> = if running.is_empty() {
        all.iter().map(|(n, _)| *n).collect()
    } else {
        running
    };
    match names.split_last() {
        Some((last, [])) => (*last).into(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// Where the captain answers: the seat that raised it if its thread is live, else the
/// live L1 thread, else which app to open. Returns (app, seat, why, open link).
pub fn route(
    snap: &Value,
    host: &Host,
    by: Option<u64>,
) -> (String, String, String, Option<String>) {
    let seats = arr(snap, "seats");
    let open = |s: &Value, app: &str| -> Option<String> {
        PROVEN_DEEP_LINKS
            .contains(&app)
            .then(|| s["occupied_by"]["link"].as_str().map(str::to_owned))
            .flatten()
    };
    let raiser = by.and_then(|b| seats.iter().find(|s| s["pid"].as_u64() == Some(b)));
    if let Some(s) = raiser
        && let Some(app) = live_thread(s)
    {
        return (
            app_name(&app),
            seat_label(s),
            format!(
                "Raised by {}; its thread is live in {}.",
                seat_label(s),
                app_name(&app)
            ),
            open(s, &app),
        );
    }
    let why_not = match (by, raiser) {
        (None, _) => "The kernel does not say which seat raised it".to_string(),
        (Some(b), None) => format!("Raised by PID {b}, which has no seat now"),
        (Some(_), Some(s)) => format!("Raised by {}, which has no live thread", seat_label(s)),
    };
    if let Some(l1) = seats.iter().find(|s| s["rank"].as_u64() == Some(1))
        && let Some(app) = live_thread(l1)
    {
        return (
            app_name(&app),
            "L1".into(),
            format!("{why_not}; the L1 thread is live in {}.", app_name(&app)),
            open(l1, &app),
        );
    }
    let apps = open_apps(host);
    (
        apps.clone(),
        "any thread".into(),
        format!(
            "{why_not}, and no L1 thread is live: open {apps} and type the command in any thread."
        ),
        None,
    )
}

// ---------------------------------------------------------------- build

pub struct Inputs<'a> {
    pub snap: &'a Value,
    pub host: &'a Host,
    pub now_ms: u64,
    pub sim: bool,
}

impl Model {
    pub fn build(i: Inputs) -> Model {
        let Inputs {
            snap,
            host,
            now_ms: now,
            sim,
        } = i;
        let kernel = kernel_light(snap, now);
        let drivers = driver_lights(host, now);
        let surfaces = surface_lights(snap, host, now);
        let mut crew = crew(snap, host, now);
        let context = context(host, now);
        mark_workers(&mut crew, &context);
        let fuel = fuel(snap, host, now);
        let (needs, settled) = needs(snap, host, now, &kernel, &drivers, &fuel);
        let activity = activity(host, now);
        let (status_tone, status) = status(&kernel, &needs, &drivers, &surfaces, &crew, &fuel);
        Model {
            sim,
            fixture: sim.then(|| "problem".to_owned()),
            now_ms: now,
            status_tone,
            status,
            needs,
            settled,
            activity,
            kernel,
            drivers,
            surfaces,
            crew,
            fuel,
            context,
        }
    }

    /// The drawer text behind a key (row, light, gauge).
    pub fn detail(&self, key: &str) -> Option<Detail> {
        let mut all: Vec<(&str, &Detail)> = vec![(&self.kernel.key, &self.kernel.detail)];
        all.extend(self.needs.iter().map(|n| (n.key.as_str(), &n.detail)));
        all.extend(self.settled.iter().map(|n| (n.key.as_str(), &n.detail)));
        all.extend(self.drivers.iter().map(|l| (l.key.as_str(), &l.detail)));
        all.extend(self.surfaces.iter().map(|l| (l.key.as_str(), &l.detail)));
        let seats = self
            .crew
            .l1
            .iter()
            .chain(self.crew.active.iter())
            .chain(self.crew.idle.iter());
        for s in seats {
            all.push((&s.key, &s.detail));
            all.extend(s.workers.iter().map(|w| (w.key.as_str(), &w.detail)));
        }
        all.extend(self.crew.loose.iter().map(|w| (w.key.as_str(), &w.detail)));
        all.extend(
            self.context
                .rows
                .iter()
                .map(|r| (r.key.as_str(), &r.detail)),
        );
        all.extend(self.fuel.quota.iter().map(|g| (g.key.as_str(), &g.detail)));
        all.push((&self.fuel.disk.key, &self.fuel.disk.detail));
        all.push((&self.fuel.ws.key, &self.fuel.ws.detail));
        all.extend(
            self.fuel
                .ws_rows
                .iter()
                .map(|w| (w.key.as_str(), &w.detail)),
        );
        all.into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| d.clone())
    }
}

// ---------------------------------------------------------------- context ownership

const SRC_OWNERSHIP: &str = "tasks/pid-<n>/ownership.json";

fn items(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

fn listed(items: &[String]) -> String {
    match items.len() {
        0 => "not given".into(),
        _ if items.iter().all(|i| i.trim().eq_ignore_ascii_case("none")) => {
            "none (said explicitly)".into()
        }
        n => format!("{n}"),
    }
}

fn ctx_row(v: &Value, now: u64) -> CtxRow {
    let pid = v["pid"].as_u64().unwrap_or(0);
    let c = &v["check"];
    let h = &c["handoff"];
    let leaks = items(&c["leaks"]);
    let bounces = v["bounces"].as_u64().unwrap_or(0);
    let max = v["max_bounces"].as_u64().unwrap_or(3);
    let (verdict, tone, reason) = match v["verdict"].as_str().unwrap_or("") {
        "accepted" if bounces > 0 => (
            format!("passed after {bounces} bounce{}", if bounces == 1 { "" } else { "s" }),
            Tone::Ok,
            "The check sent the result back, the worker fixed it, and UNVRS now holds this task's brief, record, deliverables and learnings.".to_owned(),
        ),
        "accepted" => (
            "passed".into(),
            Tone::Ok,
            "UNVRS holds this task's brief, record, deliverables and learnings; its handoff passed the check.".into(),
        ),
        "bounced" => (
            "bounced".into(),
            Tone::Warn,
            format!("The check failed ({bounces}/{max}); the result went back to the worker with each leak below."),
        ),
        "blocked" => (
            "blocked".into(),
            Tone::Bad,
            format!("The check failed {} times; the task ended blocked.", bounces + 1),
        ),
        other => (
            format!("unknown ({other})"),
            Tone::Unknown,
            "The check record has no verdict UNVRS knows.".into(),
        ),
    };
    let deliverables: Vec<String> = c["deliverables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            let mark = match (d["owned"].as_bool(), d["exists"].as_bool()) {
                (Some(true), Some(true)) => "✓ held",
                (Some(false), _) => "✗ outside UNVRS",
                _ => "✗ missing",
            };
            format!(
                "{mark} · {} · {}",
                d["kind"].as_str().unwrap_or("?"),
                d["resolved"]
                    .as_str()
                    .unwrap_or(d["item"].as_str().unwrap_or(""))
            )
        })
        .collect();
    let decisions = items(&h["decisions"]);
    let learnings = items(&h["learnings"]);
    let notes = items(&v["notes"]);
    let mut text = vec![if leaks.is_empty() {
        "Leaks: none.".to_owned()
    } else {
        format!("Leaks: {}", leaks.join(" · "))
    }];
    if !deliverables.is_empty() {
        text.push(format!("Deliverables: {}", deliverables.join(" · ")));
    }
    if !decisions.is_empty() {
        text.push(format!("Decisions: {}", decisions.join(" · ")));
    }
    if !learnings.is_empty() {
        text.push(format!("Learnings: {}", learnings.join(" · ")));
    }
    let history: Vec<String> = v["history"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let n = e["leaks"].as_array().map_or(0, Vec::len);
            match n {
                0 => e["verdict"].as_str().unwrap_or("?").to_owned(),
                _ => format!(
                    "{} ({n} leak{})",
                    e["verdict"].as_str().unwrap_or("?"),
                    if n == 1 { "" } else { "s" }
                ),
            }
        })
        .collect();
    if history.len() > 1 {
        text.push(format!("History: {}", history.join(" → ")));
    }
    let at = v["at"].as_u64().unwrap_or(0);
    let age = if at == 0 {
        "age unknown".into()
    } else {
        ago(now, at)
    };
    let harness = v["harness"].as_str().unwrap_or("").to_owned();
    let title = v["intent"]
        .as_str()
        .map(|t| clip(t, 90))
        .unwrap_or_else(|| format!("PID {pid}"));
    let owned = c["deliverables"].as_array().map_or(0, |d| {
        d.iter()
            .filter(|d| d["owned"] == true && d["exists"] == true)
            .count()
    });
    let record = c["record"]
        .as_array()
        .and_then(|r| r.first())
        .and_then(|r| r.as_str())
        .unwrap_or("not held");
    let facts = vec![
        fact("Check", format!("{verdict} · {age}"), SRC_OWNERSHIP),
        fact("Task", title.clone(), "contract.json"),
        fact(
            "Harness",
            match v["model"].as_str() {
                Some(m) => format!("{harness} · {m}"),
                None if harness.is_empty() => "unknown".into(),
                None => harness.clone(),
            },
            "result.json",
        ),
        fact(
            "Brief",
            c["brief"].as_str().unwrap_or("not held").to_owned(),
            SRC_OWNERSHIP,
        ),
        fact(
            "Record",
            format!(
                "{} turns · {record}",
                c["record_turns"].as_u64().unwrap_or(0)
            ),
            SRC_OWNERSHIP,
        ),
        fact(
            "Deliverables",
            match deliverables.len() {
                0 => listed(&items(&h["deliverables"])),
                n => format!("{n} · {owned} held by UNVRS"),
            },
            SRC_OWNERSHIP,
        ),
        fact("Decisions", listed(&decisions), SRC_OWNERSHIP),
        fact("Learnings", listed(&learnings), SRC_OWNERSHIP),
        fact(
            "Notes filed",
            if notes.is_empty() {
                "none".into()
            } else {
                notes.join(", ")
            },
            "projects/<p>/memory/notes",
        ),
        fact("Bounces", format!("{bounces} of {max}"), SRC_OWNERSHIP),
        fact(
            "Task folder",
            v["dir"].as_str().unwrap_or("unknown").to_owned(),
            SRC_OWNERSHIP,
        ),
    ];
    CtxRow {
        key: format!("ctx:{pid}"),
        pid,
        project: v["project"].as_str().unwrap_or("").to_owned(),
        harness,
        title: title.clone(),
        verdict: verdict.clone(),
        tone,
        age,
        leak: leaks.first().cloned(),
        bounces,
        detail: Detail {
            title: format!("PID {pid} · context ownership"),
            tone,
            reason,
            facts,
            text,
        },
    }
}

fn context(host: &Host, now: u64) -> Context {
    let Some(list) = &host.ownership else {
        return Context::default();
    };
    let rows: Vec<CtxRow> = list.iter().map(|v| ctx_row(v, now)).collect();
    Context {
        passed: rows.iter().filter(|r| r.tone == Tone::Ok).count(),
        bounced: rows
            .iter()
            .filter(|r| r.tone == Tone::Warn || r.bounces > 0)
            .count(),
        blocked: rows.iter().filter(|r| r.tone == Tone::Bad).count(),
        rows,
        read: true,
    }
}

/// A live worker whose result was checked shows the verdict in its drawer.
fn mark_workers(crew: &mut Crew, ctx: &Context) {
    let seats = crew
        .l1
        .iter_mut()
        .chain(crew.active.iter_mut())
        .chain(crew.idle.iter_mut());
    let mut workers: Vec<&mut Worker> = vec![];
    for s in seats {
        workers.extend(s.workers.iter_mut());
        workers.extend(s.resting.iter_mut());
    }
    workers.extend(crew.loose.iter_mut());
    for w in workers {
        if let Some(r) = ctx.rows.iter().find(|r| r.pid == w.pid) {
            let value = match &r.leak {
                Some(l) => format!("{} · {l}", r.verdict),
                None => r.verdict.clone(),
            };
            w.detail
                .facts
                .push(fact("Context check", value, SRC_OWNERSHIP));
        }
    }
}

fn kernel_light(snap: &Value, now: u64) -> Light {
    let k = &snap["kernel"];
    let at = snap["at"].as_u64().unwrap_or(0);
    let version = k["version"].as_str().map(str::to_owned);
    let uptime = k["uptime_s"].as_u64();
    let hb = (at > 0).then(|| now.saturating_sub(at) / 1000);
    let (tone, summary, reason) = match (k.is_object(), hb) {
        (true, Some(h)) if h <= HB_WARN_S => (
            Tone::Ok,
            format!(
                "v{} · up {} · heartbeat {}",
                version.clone().unwrap_or_else(|| "unknown".into()),
                uptime.map(dur).unwrap_or_else(|| "unknown".into()),
                dur(h)
            ),
            "The kernel answered within the last few seconds.".to_string(),
        ),
        (true, Some(h)) if h <= HB_DOWN_S => (
            Tone::Warn,
            format!("heartbeat {} old", dur(h)),
            format!(
                "No fresh snapshot for {}; the kernel is slow to answer.",
                dur(h)
            ),
        ),
        (true, Some(h)) => (
            Tone::Bad,
            format!("not answering for {}", dur(h)),
            format!(
                "The last snapshot is {} old; the kernel is not answering.",
                dur(h)
            ),
        ),
        _ => (
            Tone::Bad,
            "no snapshot".into(),
            "The kernel has not answered since this page started.".into(),
        ),
    };
    Light {
        key: "kernel".into(),
        name: "Kernel".into(),
        tone,
        summary,
        age: hb
            .map(|h| format!("{} ago", dur(h)))
            .unwrap_or_else(|| "age unknown".into()),
        detail: Detail {
            title: "Kernel".into(),
            tone,
            reason,
            facts: vec![
                fact(
                    "version",
                    version.unwrap_or_else(|| "unknown".into()),
                    "/api/snapshot kernel.version",
                ),
                fact(
                    "uptime",
                    uptime.map(dur).unwrap_or_else(|| "unknown".into()),
                    "/api/snapshot kernel.uptime_s",
                ),
                fact(
                    "heartbeat",
                    hb.map(|h| format!("{} ago", dur(h)))
                        .unwrap_or_else(|| "never".into()),
                    "/api/snapshot at (kernel clock) vs now",
                ),
                fact("os pid", st(k, "os_pid"), "/api/snapshot kernel.os_pid"),
                fact("home", st(k, "home"), "/api/snapshot kernel.home"),
            ],
            text: vec![],
        },
    }
}

struct DriverSpec {
    key: &'static str,
    name: &'static str,
    role: &'static str,
    kinds: &'static [&'static str],
    built: bool,
    why_not_built: &'static str,
    /// For a planned driver: what happens today instead, shown inline on the strip.
    instead: &'static str,
}

// Live drivers first; planned Boot never counts toward health.
const DRIVERS: [DriverSpec; 6] = [
    DriverSpec {
        key: "agent",
        name: "Agent",
        role: "DrvAgent runs the CPUs: every driven turn",
        kinds: &["turn"],
        built: true,
        why_not_built: "",
        instead: "",
    },
    DriverSpec {
        key: "hdff",
        name: "Hdff",
        role: "DrvHdff folds briefs and carries seat moves",
        kinds: &["fold", "move"],
        built: true,
        why_not_built: "",
        instead: "",
    },
    DriverSpec {
        key: "obs",
        name: "Obs",
        role: "DrvObs (folded into uKe in 0.8) writes the kernel journal",
        kinds: &[],
        built: true,
        why_not_built: "",
        instead: "",
    },
    DriverSpec {
        key: "intf",
        name: "Intf",
        role: "DrvIntf carries the captain's commands ($unvrs:…)",
        kinds: &["captain"],
        built: true,
        why_not_built: "",
        instead: "",
    },
    DriverSpec {
        key: "econ",
        name: "Econ",
        role: "DrvEcon admits work using policy, profiles and live harness evidence",
        kinds: &[
            "econ.catalog",
            "econ.route",
            "econ.error",
            "econ.catalog.invalidated",
        ],
        built: true,
        why_not_built: "",
        instead: "",
    },
    DriverSpec {
        key: "boot",
        name: "Boot",
        role: "DrvBoot would make a seat ready before work is assigned",
        kinds: &[],
        built: false,
        why_not_built: "DrvBoot is a stub inside uKe (uke/src/boot.rs); the crate is deferred (docs/design/codebase-architecture.md).",
        instead: "seats boot on first use",
    },
];

/// Did this journal event fail, and why.
fn failure(e: &Value) -> Option<String> {
    if let Some(err) = e["error"].as_str().filter(|s| !s.is_empty()) {
        return Some(err.to_owned());
    }
    if e["result"].as_str() == Some("rejected") {
        return Some(format!("{} rejected", st(e, "kind")));
    }
    if e["ok"].as_bool() == Some(false) {
        return Some(format!("{} {} failed", st(e, "kind"), st(e, "op")));
    }
    None
}

fn driver_lights(host: &Host, now: u64) -> Vec<Light> {
    DRIVERS
        .iter()
        .map(|d| {
            let src = format!("{SRC_JOURNAL} kind={}", d.kinds.join("|"));
            let mk = |tone: Tone,
                      summary: String,
                      age: String,
                      reason: String,
                      facts: Vec<Fact>,
                      text: Vec<String>| Light {
                key: format!("drv:{}", d.key),
                name: d.name.into(),
                tone,
                summary,
                age,
                detail: Detail {
                    title: format!("{} driver", d.name),
                    tone,
                    reason,
                    facts,
                    text,
                },
            };
            if !d.built {
                // planned, not an outage: say so inline, with what happens instead
                return mk(
                    Tone::Off,
                    format!("planned · not in 0.8 · {}", d.instead),
                    "—".into(),
                    format!(
                        "{} Planned, not part of 0.8; it never counts toward health.",
                        d.why_not_built
                    ),
                    vec![
                        fact("role", d.role, "docs/design"),
                        fact("today", d.instead, "docs/design"),
                    ],
                    vec![],
                );
            }
            let Some(j) = &host.journal else {
                let (tone, what) = if d.key == "obs" {
                    (Tone::Bad, "the kernel journal cannot be read")
                } else {
                    (Tone::Unknown, "no journal to read its events from")
                };
                return mk(
                    tone,
                    what.into(),
                    "age unknown".into(),
                    format!("{what}."),
                    vec![fact("role", d.role, "docs/design")],
                    vec![],
                );
            };
            if d.kinds.is_empty() {
                // Obs: the journal itself is its output
                let last = j.events.last().and_then(|e| e["at"].as_u64()).unwrap_or(0);
                return mk(
                    Tone::Ok,
                    format!("journal writing · last event {}", ago(now, last)),
                    ago(now, last),
                    "The kernel journal is readable and has events.".into(),
                    vec![
                        fact("role", d.role, "docs/design"),
                        fact("journal", j.path.clone(), SRC_JOURNAL),
                        fact("last event", ago(now, last), SRC_JOURNAL),
                        fact("events read", j.events.len().to_string(), "journal tail"),
                    ],
                    vec![],
                );
            }
            let evs: Vec<&Value> = j
                .events
                .iter()
                .filter(|e| d.kinds.contains(&e["kind"].as_str().unwrap_or("")))
                .collect();
            let Some(last) = evs.last() else {
                return mk(
                    Tone::Unknown,
                    "no events yet".into(),
                    "age unknown".into(),
                    format!(
                        "No {} events in the journal tail, so its health is unknown.",
                        d.kinds.join("/")
                    ),
                    vec![
                        fact("role", d.role, "docs/design"),
                        fact("source", src.clone(), SRC_JOURNAL),
                    ],
                    vec![],
                );
            };
            let latest = *last;
            let recent = |e: &Value| {
                e["at"]
                    .as_u64()
                    .is_none_or(|at| now.saturating_sub(at) < DRIVER_FAILURE_TTL_MS)
            };
            let mut seen = std::collections::BTreeSet::new();
            let outstanding = if d.key == "agent" {
                evs.iter().rev().copied().find(|e| {
                    seen.insert(e["pid"].as_u64()) && failure(e).is_some() && recent(e)
                })
            } else {
                None
            };
            let last = outstanding.unwrap_or(latest);
            let error = if d.key == "agent" {
                outstanding.and_then(failure)
            } else {
                failure(last)
            };
            let expired = d.key == "agent" && error.is_none() && failure(latest).is_some();
            let at = last["at"].as_u64().unwrap_or(0);
            let fails = evs
                .iter()
                .rev()
                .take_while(|e| failure(e).is_some() && (d.key != "agent" || recent(e)))
                .count();
            let mut facts = vec![
                fact("role", d.role, "docs/design"),
                fact(
                    if d.key == "agent" { "health event" } else { "last event" },
                    format!("{} · {}", st(last, "kind"), ago(now, at)),
                    &src,
                ),
            ];
            if let Some(p) = last["pid"].as_u64() {
                facts.push(fact("pid", format!("PID {p}"), &src));
            }
            match error {
                None => mk(
                    Tone::Ok,
                    if expired {
                        "no failures in 10m".into()
                    } else {
                        format!("last {} {}", st(last, "kind"), ago(now, at))
                    },
                    ago(now, at),
                    if expired {
                        "Its last failure is at least 10 minutes old; no new failure was recorded.".into()
                    } else {
                        format!("Its last {} succeeded.", st(last, "kind"))
                    },
                    facts,
                    vec![],
                ),
                Some(err) => {
                    let tone = if fails >= 3 { Tone::Bad } else { Tone::Warn };
                    facts.push(fact("failed in a row", fails.to_string(), &src));
                    let pid = last["pid"].as_u64().map_or_else(
                        || "PID unknown".into(),
                        |pid| format!("PID {pid}"),
                    );
                    mk(
                        tone,
                        if d.key == "agent" {
                            format!("{pid} · {} · {}", clip(&err, 160), ago(now, at))
                        } else {
                            format!("{} failed {}", st(last, "kind"), ago(now, at))
                        },
                        ago(now, at),
                        if d.key == "agent" {
                            format!("{pid}'s last turn failed. {fails} consecutive driver turns failed. Clears on that PID's next successful turn or after 10 minutes without a new failure.")
                        } else if fails >= 3 {
                            format!("Its last {fails} {} events failed.", st(last, "kind"))
                        } else {
                            format!("Its last {} failed.", st(last, "kind"))
                        },
                        facts,
                        vec![format!("Last error: {}", clip(&err, 400))],
                    )
                }
            }
        })
        .collect()
}

fn surface_lights(snap: &Value, host: &Host, now: u64) -> Vec<Light> {
    let seats = arr(snap, "seats");
    // Every surface is built: uke procinfo::app_of tells claude-desktop threads apart.
    let spec: [(&str, &str, Option<&str>); 3] = [
        ("t3", "T3 Code", Some("t3")),
        ("codex", "Codex Desktop", Some("codex-app")),
        ("claude", "Claude Desktop", Some("claude-desktop")),
    ];
    spec.iter()
        .map(|(key, name, app)| {
            let running = host.procs.as_ref().map(|p| match *key {
                "t3" => p.t3,
                "codex" => p.codex,
                _ => p.claude_desktop,
            });
            let procs_at = host.procs.as_ref().map(|p| p.at_ms).unwrap_or(0);
            let proc_fact = fact(
                "app process",
                match running {
                    Some(true) => "running".to_string(),
                    Some(false) => "not running".to_string(),
                    None => "unknown".to_string(),
                },
                "ps -axo comm",
            );
            let mk =
                |tone: Tone, summary: String, age: String, reason: String, facts: Vec<Fact>| {
                    Light {
                        key: format!("srf:{key}"),
                        name: (*name).into(),
                        tone,
                        summary,
                        age,
                        detail: Detail {
                            title: (*name).into(),
                            tone,
                            reason,
                            facts,
                            text: vec![],
                        },
                    }
                };
            let Some(app) = app else {
                return mk(
                    Tone::Off,
                    "not built".into(),
                    "—".into(),
                    format!(
                        "The kernel cannot tell {name} threads apart, so UNVRS has no seat there."
                    ),
                    vec![proc_fact],
                );
            };
            let here: Vec<&Value> = seats
                .iter()
                .filter(|s| s["occupied_by"]["app"].as_str() == Some(app))
                .collect();
            let live: Vec<&&Value> = here.iter().filter(|s| live_thread(s).is_some()).collect();
            let seen = host
                .state
                .as_ref()
                .map(|k| {
                    k.threads
                        .iter()
                        .filter(|t| t.app == *app)
                        .map(|t| t.last_seen)
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let mut facts = vec![
                proc_fact,
                fact(
                    "seats here",
                    if here.is_empty() {
                        "none".into()
                    } else {
                        here.iter()
                            .map(|s| format!("{} ({})", seat_label(s), st(s, "state")))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    "/api/snapshot seats[].occupied_by.app",
                ),
                fact(
                    "last seen",
                    ago(now, seen),
                    "kernel/state.json threads[].last_seen",
                ),
                fact(
                    "deep link",
                    "not proven: no Open button",
                    "PROVEN_DEEP_LINKS",
                ),
            ];
            if !live.is_empty() {
                let names = live
                    .iter()
                    .map(|s| seat_label(s))
                    .collect::<Vec<_>>()
                    .join(", ");
                facts.truncate(4);
                return mk(
                    Tone::Ok,
                    format!("{names} live here"),
                    ago(now, seen),
                    format!("{names} has a live thread in {name}."),
                    facts,
                );
            }
            match running {
                Some(true) => mk(
                    Tone::Idle,
                    "open · no live seat".into(),
                    ago(now, procs_at),
                    format!("{name} is running; no UNVRS seat is live in it."),
                    facts,
                ),
                Some(false) => mk(
                    Tone::Idle,
                    "closed".into(),
                    ago(now, procs_at),
                    format!("{name} is not running."),
                    facts,
                ),
                None => mk(
                    Tone::Unknown,
                    "unknown".into(),
                    "age unknown".into(),
                    "The process list could not be read.".into(),
                    facts,
                ),
            }
        })
        .collect()
}

fn worker(w: &Value, state: Option<&KernelState>, host: &Host, now: u64) -> Worker {
    let pid = w["pid"].as_u64().unwrap_or(0);
    let info: Option<&PidInfo> = state.and_then(|k| k.pids.get(&(pid as usize)));
    let requested_model = w["model"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown".into());
    let requested_effort = w["effort"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown".into());
    let model = w["actual_model"]
        .as_str()
        .unwrap_or(&requested_model)
        .to_owned();
    let effort = w["actual_effort"]
        .as_str()
        .unwrap_or(&requested_effort)
        .to_owned();
    let activity = w["last_activity"].as_u64();
    let out = host.outputs.get(&(pid as usize)).copied();
    let out_s = activity.or(out).map(|t| now.saturating_sub(t) / 1000);
    let busy = w["busy"].as_bool().or_else(|| info.map(|p| p.busy));
    let elapsed = w["elapsed_s"].as_u64();
    let kstate = st(w, "state");
    let (tone, state, pulse, reason) = match (kstate.as_str(), out_s, busy) {
        ("working", Some(o), Some(true)) if o >= STALL_S => (
            Tone::Warn,
            format!("stalled · no activity {}", dur(o)),
            false,
            format!(
                "A turn is running but the worker has reported no activity for {} (stalled after {}).",
                dur(o),
                dur(STALL_S)
            ),
        ),
        ("working", Some(o), _) if o <= PRODUCING_S => (
            Tone::Ok,
            "active".into(),
            true,
            "Its lifecycle event or transcript changed in the last half minute.".into(),
        ),
        ("working", Some(o), Some(false)) => (
            Tone::Ok,
            format!("between turns · activity {} ago", dur(o)),
            false,
            "No turn is running; it waits for its next turn.".into(),
        ),
        ("working", Some(o), _) => (
            Tone::Ok,
            format!("working · activity {} ago", dur(o)),
            false,
            "Working; its last activity is recent enough.".into(),
        ),
        ("working", None, _) => (
            Tone::Ok,
            "working · activity unknown".into(),
            false,
            "Working; its last activity is unknown (no lifecycle timestamp or transcript found)."
                .into(),
        ),
        ("blocked" | "held", ..) => (
            Tone::Warn,
            kstate.clone(),
            false,
            "The kernel reports it held or blocked.".into(),
        ),
        (other, ..) => (
            Tone::Idle,
            if other.is_empty() {
                "unknown".into()
            } else {
                other.to_owned()
            },
            false,
            "Not running a turn.".into(),
        ),
    };
    let title = clip(&st(w, "intent"), 90);
    let routed = w["econ"]["role"]
        .as_str()
        .zip(w["econ"]["reason"].as_str())
        .map(|(role, reason)| format!("routed: {role} · {reason}"));
    let alignment = |field: &str| {
        w[field]
            .as_str()
            .or_else(|| w["econ"][field].as_str())
            .map(str::to_owned)
    };
    Worker {
        routed: routed.clone(),
        go_quote: alignment("go_quote"),
        done_when: alignment("done_when"),
        key: format!("pid:{pid}"),
        pid,
        parent: w["parent"]
            .as_u64()
            .or_else(|| info.map(|p| p.parent as u64)),
        title: title.clone(),
        project: st(w, "project"),
        doing: doing(w),
        harness: st(w, "harness"),
        model: model.clone(),
        effort: effort.clone(),
        tone,
        state: state.clone(),
        pulse,
        age: elapsed.map(dur).unwrap_or_else(|| "age unknown".into()),
        detail: Detail {
            title: format!("PID {pid} · {}", st(w, "project")),
            tone,
            reason,
            facts: vec![
                fact(
                    "captain go",
                    alignment("go_quote").unwrap_or_else(|| "not reported".into()),
                    "/api/snapshot workers[].go_quote",
                ),
                fact(
                    "done when",
                    alignment("done_when").unwrap_or_else(|| "not reported".into()),
                    "/api/snapshot workers[].done_when",
                ),
                fact(
                    "state",
                    format!("{kstate} · {state}"),
                    "/api/snapshot workers[].state",
                ),
                fact(
                    "harness",
                    st(w, "harness"),
                    "/api/snapshot workers[].harness",
                ),
                fact(
                    "route",
                    routed.unwrap_or_else(|| "no route receipt".into()),
                    "/api/snapshot workers[].econ",
                ),
                fact(
                    "requested model",
                    requested_model,
                    "/api/snapshot workers[].model",
                ),
                fact(
                    "accepted model",
                    w["actual_model"].as_str().unwrap_or("not yet accepted"),
                    "/api/snapshot workers[].actual_model",
                ),
                fact(
                    "requested effort",
                    requested_effort,
                    "/api/snapshot workers[].effort",
                ),
                fact(
                    "accepted effort",
                    w["actual_effort"].as_str().unwrap_or("not yet accepted"),
                    "/api/snapshot workers[].actual_effort",
                ),
                fact(
                    "configuration evidence",
                    w["effort_evidence"].as_str().unwrap_or("not yet reported"),
                    "/api/snapshot workers[].effort_evidence",
                ),
                fact(
                    "session",
                    w["session"].as_str().unwrap_or("unknown"),
                    "/api/snapshot workers[].session",
                ),
                fact(
                    "recent tools",
                    w["tools"]
                        .as_array()
                        .map(|tools| {
                            tools
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "none reported".into()),
                    "/api/snapshot workers[].tools (names only)",
                ),
                fact(
                    "running for",
                    elapsed.map(dur).unwrap_or_else(|| "unknown".into()),
                    "/api/snapshot workers[].elapsed_s",
                ),
                fact(
                    "last activity",
                    out_s
                        .map(|o| format!("{} ago", dur(o)))
                        .unwrap_or_else(|| "unknown".into()),
                    if activity.is_some() {
                        "/api/snapshot workers[].last_activity (driver lifecycle event)"
                    } else {
                        "transcript mtime (~/.claude/projects/<cwd>/<session>.jsonl)"
                    },
                ),
                fact(
                    "turn running",
                    match busy {
                        Some(true) => "yes",
                        Some(false) => "no",
                        None => "unknown",
                    },
                    "/api/snapshot workers[].busy; kernel state fallback",
                ),
                fact("shape", st(w, "shape"), "/api/snapshot workers[].shape"),
            ],
            text: vec![st(w, "intent")],
        },
    }
}

/// What a fresh brief says before its worker has reported anything.
const BRIEF_PLACEHOLDERS: [&str; 2] = ["not started", "start the task"];

/// A brief line from the snapshot (`now`, else `next`), None when neither is reported.
/// A fresh brief's placeholders count as nothing, so a running worker shows its task
/// intent instead of "not started".
fn doing(v: &Value) -> Option<String> {
    ["now", "next"]
        .iter()
        .find_map(|k| {
            v[*k].as_str().map(str::trim).filter(|t| {
                !t.is_empty() && !BRIEF_PLACEHOLDERS.iter().any(|p| t.eq_ignore_ascii_case(p))
            })
        })
        .map(|t| clip(t, 140))
}

fn crew(snap: &Value, host: &Host, now: u64) -> Crew {
    let state = host.state.as_ref();
    let seats = arr(snap, "seats");
    let workers = arr(snap, "workers");
    // a worker's lead: the snapshot's workers[].parent; state.json, then the project,
    // only for kernels that do not report it
    let parent_of = |w: &Value| -> Option<u64> {
        w["parent"].as_u64().or_else(|| {
            state
                .and_then(|k| k.pids.get(&(w["pid"].as_u64().unwrap_or(0) as usize)))
                .map(|p| p.parent as u64)
        })
    };
    let mut used = vec![false; workers.len()];
    let mut seat_rows: Vec<Seat> = vec![];
    for s in &seats {
        let pid = s["pid"].as_u64().unwrap_or(0);
        let project = s["project"].as_str();
        let idx: Vec<usize> = (0..workers.len())
            .filter(|i| {
                let w = &workers[*i];
                !used[*i]
                    && match parent_of(w) {
                        Some(p) => p == pid,
                        None => project.is_some() && w["project"].as_str() == project,
                    }
            })
            .collect();
        for i in &idx {
            used[*i] = true;
        }
        let mine: Vec<Worker> = idx
            .iter()
            .map(|i| worker(&workers[*i], state, host, now))
            .collect();
        let (active_w, idle_w): (Vec<Worker>, Vec<Worker>) =
            mine.into_iter().partition(|w| w.tone != Tone::Idle);
        let sstate = st(s, "state");
        let thread = state.and_then(|k| {
            k.threads
                .iter()
                .filter(|t| t.pid == pid as usize && t.bound)
                .max_by_key(|t| t.last_seen)
        });
        let turn_open = thread.is_some_and(|t| t.turn_open);
        let info = state.and_then(|k| k.pids.get(&(pid as usize)));
        let (tone, word, pulse) = match sstate.as_str() {
            "running" => (Tone::Ok, "running a turn".to_string(), true),
            "live" => (
                Tone::Ok,
                if turn_open {
                    "live · in a turn".into()
                } else {
                    "live".into()
                },
                turn_open,
            ),
            "away" => (Tone::Idle, "away (thread not live)".into(), false),
            "free" | "idle" => (Tone::Idle, "idle".into(), false),
            "held" => (Tone::Warn, "held".into(), false),
            other => (Tone::Idle, other.to_owned(), false),
        };
        let seen = thread.map(|t| t.last_seen).unwrap_or(0);
        // the seat's newest journal event (a seat run, an outcome, a wake): pids[].updated
        // only moves on some state changes, so the journal is the better clock
        let event = host
            .journal
            .as_ref()
            .and_then(|j| {
                j.events
                    .iter()
                    .filter(|e| e["pid"].as_u64() == Some(pid))
                    .filter_map(|e| e["at"].as_u64())
                    .max()
            })
            .unwrap_or(0);
        let updated = info.map(|p| p.updated).unwrap_or(0);
        let age = if seen > 0 {
            format!("seen {}", ago(now, seen))
        } else if event.max(updated) > 0 {
            format!("last event {}", ago(now, event.max(updated)))
        } else {
            "age unknown".into()
        };
        let app = s["occupied_by"]["app"].as_str().map(app_name);
        let harness = {
            let h = s["occupied_by"]["harness"]
                .as_str()
                .or_else(|| s["harness"].as_str())
                .unwrap_or("");
            if h.is_empty() {
                info.map(|p| p.harness.clone()).unwrap_or_default()
            } else {
                h.to_owned()
            }
        };
        // model · effort as the kernel reports them (never the requested model)
        let model = s["model"].as_str().unwrap_or("unknown").to_owned();
        let effort = s["effort"].as_str().unwrap_or("unknown").to_owned();
        let source = match s["source"].as_str() {
            Some("seat-run") => "/api/snapshot seats[] · accepted by the seat run in flight",
            Some("transcript") => "/api/snapshot seats[] · the thread transcript's last turn",
            Some("last seat-run") => "/api/snapshot seats[] · accepted by the seat's last run",
            _ => "not reported by the kernel",
        };
        let reported = |v: &str| {
            if v == "unknown" {
                "not reported".to_owned()
            } else {
                v.to_owned()
            }
        };
        let vacant = s["occupied_by"].is_null() && s["source"].is_null() && sstate != "running";
        let label = seat_label(s);
        let finished = state.map_or(0, |k| {
            k.pids
                .values()
                .filter(|p| {
                    p.parent as u64 == pid
                        && p.rank == 3
                        && !matches!(p.state.as_str(), "working" | "idle" | "blocked" | "held")
                })
                .count()
        });
        let routed = (sstate == "held"
            || matches!(s["source"].as_str(), Some("seat-run" | "last seat-run")))
        .then(|| {
            s["econ"]["role"]
                .as_str()
                .zip(s["econ"]["reason"].as_str())
                .map(|(role, reason)| format!("routed: {role} · {reason}"))
        })
        .flatten();
        // the full route reason, for when the card's two lines cut it short
        let route_fact = routed
            .clone()
            .map(|r| fact("route", r, "/api/snapshot seats[].econ"));
        seat_rows.push(Seat {
            routed,
            key: format!("seat:{pid}"),
            pid,
            rank: s["rank"].as_u64().unwrap_or(0),
            project: project.map(str::to_owned),
            label: label.clone(),
            doing: doing(s),
            tone,
            state: word.clone(),
            harness: if harness.is_empty() { "harness unknown".into() } else { harness.clone() },
            app: app.clone(),
            model: model.clone(),
            effort: effort.clone(),
            vacant,
            pulse,
            age: age.clone(),
            idle_workers: idle_w.len(),
            resting: idle_w,
            finished,
            workers: active_w,
            detail: Detail {
                title: format!("{label} · PID {pid}"),
                tone,
                reason: format!("The seat is {word}."),
                facts: [
                    fact("state", sstate.clone(), "/api/snapshot seats[].state"),
                    fact(
                        "thread",
                        match (&app, s["occupied_by"]["title"].as_str()) {
                            (Some(a), Some(t)) => format!("{a} · \"{t}\""),
                            (Some(a), None) => a.clone(),
                            _ => "none".into(),
                        },
                        "/api/snapshot seats[].occupied_by",
                    ),
                    fact("harness", if harness.is_empty() { "unknown".into() } else { harness }, "/api/snapshot seats[].occupied_by.harness"),
                    fact("model", reported(&model), source),
                    fact("effort", reported(&effort), source),
                    fact("age", age, "kernel/state.json threads[].last_seen, else the newest kernel/journal.jsonl event of this PID"),
                    fact("wakes waiting", st(s, "wakes"), "/api/snapshot seats[].wakes"),
                ]
                .into_iter()
                .chain(route_fact)
                .collect(),
                text: vec![],
            },
        });
    }
    let loose: Vec<Worker> = workers
        .iter()
        .enumerate()
        .filter(|(i, _)| !used[*i])
        .map(|(_, w)| worker(w, state, host, now))
        .collect();
    let l1_at = seat_rows.iter().position(|s| s.label == "L1");
    let l1 = l1_at.map(|i| seat_rows.remove(i));
    let (active, idle): (Vec<Seat>, Vec<Seat>) = seat_rows
        .into_iter()
        .partition(|s| s.tone != Tone::Idle || !s.workers.is_empty());
    Crew {
        l1,
        active,
        idle,
        loose,
    }
}

/// Tone of a meter from what is left of it (percent).
fn quota_tone(left_pct: f64) -> Tone {
    if left_pct < QUOTA_RED {
        Tone::Bad
    } else if left_pct < QUOTA_WARN {
        Tone::Warn
    } else {
        Tone::Ok
    }
}

fn resets_text(now: u64, r: Option<u64>) -> String {
    match r {
        Some(r) if r > now => format!("in {}", dur((r - now) / 1000)),
        Some(r) => format!("{} ago", dur((now - r) / 1000)),
        None => "unknown".into(),
    }
}

/// The Claude meters: one per rate-limit window, from the usage endpoint reading in the
/// host (`ClaudeUsage`), each carrying the reading's age. Without a reading: one grey
/// meter that says why.
fn claude_gauges(snap: &Value, host: &Host, now: u64) -> Vec<Gauge> {
    // the kernel's own row (rate_limit_event of the last driven turn) stays as a fact
    let kernel_row = arr(snap, "quota")
        .into_iter()
        .find(|q| q["harness"] == "claude")
        .map(|q| {
            let pct = q["remaining_pct"].as_f64();
            format!(
                "{} · {}",
                pct.map(|p| format!("{p:.0}% left"))
                    .unwrap_or_else(|| "unknown".into()),
                st(&q, "source")
            )
        })
        .unwrap_or_else(|| "none".into());
    let Some(u) = host.claude_usage.as_ref() else {
        if host.usage_reading && host.usage_error.is_none() {
            // the first reading after a start is on its way: that is not "unknown"
            return vec![Gauge {
                key: "quota:claude".into(),
                label: "Claude".into(),
                fill: None,
                value: "reading…".into(),
                tone: Tone::Idle,
                source: crate::probe::CLAUDE_USAGE_SOURCE.into(),
                age: "reading now".into(),
                detail: Detail {
                    title: "Claude usage".into(),
                    tone: Tone::Idle,
                    reason: "The first usage reading since the Observatory started is on its way (a few seconds).".into(),
                    facts: vec![
                        fact(
                            "source",
                            crate::probe::CLAUDE_USAGE_SOURCE,
                            "the endpoint Claude Code's own /usage screen reads",
                        ),
                        fact(
                            "saved reading",
                            "none",
                            crate::probe::USAGE_FILE,
                        ),
                        fact(
                            "kernel's own row",
                            kernel_row,
                            "/api/snapshot quota[] (rate_limit_event of the last driven turn; unreliable)",
                        ),
                    ],
                    text: vec![],
                },
            }];
        }
        let why = host
            .usage_error
            .clone()
            .unwrap_or_else(|| "No reading yet.".into());
        return vec![Gauge {
            key: "quota:claude".into(),
            label: "Claude".into(),
            fill: None,
            value: "unknown".into(),
            tone: Tone::Unknown,
            source: crate::probe::CLAUDE_USAGE_SOURCE.into(),
            age: "no reading".into(),
            detail: Detail {
                title: "Claude usage".into(),
                tone: Tone::Unknown,
                reason: format!("No reliable reading: {why}"),
                facts: vec![
                    fact(
                        "source",
                        crate::probe::CLAUDE_USAGE_SOURCE,
                        "the endpoint Claude Code's own /usage screen reads",
                    ),
                    fact(
                        "kernel's own row",
                        kernel_row,
                        "/api/snapshot quota[] (rate_limit_event of the last driven turn; unreliable)",
                    ),
                ],
                text: vec![],
            },
        }];
    };
    let age_ms = now.saturating_sub(u.at_ms);
    let stale = age_ms > USAGE_STALE_MS;
    let age = ago(now, u.at_ms);
    let plan = if u.plan.is_empty() {
        "unknown".to_string()
    } else {
        u.plan.clone()
    };
    let mut out = vec![];
    for (k, name, w) in [
        ("5h", "5-hour window", &u.five_hour),
        ("7d", "7-day window", &u.seven_day),
    ] {
        let Some(w) = w else { continue };
        let left = (100.0 - w.used_pct).max(0.0);
        let tone = if stale {
            Tone::Unknown
        } else {
            quota_tone(left)
        };
        let mut facts = vec![
            fact("used", format!("{:.0}%", w.used_pct), "usage.utilization"),
            fact("left", format!("{left:.0}%"), "100 − used"),
            fact("resets", resets_text(now, w.resets_at), "usage.resets_at"),
            fact("plan", plan.clone(), "Claude Code login · subscriptionType"),
            fact(
                "read",
                age.clone(),
                "when the Observatory last got an answer (every 3 min while a page is open; saved in kernel/claude-usage.json)",
            ),
            fact(
                "source",
                u.source.clone(),
                "the endpoint Claude Code's own /usage screen reads",
            ),
            fact(
                "kernel's own row",
                kernel_row.clone(),
                "/api/snapshot quota[] (rate_limit_event of the last driven turn; unreliable)",
            ),
        ];
        if let Some(e) = &host.usage_error {
            facts.push(fact(
                "last refresh",
                format!("failed: {e}"),
                "the reading shown is the last good one",
            ));
        }
        out.push(Gauge {
            key: format!("quota:claude:{k}"),
            label: format!("Claude · {k}"),
            fill: Some((w.used_pct / 100.0).clamp(0.0, 1.0)),
            value: format!("{:.0}%", w.used_pct),
            tone,
            source: u.source.clone(),
            age: age.clone(),
            detail: Detail {
                title: format!("Claude · {name}"),
                tone,
                reason: if stale {
                    format!(
                        "This reading is {age} and may be out of date: it is only refreshed while an Observatory page is open{}.",
                        if host.usage_reading {
                            "; a fresh one is being read now"
                        } else {
                            ""
                        }
                    )
                } else if left < QUOTA_WARN {
                    format!(
                        "{:.0}% of the {name} is used; resets {}.",
                        w.used_pct,
                        resets_text(now, w.resets_at)
                    )
                } else {
                    format!("{:.0}% of the {name} is used.", w.used_pct)
                },
                facts,
                text: vec![],
            },
        });
    }
    out
}

fn fuel(snap: &Value, host: &Host, now: u64) -> Fuel {
    let mut quota: Vec<Gauge> = claude_gauges(snap, host, now);
    quota.extend(
        arr(snap, "quota")
            .iter()
            .filter(|q| q["harness"] != "claude")
            .enumerate()
            .map(|(i, q)| {
                let left = q["remaining_pct"].as_f64();
                let tone = left.map(quota_tone).unwrap_or(Tone::Unknown);
                let source = {
                    let s = st(q, "source");
                    if s.is_empty() { "unknown".into() } else { s }
                };
                let harness = st(q, "harness");
                let name = match harness.as_str() {
                    "codex" => "Codex".to_string(),
                    "" => format!("quota {i}"),
                    h => h.to_string(),
                };
                let account = st(q, "account");
                let label = name.clone();
                Gauge {
                    key: format!(
                        "quota:{}",
                        if harness.is_empty() {
                            i.to_string()
                        } else {
                            harness.clone()
                        }
                    ),
                    label: label.clone(),
                    fill: left.map(|p| ((100.0 - p) / 100.0).clamp(0.0, 1.0)),
                    value: left
                        .map(|p| format!("{:.0}%", 100.0 - p))
                        .unwrap_or_else(|| "unknown".into()),
                    tone,
                    source: source.clone(),
                    age: "age unknown".into(),
                    detail: Detail {
                        title: format!("{label} usage"),
                        tone,
                        reason: match left {
                            None => "The kernel has no reading of this account's quota.".into(),
                            Some(p) if p < QUOTA_WARN => format!(
                                "Only {p:.0}% left; resets {}.",
                                resets_text(now, q["resets_at"].as_u64())
                            ),
                            Some(p) => format!("{:.0}% used, {p:.0}% left.", 100.0 - p),
                        },
                        facts: vec![
                            fact(
                                "account",
                                if account.is_empty() {
                                    "unknown".into()
                                } else {
                                    account
                                },
                                "/api/snapshot quota[].account",
                            ),
                            fact(
                                "used",
                                left.map(|p| format!("{:.0}%", 100.0 - p))
                                    .unwrap_or_else(|| "unknown".into()),
                                "100 − quota[].remaining_pct",
                            ),
                            fact(
                                "left",
                                left.map(|p| format!("{p:.0}%"))
                                    .unwrap_or_else(|| "unknown".into()),
                                "/api/snapshot quota[].remaining_pct",
                            ),
                            fact(
                                "resets",
                                resets_text(now, q["resets_at"].as_u64()),
                                "/api/snapshot quota[].resets_at",
                            ),
                            fact("measured by", source, "/api/snapshot quota[].source"),
                            fact(
                                "age",
                                "unknown (the kernel's row has no reading time)",
                                "/api/snapshot",
                            ),
                        ],
                        text: vec![],
                    },
                }
            }),
    );
    let disk = match &host.disk {
        Some(d) => {
            let tone = if d.free < DISK_RED {
                Tone::Bad
            } else if d.free < DISK_WARN {
                Tone::Warn
            } else {
                Tone::Ok
            };
            let used = 1.0 - d.free as f64 / d.total.max(1) as f64;
            Gauge {
                key: "disk".into(),
                label: "Disk".into(),
                fill: Some(used.clamp(0.0, 1.0)),
                value: format!("{} free", gb(d.free)),
                tone,
                source: "statvfs".into(),
                age: ago(now, d.at_ms),
                detail: Detail {
                    title: "Disk".into(),
                    tone,
                    reason: match tone {
                        Tone::Bad => {
                            format!("Under {} free: builds and workers fail.", gb(DISK_RED))
                        }
                        Tone::Warn => format!("Under {} free.", gb(DISK_WARN)),
                        _ => format!("More than {} free.", gb(DISK_WARN)),
                    },
                    facts: vec![
                        fact("free", gb(d.free), "statvfs f_bavail"),
                        fact("total", gb(d.total), "statvfs f_blocks"),
                        fact("used", format!("{:.0}%", used * 100.0), "statvfs"),
                        fact("volume of", d.path.clone(), "the UNVRS home"),
                        fact(
                            "threshold",
                            format!("red under {}, amber under {}", gb(DISK_RED), gb(DISK_WARN)),
                            "preview rule",
                        ),
                        fact("measured", ago(now, d.at_ms), "statvfs"),
                    ],
                    text: vec![],
                },
            }
        }
        None => Gauge {
            key: "disk".into(),
            label: "Disk".into(),
            fill: None,
            value: "unknown".into(),
            tone: Tone::Unknown,
            source: "statvfs".into(),
            age: "age unknown".into(),
            detail: Detail {
                title: "Disk".into(),
                tone: Tone::Unknown,
                reason: "statvfs could not read the volume.".into(),
                facts: vec![],
                text: vec![],
            },
        },
    };
    let pid_state = |pid: usize| {
        host.state
            .as_ref()
            .and_then(|k| k.pids.get(&pid))
            .map(|p| p.state.clone())
    };
    let (ws, ws_rows, ws_more) = match &host.workspaces {
        None => (
            Gauge {
                key: "ws".into(),
                label: "Workspaces".into(),
                fill: None,
                value: "unknown".into(),
                tone: Tone::Unknown,
                source: "git worktree list".into(),
                age: "age unknown".into(),
                detail: Detail {
                    title: "Workspaces".into(),
                    tone: Tone::Unknown,
                    reason: "No git source could be read.".into(),
                    facts: vec![],
                    text: vec![],
                },
            },
            vec![],
            0,
        ),
        Some(w) => {
            let mut rows: Vec<(bool, WsRow)> =
                w.list
                    .iter()
                    .filter(|x| !x.main)
                    .map(|x| {
                        let pstate = x.owner_pid.and_then(pid_state);
                        let live = pstate.as_deref() == Some("working");
                        let owner = match (&x.owner, &pstate) {
                            (Some(o), Some(s)) => format!("{o} ({s})"),
                            (Some(o), None) => o.clone(),
                            (None, _) => "unknown".into(),
                        };
                        let merged = match (x.merged, &x.into) {
                            (Some(true), Some(b)) => format!("merged into {b}"),
                            (Some(false), Some(b)) => format!("not merged into {b}"),
                            _ => "merged: unknown".into(),
                        };
                        let size = match (x.size, x.target) {
                            (Some(s), Some(t)) if t > 0 => format!("{} ({} target/)", gb(s), gb(t)),
                            (Some(s), _) => gb(s),
                            _ if w.measuring.is_some() => "measuring…".into(),
                            _ => "unknown (du could not read it)".into(),
                        };
                        let name = x.branch.clone().unwrap_or_else(|| {
                            format!("detached {}", &x.head.get(..7).unwrap_or(""))
                        });
                        let row = WsRow {
                            key: format!("ws:{}", x.path),
                            name: name.clone(),
                            owner: owner.clone(),
                            merged: merged.clone(),
                            size: size.clone(),
                            live,
                            detail: Detail {
                                title: format!("Workspace · {name}"),
                                tone: if live { Tone::Ok } else { Tone::Idle },
                                reason: if live {
                                    "Its owner is working in it now.".into()
                                } else {
                                    "Nobody is working in it now.".into()
                                },
                                facts: vec![
                                    fact("path", x.path.clone(), "git worktree list"),
                                    fact("source", x.source.clone(), "~/.unvrs/sources.toml"),
                                    fact("branch", name, "git worktree list"),
                                    fact(
                                        "owner",
                                        owner,
                                        "task dir in the path + kernel/state.json pids[].state",
                                    ),
                                    fact("merged", merged, "git merge-base --is-ancestor"),
                                    fact("size", size, "du -k -d 1 (target/ is rebuildable)"),
                                    fact(
                                        "measured",
                                        w.sized_ms
                                            .map(|t| ago(now, t))
                                            .unwrap_or_else(|| "not yet".into()),
                                        "du",
                                    ),
                                ],
                                text: vec![],
                            },
                        };
                        (x.merged == Some(false), row)
                    })
                    .collect();
            // live first, then unmerged, then the rest; biggest first within each
            rows.sort_by_key(|(unmerged, r)| (!r.live, !*unmerged));
            // known sizes add up even while some are still being measured (a new worktree
            // does not turn the whole total into "unknown")
            let sized: Vec<u64> = w.list.iter().filter_map(|x| x.size).collect();
            let not_sized = w.list.len() - sized.len();
            let total: Option<u64> = (!sized.is_empty()).then(|| sized.iter().sum());
            let target: Option<u64> = w.list.iter().filter_map(|x| x.target).reduce(|a, b| a + b);
            let progress = w.measuring.map(|(d, of)| format!("measuring {d}/{of}"));
            let n = rows.len();
            let live_n = rows.iter().filter(|(_, r)| r.live).count();
            let merged_n = w.list.iter().filter(|x| x.merged == Some(true)).count();
            let size = match (total, not_sized, &progress) {
                (Some(t), 0, _) => gb(t),
                (Some(t), _, Some(p)) => format!("≥ {} · {p}", gb(t)),
                (Some(t), u, None) => format!("≥ {} · {u} not sized", gb(t)),
                (None, _, Some(p)) => format!("size {p}"),
                (None, _, None) => "size unknown".into(),
            };
            let value = format!("{n} worktrees · {live_n} live · {size}");
            let mut text: Vec<String> = rows
                .iter()
                .map(|(_, r)| format!("{} · {} · {} · {}", r.name, r.owner, r.merged, r.size))
                .collect();
            if text.is_empty() {
                text.push("No worktrees besides the main checkouts.".into());
            }
            let shown: Vec<WsRow> = rows
                .iter()
                .filter(|(_, r)| r.live)
                .take(ZONE_MAX)
                .map(|(_, r)| r.clone())
                .collect();
            let more = n - shown.len();
            (
                Gauge {
                    key: "ws".into(),
                    label: "Workspaces".into(),
                    fill: None,
                    value,
                    tone: Tone::Idle,
                    source: "git worktree list · du".into(),
                    age: match (w.sized_ms, &progress) {
                        (Some(t), Some(p)) if not_sized == 0 => {
                            format!("sized {} · {p}", ago(now, t))
                        }
                        (Some(t), _) => format!("sized {}", ago(now, t)),
                        (None, _) => format!("listed {}", ago(now, w.at_ms)),
                    },
                    detail: Detail {
                        title: "Workspaces".into(),
                        tone: Tone::Idle,
                        reason: format!(
                            "{n} worktrees of the git sources; {live_n} with a working owner; {merged_n} already merged."
                        ),
                        facts: vec![
                            fact(
                                "worktrees",
                                n.to_string(),
                                "git worktree list (git sources in sources.toml)",
                            ),
                            fact(
                                "with a working owner",
                                live_n.to_string(),
                                "kernel/state.json pids[].state",
                            ),
                            fact(
                                "merged",
                                merged_n.to_string(),
                                "git merge-base --is-ancestor",
                            ),
                            fact(
                                "total size",
                                match (total, not_sized) {
                                    (Some(t), 0) => gb(t),
                                    (Some(t), u) => format!("≥ {} ({u} not sized yet)", gb(t)),
                                    (None, _) => "not measured yet".into(),
                                },
                                "du -k -d 1 (includes main checkouts)",
                            ),
                            fact(
                                "rebuildable (target/)",
                                target.map(gb).unwrap_or_else(|| "not measured yet".into()),
                                "du -k -d 1 <worktree> (its target/ line)",
                            ),
                            fact(
                                "sized",
                                w.sized_ms
                                    .map(|t| ago(now, t))
                                    .unwrap_or_else(|| "not yet".into()),
                                "du every 15m, 4 at a time; saved in kernel/ws-sizes.json",
                            ),
                            fact(
                                "measuring",
                                progress.clone().unwrap_or_else(|| "no".into()),
                                "the running du pass (worktrees done / in the pass)",
                            ),
                        ],
                        text,
                    },
                },
                shown,
                more,
            )
        }
    };
    Fuel {
        quota,
        disk,
        ws,
        ws_rows,
        ws_more,
    }
}

/// Only a kernel hold.close event settles an open snapshot entry. Seat notes and task
/// intents can quote answers, but do not carry captain authority. A later hold.reopen
/// (the captain undid a moot close) makes the call open again; a moot close shows its
/// evidence, which retires the question and approves nothing (D46/D48).
pub fn settled_evidence(host: &Host, id: &str, now: u64) -> Option<String> {
    let e =
        host.journal.as_ref()?.events.iter().rev().find(|e| {
            (e["kind"] == "hold.close" || e["kind"] == "hold.reopen") && e["hold"] == id
        })?;
    if e["kind"] == "hold.reopen" {
        return None;
    }
    let at = ago(now, e["at"].as_u64().unwrap_or(0));
    if e["status"] == "moot" {
        let by = e["by"]
            .as_u64()
            .map_or_else(|| "the captain".to_owned(), |p| format!("PID {p}"));
        return Some(format!(
            "Closed as moot {at} by {by}: {} (journal hold.close; approves nothing).",
            st(e, "evidence")
        ));
    }
    Some(format!(
        "Closed {at} (journal hold.close, status {}).",
        st(e, "status")
    ))
}

/// Every open call and fault, most important first, and the settled calls apart.
fn needs(
    snap: &Value,
    host: &Host,
    now: u64,
    kernel: &Light,
    drivers: &[Light],
    fuel: &Fuel,
) -> (Vec<Need>, Vec<Need>) {
    let mut out: Vec<(u8, i64, Need)> = vec![];
    let mut settled: Vec<Need> = vec![];
    // faults that stop work are blocking; they come first
    if kernel.tone == Tone::Bad {
        out.push((
            0,
            0,
            Need {
                key: "need:kernel".into(),
                id: "kernel".into(),
                kind: NeedKind::Fault,
                blocking: true,
                tone: Tone::Bad,
                title: format!("Kernel {}", kernel.summary),
                age: kernel.age.clone(),
                app: "Terminal".into(),
                seat: "your shell".into(),
                route: "With the kernel down no seat can take a command; start it from a terminal."
                    .into(),
                copy: "unvrs kernel start".into(),
                shown: "unvrs kernel start".into(),
                open: None,
                settled: None,
                detail: kernel.detail.clone(),
            },
        ));
    }
    if fuel.disk.tone == Tone::Bad {
        let (app, seat, why, open) = route(snap, host, None);
        let text = format!(
            "Disk is down to {}. What can we clear safely?",
            fuel.disk.value.trim_end_matches(" free")
        );
        out.push((
            0,
            1,
            Need {
                key: "need:disk".into(),
                id: "disk".into(),
                kind: NeedKind::Fault,
                blocking: true,
                tone: Tone::Bad,
                title: format!("Disk almost full · {}", fuel.disk.value),
                age: fuel.disk.age.clone(),
                app,
                seat,
                route: format!("A host fault, not a call: tell a live seat. {why}"),
                copy: text.clone(),
                shown: text,
                open,
                settled: None,
                detail: fuel.disk.detail.clone(),
            },
        ));
    }
    for d in drivers.iter().filter(|d| d.tone == Tone::Bad) {
        let (app, seat, why, open) = route(snap, host, None);
        // no ages in a command: it must not change while the captain copies it
        let text = format!(
            "The {} driver is down: {} Please look into it.",
            d.name, d.detail.reason
        );
        out.push((
            1,
            0,
            Need {
                key: format!("need:{}", d.key),
                id: d.name.to_lowercase(),
                kind: NeedKind::Fault,
                blocking: false,
                tone: Tone::Bad,
                title: format!("{} driver down · {}", d.name, d.summary),
                age: d.age.clone(),
                app,
                seat,
                route: format!("A driver fault, not a call: tell a live seat. {why}"),
                copy: text.clone(),
                shown: text,
                open,
                settled: None,
                detail: d.detail.clone(),
            },
        ));
    }
    let state = host.state.as_ref();
    for c in arr(snap, "calls") {
        let id = st(&c, "id");
        let proposal = st(&c, "kind") == "proposal";
        let age_s = c["age_s"].as_u64();
        let by = state.and_then(|k| k.raised_by.get(&id)).map(|b| *b as u64);
        let until = state.and_then(|k| k.until.get(&id)).copied();
        let (app, seat, why, open) = route(snap, host, by);
        let (copy, shown) = call_copy(&c, &app);
        let options: Vec<String> = arr(&c, "options")
            .iter()
            .enumerate()
            .map(|(i, o)| {
                format!(
                    "{}. {}",
                    i + 1,
                    o.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| o.to_string())
                )
            })
            .collect();
        let mut text = vec![st(&c, "question")];
        text.extend(options);
        let evidence = settled_evidence(host, &id, now);
        // importance: a deadline (soonest first); then decisions whose raising seat is
        // still there (someone waits on the answer), then decisions whose raiser is gone,
        // then proposals; inside a group the newest first (what the crew works on now)
        let raiser_gone = by
            .and_then(|b| state.and_then(|k| k.pids.get(&(b as usize))))
            .is_some_and(|p| p.state == "ended");
        let newest = age_s.map(|a| a as i64).unwrap_or(i64::MAX);
        let (group, order) = match until {
            Some(u) => (2, u as i64),
            None if proposal => (5, newest),
            None if raiser_gone => (4, newest),
            None => (3, newest),
        };
        let project = st(&c, "project");
        let mut reason = format!(
            "Waiting for you for {}.",
            age_s.map(dur).unwrap_or_else(|| "an unknown time".into())
        );
        if let Some(e) = &evidence {
            reason = format!("Settled, still open in the kernel. {e}");
        }
        let need = Need {
            key: format!("need:{id}"),
            id: id.clone(),
            kind: if proposal {
                NeedKind::Proposal
            } else {
                NeedKind::Decision
            },
            blocking: false,
            tone: Tone::Warn,
            title: clip(&st(&c, "question"), 120),
            age: age_s.map(dur).unwrap_or_else(|| "age unknown".into()),
            app: app.clone(),
            seat: seat.clone(),
            route: why.clone(),
            copy: copy.clone(),
            shown: shown.clone(),
            open,
            settled: evidence.clone(),
            detail: Detail {
                title: format!("{} {id}", if proposal { "Proposal" } else { "Decision" }),
                tone: if evidence.is_some() {
                    Tone::Idle
                } else {
                    Tone::Warn
                },
                reason,
                facts: vec![
                    fact("id", id, "/api/snapshot calls[].id"),
                    fact(
                        "project",
                        if project.is_empty() {
                            "none".into()
                        } else {
                            project
                        },
                        "/api/snapshot calls[].project",
                    ),
                    fact(
                        "waiting",
                        age_s.map(dur).unwrap_or_else(|| "unknown".into()),
                        "/api/snapshot calls[].age_s",
                    ),
                    fact(
                        "raised by",
                        by.map(|b| format!("PID {b}"))
                            .unwrap_or_else(|| "unknown".into()),
                        "kernel/state.json holds[].by",
                    ),
                    fact(
                        "deadline",
                        until
                            .map(|u| ago(now, u))
                            .map(|a| a.replace(" ago", ""))
                            .unwrap_or_else(|| "none".into()),
                        "kernel/state.json holds[].until",
                    ),
                    fact(
                        "blocking",
                        "unknown (the kernel has no blocking flag for calls)",
                        "—",
                    ),
                    fact(
                        "settled",
                        evidence
                            .clone()
                            .unwrap_or_else(|| "no evidence: still waiting".into()),
                        "kernel/journal.jsonl hold.close",
                    ),
                    fact("answer in", format!("{app} · {seat}"), "route rule"),
                    fact(
                        "why there",
                        why,
                        "/api/snapshot seats[] + kernel/state.json",
                    ),
                    fact("command", shown, "the command the captain types there"),
                    fact(
                        "open",
                        "no deep link proven: copy the command instead",
                        "PROVEN_DEEP_LINKS",
                    ),
                ],
                text,
            },
        };
        if evidence.is_some() {
            settled.push(need);
        } else {
            out.push((group, order, need));
        }
    }
    out.sort_by_key(|(g, o, _)| (*g, *o));
    (out.into_iter().map(|(_, _, n)| n).collect(), settled)
}

/// Kernel activity for the sparkline: journal events per 5-minute bucket over the last
/// hour, oldest first. Buckets older than the journal window (the probe reads the last
/// 600 events) are None, never 0.
pub fn activity(host: &Host, now: u64) -> Vec<Option<u32>> {
    const BUCKET_MS: u64 = 5 * 60_000;
    const N: usize = 12;
    let Some(j) = host.journal.as_ref() else {
        return vec![None; N];
    };
    let start = now.saturating_sub(BUCKET_MS * N as u64);
    let oldest = j
        .events
        .iter()
        .filter_map(|e| e["at"].as_u64())
        .min()
        .unwrap_or(0);
    // a full window (600 events) may not reach back an hour
    let reach = if j.events.len() >= 600 { oldest } else { 0 };
    let mut counts = vec![0u32; N];
    for at in j.events.iter().filter_map(|e| e["at"].as_u64()) {
        if at >= start && at <= now {
            let i = ((at - start) / BUCKET_MS) as usize;
            counts[i.min(N - 1)] += 1;
        }
    }
    counts
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let bucket_start = start + i as u64 * BUCKET_MS;
            (bucket_start >= reach).then_some(c)
        })
        .collect()
}

fn status(
    kernel: &Light,
    needs: &[Need],
    drivers: &[Light],
    surfaces: &[Light],
    crew: &Crew,
    fuel: &Fuel,
) -> (Tone, String) {
    let mut parts: Vec<String> = vec![];
    let mut tone = Tone::Ok;
    let mut bump = |t: Tone| {
        if t > tone {
            tone = t;
        }
    };
    if kernel.tone.is_fault() {
        bump(kernel.tone);
        parts.push(format!("Kernel {}", kernel.summary));
    }
    if fuel.disk.tone.is_fault() {
        bump(fuel.disk.tone);
        parts.push(format!("Disk {}", fuel.disk.value));
    }
    let calls = needs.iter().filter(|n| n.kind != NeedKind::Fault).count();
    if calls > 0 {
        bump(Tone::Warn);
        parts.push(format!(
            "{calls} call{} wait{} for you",
            if calls == 1 { "" } else { "s" },
            if calls == 1 { "s" } else { "" }
        ));
    }
    for d in drivers
        .iter()
        .chain(surfaces.iter())
        .filter(|d| d.tone.is_fault())
    {
        bump(d.tone);
        parts.push(format!("{} {}", d.name, d.tone.word()));
    }
    let stalled: Vec<u64> = crew
        .active
        .iter()
        .flat_map(|s| s.workers.iter())
        .chain(crew.loose.iter())
        .filter(|w| w.tone == Tone::Warn)
        .map(|w| w.pid)
        .collect();
    if !stalled.is_empty() {
        bump(Tone::Warn);
        parts.push(if stalled.len() == 1 {
            format!("PID {} stalled", stalled[0])
        } else {
            format!("{} workers stalled", stalled.len())
        });
    }
    for q in fuel.quota.iter().filter(|q| q.tone.is_fault()) {
        bump(q.tone);
        parts.push(format!("{} {} used", q.label, q.value));
    }
    if parts.is_empty() {
        (Tone::Ok, "All quiet.".into())
    } else {
        (tone, parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{ClaudeUsage, Disk, Journal, Procs, ThreadInfo};
    use serde_json::json;

    const NOW: u64 = 1_790_000_000_000;

    fn snap() -> Value {
        json!({
            "at": NOW - 1000,
            "kernel": {"version": "0.8.0-bridge.4", "os_pid": 763, "uptime_s": 17_000, "home": "/h"},
            "calls": [
                {"id": "d1", "kind": "decision", "question": "Old question?", "options": ["a", "b"], "age_s": 9000, "project": "unvrs-rs"},
                {"id": "d13", "kind": "decision", "question": "Reasoning level?", "options": ["x"], "age_s": 100, "project": "unvrs-rs"},
                {"id": "d14", "kind": "proposal", "question": "Create project general?", "options": ["approve", "no"], "age_s": 90, "project": null},
                {"id": "d15", "kind": "decision", "question": "Newest?", "options": [], "age_s": 10, "project": null}
            ],
            "seats": [
                {"rank": 1, "pid": 1, "project": null, "state": "live", "wakes": 0,
                 "occupied_by": {"app": "t3", "harness": "claude", "thread_id": "abc", "title": "L1", "link": null}},
                {"rank": 2, "pid": 2, "project": "unvrs-rs", "state": "free", "wakes": 0, "occupied_by": null},
                {"rank": 2, "pid": 3, "project": "acme", "state": "free", "wakes": 0, "occupied_by": null}
            ],
            "workers": [
                {"pid": 25, "project": "unvrs-rs", "intent": "Build the preview", "shape": "report", "harness": "claude", "model": null, "elapsed_s": 700, "state": "working"}
            ],
            "quota": [
                {"account": "default", "harness": "claude", "remaining_pct": null, "resets_at": null, "source": "claude rate_limit_event"},
                {"account": "me", "harness": "codex", "remaining_pct": 74.0, "resets_at": NOW + 3_600_000, "source": "codex app-server"}
            ]
        })
    }

    fn host(output_age_s: u64, busy: bool) -> Host {
        let mut ks = KernelState::default();
        ks.raised_by.insert("d1".into(), 2);
        ks.raised_by.insert("d14".into(), 1);
        ks.pids.insert(
            25,
            PidInfo {
                pid: 25,
                parent: 2,
                rank: 3,
                state: "working".into(),
                busy,
                harness: "claude".into(),
                ..Default::default()
            },
        );
        ks.threads.push(ThreadInfo {
            pid: 1,
            app: "t3".into(),
            harness: "claude".into(),
            bound: true,
            last_seen: NOW - 4000,
            turn_open: true,
        });
        let mut outputs = crate::probe::Outputs::new();
        outputs.insert(25, NOW - output_age_s * 1000);
        Host {
            at_ms: NOW,
            disk: Some(Disk {
                path: "/h".into(),
                free: 26_000_000_000,
                total: 460_000_000_000,
                at_ms: NOW - 2000,
            }),
            state: Some(ks),
            journal: Some(Journal {
                path: "j".into(),
                at_ms: NOW,
                events: vec![
                    json!({"at": NOW - 60_000, "kind": "turn", "pid": 25, "error": null}),
                    json!({"at": NOW - 50_000, "kind": "fold", "pid": 1, "result": "rejected", "error": "Brief worker failed; previous brief kept"}),
                    json!({"at": NOW - 40_000, "kind": "captain", "op": "l1", "ok": true}),
                ],
            }),
            procs: Some(Procs {
                at_ms: NOW,
                t3: true,
                codex: true,
                claude_desktop: true,
            }),
            outputs,
            workspaces: None,
            claude_usage: None,
            usage_error: Some("no Claude Code login on this machine".into()),
            usage_reading: false,
            ownership: None,
        }
    }

    fn build(snap: &Value, host: &Host) -> Model {
        Model::build(Inputs {
            snap,
            host,
            now_ms: NOW,
            sim: false,
        })
    }

    #[test]
    fn needs_come_newest_decision_first_then_proposals_with_a_route_and_a_command() {
        let m = build(&snap(), &host(5, true));
        let ids: Vec<&str> = m.needs.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["d15", "d13", "d1", "d14"]);
        for n in &m.needs {
            assert_eq!(n.app, "T3 Code", "{}", n.id);
            assert_eq!(n.seat, "L1", "{}", n.id);
            assert!(n.open.is_none(), "no Open without a proven deep link");
            assert!(n.copy.starts_with("$unvrs:"));
        }
        let d1 = &m.needs[2];
        assert_eq!(d1.copy, "$unvrs:answer d1 ");
        assert_eq!(d1.shown, "$unvrs:answer d1 <your answer>");
        assert!(d1.route.contains("L2 unvrs-rs"), "{}", d1.route);
        assert_eq!(m.needs[3].copy, "$unvrs:approve d14");
        assert!(
            m.needs[3].route.starts_with("Raised by L1"),
            "{}",
            m.needs[3].route
        );
    }

    #[test]
    fn with_no_live_thread_the_route_says_which_app_to_open() {
        let mut s = snap();
        s["seats"][0]["state"] = json!("away");
        let mut h = host(5, true);
        let (app, seat, why, open) = route(&s, &h, Some(2));
        assert_eq!(app, "T3 Code, Codex Desktop or Claude Desktop");
        assert_eq!(seat, "any thread");
        assert!(
            why.contains("open T3 Code, Codex Desktop or Claude Desktop"),
            "{why}"
        );
        assert!(open.is_none());
        // only the apps that run are named
        let p = h.procs.as_mut().unwrap();
        (p.t3, p.codex) = (false, false);
        let (app, _, why, _) = route(&s, &h, Some(2));
        assert_eq!(app, "Claude Desktop");
        assert!(why.contains("open Claude Desktop and type"), "{why}");
        // Claude's slash form there; `$` elsewhere
        let d1 = json!({"id": "d1", "kind": "decision"});
        assert_eq!(call_copy(&d1, &app).0, "/unvrs:answer d1 ");
        // none running, or ps unread: all three
        h.procs.as_mut().unwrap().claude_desktop = false;
        assert_eq!(
            route(&s, &h, Some(2)).0,
            "T3 Code, Codex Desktop or Claude Desktop"
        );
        h.procs = None;
        assert_eq!(
            route(&s, &h, Some(2)).0,
            "T3 Code, Codex Desktop or Claude Desktop"
        );
    }

    #[test]
    fn calls_route_to_an_l1_live_in_claude_desktop_with_slash_commands() {
        let mut s = snap();
        s["seats"][0]["occupied_by"]["app"] = json!("claude-desktop");
        let m = build(&s, &host(5, true));
        assert_eq!(m.needs[0].app, "Claude Desktop");
        for n in &m.needs {
            assert_eq!(
                (n.app.as_str(), n.seat.as_str()),
                ("Claude Desktop", "L1"),
                "{}",
                n.id
            );
            assert!(n.copy.starts_with("/unvrs:"), "{}", n.copy);
        }
        let d1 = m.needs.iter().find(|n| n.id == "d1").unwrap();
        assert_eq!(d1.copy, "/unvrs:answer d1 ");
        assert_eq!(d1.shown, "/unvrs:answer d1 <your answer>");
        assert!(d1.route.contains("live in Claude Desktop"), "{}", d1.route);
        let d14 = m.needs.iter().find(|n| n.id == "d14").unwrap();
        assert_eq!(d14.copy, "/unvrs:approve d14");
    }

    #[test]
    fn a_raising_seat_with_a_live_thread_gets_the_call() {
        let mut s = snap();
        s["seats"][1]["state"] = json!("live");
        s["seats"][1]["occupied_by"] =
            json!({"app": "codex-app", "harness": "codex", "link": "codex://threads/x"});
        let (app, seat, _, open) = route(&s, &host(5, true), Some(2));
        assert_eq!(
            (app.as_str(), seat.as_str()),
            ("Codex Desktop", "L2 unvrs-rs")
        );
        assert!(open.is_none(), "codex:// links are not proven yet");
    }

    #[test]
    fn status_line_names_only_what_is_off() {
        let m = build(&snap(), &host(5, true));
        assert_eq!(m.status, "4 calls wait for you · Hdff degraded");
        assert_eq!(m.status_tone, Tone::Warn);
        let mut calm = snap();
        calm["calls"] = json!([]);
        let mut h = host(5, true);
        h.journal.as_mut().unwrap().events.remove(1);
        let m = build(&calm, &h);
        assert_eq!(m.status, "All quiet.");
        assert_eq!(m.status_tone, Tone::Ok);
    }

    #[test]
    fn drivers_light_from_their_journal_events_or_say_not_built() {
        let m = build(&snap(), &host(5, true));
        let t: Vec<(&str, Tone)> = m
            .drivers
            .iter()
            .map(|d| (d.name.as_str(), d.tone))
            .collect();
        assert_eq!(
            t,
            [
                ("Agent", Tone::Ok),
                ("Hdff", Tone::Warn),
                ("Obs", Tone::Ok),
                ("Intf", Tone::Ok),
                ("Econ", Tone::Unknown),
                ("Boot", Tone::Off)
            ]
        );
        let h = &m.drivers[1];
        assert!(
            h.detail.text[0].contains("Brief worker failed"),
            "the last error opens"
        );
        // three failures in a row: down, and it becomes a Needs-you row
        let mut hs = host(5, true);
        for i in 0..3 {
            hs.journal.as_mut().unwrap().events.push(
                json!({"at": NOW - 30_000 + i, "kind": "turn", "pid": 25, "error": "claude: rate limit"}),
            );
        }
        let m = build(&snap(), &hs);
        assert_eq!(m.drivers[0].tone, Tone::Bad);
        assert!(m.needs.iter().any(|n| n.id == "agent"));
    }

    #[test]
    fn agent_light_recovers_by_pid_or_ten_minutes_without_failures() {
        let mut h = host(5, true);
        let light = |h: &Host, now| driver_lights(h, now).remove(0);
        let failed =
            json!({"at": NOW, "kind":"turn", "pid":161, "error":"Codex protocol read failed"});
        h.journal.as_mut().unwrap().events.push(failed.clone());
        let fault = light(&h, NOW);
        assert_eq!(fault.tone, Tone::Warn);
        assert!(fault.summary.contains("PID 161"));
        assert!(fault.summary.contains("Codex protocol read failed"));
        h.journal.as_mut().unwrap().events.extend([
            json!({"at":NOW+1, "kind":"driver.event", "pid":161, "event":{"kind":"progress"}}),
            json!({"at":NOW+2, "kind":"turn", "pid":162, "error":null}),
        ]);
        assert_eq!(light(&h, NOW + 2).tone, Tone::Warn);
        assert!(light(&h, NOW + 2).summary.contains("PID 161"));
        assert_eq!(light(&h, NOW + 599_999).tone, Tone::Warn);
        let expired = light(&h, NOW + 600_000);
        assert_eq!(expired.tone, Tone::Ok);
        // An expired error is historical evidence, never described as a successful turn.
        let mut expired_only = host(5, true);
        expired_only.journal.as_mut().unwrap().events.push(failed);
        let expired = light(&expired_only, NOW + 600_000);
        assert_eq!(expired.tone, Tone::Ok);
        assert!(!expired.detail.reason.contains("succeeded"));
        h.journal.as_mut().unwrap().events.extend([
            json!({"at":NOW+3, "kind":"turn", "pid":162, "error":"second PID failure"}),
            json!({"at":NOW+4, "kind":"turn", "pid":161, "error":null}),
        ]);
        assert_eq!(light(&h, NOW + 4).tone, Tone::Warn);
        assert!(light(&h, NOW + 4).summary.contains("PID 162"));
        h.journal
            .as_mut()
            .unwrap()
            .events
            .push(json!({"at":NOW+5, "kind":"turn", "pid":162, "error":null}));
        assert_eq!(light(&h, NOW + 5).tone, Tone::Ok);
        // Only consecutive failed turns make the driver down; a success breaks that streak.
        for pid in [161, 162, 163] {
            h.journal
                .as_mut()
                .unwrap()
                .events
                .push(json!({"at":NOW+pid, "kind":"turn", "pid":pid, "error":"failed"}));
            assert_eq!(
                light(&h, NOW + 163).tone,
                if pid == 163 { Tone::Bad } else { Tone::Warn }
            );
        }
        h.journal
            .as_mut()
            .unwrap()
            .events
            .push(json!({"at":NOW+164, "kind":"turn", "pid":163, "error":null}));
        assert_eq!(light(&h, NOW + 164).tone, Tone::Warn);
        assert!(light(&h, NOW + 164).summary.contains("PID 162"));
        for pid in [161, 162] {
            h.journal
                .as_mut()
                .unwrap()
                .events
                .push(json!({"at":NOW+165, "kind":"turn", "pid":pid, "error":null}));
        }
        assert_eq!(light(&h, NOW + 165).tone, Tone::Ok);
        h.journal
            .as_mut()
            .unwrap()
            .events
            .push(json!({"at":NOW+600_001, "kind":"turn", "pid":161, "error":"new failure"}));
        assert_eq!(light(&h, NOW + 600_002).tone, Tone::Warn);
        let mut stale = host(5, true);
        stale.journal.as_mut().unwrap().events = (0..3)
            .map(|i| json!({"at":NOW-600_003+i, "kind":"turn", "pid":161, "error":"old failure"}))
            .collect();
        assert_eq!(light(&stale, NOW).tone, Tone::Ok);
        stale
            .journal
            .as_mut()
            .unwrap()
            .events
            .push(json!({"at":NOW, "kind":"turn", "pid":161, "error":"fresh failure"}));
        assert_eq!(light(&stale, NOW).tone, Tone::Warn);
    }

    #[test]
    fn planned_drivers_read_planned_and_never_count_toward_health() {
        let mut calm = snap();
        calm["calls"] = json!([]);
        let mut h = host(5, true);
        h.journal.as_mut().unwrap().events.remove(1);
        let m = build(&calm, &h);
        let planned = |k: &str| m.drivers.iter().find(|d| d.key == k).unwrap();
        assert_eq!(planned("drv:econ").summary, "no events yet");
        assert_eq!(
            planned("drv:boot").summary,
            "planned · not in 0.8 · seats boot on first use"
        );
        let d = planned("drv:boot");
        assert_eq!(d.tone, Tone::Off);
        assert_eq!(d.tone.word(), "planned");
        assert!(!d.tone.is_fault());
        assert!(!d.summary.contains("not built"), "{}", d.summary);
        assert!(d.detail.reason.contains("never counts toward health"));
        assert_eq!((m.status_tone, m.status.as_str()), (Tone::Ok, "All quiet."));
        assert!(!m.needs.iter().any(|n| n.id == "econ" || n.id == "boot"));
    }

    #[test]
    fn surfaces_show_live_seats_open_apps_and_what_is_not_built() {
        let m = build(&snap(), &host(5, true));
        let s: Vec<(&str, Tone, &str)> = m
            .surfaces
            .iter()
            .map(|l| (l.name.as_str(), l.tone, l.summary.as_str()))
            .collect();
        assert_eq!(
            s,
            [
                ("T3 Code", Tone::Ok, "L1 live here"),
                ("Codex Desktop", Tone::Idle, "open · no live seat"),
                ("Claude Desktop", Tone::Idle, "open · no live seat")
            ]
        );
    }

    #[test]
    fn a_seat_in_claude_desktop_lights_its_surface() {
        let mut sn = snap();
        let seats = sn["seats"].as_array_mut().unwrap();
        let l2 = seats
            .iter_mut()
            .find(|s| s["rank"] == 2)
            .expect("fixture has an L2 seat");
        l2["state"] = "live".into();
        l2["occupied_by"] = serde_json::json!({"app": "claude-desktop", "harness": "claude"});
        let m = build(&sn, &host(5, true));
        let claude = m
            .surfaces
            .iter()
            .find(|l| l.name == "Claude Desktop")
            .unwrap();
        assert_eq!(claude.tone, Tone::Ok, "{}", claude.summary);
        assert!(claude.summary.ends_with("live here"), "{}", claude.summary);
    }

    #[test]
    fn codex_worker_shows_accepted_configuration_and_lifecycle_activity() {
        let w = worker(
            &serde_json::json!({"pid": 42, "intent": "fixture", "harness": "codex", "state": "working", "model": "gpt-6.1-sol", "effort": "high", "actual_model": "gpt-6.1-sol", "actual_effort": "high", "effort_evidence": "thread/start accepted configuration", "session": "receipt-session", "tools": ["shell"], "busy": true, "last_activity": NOW - 5_000}),
            None,
            &Host::default(),
            NOW,
        );
        assert!(w.pulse);
        assert_eq!(w.state, "active");
        assert_eq!(w.model, "gpt-6.1-sol");
        for (name, value) in [
            ("accepted effort", "high"),
            ("session", "receipt-session"),
            ("recent tools", "shell"),
            ("last activity", "5s ago"),
        ] {
            assert!(
                w.detail
                    .facts
                    .iter()
                    .any(|f| f.label == name && f.value == value)
            );
        }
        assert!(
            w.detail
                .facts
                .iter()
                .any(|f| f.label == "last activity" && f.source.contains("last_activity"))
        );
    }

    #[test]
    fn crew_expands_running_work_collapses_idle_leads_and_says_unknown() {
        let m = build(&snap(), &host(5, true));
        let l1 = m.crew.l1.as_ref().unwrap();
        assert!(l1.pulse, "L1 is in a turn");
        assert_eq!(
            (l1.model.as_str(), l1.effort.as_str()),
            ("unknown", "unknown")
        );
        assert_eq!(m.crew.active.len(), 1);
        assert_eq!(m.crew.active[0].label, "L2 unvrs-rs");
        let w = &m.crew.active[0].workers[0];
        assert!(w.pulse && w.tone == Tone::Ok, "fresh output pulses");
        assert_eq!(
            (w.model.as_str(), w.effort.as_str()),
            ("unknown", "unknown")
        );
        assert_eq!(m.crew.idle.len(), 1);
        // no output for 11 minutes during a turn: stalled, amber, no pulse
        let m = build(&snap(), &host(660, true));
        let w = &m.crew.active[0].workers[0];
        assert_eq!(w.tone, Tone::Warn);
        assert!(!w.pulse && w.state.starts_with("stalled"));
        assert!(m.status.contains("PID 25 stalled"));
        // the same silence between turns is not a stall
        let m = build(&snap(), &host(660, false));
        assert_eq!(m.crew.active[0].workers[0].tone, Tone::Ok);
    }

    #[test]
    fn crew_nests_workers_by_snapshot_parent_and_shows_reported_seat_facts() {
        let mut sn = snap();
        sn["seats"][0]["model"] = json!("claude-opus-5-5");
        sn["seats"][0]["effort"] = json!("high");
        sn["seats"][0]["source"] = json!("transcript");
        sn["seats"][2]["model"] = json!("gpt-6.1-sol");
        sn["seats"][2]["source"] = json!("last seat-run");
        // PID 26 works for the acme lead although its project reads unvrs-rs;
        // state.json does not know it, so only the snapshot's parent can place it
        sn["workers"].as_array_mut().unwrap().push(json!({"pid": 26, "parent": 3, "project": "unvrs-rs", "intent": "Other", "shape": "report", "harness": "codex", "model": null, "elapsed_s": 10, "state": "working"}));
        let mut h = host(5, true);
        // the vacant seat's requested model (state.json) is not what a harness runs
        h.state.as_mut().unwrap().pids.insert(
            2,
            PidInfo {
                pid: 2,
                rank: 2,
                model: Some("asked-for".into()),
                ..Default::default()
            },
        );
        let m = build(&sn, &h);
        let l1 = m.crew.l1.as_ref().unwrap();
        assert_eq!(
            (l1.model.as_str(), l1.effort.as_str()),
            ("claude-opus-5-5", "high")
        );
        assert!(!l1.vacant);
        let fact = |s: &Seat, label: &str| {
            let f = s.detail.facts.iter().find(|f| f.label == label).unwrap();
            (f.value.clone(), f.source.clone())
        };
        let (v, src) = fact(l1, "model");
        assert_eq!(v, "claude-opus-5-5");
        assert!(src.contains("transcript"), "{src}");
        let seat = |pid: u64| {
            m.crew
                .active
                .iter()
                .chain(&m.crew.idle)
                .find(|s| s.pid == pid)
                .unwrap()
        };
        let pids = |s: &Seat| s.workers.iter().map(|w| w.pid).collect::<Vec<_>>();
        assert_eq!(pids(seat(2)), [25]);
        assert_eq!(pids(seat(3)), [26]);
        assert!(m.crew.loose.is_empty());
        let l2 = seat(3);
        assert_eq!(
            (l2.model.as_str(), l2.effort.as_str()),
            ("gpt-6.1-sol", "unknown")
        );
        assert!(!l2.vacant);
        assert_eq!(fact(l2, "effort").0, "not reported");
        assert!(fact(l2, "model").1.contains("last run"));
        // no thread, no run, nothing reported: vacant, no chip, and no requested model shown
        let vacant = seat(2);
        assert!(vacant.vacant);
        assert_eq!(vacant.model, "unknown");
        assert_eq!(
            fact(vacant, "model"),
            ("not reported".into(), "not reported by the kernel".into())
        );
    }

    #[test]
    fn held_workers_show_route_receipts_on_crew_cards() {
        let mut sn = snap();
        sn["workers"][0]["state"] = json!("held");
        sn["workers"][0]["econ"] =
            json!({"role":"judge", "reason":"high judgment; default reasoning"});
        let m = build(&sn, &host(5, true));
        let cards = super::super::diagram::crew_cards(&m.crew);
        let card = cards
            .leads
            .iter()
            .chain(cards.own.iter())
            .flat_map(|c| &c.kids)
            .find(|c| c.pid == 25)
            .unwrap();
        assert_eq!(card.state, "held");
        assert_eq!(card.tone, Tone::Warn);
        assert_eq!(
            card.routed.as_deref(),
            Some("routed: judge · high judgment; default reasoning")
        );
        sn["workers"][0]["state"] = json!("working");
        sn["workers"][0]["econ"]["reason"] =
            json!("quota unknown, admitted; high judgment; default reasoning");
        let admitted = worker(&sn["workers"][0], None, &host(5, true), NOW);
        assert!(
            admitted
                .routed
                .unwrap()
                .starts_with("routed: judge · quota unknown, admitted")
        );
        // Older snapshots do not fabricate routing evidence.
        let old = worker(&json!({"pid":99}), None, &host(5, true), NOW);
        assert!(old.routed.is_none());
    }

    #[test]
    fn crew_carries_brief_lines_resting_workers_finished_counts_and_l1_workers() {
        let mut sn = snap();
        sn["seats"][2]["now"] = json!("  routing the launch  ");
        sn["seats"][1]["next"] = json!("review PID 25");
        sn["workers"][0]["now"] = json!("writing layout tests");
        sn["workers"].as_array_mut().unwrap().extend([
            json!({"pid": 27, "parent": 2, "project": "unvrs-rs", "intent": "Resting intent", "harness": "codex", "elapsed_s": 5, "state": "idle"}),
            json!({"pid": 28, "parent": 1, "project": "unvrs-rs", "intent": "Direct from L1", "harness": "claude", "elapsed_s": 5, "state": "working"}),
        ]);
        let mut h = host(5, true);
        let pids = &mut h.state.as_mut().unwrap().pids;
        for (pid, st) in [(90, "ended"), (91, "handed-off"), (92, "working")] {
            pids.insert(
                pid,
                PidInfo {
                    pid,
                    parent: 2,
                    rank: 3,
                    state: st.into(),
                    ..Default::default()
                },
            );
        }
        let m = build(&sn, &h);
        let seat = |pid: u64| {
            m.crew
                .active
                .iter()
                .chain(&m.crew.idle)
                .find(|s| s.pid == pid)
                .unwrap()
        };
        let l2 = seat(2);
        assert_eq!(
            l2.doing.as_deref(),
            Some("review PID 25"),
            "next when there is no now"
        );
        assert_eq!(seat(3).doing.as_deref(), Some("routing the launch"));
        assert_eq!((l2.rank, l2.project.as_deref()), (2, Some("unvrs-rs")));
        assert_eq!(l2.workers[0].doing.as_deref(), Some("writing layout tests"));
        assert_eq!(l2.workers[0].project, "unvrs-rs");
        // idle workers: counted as before, and kept for the diagram
        assert_eq!(l2.idle_workers, 1);
        assert_eq!(l2.resting.iter().map(|w| w.pid).collect::<Vec<_>>(), [27]);
        assert_eq!(l2.resting[0].doing, None);
        // ended and handed-off L3s under this lead, never working ones
        assert_eq!(l2.finished, 2);
        // L1's own worker stays under L1
        let l1 = m.crew.l1.as_ref().unwrap();
        assert_eq!((l1.rank, l1.doing.clone()), (1, None));
        assert_eq!(l1.workers.iter().map(|w| w.pid).collect::<Vec<_>>(), [28]);
    }

    #[test]
    fn claude_meters_come_from_the_usage_reading_with_their_age_or_say_why_not() {
        let mut h = host(5, true);
        h.claude_usage = Some(ClaudeUsage {
            at_ms: NOW - 120_000,
            plan: "max".into(),
            five_hour: Some(crate::probe::UsageWindow {
                used_pct: 35.0,
                resets_at: Some(NOW + 3_600_000),
            }),
            seven_day: Some(crate::probe::UsageWindow {
                used_pct: 93.0,
                resets_at: None,
            }),
            source: "usage endpoint".into(),
        });
        h.usage_error = None;
        let m = build(&snap(), &h);
        let q: Vec<(&str, &str, Tone, &str)> = m
            .fuel
            .quota
            .iter()
            .map(|g| (g.key.as_str(), g.value.as_str(), g.tone, g.age.as_str()))
            .collect();
        assert_eq!(
            q,
            [
                ("quota:claude:5h", "35%", Tone::Ok, "2m ago"),
                ("quota:claude:7d", "93%", Tone::Bad, "2m ago"),
                ("quota:codex", "26%", Tone::Ok, "age unknown"),
            ]
        );
        assert_eq!(m.fuel.quota[0].fill, Some(0.35));
        assert!(
            m.fuel.quota[1]
                .detail
                .facts
                .iter()
                .any(|f| f.label == "left" && f.value == "7%")
        );
        assert!(m.status.contains("Claude · 7d 93% used"), "{}", m.status);
        // an old reading is shown with its age but no colour
        h.claude_usage.as_mut().unwrap().at_ms = NOW - 45 * 60_000;
        let m = build(&snap(), &h);
        assert_eq!(m.fuel.quota[1].tone, Tone::Unknown);
        assert!(m.fuel.quota[1].detail.reason.contains("45m ago"));
        // a reading in flight shows through: still the old number, "being read now"
        h.usage_reading = true;
        let m = build(&snap(), &h);
        assert_eq!(m.fuel.quota[1].value, "93%");
        assert!(m.fuel.quota[1].detail.reason.contains("being read now"));
        // cold start, first reading on its way: "reading…", quiet, not "unknown"
        let mut cold = host(5, true);
        cold.usage_error = None;
        cold.usage_reading = true;
        let m = build(&snap(), &cold);
        let g = &m.fuel.quota[0];
        assert_eq!(
            (g.key.as_str(), g.value.as_str(), g.tone, g.age.as_str()),
            ("quota:claude", "reading…", Tone::Idle, "reading now")
        );
        // no reading: one grey meter that says why
        let m = build(&snap(), &host(5, true));
        assert_eq!(m.fuel.quota[0].key, "quota:claude");
        assert!(
            m.fuel.quota[0]
                .detail
                .reason
                .contains("no Claude Code login")
        );
    }

    #[test]
    fn settled_calls_leave_the_list_with_their_evidence() {
        let mut h = host(5, true);
        h.journal.as_mut().unwrap().events.push(json!({
            "at": NOW - 30_000, "kind": "wake", "from": 1, "pid": 2,
            "text": "from PID 1: Captain's words, answering d13: 'Ok, fix the kernel as well'."
        }));
        h.journal.as_mut().unwrap().events.push(
            json!({"at": NOW - 20_000, "kind": "hold.close", "hold": "d1", "status": "answered"}),
        );
        let m = build(&snap(), &h);
        let ids: Vec<&str> = m.needs.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["d15", "d13", "d14"]);
        let settled: Vec<&str> = m.settled.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(settled, ["d1"]);
        assert!(
            settled_evidence(&h, "d13", NOW).is_none(),
            "a seat note cannot answer for the captain"
        );
        assert!(
            m.settled[0]
                .settled
                .as_deref()
                .unwrap()
                .contains("hold.close")
        );
        assert!(
            m.detail("need:d1").is_some(),
            "settled calls still open a drawer"
        );
        // Only the exact hold ID in a kernel close event settles a call.
        assert!(settled_evidence(&h, "d130", NOW).is_none());
        assert_eq!(m.status, "3 calls wait for you · Hdff degraded");
    }

    #[test]
    fn a_moot_close_settles_with_its_evidence_and_a_reopen_opens_it_again() {
        let mut h = host(5, true);
        h.journal.as_mut().unwrap().events.push(json!({
            "at": NOW - 20_000, "kind": "hold.close", "hold": "d14", "status": "moot",
            "evidence": "superseded by d15", "by": 2
        }));
        let m = build(&snap(), &h);
        let moot = m
            .settled
            .iter()
            .find(|n| n.id == "d14")
            .expect("d14 settled");
        let text = moot.settled.as_deref().unwrap();
        assert!(
            text.contains("as moot")
                && text.contains("PID 2")
                && text.contains("superseded by d15")
                && text.contains("approves nothing"),
            "{text}"
        );
        assert!(m.needs.iter().all(|n| n.id != "d14"));
        h.journal.as_mut().unwrap().events.push(
            json!({"at": NOW - 10_000, "kind": "hold.reopen", "hold": "d14", "was": "superseded by d15"}),
        );
        assert!(settled_evidence(&h, "d14", NOW).is_none(), "reopened");
        let m = build(&snap(), &h);
        assert!(m.needs.iter().any(|n| n.id == "d14"));
        assert!(m.settled.iter().all(|n| n.id != "d14"));
    }

    #[test]
    fn a_call_whose_raiser_ended_ranks_below_one_still_waited_on() {
        let mut h = host(5, true);
        h.state.as_mut().unwrap().raised_by.insert("d15".into(), 9);
        h.state.as_mut().unwrap().pids.insert(
            9,
            PidInfo {
                pid: 9,
                state: "ended".into(),
                ..Default::default()
            },
        );
        let m = build(&snap(), &h);
        let ids: Vec<&str> = m.needs.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["d13", "d1", "d15", "d14"]);
    }

    #[test]
    fn deadlines_rank_soonest_first_ahead_of_newer_calls_below_faults() {
        let mut h = host(5, true);
        h.disk.as_mut().unwrap().free = 6_400_000_000;
        let state = h.state.as_mut().unwrap();
        state.until.insert("d1".into(), NOW + 60_000);
        state.until.insert("d14".into(), NOW + 30_000);
        let m = build(&snap(), &h);
        let ids: Vec<&str> = m.needs.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["disk", "d14", "d1", "d15", "d13"]);
    }

    #[test]
    fn activity_counts_journal_events_per_five_minutes_and_says_none_beyond_the_window() {
        let m = build(&snap(), &host(5, true));
        assert_eq!(m.activity.len(), 12);
        assert_eq!(
            m.activity[11],
            Some(3),
            "three events in the last five minutes"
        );
        assert!(m.activity[..11].iter().all(|b| *b == Some(0)));
        let mut h = host(5, true);
        let ev = &mut h.journal.as_mut().unwrap().events;
        ev.clear();
        for i in 0..600u64 {
            ev.push(json!({"at": NOW - 20 * 60_000 + i, "kind": "turn"}));
        }
        let a = activity(&h, NOW);
        assert!(a[..8].iter().all(|b| b.is_none()), "{a:?}");
        assert_eq!(a[8], Some(600));
        assert_eq!(activity(&Host::default(), NOW), vec![None; 12]);
    }

    #[test]
    fn workspaces_show_partial_totals_and_progress_never_a_blank_measuring() {
        use crate::probe::{Workspace, Workspaces};
        let wt = |p: &str, size: Option<u64>, main: bool| Workspace {
            source: "unvrs-rs".into(),
            path: p.into(),
            main,
            size,
            target: size.map(|s| s / 2),
            ..Default::default()
        };
        let mut h = host(5, true);
        let g = 1u64 << 30;
        // cold start, nothing sized yet: progress, not "size measuring…"
        h.workspaces = Some(Workspaces {
            at_ms: NOW,
            sized_ms: None,
            measuring: Some((0, 3)),
            list: vec![
                wt("/m", None, true),
                wt("/a", None, false),
                wt("/b", None, false),
            ],
        });
        let m = build(&snap(), &h);
        assert_eq!(m.fuel.ws.value, "2 worktrees · 0 live · size measuring 0/3");
        // part way: what is known adds up
        let w = h.workspaces.as_mut().unwrap();
        w.measuring = Some((2, 3));
        w.list[0].size = Some(10 * g);
        w.list[1].size = Some(2 * g);
        let m = build(&snap(), &h);
        assert!(
            m.fuel.ws.value.ends_with("· measuring 2/3") && m.fuel.ws.value.contains("≥ "),
            "{}",
            m.fuel.ws.value
        );
        // a saved total with a re-measure running: the total, and the pass in the age
        let w = h.workspaces.as_mut().unwrap();
        w.list[2].size = Some(g);
        w.sized_ms = Some(NOW - 20 * 60_000);
        w.measuring = Some((1, 3));
        let m = build(&snap(), &h);
        assert!(
            !m.fuel.ws.value.contains("measuring"),
            "{}",
            m.fuel.ws.value
        );
        assert_eq!(m.fuel.ws.age, "sized 20m ago · measuring 1/3");
        // a new worktree after the pass: the known total stays, "1 not sized"
        let w = h.workspaces.as_mut().unwrap();
        w.measuring = None;
        w.list.push(wt("/new", None, false));
        let m = build(&snap(), &h);
        assert!(
            m.fuel.ws.value.ends_with("· 1 not sized"),
            "{}",
            m.fuel.ws.value
        );
        assert!(!m.fuel.ws.value.contains("unknown"), "{}", m.fuel.ws.value);
    }

    #[test]
    fn fuel_says_unknown_for_unmeasured_quota_and_names_sources() {
        let m = build(&snap(), &host(5, true));
        assert_eq!(m.fuel.quota[0].value, "unknown");
        assert_eq!(m.fuel.quota[0].tone, Tone::Unknown);
        assert!(m.fuel.quota[0].fill.is_none());
        assert_eq!(m.fuel.quota[1].value, "26%");
        assert!(
            m.fuel.quota[1]
                .detail
                .facts
                .iter()
                .any(|f| f.label == "left" && f.value == "74%")
        );
        assert_eq!(m.fuel.disk.value, "26.0 GB free");
        assert_eq!(m.fuel.disk.tone, Tone::Ok);
        assert_eq!(m.fuel.ws.value, "unknown");
        for g in m.fuel.quota.iter().chain([&m.fuel.disk]) {
            assert!(!g.source.is_empty());
            assert!(!g.age.is_empty());
        }
    }

    #[test]
    fn low_disk_is_red_blocking_and_first() {
        let mut h = host(5, true);
        h.disk.as_mut().unwrap().free = 6_400_000_000;
        let m = build(&snap(), &h);
        assert_eq!(m.fuel.disk.tone, Tone::Bad);
        assert_eq!(m.needs[0].id, "disk");
        assert!(m.needs[0].blocking);
        assert!(m.status.starts_with("Disk 6.4 GB free"), "{}", m.status);
    }

    #[test]
    fn no_snapshot_means_kernel_down_and_nothing_invented() {
        let m = build(&Value::Null, &Host::default());
        assert_eq!(m.kernel.tone, Tone::Bad);
        assert_eq!(m.needs[0].id, "kernel");
        assert!(m.crew.l1.is_none() && m.crew.active.is_empty());
        assert_eq!(m.fuel.disk.value, "unknown");
        assert!(
            m.drivers
                .iter()
                .all(|d| matches!(d.tone, Tone::Unknown | Tone::Off | Tone::Bad))
        );
    }

    #[test]
    fn every_key_opens_a_drawer() {
        let m = build(&snap(), &host(5, true));
        for k in [
            "kernel",
            "drv:hdff",
            "srf:t3",
            "seat:1",
            "seat:2",
            "pid:25",
            "quota:claude",
            "quota:codex",
            "disk",
            "ws",
            "need:d13",
        ] {
            assert!(m.detail(k).is_some(), "{k}");
        }
    }
}
