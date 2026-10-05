//! The preview page: glance (the status line), scan (the zones, drawn as lights, a tree
//! and gauges), detail (click anything: a drawer with the text and where each value came
//! from). Read-only: the only action is copying the command the captain types.
use super::diagram::{self, Kind, Node, route_lines};
use super::model::{
    Context, Crew, CtxRow, Fuel, Gauge, Light, Model, Need, NeedKind, Seat, Tone, Worker, ZONE_MAX,
};
use crate::view::Cockpit;
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};

fn glyph(t: Tone) -> &'static str {
    match t {
        Tone::Ok => "●",
        Tone::Idle => "○",
        Tone::Unknown => "◌",
        Tone::Off => "○",
        Tone::Warn => "▲",
        Tone::Bad => "■",
    }
}

/// A light: a drawn dot whose colour and shape carry the tone; it pulses only when
/// `pulse` (a worker producing output, a seat in a turn).
fn dot(t: Tone, pulse: bool) -> Element {
    rsx! {
        span {
            class: if pulse { "dot {t.class()} pulse" } else { "dot {t.class()}" },
            role: "img",
            aria_label: "{t.word()}",
        }
    }
}

/// The copy button: an icon (two sheets); the command it copies is in its tooltip.
fn copy_button(copy: &str, shown: &str) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "copy",
            "data-copy": "{copy}",
            title: "Copy: {shown}",
            aria_label: "Copy command: {shown}",
            svg { class: "ic", view_box: "0 0 16 16", "aria-hidden": "true",
                rect { x: "5.5", y: "5.5", width: "8", height: "8" }
                path { d: "M10.5 5.5V2.5h-8v8h3" }
            }
        }
    }
}

/// The chip's words for a model and effort, only the parts the kernel reported:
/// "fable-5-1 · high", "6-sol", "high effort"; None when it reported neither.
fn chip_text(model: &str, effort: &str) -> Option<String> {
    let known = |v: &str| !v.is_empty() && v != "unknown";
    let short = model
        .trim_start_matches("claude-")
        .trim_start_matches("gpt-");
    match (known(model), known(effort)) {
        (true, true) => Some(format!("{short} · {effort}")),
        (true, false) => Some(short.to_owned()),
        (false, true) => Some(format!("{effort} effort")),
        (false, false) => None,
    }
}

/// A short chip for a seat's or worker's model and effort, never a "?": the reported
/// parts, else "model not reported" in words, or nothing at all when `quiet` (idle and
/// vacant cards). The tooltip spells out both.
fn model_chip(harness: &str, model: &str, effort: &str, quiet: bool) -> Element {
    let said = |v: &str| {
        if v.is_empty() || v == "unknown" {
            "not reported".to_owned()
        } else {
            v.to_owned()
        }
    };
    let tip = format!(
        "{harness} · model {} · effort {}",
        said(model),
        said(effort)
    );
    match chip_text(model, effort) {
        Some(t) => rsx! { span { class: "chip", title: "{tip}", "{t}" } },
        None if quiet => rsx! {},
        None => rsx! { span { class: "chip unk", title: "{tip}", "model not reported" } },
    }
}

fn answer_bar(n: &Need) -> Element {
    rsx! {
        div { class: "answer",
            span { class: "where", title: "Answer in {n.app} · {n.seat}", "{n.app} · {n.seat}" }
            span { class: "cmd", title: "{n.shown}", "{n.shown}" }
            {copy_button(&n.copy, &n.shown)}
            // only where a deep link is proven to open the right thread
            if let Some(link) = &n.open {
                a { class: "open", href: "{link}", "Open" }
            }
        }
    }
}

fn need_row(n: &Need, mut drawer: Signal<Option<String>>) -> Element {
    let k = n.key.clone();
    let kind = match n.kind {
        NeedKind::Fault => "fault",
        NeedKind::Decision => "decision",
        NeedKind::Proposal => "proposal",
    };
    rsx! {
        li { key: "{n.key}", class: "need {n.tone.class()} k-{kind}", "data-id": "{n.id}",
            button {
                r#type: "button",
                class: "nrow",
                aria_haspopup: "dialog",
                onclick: move |_| drawer.set(Some(k.clone())),
                span { class: "g", aria_hidden: "true", if n.blocking { "■" } else if n.kind == NeedKind::Fault { "▲" } else { "◆" } }
                span { class: "nid", "{n.id}" }
                span { class: "ntitle", "{n.title}" }
                span { class: "age", "{n.age}" }
            }
            {answer_bar(n)}
        }
    }
}

/// The activity sparkline: journal events per 5 minutes over the last hour, drawn as
/// twelve bars (oldest left). A bucket the journal does not reach is a hollow mark, not a
/// zero.
fn spark(a: &[Option<u32>]) -> Element {
    let max = a.iter().flatten().copied().max().unwrap_or(0).max(1);
    let n = a.len().max(1);
    let w = 60.0 / n as f64;
    let last = a.last().copied().flatten();
    let label = match last {
        Some(c) => format!("{c} kernel events in the last 5 min; bars: last hour"),
        None => "kernel activity unknown".into(),
    };
    rsx! {
        svg { class: "spark", view_box: "0 0 60 14", role: "img", "aria-label": "{label}", preserve_aspect_ratio: "none",
            for (i, b) in a.iter().enumerate() {
                {
                    let x = i as f64 * w + 0.5;
                    match b {
                        Some(c) => {
                            let h = if *c == 0 { 1.0 } else { 1.0 + 13.0 * (*c as f64 / max as f64) };
                            rsx! { rect { key: "b{i}", class: if *c == 0 { "sb zero" } else { "sb" }, x: "{x}", y: "{14.0 - h}", width: "{w - 1.0}", height: "{h}" } }
                        }
                        None => rsx! { rect { key: "b{i}", class: "sb none", x: "{x}", y: "12", width: "{w - 1.0}", height: "1" } },
                    }
                }
            }
        }
    }
}

