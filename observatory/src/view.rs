//! The bridge console: one pure view of one kernel snapshot. Nothing here sends anything
//! back to the kernel. Every field may be missing; the view renders what it has.
use dioxus::prelude::*;
use serde_json::Value;

fn s(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(x) => x.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
fn arr(v: &Value, k: &str) -> Vec<Value> {
    v[k].as_array().cloned().unwrap_or_default()
}
fn short(text: &str, n: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= n {
        t.to_owned()
    } else {
        format!("{}…", t.chars().take(n).collect::<String>())
    }
}
/// Seconds as the console says them: 42s, 7m, 3h 12m, 2d 4h.
pub fn dur(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h {:02}m", secs / 3600, secs / 60 % 60),
        _ => format!("{}d {}h", secs / 86_400, secs / 3600 % 24),
    }
}
/// A running clock for elapsed work: 0:42, 12:07, 1:02:33.
fn clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}
fn ago_ms(now: u64, at: &Value) -> String {
    match at.as_u64() {
        Some(at) if now > 0 => dur(now.saturating_sub(at) / 1000),
        _ => "—".into(),
    }
}
fn secs(v: &Value, k: &str) -> Option<u64> {
    v[k].as_u64()
        .or_else(|| v[k].as_f64().map(|f| f.max(0.0) as u64))
}
pub fn bytes(n: u64) -> String {
    match n {
        0..=999 => format!("{n} B"),
        1000..=999_999 => format!("{:.1} KB", n as f64 / 1000.0),
        _ => format!("{:.1} MB", n as f64 / 1_000_000.0),
    }
}
/// The kernel's build: `abc123def456 (branch)` from a deploy, `dev build` otherwise.
fn build_tag(kernel: &Value) -> String {
    match kernel["commit"].as_str() {
        Some(c) => {
            let c: String = c.chars().take(12).collect();
            match kernel["ref"].as_str() {
                Some(r) if !r.is_empty() => format!("{c} ({r})"),
                _ => c,
            }
        }
        None => "dev build".into(),
    }
}
/// One line on the running or last deploy and the class that colors it; `None` when there
/// never was one.
fn deploy_line(now: u64, deploy: &Value) -> Option<(String, &'static str)> {
    let run = &deploy["run"];
    if !run.is_object() {
        return None;
    }
    let kind = s(run, "kind");
    let what = match run["ref"].as_str() {
        Some(r) if kind != "rollback" => format!("{kind} {r}"),
        _ => kind,
    };
    if run["stale"] == true {
        let text = format!("{what} stopped at {} (deployer gone)", s(run, "phase"));
        return Some((text, "kmeta deploy bad"));
    }
    if run["done"] != true {
        let text = format!(
            "{} · {} {}",
            what.to_uppercase(),
            s(run, "phase"),
            ago_ms(now, &run["started"])
        );
        return Some((text, "kmeta deploy busy"));
    }
    let result = s(run, "result");
    let class = match result.as_str() {
        "live" | "staged" => "kmeta deploy",
        "rolled_back" | "aborted" => "kmeta deploy warn",
        _ => "kmeta deploy bad",
    };
    let text = format!(
        "last {what}: {result} {} ago · {}",
        ago_ms(now, &run["ended"]),
        short(&s(run, "reason"), 80)
    );
    Some((text, class))
}
fn harness_class(h: &str) -> &'static str {
    let h = h.to_ascii_lowercase();
    if h.contains("claude") {
        "h-claude"
    } else if h.contains("codex") {
        "h-codex"
    } else if h == "pi" {
        "h-pi"
    } else {
        "h-other"
    }
}
fn state_class(st: &str) -> &'static str {
    match st {
        "live" | "occupied" | "attached" | "bound" => "st-live",
        "working" | "running" | "booting" | "driven" | "waking" => "st-working",
        "blocked" | "offline" | "failed" | "error" => "st-alert",
        "done" | "delivered" => "st-done",
        _ => "st-idle",
    }
}
fn quota_class(p: f64) -> &'static str {
    if p >= 50.0 {
        "q-ok"
    } else if p >= 20.0 {
        "q-mid"
    } else {
        "q-low"
    }
}
fn recent_class(kind: &str) -> &'static str {
    let k = kind.to_ascii_lowercase();
    if k.contains("fail") || k.contains("refus") || k.contains("error") || k.contains("quota") {
        "rc-bad"
    } else if k.contains("decid") || k.contains("answer") || k.contains("approv") {
        "rc-decided"
    } else if k.contains("deliver") || k.contains("done") || k.contains("result") {
        "rc-good"
    } else if k.contains("move") || k.contains("swap") || k.contains("handoff") {
        "rc-move"
    } else {
        "rc-dim"
    }
}
fn is_link(l: &str) -> bool {
    [
        "codex://",
        "t3://",
        "t3code://",
        "claude://",
        "http://",
        "https://",
        "vscode://",
        "cursor://",
    ]
    .iter()
    .any(|p| l.starts_with(p))
}

// ---------------------------------------------------------------- deck model
//
// The bridge shows one slide at a time. The model is plain Rust so the navigation rules
// (keys, swipes, direct jumps, ends) are tested without a browser.

/// One screen of the bridge deck. The order here is the order on the deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slide {
    /// Captain calls and live status: what needs the captain now.
    Bridge,
    /// Seats: who holds L1/L2 and where.
    Crew,
    /// L3 workers running now.
    Missions,
    Projects,
    Quota,
    /// Seat moves, recent events and context reads.
    Log,
}

impl Slide {
    pub const ALL: [Slide; 6] = [
        Slide::Bridge,
        Slide::Crew,
        Slide::Missions,
        Slide::Projects,
        Slide::Quota,
        Slide::Log,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
    pub fn at(i: usize) -> Slide {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
    /// DOM id and URL fragment.
    pub fn id(self) -> &'static str {
        match self {
            Slide::Bridge => "calls",
            Slide::Crew => "crew",
            Slide::Missions => "missions",
            Slide::Projects => "projects",
            Slide::Quota => "quota",
            Slide::Log => "log",
        }
    }
    #[allow(dead_code)] // deep links and tests
    pub fn from_id(id: &str) -> Option<Slide> {
        let id = id.trim_start_matches('#');
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
    pub fn title(self) -> &'static str {
        match self {
            Slide::Bridge => "Captain calls",
            Slide::Crew => "Crew",
            Slide::Missions => "Missions",
            Slide::Projects => "Projects",
            Slide::Quota => "Quota",
            Slide::Log => "Ship log",
        }
    }
    /// The small readout on the slide's nav tab, and whether it wants attention.
    pub fn badge(self, sum: &Summary) -> (String, bool) {
        match self {
            Slide::Bridge => (sum.calls.to_string(), sum.calls > 0),
            Slide::Crew => (format!("{}/{}", sum.occupied, sum.seats), false),
            Slide::Missions => (sum.workers.to_string(), sum.blocked > 0),
            Slide::Projects => (sum.projects.to_string(), false),
            Slide::Quota => match sum.min_quota {
                Some(p) => (format!("{p:.0}%"), p < 20.0),
                None => ("?".into(), false),
            },
            Slide::Log => (sum.events.to_string(), sum.failures > 0),
        }
    }
}

/// Where the deck is. Ends clamp (no wrap), so "next" on the last slide stays put and the
/// captain always knows where the deck starts and ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deck {
    at: usize,
    /// Which way the deck last travelled (-1 back, 1 forward, 0 not yet): the slide
    /// transition enters from that side. Unchanged when a move clamps at an end.
    dir: i8,
}

