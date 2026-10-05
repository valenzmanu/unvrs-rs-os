//! The crew diagram (PID 132, rows by PID 144): the crew as an org chart, one level per
//! row. L1 on top; the workers L1 dispatched itself in a framed band right under it; then
//! every L2 lead side by side in one row, and each lead's L3 workers stacked under it on a
//! spine. Pure layout: cards get pixel boxes, edges get orthogonal SVG paths; the view only
//! draws them.
//!
//! The lead row never wraps: with many leads the cards narrow down to `MIN_W`, and past
//! that the canvas grows wider than the screen and scrolls sideways. L1's own workers sit
//! left of the trunk that runs from L1 down to the leads, so no line crosses a card.
//! Workers whose lead is not in the snapshot hang under a "no seat" group in the lead row.
use super::model::{Crew, Seat, Tone, Worker};
use serde::Serialize;

/// A lead card at full width (and L1's card).
pub const CARD_W: f64 = 236.0;
/// The narrowest a lead card gets before the lead row scrolls sideways.
pub const MIN_W: f64 = 168.0;
/// A card's height without a route line.
pub const CARD_H: f64 = 74.0;
/// One line of a route receipt ("routed: judge · seat judgment; default reasoning"),
/// which wraps instead of being cut off: up to `ROUTE_MAX` lines, the card taller for them.
pub const ROUTE_LINE: f64 = 14.0;
pub const ROUTE_MAX: usize = 3;
/// A generous average glyph width of the 11 px route text, word breaks included.
const ROUTE_EM: f64 = 6.6;
/// A card's horizontal padding and borders.
const CARD_INSET: f64 = 23.0;
/// How far a worker card sits right of its parent's spine column (the spine runs in it).
pub const INDENT: f64 = 22.0;
pub const GAP_X: f64 = 18.0;
pub const GAP_Y: f64 = 10.0;
/// From a lead's bottom to its first worker.
pub const DROP: f64 = 16.0;
/// Room above a row for its bus.
pub const BUS: f64 = 34.0;
pub const PAD: f64 = 24.0;
pub const NOTE_H: f64 = 22.0;
/// The clear lane around the trunk from L1 down to the lead row.
pub const LANE: f64 = 36.0;
/// Inner margin of the frame round L1's own workers, and its caption line.
pub const FRAME_PAD: f64 = 10.0;
pub const CAPTION_H: f64 = 18.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Kind {
    L1,
    L2,
    L3,
    /// A placeholder parent ("no seat").
    Group,
    /// A count line under a column ("✓ 12 finished by L1").
    Note,
    /// The dashed frame round L1's own workers, captioned; drawn behind the cards.
    Frame,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Card {
    pub routed: Option<String>,
    pub go_quote: Option<String>,
    pub done_when: Option<String>,
    pub parent: Option<u64>,
    /// Stable across snapshots (the DOM key): nodes keep their element and slide.
    pub key: String,
    /// The drawer this card opens, if any.
    pub drawer: Option<String>,
    pub kind: Kind,
    pub pid: u64,
    /// "L2 · acme · Codex"
    pub head: String,
    pub harness: String,
    pub model: String,
    pub effort: String,
    pub tone: Tone,
    pub pulse: bool,
    pub state: String,
    /// One line: what it is working on.
    pub doing: String,
    /// Idle or vacant: drawn dimmed.
    pub dim: bool,
}

/// One parent and what hangs under it.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub head: Option<Card>,
    pub kids: Vec<Card>,
    pub note: Option<Card>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Node {
    pub card: Card,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Edge {
    /// "e:" + the child's key.
    pub key: String,
    pub from: String,
    pub to: String,
    /// SVG path data, orthogonal segments ending at the child's edge.
    pub d: String,
    pub dim: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Diagram {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub w: f64,
    pub h: f64,
    /// The width of a lead card in the lead row (`MIN_W`..=`CARD_W`).
    pub lead_w: f64,
}

impl Diagram {
    pub fn node(&self, key: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.card.key == key)
    }
}

/// The diagram's cards before layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cards {
    pub l1: Option<Card>,
    /// The workers L1 dispatched itself (no head card), and L1's finished count.
    pub own: Option<Column>,
    /// One column per lead, then the "no seat" group.
    pub leads: Vec<Column>,
}

/// "claude" → "Claude"; unknown → "".
pub fn harness_name(h: &str) -> String {
    match h {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "" | "harness unknown" | "unknown" => String::new(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}

fn head(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn is_l1(s: &Seat) -> bool {
    s.rank == 1 || s.label == "L1"
}

/// Whose a seat's finished workers are: "L1", "L2 acme".
fn owner(s: &Seat) -> String {
    if is_l1(s) {
        "L1".into()
    } else {
        format!(
            "L2 {}",
            s.project
                .clone()
                .unwrap_or_else(|| format!("PID {}", s.pid))
        )
    }
}

fn seat_card(s: &Seat) -> Card {
    let (kind, level, place) = if is_l1(s) {
        (Kind::L1, "L1", "UNVRS".to_owned())
    } else {
        (
            Kind::L2,
            "L2",
            s.project
                .clone()
                .unwrap_or_else(|| format!("PID {}", s.pid)),
        )
    };
    let harness = harness_name(&s.harness);
    let doing = s.doing.clone().unwrap_or_else(|| {
        if s.vacant {
            "vacant · no thread bound".into()
        } else if s.tone == Tone::Idle {
            "nothing running".into()
        } else {
            "no brief yet".into()
        }
    });
    Card {
        routed: s.routed.clone(),
        go_quote: None,
        done_when: None,
        parent: None,
        key: s.key.clone(),
        drawer: Some(s.key.clone()),
        kind,
        pid: s.pid,
        head: head(&[level, &place, &harness]),
        harness: s.harness.clone(),
        // a vacant lead runs nothing: no model chip at all
        model: if s.vacant {
            String::new()
        } else {
            s.model.clone()
        },
        effort: if s.vacant {
            String::new()
        } else {
            s.effort.clone()
        },
        tone: s.tone,
        pulse: s.pulse,
        state: s.state.clone(),
        doing,
        dim: s.tone == Tone::Idle && s.workers.is_empty(),
    }
}

fn worker_card(w: &Worker) -> Card {
    Card {
        routed: w.routed.clone(),
        go_quote: w.go_quote.clone(),
        done_when: w.done_when.clone(),
        parent: w.parent,
        key: w.key.clone(),
        drawer: Some(w.key.clone()),
        kind: Kind::L3,
        pid: w.pid,
        head: head(&["L3", &w.project, &harness_name(&w.harness)]),
        harness: w.harness.clone(),
        model: w.model.clone(),
        effort: w.effort.clone(),
        tone: w.tone,
        pulse: w.pulse,
        state: w.state.clone(),
        doing: w.doing.clone().unwrap_or_else(|| w.title.clone()),
        dim: w.tone == Tone::Idle,
    }
}

fn plain(key: String, kind: Kind, text: String) -> Card {
    Card {
        routed: None,
        go_quote: None,
        done_when: None,
        parent: None,
        key,
        drawer: None,
        kind,
        pid: 0,
        head: String::new(),
        harness: String::new(),
        model: String::new(),
        effort: String::new(),
        tone: Tone::Idle,
        pulse: false,
        state: String::new(),
        doing: text,
        dim: true,
    }
}

/// "✓ 12 finished by L2 acme" under its parent; none when nothing finished.
fn note(key: &str, n: usize, by: &str) -> Option<Card> {
    (n > 0).then(|| {
        plain(
            format!("note:{key}"),
            Kind::Note,
            format!("✓ {n} finished by {by}"),
        )
    })
}

fn push_worker(w: &Worker, all: &[&Worker], seen: &mut Vec<u64>, cards: &mut Vec<Card>) {
    if seen.contains(&w.pid) {
        return;
    }
    seen.push(w.pid);
    cards.push(worker_card(w));
    // ponytail: crew-sized scans; index by parent if snapshots grow large.
    for child in all.iter().filter(|c| c.parent == Some(w.pid)) {
        push_worker(child, all, seen, cards);
    }
}

fn column(s: &Seat, all: &[&Worker], seen: &mut Vec<u64>) -> Column {
    let mut kids = vec![];
    for w in s.workers.iter().chain(s.resting.iter()) {
        push_worker(w, all, seen, &mut kids);
    }
    Column {
        head: Some(seat_card(s)),
        kids,
        note: note(&s.key, s.finished, &owner(s)),
    }
}

/// The diagram's cards from the crew: L1 and its own workers, then one column per lead
/// (running leads first, then idle ones), and workers without a seat.
pub fn crew_cards(c: &Crew) -> Cards {
    let all: Vec<&Worker> =
        c.l1.iter()
            .chain(c.active.iter())
            .chain(c.idle.iter())
            .flat_map(|s| s.workers.iter().chain(s.resting.iter()))
            .chain(c.loose.iter())
            .collect();
    let mut seen = vec![];
    let l1 = c.l1.as_ref().map(seat_card);
    let own =
        c.l1.as_ref()
            .filter(|s| !s.workers.is_empty() || !s.resting.is_empty() || s.finished > 0)
            .map(|s| Column {
                head: None,
                ..column(s, &all, &mut seen)
            });
    let mut leads: Vec<Column> = c
        .active
        .iter()
        .chain(c.idle.iter())
        .map(|s| column(s, &all, &mut seen))
        .collect();
    let mut loose = vec![];
    for w in c
        .loose
        .iter()
        .filter(|w| !all.iter().any(|p| w.parent == Some(p.pid)))
    {
        push_worker(w, &all, &mut seen, &mut loose);
    }
    for w in &c.loose {
        push_worker(w, &all, &mut seen, &mut loose);
    }
    if !loose.is_empty() {
        leads.push(Column {
            head: Some(Card {
                routed: None,
                key: "group:loose".into(),
                head: "No seat".into(),
                tone: Tone::Unknown,
                state: "lead not in the snapshot".into(),
                ..plain(
                    "group:loose".into(),
                    Kind::Group,
                    format!(
                        "{} worker{}",
                        loose.len(),
                        if loose.len() == 1 { "" } else { "s" }
                    ),
                )
            }),
            kids: loose,
            note: None,
        });
    }
    Cards { l1, own, leads }
}

/// The lead card width for `n` leads on a screen `width` pixels wide: full width when
/// they fit, narrower down to `MIN_W`, never less (the row then scrolls sideways).
pub fn lead_width(width: f64, n: usize) -> f64 {
    if n == 0 {
        return CARD_W;
    }
    let fit = (width - 2.0 * PAD - (n - 1) as f64 * GAP_X) / n as f64;
    // whole pixels, so the paths (one decimal) land exactly on the card edges
    fit.floor().clamp(MIN_W, CARD_W)
}

/// How many lines a route receipt needs on a card `w` pixels wide (at most `ROUTE_MAX`;
/// past that the card's tooltip and drawer carry the whole reason).
pub fn route_lines(text: &str, w: f64) -> usize {
    let need = text.chars().count() as f64 * ROUTE_EM / (w - CARD_INSET).max(1.0);
    (need.ceil() as usize).clamp(1, ROUTE_MAX)
}

/// A card's height at width `w`: taller by the lines of its route receipt, if any.
pub fn card_h(c: &Card, w: f64) -> f64 {
    let base = match &c.routed {
        Some(r) => CARD_H + 2.0 + route_lines(r, w) as f64 * ROUTE_LINE,
        None => CARD_H,
    };
    base + 18.0 * (usize::from(c.go_quote.is_some()) + usize::from(c.done_when.is_some())) as f64
}

fn f(v: f64) -> String {
    format!("{v:.1}")
}

/// A column's worker stack (and its count note) from `y` down, the spine at `spine`
/// (cards start `INDENT / 2` right of it, `kw` wide). `feed(mid)` is the path to a
/// worker's middle height up to the spine. Returns the stack's bottom.
#[allow(clippy::too_many_arguments)]
fn stack(
    col: &Column,
    parent: &str,
    spine: f64,
    kw: f64,
    mut y: f64,
    feed: &dyn Fn(f64) -> String,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> f64 {
    let kx = spine + INDENT / 2.0;
    for kid in &col.kids {
        let worker_parent = kid.parent.and_then(|pid| {
            nodes
                .iter()
                .find(|n| n.card.kind == Kind::L3 && n.card.pid == pid)
        });
        let (x, w, from, path) = match worker_parent {
            Some(p) => {
                // ponytail: stop indenting at minimum width; expand the canvas for deeper visual nesting.
                let indent = (p.w - (MIN_W - INDENT)).clamp(0.0, INDENT / 2.0);
                let x = p.x + indent;
                (
                    x,
                    p.w - indent,
                    p.card.key.clone(),
                    format!(
                        "M{} {}V{}H{}V{}",
                        f(p.x + p.w / 2.0),
                        f(p.y + p.h),
                        f(p.y + p.h + GAP_Y / 2.0),
                        f(x - INDENT / 2.0),
                        f(y + card_h(kid, p.w - indent) / 2.0)
                    ),
                )
            }
            None => (kx, kw, parent.to_owned(), feed(y + card_h(kid, kw) / 2.0)),
        };
        let h = card_h(kid, w);
        nodes.push(Node {
            card: kid.clone(),
            x,
            y,
            w,
            h,
        });
        edges.push(Edge {
            key: format!("e:{}", kid.key),
            from,
            to: kid.key.clone(),
            d: format!("{path}H{}", f(x)),
            dim: kid.dim,
        });
        y += h + GAP_Y;
    }
    if let Some(n) = &col.note {
        nodes.push(Node {
            card: n.clone(),
            x: kx,
            y,
            w: kw,
            h: NOTE_H,
        });
        y += NOTE_H + GAP_Y;
    }
    y - GAP_Y
}

/// Places the cards for a screen `width` pixels wide.
pub fn layout(cards: Cards, width: f64) -> Diagram {
    let Cards { l1, own, leads } = cards;
    let n = leads.len();
    let lead_w = lead_width(width, n);
    let row_w = n as f64 * lead_w + n.saturating_sub(1) as f64 * GAP_X;
    // L1's own workers: a frame left of the trunk lane, as wide as one lead column
    let frame_w = 2.0 * FRAME_PAD + CARD_W;
    let own = own.filter(|_| l1.is_some());
    let half = if own.is_some() {
        frame_w + LANE / 2.0
    } else {
        0.0
    };
    let content_w = row_w.max(CARD_W).max(2.0 * half);
    let w = content_w + 2.0 * PAD;
    let cx = PAD + content_w / 2.0;
    let mut nodes = vec![];
    let mut edges = vec![];
    let mut y = PAD;
    // L1 on top, centred; its bottom middle is where every arrow from it starts
    let l1_key = l1.map(|card| {
        let h = card_h(&card, CARD_W);
        nodes.push(Node {
            card: card.clone(),
            x: cx - CARD_W / 2.0,
            y,
            w: CARD_W,
            h,
        });
        y += h;
        card.key
    });
    let l1_bottom = y;
    let mut bottom = y;
    if let (Some(k), Some(col)) = (&l1_key, &own) {
        let fx = cx - LANE / 2.0 - frame_w;
        let fy = y + BUS;
        let bus = y + BUS / 2.0;
        let spine = fx + FRAME_PAD + INDENT / 2.0;
        let feed = |mid: f64| {
            format!(
                "M{} {}V{}H{}V{}",
                f(cx),
                f(l1_bottom),
                f(bus),
                f(spine),
                f(mid)
            )
        };
        let end = stack(
            col,
            k,
            spine,
            CARD_W - INDENT,
            fy + FRAME_PAD,
            &feed,
            &mut nodes,
            &mut edges,
        );
        let fh = end + FRAME_PAD + CAPTION_H - fy;
        nodes.push(Node {
            card: plain("frame:l1".into(), Kind::Frame, "L1's own workers".into()),
            x: fx,
            y: fy,
            w: frame_w,
            h: fh,
        });
        y = fy + fh;
        bottom = y;
    }
    if n > 0 {
        let bus = y + BUS / 2.0;
        let top = y + BUS;
        let x0 = PAD + (content_w - row_w) / 2.0;
        for (j, col) in leads.iter().enumerate() {
            let lx = x0 + j as f64 * (lead_w + GAP_X);
            let Some(card) = &col.head else { continue };
            let h = card_h(card, lead_w);
            nodes.push(Node {
                card: card.clone(),
                x: lx,
                y: top,
                w: lead_w,
                h,
            });
            if let Some(k) = &l1_key {
                edges.push(Edge {
                    key: format!("e:{}", card.key),
                    from: k.clone(),
                    to: card.key.clone(),
                    d: format!(
                        "M{} {}V{}H{}V{}",
                        f(cx),
                        f(l1_bottom),
                        f(bus),
                        f(lx + lead_w / 2.0),
                        f(top)
                    ),
                    dim: card.dim,
                });
            }
            let spine = lx + INDENT / 2.0;
            let from = top + h;
            let feed = |mid: f64| format!("M{} {}V{}", f(spine), f(from), f(mid));
            let end = stack(
                col,
                &card.key,
                spine,
                lead_w - INDENT,
                from + DROP,
                &feed,
                &mut nodes,
                &mut edges,
            );
            bottom = bottom.max(end).max(from);
        }
    }
    Diagram {
        nodes,
        edges,
        w,
        h: bottom.max(PAD + CARD_H) + PAD,
        lead_w,
    }
}

/// The whole diagram for the crew at a screen width.
pub fn of(c: &Crew, width: f64) -> Diagram {
    layout(crew_cards(c), width)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::preview::model::{Inputs, Model};
    use crate::probe::Host;
    use serde_json::{Value, json};

    pub(crate) const NOW: u64 = 1_790_000_000_000;

    /// A snapshot: L1 (PID 1), leads PID 2.. (`leads` of them, the second one on Codex),
    /// and `n` workers dealt round-robin over the leads, every fourth one to L1 itself.
    fn snap(leads: u64, n: u64) -> Value {
        let mut seats = vec![
            json!({"rank": 1, "pid": 1, "project": null, "state": "live",
            "occupied_by": {"app": "claude-desktop", "harness": "claude"}, "model": "claude-opus-5-5", "effort": "high",
            "now": "steering the crew"}),
        ];
        for i in 0..leads {
            let pid = 2 + i;
            let (h, st) = if i == 1 {
                ("codex", "live")
            } else {
                ("claude", "free")
            };
            seats.push(json!({"rank": 2, "pid": pid, "project": format!("p{pid}"), "state": st,
                "occupied_by": if st == "live" { json!({"app": "codex-app", "harness": h}) } else { Value::Null }}));
        }
        let workers: Vec<Value> = (0..n)
            .map(|k| {
                let parent = if k % 4 == 3 { 1 } else { 2 + k % leads.max(1) };
                json!({"pid": 100 + k, "parent": parent, "project": format!("p{parent}"), "intent": format!("task {k}"),
                    "harness": "claude", "elapsed_s": 60, "state": if k % 5 == 4 { "idle" } else { "working" },
                    "now": format!("step {k}")})
            })
            .collect();
        json!({"at": NOW, "kernel": {"version": "t", "os_pid": 1, "uptime_s": 1, "home": "/nowhere"},
            "calls": [], "seats": seats, "workers": workers, "quota": []})
    }

    fn crew(sn: &Value) -> Crew {
        Model::build(Inputs {
            snap: sn,
            host: &Host::default(),
            now_ms: NOW,
            sim: false,
        })
        .crew
    }

    fn overlaps(a: &Node, b: &Node) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    /// The points an edge's path visits (M x y, then V y / H x).
    fn points(d: &str) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = vec![];
        let mut rest = d;
        while let Some(c) = rest.chars().next() {
            let end = rest[1..]
                .find(['M', 'V', 'H'])
                .map_or(rest.len(), |i| i + 1);
            let nums: Vec<f64> = rest[1..end]
                .split(' ')
                .map(|t| t.parse().unwrap())
                .collect();
            let last = out.last().copied().unwrap_or((0.0, 0.0));
            out.push(match c {
                'M' => (nums[0], nums[1]),
                'V' => (last.0, nums[0]),
                'H' => (nums[0], last.1),
                _ => panic!("{d}"),
            });
            rest = &rest[end..];
        }
        out
    }

    /// Does the axis-aligned segment a→b pass through the inside of the node?
    fn crosses(a: (f64, f64), b: (f64, f64), n: &Node) -> bool {
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        x1 > n.x + 1.0 && x0 < n.x + n.w - 1.0 && y1 > n.y + 1.0 && y0 < n.y + n.h - 1.0
    }

    fn inside(a: &Node, f: &Node) -> bool {
        a.x >= f.x && a.y >= f.y && a.x + a.w <= f.x + f.w && a.y + a.h <= f.y + f.h
    }

    fn owner_key<'a>(d: &'a Diagram, mut key: &'a str) -> Option<&'a str> {
        for _ in 0..d.nodes.len() {
            let edge = d.edges.iter().find(|e| e.to == key)?;
            if d.node(&edge.from)?.card.kind != Kind::L3 {
                return Some(&edge.from);
            }
            key = &edge.from;
        }
        None
    }

    pub(crate) fn check(d: &Diagram, what: &str) {
        for (i, a) in d.nodes.iter().enumerate() {
            assert!(
                a.x >= 0.0 && a.y >= 0.0 && a.x + a.w <= d.w && a.y + a.h <= d.h,
                "{what}: {} out of bounds",
                a.card.key
            );
            for b in &d.nodes[i + 1..] {
                // a frame holds only L1's own workers and note, and touches nothing else
                let framed = |f: &Node, n: &Node| {
                    f.card.kind == Kind::Frame
                        && (inside(n, f) || !overlaps(f, n))
                        && (inside(n, f)
                            == (n.card.kind == Kind::L3
                                && owner_key(d, &n.card.key) == Some("seat:1"))
                            || n.card.key == "note:seat:1")
                };
                assert!(
                    framed(a, b) || framed(b, a) || !overlaps(a, b),
                    "{what}: {} overlaps {}",
                    a.card.key,
                    b.card.key
                );
            }
        }
        let mut keys: Vec<&str> = d.nodes.iter().map(|n| n.card.key.as_str()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), d.nodes.len(), "{what}: keys are unique");
        // one level per row: every lead on the same line, below L1 and below L1's frame
        let leads: Vec<&Node> = d
            .nodes
            .iter()
            .filter(|n| matches!(n.card.kind, Kind::L2 | Kind::Group))
            .collect();
        if let Some(first) = leads.first() {
            assert!(
                leads.iter().all(|n| n.y == first.y),
                "{what}: leads in one row"
            );
            for n in &d.nodes {
                if matches!(n.card.kind, Kind::L1 | Kind::Frame) {
                    assert!(
                        n.y + n.h + BUS <= first.y,
                        "{what}: {} above the lead row",
                        n.card.key
                    );
                }
                if n.card.kind == Kind::L3 {
                    let parent = &d.edges.iter().find(|e| e.to == n.card.key).unwrap().from;
                    let p = d.node(parent).unwrap();
                    assert!(n.y >= p.y + p.h, "{what}: {} below its parent", n.card.key);
                    if owner_key(d, &n.card.key) == Some("seat:1") {
                        assert!(
                            n.y + n.h < first.y,
                            "{what}: L1's {} above the leads",
                            n.card.key
                        );
                    } else {
                        assert!(
                            n.y > first.y + CARD_H,
                            "{what}: {} under the lead row",
                            n.card.key
                        );
                    }
                }
            }
        }
        for e in &d.edges {
            let pts = points(&e.d);
            let (from, to) = (d.node(&e.from).unwrap(), d.node(&e.to).unwrap());
            // starts on the parent's bottom edge, ends on the child's top or left edge
            assert_eq!(
                pts[0].1,
                from.y + from.h,
                "{what}: {} leaves its parent's bottom",
                e.key
            );
            assert!(pts[0].0 > from.x && pts[0].0 < from.x + from.w);
            let end = *pts.last().unwrap();
            let on_top = end.1 == to.y && end.0 > to.x && end.0 < to.x + to.w;
            let on_left = end.0 == to.x && end.1 > to.y && end.1 < to.y + to.h;
            assert!(
                on_top || on_left,
                "{what}: {} ends on its child {end:?}",
                e.key
            );
            // never runs left of the canvas edge (no loop round the side)
            assert!(
                pts.iter().all(|p| p.0 >= PAD),
                "{what}: {} loops round the edge",
                e.key
            );
            // and never runs through another card
            for n in &d.nodes {
                if n.card.key != e.from && n.card.key != e.to && n.card.kind != Kind::Frame {
                    for s in pts.windows(2) {
                        assert!(
                            !crosses(s[0], s[1], n),
                            "{what}: {} runs through {}",
                            e.key,
                            n.card.key
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn worker_children_follow_their_parent_and_keep_admission_lines() {
        let mut sn = snap(2, 4);
        // Reverse order deliberately: the snapshot need not list parents before children.
        sn["workers"].as_array_mut().unwrap().extend([
            json!({"pid":301,"parent":300,"project":"p2","state":"idle","intent":"grandchild"}),
            json!({"pid":201,"parent":200,"project":"p1","state":"held","intent":"grandchild"}),
            json!({"pid":300,"parent":100,"project":"p2","state":"working","intent":"child"}),
            json!({"pid":200,"parent":103,"project":"p1","state":"working","intent":"child",
                "go_quote":"Ok, go", "done_when":"report cites sources"}),
        ]);
        for width in [776.0, 1440.0] {
            let d = of(&crew(&sn), width);
            check(&d, "worker children");
            assert!(!d.nodes.iter().any(|n| n.card.kind == Kind::Group));
            for (parent, child) in [
                ("pid:103", "pid:200"),
                ("pid:200", "pid:201"),
                ("pid:100", "pid:300"),
                ("pid:300", "pid:301"),
            ] {
                assert!(d.edges.iter().any(|e| e.from == parent && e.to == child));
            }
            let child = d.node("pid:200").unwrap();
            assert_eq!(child.card.go_quote.as_deref(), Some("Ok, go"));
            assert_eq!(
                child.card.done_when.as_deref(),
                Some("report cites sources")
            );
            assert_eq!(child.h, CARD_H + 36.0);
        }
        sn["workers"].as_array_mut().unwrap().extend([
            json!({"pid":400,"parent":401,"project":"p2","state":"working"}),
            json!({"pid":401,"parent":400,"project":"p2","state":"working"}),
        ]);
        let d = of(&crew(&sn), 776.0);
        check(&d, "parent cycle");
        assert_eq!(
            d.nodes.iter().filter(|n| n.card.kind == Kind::L3).count(),
            10
        );
    }

    #[test]
    fn deep_worker_chains_keep_cards_inside_their_column() {
        let mut sn = snap(1, 1);
        for pid in 101..130 {
            sn["workers"].as_array_mut().unwrap().push(json!({
                "pid":pid,"parent":pid-1,"project":"p2","state":"working","intent":"bounded child"
            }));
        }
        for width in [776.0, 1440.0] {
            let d = of(&crew(&sn), width);
            check(&d, "deep chain");
            assert_eq!(
                d.nodes.iter().filter(|n| n.card.kind == Kind::L3).count(),
                30
            );
            assert!(
                d.nodes
                    .iter()
                    .filter(|n| n.card.kind == Kind::L3)
                    .all(|n| n.w >= MIN_W - INDENT)
            );
        }
    }

    #[test]
    fn l1_is_on_top_leads_below_and_workers_under_their_own_lead() {
        let c = crew(&snap(2, 3));
        let d = of(&c, 1280.0);
        check(&d, "2 leads");
        let l1 = d.node("seat:1").unwrap();
        for lead in ["seat:2", "seat:3"] {
            let l = d.node(lead).unwrap();
            assert!(l.y >= l1.y + l1.h + BUS, "{lead} below L1");
            assert!(d.edges.iter().any(|e| e.from == "seat:1" && e.to == lead));
        }
        // PID 100 → lead 2, 101 → lead 3, 102 → lead 2; every worker below and inside its lead's column
        for (w, lead) in [
            ("pid:100", "seat:2"),
            ("pid:101", "seat:3"),
            ("pid:102", "seat:2"),
        ] {
            let (n, l) = (d.node(w).unwrap(), d.node(lead).unwrap());
            assert!(n.y >= l.y + l.h + DROP, "{w} under {lead}");
            assert_eq!(n.x, l.x + INDENT);
            assert!(n.x + n.w <= l.x + l.w);
            assert!(d.edges.iter().any(|e| e.from == lead && e.to == w));
        }
        assert!(
            d.node("pid:102").unwrap().y > d.node("pid:100").unwrap().y,
            "stacked in order"
        );
        // the leads sit side by side at full width, the whole tree centred under L1
        assert_eq!(d.node("seat:2").unwrap().y, d.node("seat:3").unwrap().y);
        assert_eq!(d.lead_w, CARD_W);
        let mid = |n: &Node| n.x + n.w / 2.0;
        assert_eq!(mid(l1), d.w / 2.0);
    }

    #[test]
    fn cards_name_level_project_and_harness_with_a_working_on_line() {
        let c = crew(&snap(2, 4));
        let d = of(&c, 1280.0);
        let card = |k: &str| d.node(k).unwrap().card.clone();
        assert_eq!(card("seat:1").head, "L1 · UNVRS · Claude");
        assert_eq!(card("seat:1").doing, "steering the crew");
        assert_eq!(card("seat:3").head, "L2 · p3 · Codex");
        assert_eq!(card("seat:3").kind, Kind::L2);
        assert!(!card("seat:3").dim, "a live lead is not dimmed");
        assert_eq!(
            card("seat:2").head,
            "L2 · p2",
            "no harness bound: none named"
        );
        assert_eq!(card("pid:100").head, "L3 · p2 · Claude");
        assert_eq!(card("pid:100").doing, "step 0");
        assert_eq!(
            (
                card("pid:100").model.as_str(),
                card("pid:100").drawer.as_deref()
            ),
            ("unknown", Some("pid:100"))
        );
        // a vacant lead with no workers is dimmed and says so
        let c = crew(&snap(3, 0));
        let d = of(&c, 1280.0);
        let p4 = &d.node("seat:4").unwrap().card;
        assert!(p4.dim && p4.doing.starts_with("vacant"));
        assert_eq!(harness_name("codex"), "Codex");
        assert_eq!(harness_name("harness unknown"), "");
    }

    #[test]
    fn a_fresh_brief_shows_the_task_intent_not_not_started() {
        let mut sn = snap(1, 2);
        sn["workers"][0]["now"] = json!("not started");
        sn["workers"][0]["next"] = json!("start the task");
        sn["workers"][1]["now"] = json!("Not started");
        sn["workers"][1]["next"] = json!("trace the parser");
        let d = of(&crew(&sn), 1280.0);
        assert_eq!(d.node("pid:100").unwrap().card.doing, "task 0");
        assert_eq!(d.node("pid:101").unwrap().card.doing, "trace the parser");
    }

    #[test]
    fn l1_workers_sit_in_a_framed_row_under_l1_apart_from_the_leads() {
        let c = crew(&snap(2, 5));
        let d = of(&c, 1280.0);
        check(&d, "idle and direct");
        // worker 4 (k % 5 == 4) is idle: drawn, dimmed, its edge dimmed
        let w = &d.node("pid:104").unwrap().card;
        assert!(w.dim);
        assert!(d.edges.iter().find(|e| e.to == "pid:104").unwrap().dim);
        // worker 3 was dispatched by L1: its edge comes straight from L1 into the frame
        // under L1, which sits left of the trunk and above the lead row
        let e = d.edges.iter().find(|e| e.to == "pid:103").unwrap();
        assert_eq!(e.from, "seat:1");
        let (n, fr, l1) = (
            d.node("pid:103").unwrap(),
            d.node("frame:l1").unwrap(),
            d.node("seat:1").unwrap(),
        );
        assert!(inside(n, fr));
        assert_eq!(fr.card.doing, "L1's own workers");
        assert!(fr.y >= l1.y + l1.h + BUS / 2.0);
        assert!(fr.x + fr.w <= l1.x + l1.w / 2.0 - LANE / 2.0);
        // no L1 workers, no frame
        let d = of(&crew(&snap(2, 2)), 1280.0);
        assert!(d.node("frame:l1").is_none());
        check(&d, "no own");
    }

    #[test]
    fn finished_workers_collapse_into_a_count_that_names_whose_they_are() {
        let mut cards = crew_cards(&crew(&snap(1, 1)));
        cards.leads[0].note = note("seat:2", 12, "L2 p2");
        cards.own = Some(Column {
            head: None,
            kids: vec![],
            note: note("seat:1", 10, "L1"),
        });
        let d = layout(cards, 1280.0);
        check(&d, "note");
        let n = d.node("note:seat:2").unwrap();
        assert_eq!(n.card.doing, "✓ 12 finished by L2 p2");
        assert!(
            n.y > d.node("pid:100").unwrap().y,
            "below the running workers"
        );
        // L1's count sits in L1's frame
        let n = d.node("note:seat:1").unwrap();
        assert_eq!(n.card.doing, "✓ 10 finished by L1");
        assert!(inside(n, d.node("frame:l1").unwrap()));
        assert!(note("x", 0, "L1").is_none());
        // and the crew's own counts name their owner
        let mut sn = snap(2, 0);
        sn["seats"][1]["project"] = json!("acme");
        let c = crew(&sn);
        let s = &c
            .idle
            .iter()
            .chain(c.active.iter())
            .find(|s| s.pid == 2)
            .unwrap();
        assert_eq!(owner(s), "L2 acme");
        assert_eq!(owner(c.l1.as_ref().unwrap()), "L1");
    }

    #[test]
    fn one_to_twenty_workers_lay_out_cleanly_from_phone_to_wide() {
        for width in [360.0, 800.0, 1280.0, 1920.0] {
            for leads in [1, 3, 6, 12] {
                for n in 0..=20 {
                    let d = of(&crew(&snap(leads, n)), width);
                    check(&d, &format!("{width}px {leads} leads {n} workers"));
                    // wider than the screen only when the leads are at their narrowest or
                    // L1's frame needs the room: then the canvas scrolls sideways
                    let frame = d.node("frame:l1").map_or(0.0, |f| 2.0 * (f.w + LANE / 2.0));
                    assert!(
                        d.w <= width.max(CARD_W + 2.0 * PAD).max(frame + 2.0 * PAD)
                            || d.lead_w == MIN_W,
                        "{width}px: {} wide",
                        d.w
                    );
                    assert!((MIN_W..=CARD_W).contains(&d.lead_w));
                    assert_eq!(
                        d.edges.len(),
                        d.nodes
                            .iter()
                            .filter(|m| !matches!(m.card.kind, Kind::L1 | Kind::Note | Kind::Frame))
                            .count()
                    );
                }
            }
        }
    }

    #[test]
    fn many_leads_never_wrap_they_narrow_then_scroll_sideways() {
        // 6 leads at 1440 px: one row, narrower cards, still on screen
        let d = of(&crew(&snap(6, 8)), 1440.0);
        check(&d, "1440px");
        assert!(d.lead_w < CARD_W && d.lead_w >= MIN_W);
        assert!(d.w <= 1440.0);
        // 6 leads at 800 px: one row at the narrowest, the canvas scrolls
        let d = of(&crew(&snap(6, 8)), 800.0);
        check(&d, "800px");
        assert_eq!(d.lead_w, MIN_W);
        assert!(d.w > 800.0);
        let ys: Vec<f64> = d
            .nodes
            .iter()
            .filter(|n| n.card.kind == Kind::L2)
            .map(|n| n.y)
            .collect();
        assert_eq!(ys.len(), 6);
        assert!(ys.iter().all(|y| *y == ys[0]), "{ys:?}");
        // an empty crew still draws nothing broken
        let d = of(&crew(&json!({})), 800.0);
        assert!(d.nodes.is_empty() && d.edges.is_empty() && d.h > 0.0);
    }

    /// The crew L1 saw on the live build (PID 159): L1, six leads (a Codex lead whose
    /// effort is not reported, two leads carrying a judge route receipt, a vacant one),
    /// and three workers: one of L1's own, one under a lead, one held with a route.
    pub(crate) fn live_crew() -> Value {
        let route = json!({"role": "judge", "reason": "seat judgment; default reasoning"});
        let lead = |pid: u64, project: &str, h: &str, model: &str, effort: Value, state: &str| {
            json!({"rank": 2, "pid": pid, "project": project, "state": state,
                "occupied_by": {"app": "claude-desktop", "harness": h}, "model": model, "effort": effort,
                "source": "transcript"})
        };
        let mut side = lead(
            4,
            "side-project",
            "claude",
            "claude-opus-5-5",
            json!("xhigh"),
            "free",
        );
        side["source"] = json!("last seat-run");
        side["econ"] = route.clone();
        let mut times = lead(
            5,
            "newsletter",
            "claude",
            "claude-opus-5-5",
            json!("xhigh"),
            "free",
        );
        times["source"] = json!("last seat-run");
        times["econ"] = route.clone();
        json!({"at": NOW, "kernel": {"version": "t", "os_pid": 1, "uptime_s": 1, "home": "/nowhere"},
        "calls": [], "quota": [],
        "seats": [
            {"rank": 1, "pid": 1, "project": null, "state": "live",
                "occupied_by": {"app": "claude-desktop", "harness": "claude"},
                "model": "claude-opus-5-5", "effort": "high", "now": "The new research request is pending"},
            lead(6, "brand", "claude", "claude-opus-5-5", json!("high"), "live"),
            lead(2, "unvrs-rs", "codex", "gpt-6-sol", Value::Null, "free"),
            lead(3, "acme", "claude", "claude-opus-5-5", Value::Null, "free"),
            side,
            times,
            {"rank": 2, "pid": 7, "project": "personal", "state": "free", "occupied_by": null,
                "source": null, "model": null, "effort": null},
        ],
        "workers": [
            {"pid": 159, "parent": 1, "project": "unvrs-rs", "intent": "Crew diagram regressions",
                "harness": "claude", "model": "claude-opus-5-5", "effort": "high",
                "elapsed_s": 60, "state": "working", "now": "porting the fixes"},
            {"pid": 160, "parent": 2, "project": "unvrs-rs", "intent": "a lead's task",
                "harness": "codex", "elapsed_s": 30, "state": "working", "now": "reading"},
            {"pid": 161, "parent": 4, "project": "side-project", "intent": "held for a judge",
                "harness": "claude", "elapsed_s": 5, "state": "held", "econ": route},
        ]})
    }

    #[test]
    fn l1s_live_crew_at_776_and_1440_px_one_row_per_level_and_full_routes() {
        let c = crew(&live_crew());
        for width in [776.0, 1440.0] {
            let what = format!("live crew at {width}px");
            let d = of(&c, width);
            // no overlap, no loop round the edge, leads in one row, L3s under their parent
            check(&d, &what);
            let leads: Vec<&Node> = d.nodes.iter().filter(|n| n.card.kind == Kind::L2).collect();
            assert_eq!(leads.len(), 6, "{what}: six lead cards");
            assert!(
                leads.iter().all(|n| n.y == leads[0].y),
                "{what}: never wrapped"
            );
            let l1 = d.node("seat:1").unwrap();
            let rows = |k: Kind| {
                let mut ys: Vec<i64> = d
                    .nodes
                    .iter()
                    .filter(|n| n.card.kind == k)
                    .map(|n| n.y as i64)
                    .collect();
                ys.dedup();
                ys.len()
            };
            assert_eq!((rows(Kind::L1), rows(Kind::L2)), (1, 1), "{what}");
            assert!(leads[0].y > l1.y + l1.h, "{what}: leads below L1");
            // a route receipt makes its card taller, by as many lines as its width needs
            for n in &d.nodes {
                if matches!(n.card.kind, Kind::Note | Kind::Frame) {
                    continue;
                }
                match &n.card.routed {
                    Some(r) => {
                        let lines = route_lines(r, n.w);
                        assert!(lines >= 2, "{what}: {} route on {lines} line", n.card.key);
                        assert_eq!(n.h, CARD_H + 2.0 + lines as f64 * ROUTE_LINE);
                    }
                    None => assert_eq!(n.h, CARD_H, "{what}: {} height", n.card.key),
                }
            }
            assert_eq!(
                d.nodes.iter().filter(|n| n.card.routed.is_some()).count(),
                3,
                "{what}: side-project, newsletter and the held worker carry routes"
            );
            // the vacant lead has no model to show
            let personal = d.node("seat:7").unwrap();
            assert!(personal.card.model.is_empty() && personal.card.effort.is_empty());
        }
        // wide: the row fits on screen; narrow: the cards keep their minimum and the
        // canvas scrolls sideways rather than wrapping the row
        let wide = of(&c, 1440.0);
        assert!(wide.w <= 1440.0 && wide.lead_w > MIN_W, "{}", wide.w);
        let narrow = of(&c, 776.0);
        assert_eq!(narrow.lead_w, MIN_W);
        assert!(narrow.w > 776.0);
    }

    #[test]
    fn route_lines_grow_as_cards_narrow_up_to_three() {
        let r = "routed: judge · seat judgment; default deep reasoning";
        assert_eq!(route_lines(r, CARD_W), 2);
        assert_eq!(route_lines(r, 217.0), 2);
        assert_eq!(route_lines(r, MIN_W), 3);
        assert_eq!(route_lines(r, MIN_W - INDENT), 3);
        assert_eq!(route_lines("routed: judge · ok", CARD_W), 1);
        assert_eq!(route_lines(&r.repeat(4), MIN_W), ROUTE_MAX);
    }
}
