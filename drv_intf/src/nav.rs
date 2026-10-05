//! NAV panel: a live pixel universe. Flight status is part of the scene, never a text dump.
//!
//! The scene is a deterministic function of `(width, height, tick, motion, SceneView)`, so the
//! whole panel is unit testable without a `Ship`. `draw` only adapts `Ship` into a `SceneView`.
use crate::bridge::Ship;
use crate::theme::*;
use ratatui::{
    Frame,
    layout::Rect,
    style::Color,
    text::{Line, Span},
    widgets::Paragraph,
};
use uke::glyph;

/// Faintest parallax layer: between PANEL and DIM so depth reads on a CRT.
const FAR: Color = Color::Rgb(46, 62, 86);
/// Ship sprites in the header's half-block language, one per field height.
const SHIP1: [&str; 1] = ["▟█▙▶"];
const SHIP2: [&str; 2] = ["▗▄▖  ", "▟██▙▶"];
const SHIP3: [&str; 3] = [" ▗▄▖  ", "▟███▙▶", " ▝▀▘  "];
/// Thruster exhaust, hottest first.
const TRAIL: [char; 4] = ['≡', '═', '─', '·'];
/// Star order is the crew order: what flies first is what the captain watches.
const STATES: [&str; 5] = ["working", "blocked", "idle", "booting", "offline"];

/// One crew seat as the universe sees it: a world whose colour is its state.
#[derive(Clone, Debug)]
pub struct SeatView {
    pub pid: usize,
    pub name: String,
    pub state: String,
    /// The seat changed state recently (`pulse` < 1.6 s): flare it white.
    pub flash: bool,
    pub unread: bool,
}

/// Everything the scene needs, decoupled from `Ship` so it can be unit tested.
#[derive(Clone, Debug, Default)]
pub struct SceneView {
    pub seats: Vec<SeatView>,
    /// Most recent routed handoff as `(from_pid, to_pid)`; drawn as a travelling packet.
    pub handoff: Option<(usize, usize)>,
    pub handoffs: usize,
    pub focus: String,
    pub event: String,
    pub notice: String,
}

impl SceneView {
    /// Any seat working means the universe drifts at flight speed.
    pub fn in_flight(&self) -> bool {
        self.seats.iter().any(|s| s.state == "working")
    }
}

pub fn draw(f: &mut Frame, r: Rect, s: &Ship) {
    let view = view_of(s);
    let panel = block(
        if view.in_flight() {
            "NAV / IN FLIGHT"
        } else {
            "NAV / HOLDING"
        },
        DIM,
    );
    let inner = panel.inner(r);
    f.render_widget(panel, r);
    let lines = scene(inner.width, inner.height, s.tick, s.motion, &view);
    f.render_widget(Paragraph::new(lines), inner);
}

/// Adapt the live ship into the scene's flat view.
fn view_of(s: &Ship) -> SceneView {
    SceneView {
        seats: s
            .obs
            .crew
            .iter()
            .map(|a| SeatView {
                pid: a.pid,
                name: a.name.clone(),
                state: a.state.clone(),
                flash: a.pulse.elapsed().as_millis() < 1600,
                unread: a.unread,
            })
            .collect(),
        handoff: s
            .obs
            .handoffs
            .iter()
            .rev()
            .find_map(|h| h.to_pid.map(|to| (h.from_pid, to))),
        handoffs: s.obs.handoffs.len(),
        focus: s.obs.focus().to_string(),
        event: s.obs.tail.back().cloned().unwrap_or_default(),
        notice: s.notice.clone(),
    }
}

/// Render the whole panel body: HUD row, universe field, transmission ticker.
///
/// Always returns exactly `height` lines and never panics, down to 0×0.
pub fn scene(
    width: u16,
    height: u16,
    tick: u64,
    motion: bool,
    v: &SceneView,
) -> Vec<Line<'static>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return vec![Line::default(); h];
    }
    let flight = v.in_flight();
    let mut lines = Vec::with_capacity(h);
    lines.push(hud(w, v, flight));
    if h == 1 {
        return lines;
    }
    let field_h = if h >= 3 { h - 2 } else { h - 1 };
    for row in field(w, field_h, tick, motion, v, flight) {
        lines.push(row_line(&row));
    }
    if h >= 3 {
        lines.push(ticker(w, tick, motion, v));
    }
    lines
}