/// Horizontal travel (CSS px) a touch must cover to count as a swipe.
pub const SWIPE_MIN: f64 = 48.0;

impl Deck {
    #[allow(dead_code)]
    pub fn new(slide: Slide) -> Self {
        Deck {
            at: slide.index(),
            dir: 0,
        }
    }
    /// The side the current slide entered from, for the transition: "back" or "fwd".
    pub fn entered_from(self) -> &'static str {
        if self.dir < 0 { "back" } else { "fwd" }
    }
    pub fn slide(self) -> Slide {
        Slide::at(self.at)
    }
    #[allow(dead_code)]
    pub fn index(self) -> usize {
        self.at
    }
    pub fn len(self) -> usize {
        Slide::ALL.len()
    }
    pub fn is_first(self) -> bool {
        self.at == 0
    }
    pub fn is_last(self) -> bool {
        self.at + 1 == self.len()
    }
    /// "2 / 6" as shown on the bridge.
    pub fn position(self) -> String {
        format!("{} / {}", self.at + 1, self.len())
    }
    pub fn next(self) -> Self {
        self.go(self.at + 1)
    }
    pub fn prev(self) -> Self {
        self.go(self.at.saturating_sub(1))
    }
    pub fn go(self, i: usize) -> Self {
        let at = i.min(self.len() - 1);
        let dir = match at.cmp(&self.at) {
            std::cmp::Ordering::Greater => 1,
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => self.dir,
        };
        Deck { at, dir }
    }
    /// A keyboard key (DOM `KeyboardEvent.key`). `None` when the key is not the deck's,
    /// so the page keeps its default: Tab, Enter on links, and Up/Down/PageUp/PageDown,
    /// which scroll a long slide.
    pub fn key(self, key: &str) -> Option<Self> {
        match key {
            "ArrowRight" => Some(self.next()),
            "ArrowLeft" => Some(self.prev()),
            "Home" => Some(self.go(0)),
            "End" => Some(self.go(self.len() - 1)),
            k if k.len() == 1 => match k.as_bytes()[0] {
                b @ b'1'..=b'9' if ((b - b'1') as usize) < self.len() => {
                    Some(self.go((b - b'1') as usize))
                }
                _ => None,
            },
            _ => None,
        }
    }
    /// A finished touch: travel `dx`, `dy` in px. Left swipe (negative dx) goes forward,
    /// like turning a page; mostly-vertical moves are scrolls, not swipes.
    pub fn swipe(self, dx: f64, dy: f64) -> Option<Self> {
        if dx.abs() < SWIPE_MIN || dx.abs() < dy.abs() * 1.5 {
            None
        } else if dx < 0.0 {
            Some(self.next())
        } else {
            Some(self.prev())
        }
    }
}

/// The cockpit: the translucent panel of instruments laid over the ship's window. The
/// captain can fold it away to just watch the universe, and bring it back the same ways:
/// the toggle button, the `H` key, a swipe on the helm (down hides) or on the sky (up
/// shows), or a tap on the sky. The page remembers the choice (boot.js).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cockpit {
    hidden: bool,
}

/// Travel (CSS px) under which a touch on the sky counts as a tap.
pub const TAP_MAX: f64 = 10.0;

impl Cockpit {
    pub fn with_hidden(self, hidden: bool) -> Self {
        Cockpit { hidden }
    }
    pub fn shown(self) -> bool {
        !self.hidden
    }
    pub fn toggle(self) -> Self {
        Cockpit {
            hidden: !self.hidden,
        }
    }
    /// "shown" or "hidden": the bridge's `data-cockpit` and the remembered value.
    pub fn state(self) -> &'static str {
        if self.hidden { "hidden" } else { "shown" }
    }
    /// A keyboard key (DOM `KeyboardEvent.key`): `h` toggles; everything else is not ours.
    pub fn key(self, key: &str) -> Option<Self> {
        matches!(key, "h" | "H").then(|| self.toggle())
    }
    /// A finished touch on the helm (cockpit up) or the sky (cockpit down): travel `dx`,
    /// `dy` in px. A mostly-vertical swipe down folds the cockpit away, up brings it back;
    /// a tap on the sky brings it back too. Sideways swipes stay the deck's.
    pub fn touch(self, dx: f64, dy: f64) -> Option<Self> {
        let tap = dx.abs() < TAP_MAX && dy.abs() < TAP_MAX;
        let vertical = dy.abs() >= SWIPE_MIN && dy.abs() > dx.abs() * 1.5;
        match (self.hidden, tap, vertical) {
            (true, true, _) => Some(self.toggle()),
            (true, _, true) if dy < 0.0 => Some(self.toggle()),
            (false, _, true) if dy > 0.0 => Some(self.toggle()),
            _ => None,
        }
    }
}

/// The numbers the bridge instruments show, read once from a snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub live: bool,
    pub calls: usize,
    pub proposals: usize,
    pub seats: usize,
    pub occupied: usize,
    pub workers: usize,
    pub blocked: usize,
    pub projects: usize,
    pub min_quota: Option<f64>,
    pub events: usize,
    pub failures: usize,
    pub moves: usize,
    pub reads: usize,
    /// Epoch ms of the newest seat move, 0 if none.
    pub last_move: u64,
}

impl Summary {
    pub fn of(snap: &Value) -> Self {
        let calls = arr(snap, "calls");
        let seats = arr(snap, "seats");
        let workers = arr(snap, "workers");
        let recent = arr(snap, "recent");
        let moves = arr(snap, "moves");
        Summary {
            live: snap["kernel"].is_object(),
            calls: calls.len(),
            proposals: calls.iter().filter(|c| s(c, "kind") == "proposal").count(),
            seats: seats.len(),
            occupied: seats
                .iter()
                .filter(|x| x["occupied_by"].is_object())
                .count(),
            workers: workers.len(),
            blocked: workers
                .iter()
                .filter(|w| state_class(&s(w, "state")) == "st-alert")
                .count(),
            projects: arr(snap, "projects").len(),
            min_quota: arr(snap, "quota")
                .iter()
                .filter_map(|q| q["remaining_pct"].as_f64())
                .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.min(p)))),
            events: recent.len(),
            failures: recent
                .iter()
                .filter(|e| recent_class(&s(e, "kind")) == "rc-bad")
                .count(),
            moves: moves.len(),
            reads: arr(snap, "context_reads").len(),
            last_move: moves
                .iter()
                .filter_map(|m| m["at"].as_u64())
                .max()
                .unwrap_or(0),
        }
    }
}