/// A settled call: closed in the kernel journal, though the snapshot still lists it.
fn settled_row(n: &Need, mut drawer: Signal<Option<String>>) -> Element {
    let k = n.key.clone();
    rsx! {
        li { key: "{n.key}", class: "need settled", "data-id": "{n.id}",
            button {
                r#type: "button",
                class: "nrow",
                aria_haspopup: "dialog",
                onclick: move |_| drawer.set(Some(k.clone())),
                span { class: "g", aria_hidden: "true", "✓" }
                span { class: "nid", "{n.id}" }
                span { class: "ntitle", "{n.title}" }
                span { class: "age", "{n.age}" }
            }
            p { class: "evidence", {n.settled.clone().unwrap_or_default()} }
        }
    }
}

/// The kernel row's words: the summary without its version and heartbeat segments (the
/// status line has the heartbeat; the drawer has the version).
fn kernel_brief(summary: &str) -> String {
    summary
        .split(" · ")
        .filter(|p| {
            !(p.starts_with("heartbeat")
                || p.starts_with('v') && p[1..].starts_with(|c: char| c.is_ascii_digit()))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn light_button(l: &Light, mut drawer: Signal<Option<String>>) -> Element {
    let k = l.key.clone();
    // the dot's shape says ok / idle / unknown; Agent faults name the PID and error;
    // other faults add a word and their age, and
    // a planned (not built) light says "planned" so it never reads like an outage
    let word = if l.tone.is_fault() && l.key == "drv:agent" {
        format!("{} · {}", l.tone.word(), l.summary)
    } else if l.tone.is_fault() {
        format!("{} {}", l.tone.word(), l.age.trim_end_matches(" ago"))
    } else if l.tone == Tone::Off {
        l.tone.word().into()
    } else {
        String::new()
    };
    rsx! {
        li { key: "{l.key}",
            button {
                r#type: "button",
                class: "light {l.tone.class()}",
                "data-key": "{l.key}",
                title: "{l.name}: {l.summary} ({l.age})",
                aria_haspopup: "dialog",
                onclick: move |_| drawer.set(Some(k.clone())),
                {dot(l.tone, false)}
                span { class: "lname", "{l.name}" }
                if !word.is_empty() {
                    span { class: "lword", "{word}" }
                }
            }
        }
    }
}

/// "L3 · unvrs-rs · Claude": level, project and harness, whatever of it is known.
fn worker_head(w: &Worker) -> String {
    [
        "L3",
        w.project.as_str(),
        diagram::harness_name(&w.harness).as_str(),
    ]
    .into_iter()
    .filter(|p| !p.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

/// One worker of the crew tree: two fixed lines (who, then what it is working on, one line
/// each, cut with an ellipsis) so a live update never moves the rows below it. `depth`
/// indents it under the worker that started it. Clicking opens its drawer.
fn worker_row(w: &Worker, depth: usize, mut drawer: Signal<Option<String>>) -> Element {
    let k = w.key.clone();
    let head = worker_head(w);
    let doing = w.doing.clone().unwrap_or_else(|| w.title.clone());
    rsx! {
        li { key: "{w.key}", class: "wk {w.tone.class()}", style: "--d:{depth}",
            button {
                r#type: "button",
                class: "crow wrow",
                "data-pid": "{w.pid}",
                "data-depth": "{depth}",
                aria_haspopup: "dialog",
                title: "PID {w.pid} · {head} · {doing}",
                onclick: move |_| drawer.set(Some(k.clone())),
                {dot(w.tone, w.pulse)}
                span { class: "cmain",
                    span { class: "wline",
                        span { class: "whead", span { class: "pid", "{w.pid}" } " {head}" }
                        if w.tone == Tone::Warn { span { class: "state", "{w.state}" } }
                        {model_chip(&w.harness, &w.model, &w.effort, w.tone == Tone::Idle)}
                        span { class: "age", "{w.age}" }
                    }
                    if let Some(routed) = &w.routed { span { class: "nroute", title: "{routed}", "{routed}" } }
                    span { class: "wdoing", "{doing}" }
                }
            }
        }
    }
}

fn seat_row(s: &Seat, mut drawer: Signal<Option<String>>) -> Element {
    let k = s.key.clone();
    let app = s.app.clone().unwrap_or_default();
    rsx! {
        button {
            r#type: "button",
            class: "crow srow {s.tone.class()}",
            "data-pid": "{s.pid}",
            aria_haspopup: "dialog",
            onclick: move |_| drawer.set(Some(k.clone())),
            {dot(s.tone, s.pulse)}
            span { class: "cmain",
                span { class: "cname",
                    strong { "{s.label}" }
                    if !app.is_empty() { span { class: "app", " {app}" } }
                }
                if let Some(routed) = &s.routed { span { class: "nroute", title: "{routed}", "{routed}" } }
                span { class: "cmeta",
                    span { class: "state", "{s.state}" }
                    if !s.vacant { {model_chip(&s.harness, &s.model, &s.effort, s.tone == Tone::Idle)} }
                    span { class: "age", "{s.age}" }
                }
            }
        }
    }
}

/// How many worker rows the Crew zone draws before "+N more · open diagram".
const TREE_ROWS: usize = 8;

/// A running or blocked worker in the tree and how deep it hangs.
struct Row<'a> {
    w: &'a Worker,
    depth: usize,
}

/// `w` and, under it, the workers it started (`kids` by parent PID), depth first. A PID
/// is drawn once, so a loop in the snapshot's parents cannot repeat rows.
fn nest<'a>(
    w: &'a Worker,
    depth: usize,
    kids: &HashMap<u64, Vec<&'a Worker>>,
    seen: &mut HashSet<u64>,
    out: &mut Vec<Row<'a>>,
) {
    if !seen.insert(w.pid) {
        return;
    }
    out.push(Row { w, depth });
    for k in kids.get(&w.pid).into_iter().flatten() {
        nest(k, depth + 1, kids, seen, out);
    }
}

/// The worker rows still allowed in the zone, and how many were cut.
struct Budget {
    left: usize,
    hidden: usize,
}

impl Budget {
    /// The first rows that still fit; the rest are counted in `hidden`.
    fn cap<'a>(&mut self, mut rows: Vec<Row<'a>>) -> Vec<Row<'a>> {
        let n = rows.len().min(self.left);
        self.left -= n;
        self.hidden += rows.len() - n;
        rows.truncate(n);
        rows
    }
}

/// A seat's running workers and what they started, within the budget.
fn under<'a>(
    s: &'a Seat,
    kids: &HashMap<u64, Vec<&'a Worker>>,
    seen: &mut HashSet<u64>,
    budget: &mut Budget,
) -> Vec<Row<'a>> {
    let mut rows = vec![];
    for w in &s.workers {
        nest(w, 0, kids, seen, &mut rows);
    }
    budget.cap(rows)
}