/// Slim status strip: state counts, handoff count, current focus.
fn hud(w: usize, v: &SceneView, flight: bool) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for state in STATES {
        let n = v.seats.iter().filter(|s| s.state == state).count();
        if n == 0 {
            continue;
        }
        add(
            &mut spans,
            &mut used,
            w,
            format!("{}{} ", glyph(state), n),
            status(state),
        );
    }
    if v.handoffs > 0 {
        add(
            &mut spans,
            &mut used,
            w,
            format!("· {} hop ", v.handoffs),
            DIM,
        );
    }
    add(
        &mut spans,
        &mut used,
        w,
        "FOCUS / ".into(),
        if flight { CYAN } else { DIM },
    );
    let focus = one_line(&v.focus, w.saturating_sub(used) as u16);
    add(&mut spans, &mut used, w, focus, GOLD);
    Line::from(spans)
}

/// The transmission ticker: the notice, else the latest event, marqueed if it overflows.
fn ticker(w: usize, tick: u64, motion: bool, v: &SceneView) -> Line<'static> {
    let (text, color, lead) = if !v.notice.trim().is_empty() {
        (v.notice.as_str(), GOLD, "▌ ")
    } else if v.event.trim().is_empty() {
        ("Awaiting transmission…", FAR, "◂ ")
    } else {
        (v.event.as_str(), DIM, "◂ ")
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut spans = Vec::new();
    let mut used = 0usize;
    add(&mut spans, &mut used, w, lead.into(), FAR);
    let room = w.saturating_sub(used);
    let body = if motion && text.chars().count() > room && room > 0 {
        let padded: Vec<char> = format!("{text}   ·   ").chars().collect();
        let offset = (tick / 5) as usize % padded.len();
        padded
            .iter()
            .cycle()
            .skip(offset)
            .take(room)
            .collect::<String>()
    } else {
        text
    };
    add(&mut spans, &mut used, w, body, color);
    Line::from(spans)
}

/// Push `text` clipped to the width still free, tracking the used width.
fn add(spans: &mut Vec<Span<'static>>, used: &mut usize, w: usize, text: String, c: Color) {
    let room = w.saturating_sub(*used);
    if room == 0 {
        return;
    }
    let text = clip(&text, room);
    if text.is_empty() {
        return;
    }
    *used += Line::from(text.as_str()).width();
    spans.push(Span::styled(text, style(c)));
}

/// Truncate to a display width without collapsing whitespace (unlike `one_line`).
fn clip(text: &str, room: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = Line::from(ch.to_string()).width();
        if used + cw > room {
            break;
        }
        used += cw;
        out.push(ch);
    }
    out
}

type Cell = (char, Color);