/// One instrument panel inside a slide.
#[component]
fn Section(title: String, class: String, count: Option<usize>, children: Element) -> Element {
    rsx! {
        section { class: "panel {class}",
            h3 { class: "ptitle",
                span { class: "ttl", "{title}" }
                if let Some(n) = count {
                    span { class: "count", "{n}" }
                }
            }
            {children}
        }
    }
}

/// The bridge: the ship's window (the cosmos canvas, drawn by the page shell behind this,
/// fills the screen; `#viewport` is its glass) and the cockpit laid over it: the helm
/// instruments, the deck of slides and its controls on one translucent panel, around a
/// clear sight (`#sight`) where the renderer frames the galaxy.
///
/// `live` is the LiveView page: one slide at a time, driven by buttons, keys and swipes,
/// and a cockpit that folds away. Otherwise (the no-JavaScript `/static` page) every slide
/// is rendered, stacked, and the section nav is plain in-page links, so all data stays
/// readable. `hidden` starts the cockpit folded away (the page restores the captain's
/// last choice by pressing the toggle; tests render both states).
#[component]
pub fn Page(snap: Value, live: bool, #[props(default)] hidden: bool) -> Element {
    let mut deck = use_signal(Deck::default);
    let mut touch = use_signal(|| None::<(f64, f64)>);
    let mut helm_touch = use_signal(|| None::<(f64, f64)>);
    let mut sky_touch = use_signal(|| None::<(f64, f64)>);
    let mut cockpit = use_signal(|| Cockpit { hidden });
    // the static page has no toggle, so its cockpit is always up
    let cp = if live { cockpit() } else { Cockpit::default() };
    let d = deck();
    let cur = d.slide();
    let sum = Summary::of(&snap);
    let kernel = snap["kernel"].clone();
    let build = build_tag(&kernel);
    let draining = kernel["draining"] == true;
    let deploy = deploy_line(snap["at"].as_u64().unwrap_or(0), &snap["deploy"]);
    let up = secs(&kernel, "uptime_s").map(dur).unwrap_or_default();
    let shown: Vec<Slide> = if live { vec![cur] } else { Slide::ALL.to_vec() };
    let prev_label = if d.is_first() {
        "Previous section (this is the first)".to_string()
    } else {
        format!("Previous: {}", d.prev().slide().title())
    };
    let next_label = if d.is_last() {
        "Next section (this is the last)".to_string()
    } else {
        format!("Next: {}", d.next().slide().title())
    };
    let toggle_label = if cp.shown() {
        "Hide cockpit"
    } else {
        "Show cockpit"
    };
    // a touch that started at (x0, y0) ended at e: the cockpit's swipe or tap, if it is one
    let touch_end = move |start: Option<(f64, f64)>, e: &PointerEvent| {
        if let Some((x0, y0)) = start {
            let p = e.client_coordinates();
            let mut c = cockpit;
            if let Some(n) = c().touch(p.x - x0, p.y - y0) {
                c.set(n);
            }
        }
    };
    rsx! {
        div {
            class: if live { "bridge live" } else { "bridge static" },
            id: "bridge",
            "data-slide": cur.id(),
            "data-cockpit": cp.state(),
            onkeydown: move |e: KeyboardEvent| {
                if !e.modifiers().is_empty() {
                    return;
                }
                let key = e.key().to_string();
                if let Some(n) = cockpit().key(&key) {
                    cockpit.set(n);
                } else if cockpit().shown()
                    && let Some(n) = deck().key(&key)
                {
                    deck.set(n);
                }
            },
            div {
                id: "cosmos-signals",
                hidden: true,
                "data-workers": "{sum.workers}",
                "data-calls": "{sum.calls}",
                "data-move": "{sum.last_move}",
            }
            // the window's glass: the renderer frames the sky inside it; with the cockpit
            // folded away, a tap or an upward swipe on the sky brings it back
            div {
                class: "viewport",
                id: "viewport",
                aria_hidden: "true",
                onpointerdown: move |e: PointerEvent| {
                    if e.pointer_type() == "touch" {
                        let p = e.client_coordinates();
                        sky_touch.set(Some((p.x, p.y)));
                    }
                },
                onpointerup: move |e: PointerEvent| touch_end(sky_touch.take(), &e),
                onpointercancel: move |_| sky_touch.set(None),
            }
            if live {
                button {
                    r#type: "button",
                    id: "cockpit-toggle",
                    class: if cp.shown() { "cockpit-toggle" } else { "cockpit-toggle off" },
                    aria_controls: "cockpit",
                    aria_expanded: if cp.shown() { "true" } else { "false" },
                    aria_keyshortcuts: "h",
                    title: "{toggle_label} (H)",
                    onclick: move |_| cockpit.set(cockpit().toggle()),
                    span { class: "eye", aria_hidden: "true" }
                    span { class: "tlabel", "{toggle_label}" }
                    kbd { class: "tkey", aria_hidden: "true", "H" }
                }
                div { class: "sr-only", id: "cockpit-status", role: "status",
                    if !cp.shown() { "Cockpit hidden. Press H, the Show cockpit button, or tap the sky to bring it back." }
                }
            }
            div {
                class: "cockpit",
                id: "cockpit",
                role: "region",
                aria_label: "Cockpit: ship status and bridge sections",
                aria_hidden: if cp.shown() { "false" } else { "true" },
                "inert": (!cp.shown()).then_some("true"),
                header {
                    class: if sum.live { "helm on" } else { "helm off" },
                    onpointerdown: move |e: PointerEvent| {
                        if e.pointer_type() == "touch" {
                            let p = e.client_coordinates();
                            helm_touch.set(Some((p.x, p.y)));
                        }
                    },
                    onpointerup: move |e: PointerEvent| touch_end(helm_touch.take(), &e),
                    onpointercancel: move |_| helm_touch.set(None),
                    if live {
                        span { class: "grip", aria_hidden: "true" }
                    }
                    div { class: "kernel",
                        span { class: "dot", aria_hidden: "true" }
                        div { class: "ktext",
                            span { class: "kstate", if sum.live { "Kernel online" } else { "Kernel offline" } }
                            if sum.live {
                                span { class: "kmeta",
                                    "v{s(&kernel, \"version\")} · {build} · pid {s(&kernel, \"os_pid\")} · up {up}"
                                }
                                if draining {
                                    span { class: "kmeta deploy busy", "DRAINING · no new turns" }
                                }
                                if let Some((text, class)) = &deploy {
                                    span { class: "{class}", "{text}" }
                                }
                            } else {
                                span { class: "kmeta", "no snapshot from the kernel" }
                            }
                        }
                    }
                    ul { class: "gauges", aria_label: "Ship status",
                        li { class: if sum.calls > 0 { "gauge g-calls hot" } else { "gauge g-calls" },
                            span { class: "n", "{sum.calls}" } span { class: "l", "calls" } }
                        li { class: "gauge g-seats",
                            span { class: "n", "{sum.occupied}/{sum.seats}" } span { class: "l", "seats" } }
                        li { class: if sum.blocked > 0 { "gauge g-work warn" } else { "gauge g-work" },
                            span { class: "n", "{sum.workers}" } span { class: "l", "working" } }
                        li { class: match sum.min_quota { Some(p) => format!("gauge g-quota {}", quota_class(p)), None => "gauge g-quota".into() },
                            span { class: "n", match sum.min_quota { Some(p) => format!("{p:.0}%"), None => "?".into() } }
                            span { class: "l", "min quota" } }
                    }
                }
                if live {
                    div { class: "sight", id: "sight", aria_hidden: "true" }
                }
                nav { class: "decknav", aria_label: "Bridge sections",
                    ol {
                        for (i, sl) in Slide::ALL.into_iter().enumerate() {
                            li { key: "{sl.id()}",
                                if live {
                                    button {
                                        r#type: "button",
                                        class: if sl == cur { "tab on" } else { "tab" },
                                        "data-slide": sl.id(),
                                        aria_controls: "deck-stage",
                                        aria_current: if sl == cur { "true" } else { "false" },
                                        onclick: move |_| deck.set(deck().go(i)),
                                        {tab_body(i, sl, &sum)}
                                    }
                                } else {
                                    a { class: "tab", href: "#slide-{sl.id()}", {tab_body(i, sl, &sum)} }
                                }
                            }
                        }
                    }
                }
                section {
                    class: "deck",
                    id: "deck",
                    tabindex: if live { "0" } else { "-1" },
                    aria_roledescription: "slide deck",
                    aria_label: "Bridge deck",
                    onpointerdown: move |e: PointerEvent| {
                        if e.pointer_type() == "touch" {
                            let p = e.client_coordinates();
                            touch.set(Some((p.x, p.y)));
                        }
                    },
                    onpointerup: move |e: PointerEvent| {
                        if let Some((x0, y0)) = touch.take() {
                            let p = e.client_coordinates();
                            if let Some(n) = deck().swipe(p.x - x0, p.y - y0) {
                                deck.set(n);
                            }
                        }
                    },
                    onpointercancel: move |_| touch.set(None),
                    if live {
                        div { class: "deckbar",
                            button {
                                r#type: "button",
                                class: "step prev",
                                aria_label: "{prev_label}",
                                disabled: d.is_first(),
                                onclick: move |_| deck.set(deck().prev()),
                                span { aria_hidden: "true", "‹" }
                            }
                            div { class: "pos", id: "deck-pos", aria_live: "polite", aria_atomic: "true",
                                span { class: "posn", "{d.position()}" }
                                span { class: "post", "{cur.title()}" }
                            }
                            button {
                                r#type: "button",
                                class: "step next",
                                aria_label: "{next_label}",
                                disabled: d.is_last(),
                                onclick: move |_| deck.set(deck().next()),
                                span { aria_hidden: "true", "›" }
                            }
                        }
                    }
                    div { class: "stage", id: "deck-stage",
                        for sl in shown {
                            article {
                                key: "{sl.id()}",
                                class: "slide s-{sl.id()}",
                                id: "slide-{sl.id()}",
                                "data-dir": d.entered_from(),
                                aria_roledescription: "slide",
                                aria_label: "{sl.index() + 1} of {Slide::ALL.len()}: {sl.title()}",
                                h2 { class: "stitle",
                                    span { class: "sno", "{sl.index() + 1:02}" }
                                    "{sl.title()}"
                                }
                                {slide_body(sl, &snap, &sum)}
                            }
                        }
                    }
                }
                footer { class: "foot",
                    span { class: "ro", "Read-only" }
                    span { " · this bridge observes; captain actions stay in your threads" }
                    if live {
                        span { class: "keys", " · ← → keys, 1–6 or swipe to change section" }
                    }
                }
            }
        }
    }
}