/// What hangs under one seat: its running workers (and what they started), then a quiet
/// line for the idle and the finished ones.
fn seat_tree(
    rows: &[Row],
    idle: usize,
    finished: usize,
    drawer: Signal<Option<String>>,
) -> Element {
    if rows.is_empty() && idle == 0 && finished == 0 {
        return rsx! {};
    }
    rsx! {
        ul { class: "workers",
            for r in rows.iter() {
                {worker_row(r.w, r.depth, drawer)}
            }
            if idle > 0 {
                li { class: "wk quiet", {dot(Tone::Idle, false)} " {idle} idle" }
            }
            if finished > 0 {
                li { class: "wk quiet fin", "✓ {finished} finished" }
            }
        }
    }
}

/// The CREW zone: every seat, and under it the running workers it started, recursively,
/// at most `TREE_ROWS` of them. Its heading and the seats open the crew diagram
/// (`diagram`, focused on the row clicked); a worker row opens its drawer.
fn crew_zone(
    c: &Crew,
    mut diagram: Signal<Option<String>>,
    drawer: Signal<Option<String>>,
) -> Element {
    let idle_n = c.idle.len();
    let idle_text = format!("{idle_n} lead{} idle", if idle_n == 1 { "" } else { "s" });
    // workers whose lead is not a seat hang under the worker that started them, else
    // under "no seat"
    let running: Vec<&Worker> = c.loose.iter().filter(|w| w.tone != Tone::Idle).collect();
    let mut kids: HashMap<u64, Vec<&Worker>> = HashMap::new();
    let drawn: HashSet<u64> =
        c.l1.iter()
            .chain(c.active.iter())
            .flat_map(|s| s.workers.iter().map(|w| w.pid))
            .chain(running.iter().map(|w| w.pid))
            .collect();
    for w in &running {
        if let Some(p) = w.parent.filter(|p| *p != w.pid && drawn.contains(p)) {
            kids.entry(p).or_default().push(w);
        }
    }
    let mut seen = HashSet::new();
    let mut budget = Budget {
        left: TREE_ROWS,
        hidden: 0,
    };
    let l1_rows =
        c.l1.as_ref()
            .map(|s| under(s, &kids, &mut seen, &mut budget))
            .unwrap_or_default();
    let lead_rows: Vec<Vec<Row>> = c
        .active
        .iter()
        .map(|s| under(s, &kids, &mut seen, &mut budget))
        .collect();
    // what is left: no parent in the tree, then anything only a loop of parents reaches
    let mut rest = vec![];
    for w in running
        .iter()
        .filter(|w| !w.parent.is_some_and(|p| p != w.pid && drawn.contains(&p)))
    {
        nest(w, 0, &kids, &mut seen, &mut rest);
    }
    for w in &running {
        nest(w, 0, &kids, &mut seen, &mut rest);
    }
    // the "no seat" group is only for what no seat or worker in the tree started
    let seatless = rest.len();
    let seatless_idle = c
        .loose
        .iter()
        .filter(|w| {
            w.tone == Tone::Idle && !w.parent.is_some_and(|p| p != w.pid && drawn.contains(&p))
        })
        .count();
    let rest = budget.cap(rest);
    let hidden = budget.hidden;
    rsx! {
        section { class: "zone z-crew", aria_labelledby: "h-crew",
            h2 { id: "h-crew",
                button {
                    r#type: "button",
                    class: "zlink",
                    id: "crew-open",
                    aria_haspopup: "dialog",
                    title: "Open the crew diagram",
                    onclick: move |_| diagram.set(Some(String::new())),
                    "Crew"
                    svg { class: "ic tree-ic", view_box: "0 0 16 16", "aria-hidden": "true",
                        rect { x: "5.5", y: "1.5", width: "5", height: "3.5" }
                        rect { x: "1.5", y: "11", width: "5", height: "3.5" }
                        rect { x: "9.5", y: "11", width: "5", height: "3.5" }
                        path { d: "M8 5v3M4 11V8h8v3" }
                    }
                    span { class: "zgo", "Diagram" }
                }
            }
            ul { class: "tree",
                if let Some(l1) = &c.l1 {
                    li { class: "seat lead",
                        {seat_row(l1, diagram)}
                        {seat_tree(&l1_rows, l1.idle_workers, l1.finished, drawer)}
                    }
                }
                for (s, rows) in c.active.iter().zip(lead_rows.iter()) {
                    li { key: "{s.key}", class: "seat lead",
                        {seat_row(s, diagram)}
                        {seat_tree(rows, s.idle_workers, s.finished, drawer)}
                    }
                }
                if seatless + seatless_idle > 0 {
                    li { class: "seat lead",
                        span { class: "crow quiet", title: "Workers without a seat in the snapshot", "no seat" }
                        {seat_tree(&rest, seatless_idle, 0, drawer)}
                    }
                }
                if hidden > 0 {
                    li { class: "seat",
                        button {
                            r#type: "button",
                            class: "crow more tree-more",
                            aria_haspopup: "dialog",
                            onclick: move |_| diagram.set(Some(String::new())),
                            span { class: "cname quiet", "+{hidden} more · open diagram" }
                        }
                    }
                }
                if idle_n > 0 {
                    li { class: "seat",
                        button {
                            r#type: "button",
                            class: "crow more idle-leads",
                            aria_haspopup: "dialog",
                            onclick: move |_| diagram.set(Some(String::new())),
                            {dot(Tone::Idle, false)}
                            span { class: "cname quiet", "{idle_text}" }
                        }
                    }
                }
                if c.l1.is_none() && c.active.is_empty() && idle_n == 0 {
                    li { class: "quiet", "No seats in the snapshot." }
                }
            }
        }
    }
}