/// The universe field: parallax stars, the USS UNVRS, crew worlds and the handoff packet.
fn field(
    w: usize,
    h: usize,
    tick: u64,
    motion: bool,
    v: &SceneView,
    flight: bool,
) -> Vec<Vec<Cell>> {
    let mut g = vec![vec![(' ', FAR); w]; h];
    if h == 0 {
        return g;
    }
    // Parallax: far layers crawl, near layers race. Holding still drifts, gently.
    let phase = if !motion {
        0
    } else if flight {
        tick
    } else {
        tick / 4
    };
    for layer in 0u64..3 {
        let drift = phase * (layer + 1) / 3;
        let (density, color, marks) = match layer {
            0 => (11u64, FAR, ['·', '·', '.']),
            1 => (23, DIM, ['·', '.', '*']),
            _ => (47, WHITE, ['*', '+', '·']),
        };
        for (y, row) in g.iter_mut().enumerate() {
            for (x, cell) in row.iter_mut().enumerate() {
                let n = hash(x as u64 + drift, y as u64, layer + 7);
                if n.is_multiple_of(density) {
                    let tint = if n % 601 < 3 { PINK } else { color };
                    *cell = (marks[(n >> 13) as usize % 3], tint);
                }
            }
        }
    }
    // The USS UNVRS, nose to starboard, exhaust astern.
    let sprite: &[&str] = match h {
        1 => &SHIP1,
        2 => &SHIP2,
        _ => &SHIP3,
    };
    let top = (h - sprite.len()) / 2;
    let mut ship_w = 0usize;
    for (dy, art) in sprite.iter().enumerate() {
        ship_w = ship_w.max(art.chars().count());
        for (dx, ch) in art.chars().enumerate() {
            if ch != ' ' {
                put(&mut g, 1 + dx, top + dy, ch, CYAN);
            }
        }
    }
    let mid = top + sprite.len() / 2;
    let (trail, hot) = if flight && motion {
        (TRAIL[((tick / 2) % 4) as usize], GOLD)
    } else if flight {
        (TRAIL[0], GOLD)
    } else {
        ('·', FAR)
    };
    put(&mut g, 0, mid, trail, hot);

    // Place the crew worlds along the lane ahead of the ship. Positions first, then the worlds,
    // then the handoff on an arc one row off the labels so nothing gets smeared.
    let lane = 1 + ship_w + 2;
    let room = w.saturating_sub(lane + 1);
    let mut worlds: Vec<(usize, usize, usize)> = Vec::new();
    let cell = if v.seats.is_empty() || room < 2 {
        0
    } else {
        (room / v.seats.len()).max(2)
    };
    if cell > 0 {
        for (i, seat) in v.seats.iter().enumerate() {
            let x = lane + i * cell;
            if x >= w {
                break;
            }
            let mut y = (hash(seat.pid as u64, 3, 5) % h as u64) as usize;
            if seat.state == "working" && motion && h > 1 {
                let bob = [0i64, 1, 0, -1][((tick / 6 + i as u64) % 4) as usize];
                y = (y as i64 + bob).clamp(0, h as i64 - 1) as usize;
            }
            worlds.push((i, x, y));
        }
    }

    for &(i, x, y) in &worlds {
        let seat = &v.seats[i];
        let working = seat.state == "working";
        let blink = seat.state == "blocked" && motion && (tick / 4).is_multiple_of(2);
        let color = if seat.flash || blink {
            WHITE
        } else {
            status(&seat.state)
        };
        put(&mut g, x, y, narrow(glyph(&seat.state)), color);
        if working {
            // A moon on orbit: motion you can see even when the text holds still. The ring keeps
            // off the label row to starboard, so it never eats the seat name.
            const ORBIT: [(i64, i64); 6] = [(1, -1), (0, -1), (-1, -1), (-1, 1), (0, 1), (1, 1)];
            let step = if motion { (tick / 3 + i as u64) % 6 } else { 1 };
            let (ox, oy) = if h == 1 {
                (-1, 0)
            } else {
                ORBIT[step as usize]
            };
            let (mx, my) = (x as i64 + ox, y as i64 + oy);
            if mx >= 0 && my >= 0 {
                put(&mut g, mx as usize, my as usize, '·', CYAN);
            }
        } else if seat.flash {
            put(&mut g, x + 1, y, ')', WHITE);
        }
        let label_room = cell.saturating_sub(3);
        if label_room >= 3 {
            let label = one_line(
                &format!(
                    "{}{:02} {}",
                    if seat.unread { "!" } else { "" },
                    seat.pid,
                    seat.name
                ),
                label_room as u16,
            );
            let tone = if working || seat.flash { color } else { DIM };
            for (k, ch) in label.chars().enumerate() {
                put(&mut g, x + 2 + k, y, ch, tone);
            }
        }
    }

    // The latest handoff as a packet in transit between two worlds, arcing clear of the labels.
    if let Some((from, to)) = v.handoff
        && let (Some(a), Some(b)) = (world_of(v, &worlds, from), world_of(v, &worlds, to))
        && a.1 != b.1
    {
        let steps = 16i64;
        let k = if motion {
            ((tick / 2) % (steps as u64 - 1)) as i64 + 1
        } else {
            steps / 2
        };
        let (x0, y0) = (a.1 as i64, a.2 as i64);
        let (x1, y1) = (b.1 as i64, b.2 as i64);
        for s in 1..steps {
            let x = (x0 + (x1 - x0) * s / steps).max(0) as usize;
            let y = y0 + (y1 - y0) * s / steps;
            let y = if h > 1 && y + 1 < h as i64 {
                y + 1
            } else {
                y - 1
            };
            let y = y.clamp(0, h as i64 - 1) as usize;
            if s == k {
                put(&mut g, x, y, '▸', GOLD);
            } else if s % 4 == 0 {
                put(&mut g, x, y, '·', GOLD);
            }
        }
    }
    g
}

/// Locate a placed world by PID.
fn world_of<'a>(
    v: &SceneView,
    worlds: &'a [(usize, usize, usize)],
    pid: usize,
) -> Option<&'a (usize, usize, usize)> {
    worlds.iter().find(|p| v.seats[p.0].pid == pid)
}

/// Write one cell, clipping out of bounds. Every glyph is normalised to one column so a row's
/// rendered width can never exceed the field width.
fn put(g: &mut [Vec<Cell>], x: usize, y: usize, ch: char, c: Color) {
    if let Some(cell) = g.get_mut(y).and_then(|row| row.get_mut(x)) {
        *cell = (narrow(&ch.to_string()), c);
    }
}

/// Keep the grid one column per cell: anything wider becomes a speck.
fn narrow(s: &str) -> char {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if Line::from(ch.to_string()).width() == 1 => ch,
        _ => '·',
    }
}

/// Collapse a grid row into colour runs so a row costs a handful of spans, not a widget per star.
fn row_line(row: &[Cell]) -> Line<'static> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<Color> = None;
    for &(ch, c) in row {
        if cur != Some(c) {
            if let Some(prev) = cur {
                spans.push(Span::styled(std::mem::take(&mut buf), style(prev)));
            }
            cur = Some(c);
        }
        buf.push(ch);
    }
    if let Some(prev) = cur {
        spans.push(Span::styled(buf, style(prev)));
    }
    Line::from(spans)
}