fn tab_body(i: usize, sl: Slide, sum: &Summary) -> Element {
    let (badge, hot) = sl.badge(sum);
    rsx! {
        span { class: "tno", aria_hidden: "true", "{i + 1}" }
        span { class: "tname", "{sl.title()}" }
        span { class: if hot { "tbadge hot" } else { "tbadge" }, "{badge}" }
    }
}

fn slide_body(sl: Slide, snap: &Value, sum: &Summary) -> Element {
    match sl {
        Slide::Bridge => calls_slide(snap, sum),
        Slide::Crew => crew_slide(snap),
        Slide::Missions => missions_slide(snap),
        Slide::Projects => projects_slide(snap),
        Slide::Quota => quota_slide(snap),
        Slide::Log => log_slide(snap),
    }
}

/// The first slide: what needs the captain now, and the ship's live status.
fn calls_slide(snap: &Value, sum: &Summary) -> Element {
    let now = snap["at"].as_u64().unwrap_or(0);
    let mut calls = arr(snap, "calls");
    calls.sort_by_key(|c| std::cmp::Reverse(secs(c, "age_s").unwrap_or(0)));
    let latest = arr(snap, "recent").last().cloned();
    rsx! {
        div { class: "cols",
            Section { title: "Your calls", class: "calls", count: Some(calls.len()),
                if calls.is_empty() {
                    p { class: "empty ok", "Nothing needs you." }
                }
                ul { class: "list",
                    for c in calls.iter() {
                        li { key: "{s(c, \"id\")}", class: "call",
                            div { class: "row1",
                                span { class: if s(c, "kind") == "proposal" { "chip k-proposal" } else { "chip k-decision" },
                                    if s(c, "kind").is_empty() { "decision" } else { "{s(c, \"kind\")}" } }
                                span { class: "cid", "{s(c, \"id\")}" }
                                if !s(c, "project").is_empty() {
                                    span { class: "proj", "◆ {s(c, \"project\")}" }
                                }
                                span { class: "age", "waiting {secs(c, \"age_s\").map(dur).unwrap_or_default()}" }
                            }
                            p { class: "q", "{s(c, \"question\")}" }
                            if c["options"].as_array().is_some_and(|o| !o.is_empty()) {
                                ul { class: "opts", aria_label: "Options",
                                    for (i, o) in arr(c, "options").iter().enumerate() {
                                        li { key: "{i}", class: "opt",
                                            {match o { Value::String(t) => t.clone(), other => other.to_string() }}
                                        }
                                    }
                                }
                            }
                            p { class: "hint",
                                "To act, type in your captain thread: "
                                code { class: "cmd", {call_command(c)} }
                            }
                        }
                    }
                }
            }
            Section { title: "Live status", class: "live-status", count: None,
                dl { class: "readouts",
                    div { dt { "Kernel" } dd { class: if sum.live { "ok" } else { "bad" }, if sum.live { "online" } else { "offline" } } }
                    div { dt { "Proposals held" } dd { "{sum.proposals}" } }
                    div { dt { "Missions running" } dd { "{sum.workers}" } }
                    div { dt { "Blocked" } dd { class: if sum.blocked > 0 { "bad" } else { "" }, "{sum.blocked}" } }
                    div { dt { "Crew seated" } dd { "{sum.occupied} of {sum.seats}" } }
                    div { dt { "Last seat move" }
                        dd { if sum.last_move > 0 { "{ago_ms(now, &Value::from(sum.last_move))} ago" } else { "none yet" } } }
                }
                if let Some(e) = latest {
                    p { class: "latest ev {recent_class(&s(&e, \"kind\"))}",
                        span { class: "kind", "{s(&e, \"kind\")}" }
                        " {s(&e, \"text\")} · {ago_ms(now, &e[\"at\"])} ago"
                    }
                }
            }
        }
    }
}