/// One card of the crew diagram, placed by `diagram::layout`. Keyed by its seat or PID so
/// a live update slides the same element (CSS transitions on transform) instead of
/// redrawing it; a new card plays its entrance once. Seats and workers open their drawer.
fn node_card(n: &Node, focus: &str, mut drawer: Signal<Option<String>>) -> Element {
    let c = &n.card;
    let kind = match c.kind {
        Kind::L1 => "l1",
        Kind::L2 => "l2",
        Kind::L3 => "l3",
        Kind::Group => "group",
        Kind::Note => "note",
        Kind::Frame => "frame",
    };
    let mut class = format!("node k-{kind} {}", c.tone.class());
    if c.dim {
        class.push_str(" dim");
    }
    if !focus.is_empty() && c.key == focus {
        class.push_str(" focus");
    }
    let mut style = format!(
        "transform: translate({:.1}px, {:.1}px); width: {:.1}px; height: {:.1}px",
        n.x, n.y, n.w, n.h
    );
    // the route receipt shows as many lines as the layout made room for
    if let Some(r) = &c.routed {
        style.push_str(&format!("; --route-lines: {}", route_lines(r, n.w)));
    }
    if c.kind == Kind::Frame {
        return rsx! {
            div { key: "{c.key}", class: "dgm-frame", style: "{style}", "data-key": "{c.key}",
                span { class: "dgm-cap", "{c.doing}" }
            }
        };
    }
    if c.kind == Kind::Note {
        return rsx! {
            div { key: "{c.key}", class: "{class}", style: "{style}", "data-key": "{c.key}", "{c.doing}" }
        };
    }
    // "L2 · acme · Codex": the level as a tag, then the place and the harness
    let mut parts = c.head.split(" · ");
    let level = parts.next().unwrap_or_default().to_owned();
    let place = parts.next().unwrap_or_default().to_owned();
    let harness = parts.collect::<Vec<_>>().join(" · ");
    let pid = (c.pid > 0).then(|| c.pid.to_string());
    let tip = format!("{} · {} · {}", c.head, c.state, c.doing);
    let body = rsx! {
        span { class: "nl1",
            {dot(c.tone, c.pulse)}
            span { class: "nlvl", "{level}" }
            span { class: "nname",
                "{place}"
                if !harness.is_empty() { span { class: "nharn", " · {harness}" } }
            }
            if let Some(p) = &pid { span { class: "npid", "{p}" } }
        }
        span { class: "nl2",
            if !c.model.is_empty() { {model_chip(&c.harness, &c.model, &c.effort, c.dim)} }
            span { class: "nstate", "{c.state}" }
        }
        if let Some(routed) = &c.routed { span { class: "nroute", title: "{routed}", "{routed}" } }
        span { class: "ndoing", "{c.doing}" }
        if let Some(go) = &c.go_quote { span { class: "nalign ngo", title: "Go: {go}", "Go: {go}" } }
        if let Some(done) = &c.done_when { span { class: "nalign ndone", title: "Done when: {done}", "Done when: {done}" } }
    };
    match c.drawer.clone() {
        Some(k) => rsx! {
            button {
                key: "{c.key}",
                r#type: "button",
                class: "{class}",
                style: "{style}",
                "data-key": "{c.key}",
                title: "{tip}",
                aria_haspopup: "dialog",
                aria_label: "{tip}",
                onclick: move |_| drawer.set(Some(k.clone())),
                {body}
            }
        },
        None => rsx! {
            div { key: "{c.key}", class: "{class}", style: "{style}", "data-key": "{c.key}", title: "{tip}", {body} }
        },
    }
}