/// Deterministic 3D value hash (xxHash-style finaliser): the starfield needs no rand crate.
fn hash(x: u64, y: u64, z: u64) -> u64 {
    let mut h = x.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ y.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ z.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^ (h >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};

    fn seat(pid: usize, name: &str, state: &str) -> SeatView {
        SeatView {
            pid,
            name: name.into(),
            state: state.into(),
            flash: false,
            unread: false,
        }
    }

    fn crew() -> SceneView {
        SceneView {
            seats: vec![
                seat(1, "captain", "working"),
                seat(2, "navigator", "idle"),
                seat(3, "engineer", "blocked"),
            ],
            handoff: Some((1, 3)),
            handoffs: 2,
            focus: "ship the nav scene".into(),
            event: "0012  PID 2 / done / landed".into(),
            notice: String::new(),
        }
    }

    fn flat(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn scene_is_never_blank_even_holding_at_tick_zero() {
        let mut view = crew();
        for s in &mut view.seats {
            s.state = "idle".into();
        }
        let lines = scene(60, 8, 0, false, &view);
        assert_eq!(lines.len(), 8);
        let art = flat(&lines);
        assert!(
            art.chars().filter(|c| !c.is_whitespace()).count() > 60,
            "static scene must still be a universe, got:\n{art}"
        );
        // Ship and HUD are present whether or not anyone is flying.
        assert!(art.contains('▟') && art.contains("FOCUS / "));
    }

    #[test]
    fn scene_animates_between_ticks_while_in_flight() {
        let view = crew();
        let a = flat(&scene(60, 8, 0, true, &view));
        let b = flat(&scene(60, 8, 7, true, &view));
        assert_ne!(a, b, "starfield must drift while a seat is working");
        // With motion off the scene is frozen regardless of tick.
        assert_eq!(
            flat(&scene(60, 8, 0, false, &view)),
            flat(&scene(60, 8, 99, false, &view))
        );
    }

    #[test]
    fn scene_carries_seat_markers_counts_focus_and_event() {
        let art = flat(&scene(120, 10, 4, true, &crew()));
        for name in ["captain", "navigator", "engineer"] {
            assert!(art.contains(name), "missing seat {name} in:\n{art}");
        }
        assert!(art.contains("01 ") && art.contains("03 "));
        assert!(art.contains("◆1") && art.contains("○1") && art.contains("▲1"));
        assert!(art.contains("2 hop"));
        assert!(art.contains("ship the nav scene"));
        assert!(art.contains("PID 2 / done / landed"));
        assert!(art.contains('▸'), "handoff packet should be in transit");
    }

    #[test]
    fn notice_takes_over_the_transmission_ticker() {
        let mut view = crew();
        view.notice = "Kernel handshake failed".into();
        let last = flat(&scene(60, 6, 0, false, &view));
        let last = last.lines().last().unwrap().to_string();
        assert!(last.contains("Kernel handshake failed"), "{last}");
    }

    #[test]
    fn every_line_fits_the_given_width_at_all_sizes() {
        let view = crew();
        for (w, h) in [(19u16, 3u16), (32, 7), (80, 14), (140, 16)] {
            for tick in [0u64, 5, 41] {
                let lines = scene(w, h, tick, true, &view);
                assert_eq!(lines.len(), h as usize);
                for line in &lines {
                    assert!(
                        line.width() <= w as usize,
                        "line overflows {w}x{h}: {:?}",
                        flat(std::slice::from_ref(line))
                    );
                }
            }
        }
    }

    #[test]
    fn degenerate_rects_do_not_panic() {
        let mut view = crew();
        view.seats
            .extend((4..12).map(|p| seat(p, "escort", "offline")));
        for (w, h) in [
            (0u16, 0u16),
            (1, 1),
            (1, 9),
            (9, 1),
            (3, 2),
            (19, 3),
            (2, 16),
        ] {
            for motion in [true, false] {
                let lines = scene(w, h, 13, motion, &view);
                assert_eq!(lines.len(), h as usize);
            }
        }
    }

    #[test]
    fn scene_renders_through_a_real_backend() {
        let view = crew();
        let mut term = Terminal::new(TestBackend::new(40, 7)).unwrap();
        term.draw(|f| {
            let area = f.area();
            f.render_widget(
                Paragraph::new(scene(area.width, area.height, 3, true, &view)),
                area,
            );
        })
        .unwrap();
        let dump = term.backend().to_string();
        assert!(dump.contains("FOCUS"), "{dump}");
        assert!(dump.chars().any(|c| c == '▟'), "{dump}");
    }
}