/// The exact command the captain types to act on a call; the bridge never acts itself.
pub fn call_command(c: &Value) -> String {
    if s(c, "kind") == "proposal" {
        format!("$unvrs:approve {}", s(c, "id"))
    } else {
        format!("$unvrs:answer {} <your answer>", s(c, "id"))
    }
}

fn crew_slide(snap: &Value) -> Element {
    let seats = arr(snap, "seats");
    rsx! {
        Section { title: "Seats", class: "seats", count: Some(seats.len()),
            if seats.is_empty() {
                p { class: "empty", "No seats yet. Type " code { class: "cmd", "$unvrs:l1" } " in a thread." }
            }
            div { class: "seatgrid",
                for (i, st) in seats.iter().enumerate() {
                    Seat { key: "{i}", seat: st.clone() }
                }
            }
        }
    }
}

fn missions_slide(snap: &Value) -> Element {
    let workers = arr(snap, "workers");
    rsx! {
        Section { title: "Working now", class: "working", count: Some(workers.len()),
            if workers.is_empty() {
                p { class: "empty", "No L3 workers running." }
            }
            ul { class: "list",
                for w in workers.iter() {
                    li { key: "{s(w, \"pid\")}", class: "worker {state_class(&s(w, \"state\"))}",
                        div { class: "row1",
                            span { class: "pidno", "PID {s(w, \"pid\")}" }
                            if !s(w, "project").is_empty() {
                                span { class: "proj", "◆ {s(w, \"project\")}" }
                            }
                            span { class: "badge {harness_class(&s(w, \"harness\"))}", "{s(w, \"harness\")}" }
                            if !s(w, "model").is_empty() {
                                span { class: "model", "{s(w, \"model\")}" }
                            }
                            span { class: "elapsed", "{secs(w, \"elapsed_s\").map(clock).unwrap_or_default()}" }
                        }
                        p { class: "intent", title: "{s(w, \"intent\")}", "{short(&s(w, \"intent\"), 180)}" }
                        div { class: "row3",
                            if !s(w, "shape").is_empty() {
                                span { class: "chip", "{s(w, \"shape\")}" }
                            }
                            span { class: "chip {state_class(&s(w, \"state\"))}", "{s(w, \"state\")}" }
                            span { class: "scan", aria_hidden: "true" }
                        }
                    }
                }
            }
        }
    }
}