/// The crew diagram: a full-screen org chart over the cockpit. L1 on top, arrows down to
/// every L2 lead, and from each lead to its L3 workers; idle seats dimmed, finished
/// workers collapsed into a count. Laid out for the screen width `width`; rebuilt on every
/// snapshot, keyed so nothing flickers. Esc or × closes it; a card opens its drawer.
fn crew_diagram(
    c: &Crew,
    focus: &str,
    width: f64,
    mut diagram: Signal<Option<String>>,
    drawer: Signal<Option<String>>,
) -> Element {
    let d = diagram::of(c, width);
    let count = |k: Kind| d.nodes.iter().filter(|n| n.card.kind == k).count();
    let (leads, workers) = (count(Kind::L2), count(Kind::L3));
    let idle = d
        .nodes
        .iter()
        .filter(|n| matches!(n.card.kind, Kind::L2 | Kind::L3) && n.card.dim)
        .count();
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut sum = vec![plural(leads, "lead"), plural(workers, "worker")];
    if idle > 0 {
        sum.push(format!("{idle} idle"));
    }
    let sum = sum.join(" · ");
    let (w, h) = (format!("{:.0}", d.w), format!("{:.0}", d.h));
    rsx! {
        div {
            class: "dgm",
            id: "diagram",
            role: "dialog",
            aria_modal: "true",
            aria_labelledby: "dgm-title",
            "data-lead-w": "{d.lead_w:.0}",
            div { class: "dgm-bar",
                h2 { id: "dgm-title", "Crew" }
                span { class: "dgm-sum", "{sum}" }
                span { class: "dgm-legend", aria_hidden: "true",
                    span { {dot(Tone::Ok, false)} "working" }
                    span { {dot(Tone::Idle, false)} "idle" }
                    span { {dot(Tone::Warn, false)} "blocked" }
                }
                kbd { class: "tkey dgm-key", aria_hidden: "true", "Esc" }
                button {
                    r#type: "button",
                    class: "dclose dgm-close",
                    aria_label: "Close crew diagram (Esc)",
                    onclick: move |_| diagram.set(None),
                    "×"
                }
            }
            div { class: "dgm-scroll",
                if d.nodes.is_empty() {
                    p { class: "quiet dgm-empty", "No seats in the snapshot." }
                }
                div { class: "dgm-canvas", style: "width: {w}px; height: {h}px",
                    // frames first: behind the arrows and the cards
                    for n in d.nodes.iter().filter(|n| n.card.kind == Kind::Frame) {
                        {node_card(n, focus, drawer)}
                    }
                    svg {
                        class: "dgm-edges",
                        width: "{w}",
                        height: "{h}",
                        view_box: "0 0 {w} {h}",
                        "aria-hidden": "true",
                        defs {
                            marker { id: "dgm-arrow", view_box: "0 0 8 8", ref_x: "8", ref_y: "4",
                                marker_width: "7", marker_height: "7", marker_units: "userSpaceOnUse", orient: "auto",
                                path { class: "ahead", d: "M0 0L8 4L0 8z" }
                            }
                            marker { id: "dgm-arrow-dim", view_box: "0 0 8 8", ref_x: "8", ref_y: "4",
                                marker_width: "7", marker_height: "7", marker_units: "userSpaceOnUse", orient: "auto",
                                path { class: "ahead dim", d: "M0 0L8 4L0 8z" }
                            }
                        }
                        for e in d.edges.iter() {
                            path {
                                key: "{e.key}",
                                class: if e.dim { "edge dim" } else { "edge" },
                                d: "{e.d}",
                                "pathLength": "1",
                                marker_end: if e.dim { "url(#dgm-arrow-dim)" } else { "url(#dgm-arrow)" },
                            }
                        }
                    }
                    for n in d.nodes.iter().filter(|n| n.card.kind != Kind::Frame) {
                        {node_card(n, focus, drawer)}
                    }
                }
            }
        }
    }
}

fn gauge_row(g: &Gauge, mut drawer: Signal<Option<String>>) -> Element {
    let k = g.key.clone();
    let pct = g.fill.map(|f| format!("{:.1}%", f * 100.0));
    let age = short_age(&g.age);
    rsx! {
        li { key: "{g.key}",
            button {
                r#type: "button",
                class: "gauge {g.tone.class()}",
                "data-key": "{g.key}",
                title: "{g.label}: {g.value} · {g.source} · {g.age}",
                aria_haspopup: "dialog",
                onclick: move |_| drawer.set(Some(k.clone())),
                span { class: "glabel", "{g.label}" }
                span { class: if pct.is_some() { "bar" } else { "bar none" }, aria_hidden: "true",
                    if let Some(p) = &pct {
                        span { class: "fill", style: "width: {p}" }
                    }
                }
                span { class: "gval", "{g.value}" }
                span { class: if age.is_empty() { "gage unk" } else { "gage" }, title: "reading {g.age}", "{age}" }
            }
        }
    }
}

/// "2m ago" → "2m"; an unknown age → nothing (the tooltip says so in words).
fn short_age(age: &str) -> String {
    if age.starts_with("age unknown") || age == "no reading" || age.is_empty() {
        String::new()
    } else {
        age.trim_end_matches(" ago").to_owned()
    }
}

fn fuel_zone(f: &Fuel, mut drawer: Signal<Option<String>>) -> Element {
    rsx! {
        section { class: "zone z-fuel", aria_labelledby: "h-fuel",
            h2 { id: "h-fuel", "Fuel" }
            ul { class: "gauges",
                for g in f.quota.iter() {
                    {gauge_row(g, drawer)}
                }
                {gauge_row(&f.disk, drawer)}
            }
            div { class: "ws",
                button {
                    r#type: "button",
                    class: "wsum",
                    "data-key": "ws",
                    title: "Workspaces: {f.ws.value} · {f.ws.source} · {f.ws.age}",
                    aria_haspopup: "dialog",
                    onclick: move |_| drawer.set(Some("ws".into())),
                    span { class: "glabel", "Workspaces" }
                    span { class: "gval", "{f.ws.value}" }
                }

            }
        }
    }
}