fn projects_slide(snap: &Value) -> Element {
    let projects = arr(snap, "projects");
    rsx! {
        Section { title: "Projects", class: "projects", count: Some(projects.len()),
            if projects.is_empty() {
                p { class: "empty", "No projects. L1 proposes; you type " code { class: "cmd", "$unvrs:approve <id>" } "." }
            }
            div { class: "projgrid",
                for p in projects.iter() {
                    div { key: "{s(p, \"id\")}", class: "project",
                        div { class: "row1",
                            span { class: "pname", "{s(p, \"id\")}" }
                            if p["seat_pid"].is_null() {
                                span { class: "chip st-idle", "no seat" }
                            } else {
                                span { class: "chip st-live", "L2 · PID {s(p, \"seat_pid\")}" }
                            }
                        }
                        p { class: "purpose", "{s(p, \"purpose\")}" }
                        div { class: "sources",
                            for (i, src) in arr(p, "sources").iter().enumerate() {
                                span { key: "{i}", class: "chip src",
                                    {match src { Value::String(t) => t.clone(), other => other["id"].as_str().map(str::to_owned).unwrap_or_else(|| other.to_string()) }}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn quota_slide(snap: &Value) -> Element {
    let now = snap["at"].as_u64().unwrap_or(0);
    let quota = arr(snap, "quota");
    rsx! {
        Section { title: "Quota", class: "quota", count: Some(quota.len()),
            if quota.is_empty() {
                p { class: "empty", "No accounts reported." }
            }
            ul { class: "list",
                for (i, q) in quota.iter().enumerate() {
                    Quota { key: "{i}", q: q.clone(), now }
                }
            }
        }
    }
}

fn log_slide(snap: &Value) -> Element {
    let now = snap["at"].as_u64().unwrap_or(0);
    let moves = arr(snap, "moves");
    let recent = arr(snap, "recent");
    let reads = arr(snap, "context_reads");
    rsx! {
        div { class: "cols three",
            Section { title: "Recent", class: "recent", count: Some(recent.len()),
                if recent.is_empty() {
                    p { class: "empty", "Quiet so far." }
                }
                ul { class: "list timeline",
                    for (i, e) in recent.iter().rev().take(40).enumerate() {
                        li { key: "{i}", class: "ev {recent_class(&s(e, \"kind\"))}",
                            span { class: "when", "{ago_ms(now, &e[\"at\"])}" }
                            span { class: "kind", "{s(e, \"kind\")}" }
                            span { class: "what", "{s(e, \"text\")}" }
                        }
                    }
                }
            }
            Section { title: "Seat moves", class: "moves", count: Some(moves.len()),
                if moves.is_empty() {
                    p { class: "empty", "No seat moves, swaps or handoffs yet." }
                }
                ul { class: "list",
                    for (i, m) in moves.iter().rev().take(24).enumerate() {
                        li { key: "{i}", class: "move mv-{s(m, \"kind\")}",
                            span { class: "tag", "{s(m, \"kind\")}" }
                            span { class: "fromto",
                                span { class: "from", "{s(m, \"from\")}" }
                                span { class: "arrow", " → " }
                                span { class: "to", "{s(m, \"to\")}" }
                            }
                            span { class: "meta",
                                if !s(m, "pid").is_empty() { "PID {s(m, \"pid\")} · " }
                                if let Some(b) = m["bytes"].as_u64() { "{bytes(b)} · " }
                                "{ago_ms(now, &m[\"at\"])}"
                            }
                        }
                    }
                }
            }
            Section { title: "Context reads", class: "reads", count: Some(reads.len()),
                if reads.is_empty() {
                    p { class: "empty", "No context reads yet." }
                }
                ul { class: "list",
                    for (i, r) in reads.iter().rev().take(30).enumerate() {
                        li { key: "{i}", class: "read",
                            div { class: "row1",
                                span { class: "pidno", "PID {s(r, \"pid\")}" }
                                span { class: "chip src", "{s(r, \"source\")}" }
                                span { class: "meta",
                                    if let Some(b) = r["bytes"].as_u64() { "{bytes(b)} · " }
                                    "{ago_ms(now, &r[\"at\"])}"
                                }
                            }
                            code { class: "ref", title: "{s(r, \"ref\")}", "{s(r, \"ref\")}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Seat(seat: Value) -> Element {
    let rank = seat["rank"].as_u64().unwrap_or(0);
    let project = s(&seat, "project");
    let name = if rank == 1 || project.is_empty() {
        "CAPTAIN".to_string()
    } else {
        project.to_uppercase()
    };
    let occ = seat["occupied_by"].clone();
    let state = s(&seat, "state");
    let wakes = seat["wakes"].as_u64().unwrap_or(0);
    let link = s(&occ, "link");
    let title = {
        let t = s(&occ, "title");
        if t.is_empty() {
            short(&s(&occ, "thread_id"), 18)
        } else {
            short(&t, 64)
        }
    };
    rsx! {
        div { class: if occ.is_object() { "seat occ" } else { "seat vacant" },
            div { class: "row1",
                span { class: "rank r{rank}", "L{rank}" }
                span { class: "sname", "{name}" }
                if !state.is_empty() {
                    span { class: "chip {state_class(&state)}", "{state}" }
                }
                if wakes > 0 {
                    span { class: "chip mail", "✉ {wakes}" }
                }
            }
            if occ.is_object() {
                div { class: "occ",
                    span { class: "badge app", "{s(&occ, \"app\")}" }
                    span { class: "badge {harness_class(&s(&occ, \"harness\"))}", "{s(&occ, \"harness\")}" }
                    if !s(&seat, "pid").is_empty() {
                        span { class: "dimtext", "PID {s(&seat, \"pid\")}" }
                    }
                }
                div { class: "thread",
                    if is_link(&link) {
                        a { href: "{link}", title: "{link}", "{title} ↗" }
                    } else {
                        span { "{title}" }
                    }
                }
            } else {
                div { class: "occ dimtext", "vacant · wakes run it detached" }
            }
        }
    }
}

#[component]
fn Quota(q: Value, now: u64) -> Element {
    let pct = q["remaining_pct"].as_f64();
    let resets = q["resets_at"].as_u64().map(|t| {
        if now > 0 && t > now {
            format!("resets in {}", dur((t - now) / 1000))
        } else {
            "reset due".into()
        }
    });
    rsx! {
        li { class: "quota-row",
            div { class: "row1",
                span { class: "badge {harness_class(&s(&q, \"harness\"))}", "{s(&q, \"harness\")}" }
                span { class: "acct", "{s(&q, \"account\")}" }
                span { class: "pct",
                    match pct { Some(p) => rsx! { span { class: "{quota_class(p)}", "{p:.0}%" } }, None => rsx! { span { class: "unknown", "unknown" } } }
                }
            }
            div { class: match pct { Some(p) => format!("meter {}", quota_class(p)), None => "meter unknown".into() },
                if let Some(p) = pct {
                    div { class: "fill", style: "width: {p.clamp(0.0, 100.0):.1}%" }
                }
            }
            div { class: "row3 dimtext",
                if let Some(r) = resets { "{r}" } else { "reset time unknown" }
                if !s(&q, "source").is_empty() { " · via {s(&q, \"source\")}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deck_starts_on_captain_calls_and_clamps_at_ends() {
        let d = Deck::default();
        assert_eq!(d.slide(), Slide::Bridge);
        assert_eq!(d.position(), "1 / 6");
        assert!(d.is_first() && !d.is_last());
        assert_eq!(d.prev(), d, "prev on the first slide stays");
        let last = d.go(99);
        assert_eq!(last.slide(), Slide::Log);
        assert!(last.is_last());
        assert_eq!(last.next(), last, "next on the last slide stays");
        assert_eq!(last.position(), "6 / 6");
        let mut w = d;
        for s in Slide::ALL {
            assert_eq!(w.slide(), s);
            w = w.next();
        }
    }

    #[test]
    fn slide_ids_round_trip_and_are_unique() {
        for (i, s) in Slide::ALL.into_iter().enumerate() {
            assert_eq!(s.index(), i);
            assert_eq!(Slide::at(i), s);
            assert_eq!(Slide::from_id(s.id()), Some(s));
            assert_eq!(Slide::from_id(&format!("#{}", s.id())), Some(s));
            assert_eq!(Deck::new(s).slide(), s);
        }
        assert_eq!(Slide::from_id("nope"), None);
        let mut ids: Vec<_> = Slide::ALL.iter().map(|s| s.id()).collect();
        ids.dedup();
        assert_eq!(ids.len(), Slide::ALL.len());
    }

    #[test]
    fn keys_drive_the_deck_and_leave_others_alone() {
        let d = Deck::default();
        assert_eq!(d.key("ArrowRight").unwrap().slide(), Slide::Crew);
        assert_eq!(d.go(2).key("ArrowLeft").unwrap().slide(), Slide::Crew);
        assert_eq!(d.key("End").unwrap().slide(), Slide::Log);
        assert_eq!(d.go(4).key("Home").unwrap().slide(), Slide::Bridge);
        assert_eq!(d.key("3").unwrap().slide(), Slide::Missions);
        assert_eq!(d.key("6").unwrap().slide(), Slide::Log);
        for other in [
            "7",
            "9",
            "0",
            "Tab",
            "Enter",
            " ",
            "a",
            "Escape",
            "ArrowUp",
            "ArrowDown",
            "PageDown",
        ] {
            assert_eq!(d.key(other), None, "{other:?}");
        }
    }

    #[test]
    fn slide_transition_follows_the_direction_of_travel() {
        let d = Deck::default();
        assert_eq!(d.entered_from(), "fwd", "a fresh deck reads as arrived");
        assert_eq!(d.next().entered_from(), "fwd");
        assert_eq!(d.go(4).prev().entered_from(), "back");
        assert_eq!(d.go(4).go(1).entered_from(), "back");
        assert_eq!(d.go(1).go(5).entered_from(), "fwd");
        // a move that clamps at an end keeps the last direction and stays equal
        assert_eq!(d.prev(), d);
        assert_eq!(d.go(5).next(), d.go(5));
        assert_eq!(d.go(5).prev().next().entered_from(), "fwd");
    }

    #[test]
    fn swipes_turn_pages_scrolls_do_not() {
        let d = Deck::default().go(2);
        assert_eq!(d.swipe(-120.0, 10.0).unwrap().slide(), Slide::Projects);
        assert_eq!(d.swipe(120.0, -10.0).unwrap().slide(), Slide::Crew);
        assert_eq!(d.swipe(-20.0, 0.0), None, "too short");
        assert_eq!(d.swipe(-80.0, 200.0), None, "vertical scroll");
        assert_eq!(Deck::default().swipe(200.0, 0.0), Some(Deck::default()));
    }

    #[test]
    fn cockpit_folds_away_and_comes_back() {
        let up = Cockpit::default();
        assert!(up.shown());
        assert_eq!(up.state(), "shown");
        let down = up.toggle();
        assert!(!down.shown());
        assert_eq!(down.state(), "hidden");
        assert_eq!(down.toggle(), up);
        // the H key, either case; nothing else is the cockpit's
        assert_eq!(up.key("h"), Some(down));
        assert_eq!(down.key("H"), Some(up));
        for other in ["Escape", "ArrowRight", "1", "Home", " ", "Enter", "j"] {
            assert_eq!(up.key(other), None, "{other:?}");
        }
        // and the deck's keys are not the cockpit's
        assert_eq!(Deck::default().key("h"), None);
        assert_eq!(Deck::default().key("H"), None);
    }

    #[test]
    fn cockpit_touches_swipe_down_to_hide_up_or_tap_to_show() {
        let up = Cockpit::default();
        let down = up.toggle();
        assert_eq!(
            up.touch(4.0, 120.0),
            Some(down),
            "swipe down on the helm hides"
        );
        assert_eq!(
            up.touch(0.0, -120.0),
            None,
            "swipe up with the cockpit up does nothing"
        );
        assert_eq!(
            up.touch(2.0, 3.0),
            None,
            "a tap on the helm is not a toggle"
        );
        assert_eq!(
            up.touch(-160.0, 20.0),
            None,
            "sideways swipes are the deck's"
        );
        assert_eq!(up.touch(50.0, 60.0), None, "too diagonal");
        assert_eq!(
            down.touch(3.0, -4.0),
            Some(up),
            "tap the sky to bring it back"
        );
        assert_eq!(down.touch(0.0, -90.0), Some(up), "swipe up on the sky");
        assert_eq!(down.touch(0.0, 90.0), None, "swipe down keeps it away");
        assert_eq!(
            down.touch(40.0, 0.0),
            None,
            "short sideways drag is neither"
        );
    }

    fn snap() -> Value {
        json!({
            "at": 1_000_000u64,
            "kernel": {"version": "0.8.0", "os_pid": 7, "uptime_s": 90},
            "calls": [
                {"id": "d-1", "kind": "decision", "question": "q?", "age_s": 5},
                {"id": "p-1", "kind": "proposal", "question": "new?", "age_s": 50},
            ],
            "seats": [
                {"rank": 1, "occupied_by": {"app": "Codex app"}},
                {"rank": 2, "project": "x", "occupied_by": null},
            ],
            "workers": [
                {"pid": 1, "state": "working"},
                {"pid": 2, "state": "blocked"},
                {"pid": 3, "state": "running"},
            ],
            "quota": [
                {"remaining_pct": 64.0}, {"remaining_pct": null}, {"remaining_pct": 12.5},
            ],
            "moves": [{"at": 10}, {"at": 900_000}, {"at": 500}],
            "recent": [{"kind": "delivered"}, {"kind": "failed"}, {"kind": "woke"}],
            "context_reads": [{"ref": "ctx://a"}],
            "projects": [{"id": "x"}, {"id": "y"}],
        })
    }

    #[test]
    fn summary_maps_a_full_snapshot() {
        let sum = Summary::of(&snap());
        assert_eq!(
            sum,
            Summary {
                live: true,
                calls: 2,
                proposals: 1,
                seats: 2,
                occupied: 1,
                workers: 3,
                blocked: 1,
                projects: 2,
                min_quota: Some(12.5),
                events: 3,
                failures: 1,
                moves: 3,
                reads: 1,
                last_move: 900_000,
            }
        );
        assert_eq!(Slide::Bridge.badge(&sum), ("2".into(), true));
        assert_eq!(Slide::Crew.badge(&sum), ("1/2".into(), false));
        assert_eq!(Slide::Missions.badge(&sum), ("3".into(), true));
        assert_eq!(Slide::Quota.badge(&sum), ("12%".into(), true));
        assert_eq!(Slide::Log.badge(&sum), ("3".into(), true));
    }

    #[test]
    fn summary_of_nothing_is_offline_and_empty() {
        for v in [Value::Null, json!({}), json!({"calls": "junk", "quota": 3})] {
            let sum = Summary::of(&v);
            assert_eq!(sum, Summary::default());
            assert!(!sum.live);
            assert_eq!(Slide::Bridge.badge(&sum), ("0".into(), false));
            assert_eq!(Slide::Quota.badge(&sum), ("?".into(), false));
        }
    }

    fn render(live: bool) -> String {
        render_with(live, false)
    }

    fn render_with(live: bool, hidden: bool) -> String {
        let v = snap();
        dioxus_ssr::render_element(rsx! { Page { snap: v, live, hidden } })
    }

    #[test]
    fn live_page_has_a_cockpit_toggle_with_its_state() {
        let html = render_with(true, false);
        assert!(html.contains("id=\"cockpit-toggle\""));
        assert!(html.contains("aria-controls=\"cockpit\""));
        assert!(html.contains("aria-expanded=\"true\""));
        assert!(html.contains("aria-keyshortcuts=\"h\""));
        assert!(html.contains("data-cockpit=\"shown\""));
        assert!(html.contains("Hide cockpit"));
        assert!(html.contains("id=\"sight\""));
        assert!(!html.contains("inert"), "{html}");
        // the toggle comes before the cockpit, and the cockpit holds the instruments
        let (t, c) = (
            html.find("cockpit-toggle").unwrap(),
            html.find("id=\"cockpit\"").unwrap(),
        );
        assert!(t < c);
        for inside in [
            "class=\"helm on\"",
            "class=\"decknav\"",
            "id=\"deck\"",
            "class=\"foot\"",
        ] {
            assert!(html.find(inside).unwrap() > c, "{inside}");
        }

        let html = render_with(true, true);
        assert!(html.contains("data-cockpit=\"hidden\""));
        assert!(html.contains("aria-expanded=\"false\""));
        assert!(html.contains("Show cockpit"));
        assert!(html.contains("inert=\"true\""));
        let c = html.find("id=\"cockpit\"").unwrap();
        let tag = &html[html[..c].rfind('<').unwrap()..c + html[c..].find('>').unwrap()];
        assert!(tag.contains("aria-hidden=\"true\""), "{tag}");
        assert!(html.contains("Cockpit hidden."));
    }

    #[test]
    fn live_page_shows_one_slide_with_deck_controls() {
        let html = render(true);
        assert_eq!(html.matches("aria-roledescription=\"slide\"").count(), 1);
        assert!(html.contains("id=\"slide-calls\""));
        assert!(html.contains("aria-label=\"1 of 6: Captain calls\""));
        assert!(html.contains("aria-roledescription=\"slide deck\""));
        assert!(html.contains("1 / 6"));
        assert!(html.contains("aria-live=\"polite\""));
        assert!(html.contains("aria-label=\"Next: Crew\""));
        // prev is disabled on the first slide; every section has a direct nav button
        assert!(html.contains("disabled"));
        for sl in Slide::ALL {
            assert!(
                html.contains(&format!("data-slide=\"{}\"", sl.id())),
                "{sl:?}"
            );
        }
        assert_eq!(html.matches("aria-current=\"true\"").count(), 1);
        // truthful actions: the command to type, and no form or action button
        assert!(html.contains("$unvrs:approve p-1"));
        assert!(
            html.contains("$unvrs:answer d-1 &lt;your answer&gt;")
                || html.contains("$unvrs:answer d-1 &#60;your answer&#62;"),
            "{html}"
        );
        assert!(!html.contains("<form"));
        assert!(html.contains("Kernel online"));
        assert!(html.contains("data-calls=\"2\""));
    }

    #[test]
    fn static_page_stacks_every_slide_with_link_nav() {
        let html = render(false);
        assert_eq!(html.matches("aria-roledescription=\"slide\"").count(), 6);
        for sl in Slide::ALL {
            assert!(
                html.contains(&format!("id=\"slide-{}\"", sl.id())),
                "{sl:?}"
            );
            assert!(
                html.contains(&format!("href=\"#slide-{}\"", sl.id())),
                "{sl:?}"
            );
        }
        assert!(
            !html.contains("<button"),
            "no controls that need JavaScript"
        );
        // no toggle, and the cockpit is always up, even if asked to start hidden
        assert!(!html.contains("cockpit-toggle"));
        assert!(html.contains("data-cockpit=\"shown\""));
        let v = snap();
        let hidden =
            dioxus_ssr::render_element(rsx! { Page { snap: v, live: false, hidden: true } });
        assert!(hidden.contains("data-cockpit=\"shown\"") && !hidden.contains("inert"));
        // data from every slide is present
        for text in [
            "q?",
            "Codex app",
            "PID 2",
            "12%",
            "delivered",
            "ctx://a",
            ">x<",
        ] {
            assert!(html.contains(text), "{text}");
        }
    }

    #[test]
    fn offline_snapshot_still_renders() {
        let html = dioxus_ssr::render_element(rsx! { Page { snap: Value::Null, live: true } });
        assert!(html.contains("Kernel offline"));
        assert!(html.contains("Nothing needs you."));
    }

    #[test]
    fn call_commands_are_the_ones_the_captain_types() {
        assert_eq!(
            call_command(&json!({"id": "p-7", "kind": "proposal"})),
            "$unvrs:approve p-7"
        );
        assert_eq!(
            call_command(&json!({"id": "d-2", "kind": "decision"})),
            "$unvrs:answer d-2 <your answer>"
        );
        assert_eq!(
            call_command(&json!({"id": "d-3"})),
            "$unvrs:answer d-3 <your answer>"
        );
    }

    #[test]
    fn header_shows_the_live_build_and_the_deploy() {
        let snap = json!({"at": 61_000,
            "kernel": {"version": "0.8.0", "commit": "0123456789abcdef", "ref": "feat/x", "draining": true, "os_pid": 7},
            "deploy": {"run": {"kind": "deploy", "ref": "main", "phase": "drain", "done": false, "stale": false, "started": 1000}}});
        let html = dioxus_ssr::render_element(rsx! { Page { snap: snap, live: true } });
        for want in [
            "v0.8.0 · 0123456789ab (feat/x) · pid 7",
            "DRAINING · no new turns",
            "DEPLOY MAIN · drain 1m",
        ] {
            assert!(html.contains(want), "{want} missing");
        }
        let html = dioxus_ssr::render_element(
            rsx! { Page { snap: json!({"kernel": {"version": "0.8.0"}}), live: true } },
        );
        assert!(html.contains("v0.8.0 · dev build") && !html.contains("DRAINING"));
    }

    #[test]
    fn build_and_deploy_lines() {
        assert_eq!(build_tag(&json!({"commit": null})), "dev build");
        assert_eq!(
            build_tag(&json!({"commit": "0123456789abcdef", "ref": "feat/x"})),
            "0123456789ab (feat/x)"
        );
        assert_eq!(deploy_line(0, &json!({"run": null})), None);
        let (t, c) = deploy_line(
            61_000,
            &json!({"run": {"kind": "deploy", "ref": "main", "phase": "gate", "done": false, "stale": false, "started": 1000}}),
        )
        .unwrap();
        assert_eq!(
            (t.as_str(), c),
            ("DEPLOY MAIN · gate 1m", "kmeta deploy busy")
        );
        let (t, c) = deploy_line(
            0,
            &json!({"run": {"kind": "deploy", "ref": "main", "phase": "build", "done": false, "stale": true}}),
        )
        .unwrap();
        assert_eq!(
            (t.as_str(), c),
            (
                "deploy main stopped at build (deployer gone)",
                "kmeta deploy bad"
            )
        );
        let (t, c) = deploy_line(
            5000,
            &json!({"run": {"kind": "rollback", "done": true, "result": "rolled_back", "ended": 2000, "reason": "gate failed"}}),
        )
        .unwrap();
        assert_eq!(
            (t.as_str(), c),
            (
                "last rollback: rolled_back 3s ago · gate failed",
                "kmeta deploy warn"
            )
        );
    }
}