/// Rows the Context zone shows before "+n more".
const CTX_SHOWN: usize = 6;

/// One context-ownership check: who (PID, harness, project), the verdict, then the task
/// or its first leak. Clicking opens what UNVRS holds of its context.
fn ctx_row(r: &CtxRow, mut drawer: Signal<Option<String>>) -> Element {
    let k = r.key.clone();
    let head = [
        r.project.as_str(),
        diagram::harness_name(&r.harness).as_str(),
    ]
    .into_iter()
    .filter(|p| !p.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let line = r.leak.clone().unwrap_or_else(|| r.title.clone());
    rsx! {
        li { key: "{r.key}", class: "wk ctx {r.tone.class()}",
            button {
                r#type: "button",
                class: "crow wrow",
                "data-key": "{r.key}",
                "data-pid": "{r.pid}",
                aria_haspopup: "dialog",
                title: "PID {r.pid} · {head} · {r.verdict} · {line}",
                onclick: move |_| drawer.set(Some(k.clone())),
                {dot(r.tone, false)}
                span { class: "cmain",
                    span { class: "wline",
                        span { class: "whead", span { class: "pid", "{r.pid}" } " {head}" }
                        span { class: "state cverdict", "{r.verdict}" }
                        span { class: "age", "{short_age(&r.age)}" }
                    }
                    span { class: "wdoing", "{line}" }
                }
            }
        }
    }
}

fn context_zone(c: &Context, mut drawer: Signal<Option<String>>) -> Element {
    let more = c.rows.len().saturating_sub(CTX_SHOWN);
    rsx! {
        section { class: "zone z-context", aria_labelledby: "h-context",
            h2 { id: "h-context", "Context",
                span { class: "csum", title: "context-ownership checks: passed · sent back at least once · blocked",
                    " {c.passed} passed · {c.bounced} bounced · {c.blocked} blocked" }
            }
            if !c.read {
                p { class: "quiet", "Task folders unreadable: context checks unknown." }
            } else if c.rows.is_empty() {
                p { class: "quiet", "No task has finished through the context-ownership check yet." }
            }
            ul { class: "tree ctxrows",
                for r in c.rows.iter().take(CTX_SHOWN) {
                    {ctx_row(r, drawer)}
                }
            }
            if more > 0 {
                button {
                    r#type: "button",
                    class: "more ctx-more",
                    aria_haspopup: "dialog",
                    onclick: move |_| drawer.set(Some("ctx:all".into())),
                    "+{more} more"
                }
            }
        }
    }
}

fn drawer_body(m: &Model, key: &str, mut drawer: Signal<Option<String>>) -> Element {
    if key == "ctx:all" {
        return rsx! {
            h2 { class: "dtitle", "Context ownership · {m.context.rows.len()}" }
            p { class: "reason", "Each task's handoff, newest first: UNVRS holds the copy of record of its brief, working record, deliverables and learnings, or the check names each leak." }
            ul { class: "tree ctxrows",
                for r in m.context.rows.iter() {
                    {ctx_row(r, drawer)}
                }
            }
        };
    }
    if key == "needs:all" {
        return rsx! {
            h2 { class: "dtitle", "Needs you · {m.needs.len()}" }
            p { class: "reason", "Most urgent first: blocking faults, then calls with a deadline, then decisions and proposals, newest first." }
            ul { class: "needs all",
                for n in m.needs.iter() {
                    {need_row(n, drawer)}
                }
            }
        };
    }
    if key == "needs:settled" {
        return rsx! {
            h2 { class: "dtitle", "Settled · {m.settled.len()}" }
            p { class: "reason", "Closed in the kernel journal, but still present in the snapshot. Each shows its closure evidence." }
            ul { class: "needs all",
                for n in m.settled.iter() {
                    {settled_row(n, drawer)}
                }
            }
        };
    }
    if key == "crew:idle" {
        return rsx! {
            h2 { class: "dtitle", "Idle leads · {m.crew.idle.len()}" }
            p { class: "reason", "Leads with nothing running. They wake when a call is answered or work arrives." }
            ul { class: "tree",
                for s in m.crew.idle.iter() {
                    li { key: "{s.key}", class: "seat", {seat_row(s, drawer)} }
                }
            }
        };
    }
    let Some(d) = m.detail(key) else {
        return rsx! {
            h2 { class: "dtitle", "Gone" }
            p { class: "reason", "This item is no longer in the snapshot." }
        };
    };
    let need = m.needs.iter().find(|n| n.key == key);
    rsx! {
        h2 { class: "dtitle",
            span { class: "dg {d.tone.class()}", aria_hidden: "true", "{glyph(d.tone)}" }
            "{d.title}"
            span { class: "dtone {d.tone.class()}", "{d.tone.word()}" }
        }
        p { class: "reason", "{d.reason}" }
        if let Some(n) = need {
            {answer_bar(n)}
            p { class: "route", "{n.route}" }
        }
        for (i, t) in d.text.iter().enumerate() {
            p { key: "t{i}", class: if i == 0 { "dtext first" } else { "dtext" }, "{t}" }
        }
        if !d.facts.is_empty() {
            dl { class: "facts",
                for (i, f) in d.facts.iter().enumerate() {
                    div { key: "f{i}",
                        dt { "{f.label}" }
                        dd {
                            span { class: if f.value.starts_with("unknown") { "v unk" } else { "v" }, "{f.value}" }
                            span { class: "src", "{f.source}" }
                        }
                    }
                }
            }
        }
        if key == "ws" {
                if !m.fuel.ws_rows.is_empty() {
                    ul { class: "wsrows",
                        for r in m.fuel.ws_rows.iter() {
                            {
                                let k = r.key.clone();
                                rsx! {
                                    li { key: "{r.key}",
                                        button {
                                            r#type: "button",
                                            class: "wsrow",
                                            aria_haspopup: "dialog",
                                            onclick: move |_| drawer.set(Some(k.clone())),
                                            {dot(if r.live { Tone::Ok } else { Tone::Idle }, false)}
                                            span { class: "wname", "{r.name}" }
                                            span { class: "wmeta", "{r.owner} · {r.merged} · {r.size}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

        }
    }
}

/// The preview page. `hidden` starts the cockpit folded away, `drawer_key` starts with
/// that drawer open and `diagram_key` with the crew diagram open, focused on that card
/// ("" for none) (tests).
#[component]
pub fn Preview(
    model: Model,
    #[props(default)] hidden: bool,
    #[props(default)] drawer_key: String,
    #[props(default)] diagram_key: Option<String>,
) -> Element {
    let mut cockpit = use_signal(|| Cockpit::default().with_hidden(hidden));
    let mut drawer = use_signal(|| (!drawer_key.is_empty()).then(|| drawer_key.clone()));
    let mut diagram = use_signal(|| diagram_key.clone());
    // the screen width the diagram is laid out for; preview.js reports it (and resizes)
    let mut vw = use_signal(|| 1280.0_f64);
    let mut sky_touch = use_signal(|| None::<(f64, f64)>);
    let m = model;
    let cp = cockpit();
    let toggle_label = if cp.shown() {
        "Hide cockpit"
    } else {
        "Show cockpit"
    };
    let shown_needs: Vec<&Need> = m.needs.iter().take(ZONE_MAX).collect();
    let more = m.needs.len().saturating_sub(ZONE_MAX);
    let workers = m.crew.active.iter().map(|s| s.workers.len()).sum::<usize>() + m.crew.loose.len();
    let calls = m.needs.iter().filter(|n| n.kind != NeedKind::Fault).count();
    let open = drawer();
    let tree = diagram();
    let fixture = m.fixture.clone().unwrap_or_else(|| "off".into());
    rsx! {
        div {
            class: if m.sim { "pv bridge live sim" } else { "pv bridge live" },
            id: "bridge",
            "data-cockpit": cp.state(),
            "data-sim": "{fixture}",
            onkeydown: move |e: KeyboardEvent| {
                if !e.modifiers().is_empty() {
                    return;
                }
                let key = e.key().to_string();
                if key == "Escape" && drawer.peek().is_some() {
                    drawer.set(None);
                } else if key == "Escape" && diagram.peek().is_some() {
                    diagram.set(None);
                } else if let Some(n) = cockpit().key(&key) {
                    cockpit.set(n);
                }
            },
            input {
                id: "dgm-vw",
                class: "sr-only",
                r#type: "text",
                tabindex: "-1",
                aria_hidden: "true",
                oninput: move |e: FormEvent| {
                    if let Ok(w) = e.value().trim().parse::<f64>()
                        && w >= 200.0
                        && (w - *vw.peek()).abs() >= 1.0
                    {
                        vw.set(w);
                    }
                },
            }
            div { id: "cosmos-signals", hidden: true,
                "data-workers": "{workers}", "data-calls": "{calls}", "data-move": "0" }
            // the whole screen is the window: the galaxy, dimmed behind the cockpit
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
                onpointerup: move |e: PointerEvent| {
                    if let Some((x0, y0)) = sky_touch.take() {
                        let p = e.client_coordinates();
                        if let Some(n) = cockpit().touch(p.x - x0, p.y - y0) {
                            cockpit.set(n);
                        }
                    }
                },
                onpointercancel: move |_| sky_touch.set(None),
            }
            button {
                r#type: "button",
                id: "cockpit-toggle",
                class: if cp.shown() { "cockpit-toggle" } else { "cockpit-toggle off" },
                aria_controls: "cockpit",
                aria_label: "{toggle_label}",
                aria_expanded: if cp.shown() { "true" } else { "false" },
                aria_keyshortcuts: "h",
                title: "{toggle_label} (H)",
                onclick: move |_| cockpit.set(cockpit().toggle()),
                span { class: "eye", aria_hidden: "true" }
                span { class: "tlabel", "{toggle_label}" }
                span { class: "tshort", aria_hidden: "true", if cp.shown() { "Hide" } else { "Show" } }
                kbd { class: "tkey", aria_hidden: "true", "H" }
            }
            // settings.js opens the Settings overlay from this button (and the , key); the
            // overlay lives outside the LiveView mount, so the cockpit never re-renders for it
            button {
                r#type: "button",
                id: "settings-open",
                class: "cockpit-toggle settings-open",
                aria_label: "Settings",
                aria_haspopup: "dialog",
                aria_keyshortcuts: ",",
                title: "Settings (,)",
                svg {
                    class: "gear",
                    width: "14",
                    height: "14",
                    view_box: "0 0 16 16",
                    "aria-hidden": "true",
                    path {
                        d: "M6.6 1h2.8l.4 2 1.3.7 1.9-.7 1.4 2.4-1.5 1.4v1.4l1.5 1.4-1.4 2.4-1.9-.7-1.3.7-.4 2H6.6l-.4-2-1.3-.7-1.9.7L1.6 10.8l1.5-1.4V8l-1.5-1.4L3 4.2l1.9.7 1.3-.7z",
                    }
                    circle { cx: "8", cy: "8", r: "2.2" }
                }
            }
            div { class: "sr-only", id: "cockpit-status", role: "status",
                if !cp.shown() { "Cockpit hidden. Press H or the Show cockpit button to bring it back." }
            }
            div {
                class: "cockpit",
                id: "cockpit",
                role: "region",
                aria_label: "Observatory",
                aria_hidden: if cp.shown() { "false" } else { "true" },
                "inert": (!cp.shown()).then_some("true"),
                if m.sim {
                    div { class: "simbar", id: "sim-label", role: "note", title: "Fixture data (?sim={fixture}), not your system",
                        strong { "SIMULATED" }
                        span { class: "sfx", " {fixture}" }
                        a { href: "/preview", "real" }
                    }
                }
                header { class: "status {m.status_tone.class()}", id: "status",
                    span { class: "sg", aria_hidden: "true", "{glyph(m.status_tone)}" }
                    h1 { class: "stext", "{m.status}" }
                    span { class: "sage {m.kernel.tone.class()}", title: "kernel heartbeat {m.kernel.age}",
                        span { class: "hb", aria_hidden: "true" }
                        "{short_age(&m.kernel.age)}"
                    }
                }
                div { class: "zones",
                  div { class: "col col-a",
                    section { class: "zone z-needs", aria_labelledby: "h-needs",
                        h2 { id: "h-needs", "Needs you"
                            if !m.needs.is_empty() { span { class: "count", "{m.needs.len()}" } }
                        }
                        if m.needs.is_empty() {
                            p { class: "quiet", "Nothing needs you." }
                        }
                        ul { class: "needs",
                            for n in shown_needs.iter() {
                                {need_row(n, drawer)}
                            }
                        }
                        if more > 0 {
                            button {
                                r#type: "button",
                                class: "more needs-more",
                                aria_haspopup: "dialog",
                                onclick: move |_| drawer.set(Some("needs:all".into())),
                                "+{more} more"
                            }
                        }
                        if !m.settled.is_empty() {
                            button {
                                r#type: "button",
                                class: "more settled-more",
                                aria_haspopup: "dialog",
                                title: "Calls closed in the kernel journal, still present in the snapshot",
                                onclick: move |_| drawer.set(Some("needs:settled".into())),
                                "✓ {m.settled.len()} settled"
                            }
                        }
                    }
                    {crew_zone(&m.crew, diagram, drawer)}
                  }
                  div { class: "col col-b",
                    section { class: "zone z-alive", aria_labelledby: "h-alive",
                        h2 { id: "h-alive", "Alive" }
                        button {
                            r#type: "button",
                            class: "krow {m.kernel.tone.class()}",
                            "data-key": "kernel",
                            aria_haspopup: "dialog",
                            onclick: move |_| drawer.set(Some("kernel".into())),
                            {dot(m.kernel.tone, false)}
                            span { class: "lname", "Kernel" }
                            span { class: "ksum", title: "{m.kernel.summary}", {kernel_brief(&m.kernel.summary)} }
                            {spark(&m.activity)}
                        }
                        div { class: "lrow",
                            span { class: "lhead", "Drivers" }
                            ul { class: "lights", aria_label: "Drivers",
                                for l in m.drivers.iter() {
                                    {light_button(l, drawer)}
                                }
                            }
                        }
                        div { class: "lrow",
                            span { class: "lhead", "Surfaces" }
                            ul { class: "lights", aria_label: "Surfaces",
                                for l in m.surfaces.iter() {
                                    {light_button(l, drawer)}
                                }
                            }
                        }
                    }
                    {context_zone(&m.context, drawer)}
                    {fuel_zone(&m.fuel, drawer)}
                  }
                }
                p { class: "sr-only", "Every row, light and gauge opens a drawer with its source; the crew opens a diagram. H hides the cockpit." }
                if let Some(f) = &tree {
                    {crew_diagram(&m.crew, f, vw(), diagram, drawer)}
                }
                if let Some(k) = open {
                    aside {
                        class: "drawer",
                        id: "drawer",
                        role: "dialog",
                        aria_modal: "false",
                        aria_label: "Detail",
                        button {
                            r#type: "button",
                            class: "dclose",
                            aria_label: "Close detail (Esc)",
                            onclick: move |_| drawer.set(None),
                            "×"
                        }
                        {drawer_body(&m, &k, drawer)}
                        if m.sim {
                            p { class: "simnote", "SIMULATED fixture data." }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_chips_say_only_what_was_reported_and_never_show_a_question_mark() {
        use super::chip_text;
        assert_eq!(
            chip_text("claude-opus-5-5", "high").as_deref(),
            Some("opus-5-5 · high")
        );
        assert_eq!(chip_text("gpt-6-sol", "unknown").as_deref(), Some("6-sol"));
        assert_eq!(chip_text("unknown", "high").as_deref(), Some("high effort"));
        assert_eq!(chip_text("unknown", "unknown"), None);
        assert_eq!(chip_text("", ""), None);
    }

    #[test]
    fn compact_labels_preserve_unknowns_and_meaning() {
        assert_eq!(super::short_age("2m ago"), "2m");
        assert_eq!(super::short_age("age unknown"), "");
        assert_eq!(super::short_age("no reading"), "");
        assert_eq!(
            super::kernel_brief("v0.8.0 · up 2h · heartbeat 3s ago"),
            "up 2h"
        );
        assert_eq!(super::kernel_brief("unknown"), "unknown");
    }
}
