//! The Observatory preview (`/preview`): the new layout (status line, Needs you, Crew,
//! Alive, Fuel) over the dimmed galaxy, served beside the live bridge (`/` is unchanged).
//! `/preview?sim=problem` and `/preview?sim=calm` show labelled fixtures instead of real
//! data (a problem state and what a calm day looks like).
pub mod diagram;
pub mod model;
pub mod sim;
mod view;

use crate::probe::{Probes, now_ms};
use model::{Inputs, Model};
use serde_json::Value;
use std::path::PathBuf;

pub use view::Preview;

pub const PREVIEW_CSS: &str = include_str!("preview.css");
pub const PREVIEW_JS: &str = include_str!("preview.js");
/// The Settings overlay (opened by the gear beside Hide cockpit): its own stylesheet and
/// script, scoped to `#settings` and `.settings-open`, so nothing else on the page changes.
pub const SETTINGS_CSS: &str = include_str!("settings.css");
pub const SETTINGS_JS: &str = include_str!("settings.js");

/// Which fixture this query string asks for, if any (`sim=problem` or `sim=calm`).
pub fn wants_sim(query: Option<&str>) -> Option<&'static str> {
    let q = query?;
    q.split('&').find_map(|kv| match kv {
        "sim=problem" => Some("problem"),
        "sim=calm" => Some("calm"),
        _ => None,
    })
}

/// The UNVRS home the snapshot names (the probes read around it).
fn home_of(snap: &Value) -> Option<PathBuf> {
    snap["kernel"]["home"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("UNVRS_HOME")
                .ok()
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var("HOME")
                        .ok()
                        .map(|h| PathBuf::from(h).join(".unvrs"))
                })
        })
}

/// Reads the host once when the Observatory starts, so the saved Claude usage and
/// workspace readings are loaded and the first fresh ones start before any page asks.
/// Blocking.
pub fn warm(snap: &Value, probes: &Probes) {
    if let Some(h) = home_of(snap) {
        probes.read(&h);
    }
}

/// Builds the model: the fixture in sim mode (nothing real is read), else the snapshot
/// plus the host probes. Blocking.
pub fn build(snap: &Value, probes: &Probes, sim: Option<&str>) -> Model {
    let now = now_ms();
    if let Some(name) = sim {
        let (s, h) = if name == "calm" {
            sim::calm(now)
        } else {
            sim::problem(now)
        };
        let mut m = Model::build(Inputs {
            snap: &s,
            host: &h,
            now_ms: now,
            sim: true,
        });
        m.fixture = Some(name.to_owned());
        return m;
    }
    let host = home_of(snap).map(|h| probes.read(&h)).unwrap_or_default();
    Model::build(Inputs {
        snap,
        host: &host,
        now_ms: now,
        sim: false,
    })
}

/// The page shell: the galaxy (the same Cosmos renderer and boot glue as the bridge),
/// a scrim that dims it behind the cockpit, and the LiveView mount.
pub fn page(sim: Option<&str>, style: &str, boot_js: &str) -> String {
    let ws = match sim {
        Some(name) => format!("/preview/ws?sim={name}"),
        None => "/preview/ws".into(),
    };
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>UNVRS · Observatory{}</title>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"color-scheme\" content=\"dark\"><link rel=\"icon\" href=\"data:,\">\
<style>{style}</style><style>{SETTINGS_CSS}</style></head><body class=\"live preview\">\
<div class=\"window\" id=\"hero\"><canvas id=\"cosmos\" aria-hidden=\"true\"></canvas>\
<span id=\"cosmos-phase\" hidden></span><span id=\"cosmos-stats\" hidden></span></div>\
<div class=\"scrim\" aria-hidden=\"true\"></div>\
<div id=\"lost\" class=\"lost\" role=\"alert\" hidden>Signal lost · reconnecting</div>\
<div id=\"main\"></div>\
{}<script>{boot_js}</script><script>{PREVIEW_JS}</script><script>{SETTINGS_JS}</script></body></html>",
        if sim.is_some() { " · SIMULATED" } else { "" },
        dioxus_liveview::interpreter_glue(&ws),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Host;
    use dioxus::prelude::*;

    fn render(m: Model, hidden: bool) -> String {
        dioxus_ssr::render_element(rsx! { Preview { model: m, hidden } })
    }

    fn real_model() -> Model {
        let snap = serde_json::json!({
            "at": now_ms(),
            "kernel": {"version": "0.8.0", "uptime_s": 60, "os_pid": 1, "home": "/nowhere"},
            "calls": [{"id": "d1", "kind": "decision", "question": "Q?", "options": ["a"], "age_s": 5}],
            "seats": [{"rank": 1, "pid": 1, "state": "live", "occupied_by": {"app": "t3", "harness": "claude", "link": null}}],
            "workers": [], "quota": []
        });
        Model::build(Inputs {
            snap: &snap,
            host: &Host::default(),
            now_ms: now_ms(),
            sim: false,
        })
    }

    #[test]
    fn agent_fault_pid_and_error_render_inline() {
        let mut model = real_model();
        let agent = model
            .drivers
            .iter_mut()
            .find(|l| l.key == "drv:agent")
            .unwrap();
        agent.tone = model::Tone::Warn;
        agent.summary = "PID 161 · Codex protocol read failed · 1m ago".into();
        let html = render(model, false);
        assert!(html.contains("id=\"settings-open\""));
        let shell = page(None, PREVIEW_CSS, "");
        assert!(shell.contains(SETTINGS_CSS));
        assert!(shell.contains(SETTINGS_JS));
        let button = html
            .split("data-key=\"drv:agent\"")
            .nth(1)
            .unwrap()
            .split("</button>")
            .next()
            .unwrap();
        let word = button
            .split("class=\"lword\"")
            .nth(1)
            .unwrap()
            .split("</span>")
            .next()
            .unwrap();
        assert!(word.contains("degraded"));
        assert!(
            word.contains("PID 161"),
            "PID is only in the tooltip: {word}"
        );
        assert!(
            word.contains("Codex protocol read failed"),
            "error is only in the tooltip: {word}"
        );
    }

    #[test]
    fn crew_diagram_cards_show_captain_go_and_done_when() {
        let snap = serde_json::json!({"workers":[{
            "pid":7,"project":"fixture","intent":"inspect","state":"held",
            "go_quote":"Ok, go", "done_when":"Report quotes the captain's words",
            "econ":{"role":"worker", "reason":"quota unavailable"}
        }]});
        let model = Model::build(Inputs {
            snap: &snap,
            host: &Host::default(),
            now_ms: now_ms(),
            sim: false,
        });
        let html =
            dioxus_ssr::render_element(rsx! { Preview { model, diagram_key:Some(String::new()) } });
        assert!(html.contains("Go: Ok, go"));
        assert!(html.contains("Done when: Report quotes the captain"));
        assert!(html.contains("class=\"nalign ngo\""));
        assert!(html.contains("class=\"nalign ndone\""));
    }

    #[test]
    fn route_receipt_renders_in_worker_row_and_diagram_card() {
        let snap = serde_json::json!({
            "workers":[{"pid":7,"project":"fixture","intent":"choose","state":"held",
                "econ":{"role":"judge","reason":"high judgment; default reasoning"}}]
        });
        let model = Model::build(Inputs {
            snap: &snap,
            host: &Host::default(),
            now_ms: now_ms(),
            sim: false,
        });
        let row = render(model.clone(), false);
        let card = dioxus_ssr::render_element(
            rsx! { Preview { model, diagram_key: Some(String::new()) } },
        );
        for html in [row, card] {
            assert!(html.contains("routed: judge · high judgment; default reasoning"));
            assert!(html.contains("class=\"nroute\""));
        }
    }

    #[test]
    fn captain_fallback_receipt_renders_on_crew_cards_and_worker_rows() {
        let receipt = "captain fallback: gpt-6.1-sol at low; research task; default deep reasoning";
        let snap = serde_json::json!({"workers":[{
            "pid":7,"project":"fixture","intent":"research","state":"working",
            "econ":{"role":"research","reason":receipt,
                "chosen":{"model":"gpt-6.1-sol","effort":"low","captain_fallback":true}}
        }]});
        let model = Model::build(Inputs {
            snap: &snap,
            host: &Host::default(),
            now_ms: now_ms(),
            sim: false,
        });
        let row = render(model.clone(), false);
        let card = dioxus_ssr::render_element(
            rsx! { Preview { model, diagram_key: Some(String::new()) } },
        );
        for html in [row, card] {
            assert!(html.contains(&format!("routed: research · {receipt}")));
        }
    }

    #[test]
    fn l1s_live_crew_renders_without_question_marks_and_with_whole_routes() {
        let snap = diagram::tests::live_crew();
        let model = Model::build(Inputs {
            snap: &snap,
            host: &Host::default(),
            now_ms: diagram::tests::NOW,
            sim: false,
        });
        let html = dioxus_ssr::render_element(
            rsx! { Preview { model, diagram_key: Some(String::new()) } },
        );
        assert!(html.contains("id=\"diagram\""));
        for bad in [">?<", "· ?", "? ·", "\"?\""] {
            assert!(!html.contains(bad), "{bad:?} in the diagram");
        }
        // the Codex lead with no reported effort: its model alone
        assert!(html.contains(">6-sol<"));
        // the route reason is in the card whole (CSS wraps it to two lines)
        assert!(html.contains(">routed: judge · seat judgment; default reasoning<"));
        // the vacant lead's card has no model chip at all
        let personal = html.split("data-key=\"seat:7\"").nth(1).unwrap();
        let personal = &personal[..personal.find("</button>").unwrap()];
        assert!(personal.contains("vacant"));
        assert!(!personal.contains("class=\"chip"), "{personal}");
    }

    /// The cockpit (closed diagram) for L1, "L2 brand" (PID 2) and "L2 travel" (PID 3), both
    /// live, with these snapshot workers.
    fn tree_html(workers: Vec<serde_json::Value>) -> String {
        let seat = |pid: u64, project: Option<&str>| {
            serde_json::json!({"rank": if pid == 1 { 1 } else { 2 }, "pid": pid, "project": project,
                "state": "live", "occupied_by": {"app": "claude-desktop", "harness": "claude"},
                "model": "claude-opus-5-5", "effort": "high", "source": "transcript"})
        };
        let snap = serde_json::json!({
            "at": now_ms(),
            "kernel": {"version": "t", "uptime_s": 60, "os_pid": 1, "home": "/nowhere"},
            "calls": [], "quota": [],
            "seats": [seat(1, None), seat(2, Some("brand")), seat(3, Some("travel"))],
            "workers": workers,
        });
        let model = Model::build(Inputs {
            snap: &snap,
            host: &Host::default(),
            now_ms: now_ms(),
            sim: false,
        });
        render(model, false)
    }

    fn w(pid: u64, parent: u64, project: &str, now: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({"pid": pid, "parent": parent, "project": project,
            "intent": format!("Intent of {pid}"), "harness": "claude", "model": "claude-sonnet-5-5",
            "effort": "high", "elapsed_s": 90, "state": "working"});
        if let Some(n) = now {
            v["now"] = n.into();
        }
        v
    }

    /// The Crew zone's markup.
    fn crew_of(html: &str) -> &str {
        let at = html.find("<section class=\"zone z-crew\"").unwrap();
        let rest = &html[at..];
        &rest[..rest.find("</section>").unwrap()]
    }

    /// The tag of the worker row for `pid`.
    fn row_of(html: &str, pid: u64) -> &str {
        let at = html
            .find(&format!("data-pid=\"{pid}\""))
            .unwrap_or_else(|| panic!("no row {pid}"));
        let rest = &html[at..];
        &rest[..rest.find('>').unwrap()]
    }

    fn pos(html: &str, pid: u64) -> usize {
        html.find(&format!("data-pid=\"{pid}\""))
            .unwrap_or_else(|| panic!("no row {pid}"))
    }

    #[test]
    fn crew_tree_with_no_workers_lists_only_the_seats() {
        let html = tree_html(vec![]);
        let crew = crew_of(&html);
        assert_eq!(crew.matches("class=\"crow srow").count(), 3, "{crew}");
        for none in ["wrow", "tree-more", "finished", "class=\"workers\""] {
            assert!(!crew.contains(none), "{none} in {crew}");
        }
    }

    #[test]
    fn crew_tree_shows_l1s_own_workers_under_l1_with_who_and_what() {
        let html = tree_html(vec![
            w(180, 1, "unvrs-rs", Some("porting the cards")),
            w(181, 1, "unvrs-rs", None),
        ]);
        let crew = crew_of(&html);
        assert_eq!(crew.matches("class=\"crow wrow\"").count(), 2, "{crew}");
        // under L1, before the first lead
        assert!(pos(crew, 1) < pos(crew, 180) && pos(crew, 181) < pos(crew, 2));
        // who: level, project, harness; what: the brief's now, else the intent; the model chip
        assert!(crew.contains("L3 · unvrs-rs · Claude"));
        assert!(crew.contains(">porting the cards<") && crew.contains(">Intent of 181<"));
        assert!(crew.contains(">sonnet-5-5 · high<") && crew.contains(">1m<"));
        assert_eq!(row_of(crew, 180).matches("data-depth=\"0\"").count(), 1);
        assert!(!crew.contains(">?<"));
    }

    #[test]
    fn crew_tree_says_model_not_reported_instead_of_a_question_mark() {
        let mut quiet = w(40, 2, "brand", None);
        quiet["model"] = serde_json::Value::Null;
        quiet["effort"] = serde_json::Value::Null;
        let html = tree_html(vec![quiet]);
        let crew = crew_of(&html);
        assert!(crew.contains(">model not reported<"), "{crew}");
        assert!(!crew.contains(">?<") && !crew.contains("· ?"));
    }

    #[test]
    fn crew_tree_nests_workers_under_the_worker_that_started_them() {
        let html = tree_html(vec![
            w(20, 2, "brand", Some("top")),
            w(21, 20, "brand", Some("child")),
            w(22, 21, "brand", Some("grandchild")),
            w(30, 3, "travel", Some("other lead")),
            // an idle one hides in the lead's quiet line
            {
                let mut i = w(23, 2, "brand", None);
                i["state"] = "idle".into();
                i
            },
        ]);
        let crew = crew_of(&html);
        for (pid, depth) in [(20, 0), (21, 1), (22, 2), (30, 0)] {
            assert!(
                row_of(crew, pid).contains(&format!("data-depth=\"{depth}\"")),
                "{pid}"
            );
        }
        assert!(pos(crew, 2) < pos(crew, 20) && pos(crew, 20) < pos(crew, 21));
        assert!(pos(crew, 21) < pos(crew, 22) && pos(crew, 22) < pos(crew, 3));
        assert!(pos(crew, 3) < pos(crew, 30));
        assert!(!crew.contains("no seat"), "nested workers are not seatless");
        assert!(!crew.contains("data-pid=\"23\"") && crew.contains("1 idle"));
        // the indent is the style var the connector is drawn from
        assert!(crew.contains("--d:2"));
    }

    #[test]
    fn crew_tree_a_loop_of_parents_still_draws_each_worker_once() {
        let html = tree_html(vec![w(50, 51, "brand", None), w(51, 50, "brand", None)]);
        let crew = crew_of(&html);
        assert_eq!(crew.matches("data-pid=\"50\"").count(), 1, "{crew}");
        assert_eq!(crew.matches("data-pid=\"51\"").count(), 1);
    }

    #[test]
    fn crew_tree_caps_at_eight_worker_rows_and_links_to_the_diagram() {
        let html = tree_html((0..12).map(|i| w(100 + i, 2, "brand", None)).collect());
        let crew = crew_of(&html);
        assert_eq!(crew.matches("class=\"crow wrow\"").count(), 8, "{crew}");
        assert!(crew.contains("+4 more · open diagram"));
        // in order: the first eight stay
        assert!(crew.contains("data-pid=\"107\"") && !crew.contains("data-pid=\"108\""));
        // the cap is per zone, not per lead
        let html = tree_html(
            (0..6)
                .map(|i| w(100 + i, 2, "brand", None))
                .chain((0..6).map(|i| w(200 + i, 3, "travel", None)))
                .collect(),
        );
        let crew = crew_of(&html);
        assert_eq!(crew.matches("class=\"crow wrow\"").count(), 8);
        assert!(crew.contains("+4 more · open diagram"));
        // eight or fewer: no link
        let html = tree_html((0..8).map(|i| w(100 + i, 2, "brand", None)).collect());
        assert!(!crew_of(&html).contains("more · open diagram"));
    }

    #[test]
    fn crew_tree_changes_nothing_outside_the_crew_zone() {
        let without = tree_html(vec![]);
        let with = tree_html(
            (0..12)
                .map(|i| w(100 + i, 1 + i % 3, "brand", Some("busy")))
                .collect(),
        );
        // the Crew zone is cut out, and the cosmos's own worker count (a signal for the sky,
        // not a cockpit zone) is masked
        let strip = |h: &str| {
            let at = h.find("<section class=\"zone z-crew\"").unwrap();
            let end = at + h[at..].find("</section>").unwrap();
            let h = format!("{}{}", &h[..at], &h[end..]);
            h.replace("data-workers=\"8\"", "data-workers=\"0\"")
        };
        assert_ne!(crew_of(&without), crew_of(&with));
        assert_eq!(strip(&without), strip(&with));
        // the heading is still the diagram's button
        assert!(with.contains("id=\"crew-open\"") && !with.contains("id=\"diagram\""));
    }

    #[test]
    fn sim_is_asked_for_only_by_its_flag() {
        assert_eq!(wants_sim(Some("sim=problem")), Some("problem"));
        assert_eq!(wants_sim(Some("a=1&sim=calm")), Some("calm"));
        assert_eq!(wants_sim(Some("sim=problems")), None);
        assert_eq!(wants_sim(Some("simulate")), None);
        assert_eq!(wants_sim(None), None);
    }

    #[test]
    fn real_page_has_no_simulated_label_and_sim_page_does() {
        let html = render(real_model(), false);
        assert!(!html.contains("SIMULATED"));
        assert!(!html.contains("id=\"sim-label\""));
        assert!(html.contains("data-sim=\"off\""));
        let sim = build(&Value::Null, &Probes::default(), Some("problem"));
        let html = render(sim, false);
        assert!(html.contains("id=\"sim-label\""));
        assert!(html.contains("SIMULATED"));
        assert!(html.contains("data-sim=\"problem\""));
        let calm = build(&Value::Null, &Probes::default(), Some("calm"));
        assert_eq!(calm.status, "All quiet.");
        let html = render(calm, false);
        assert!(html.contains("id=\"sim-label\"") && html.contains("data-sim=\"calm\""));
    }

    #[test]
    fn every_need_has_answer_in_and_copy_and_no_open() {
        let sim = build(&Value::Null, &Probes::default(), Some("problem"));
        let shown = sim.needs.len().min(model::ZONE_MAX);
        let html = render(sim, false);
        assert_eq!(html.matches("class=\"need ").count(), shown);
        assert_eq!(html.matches("class=\"where\"").count(), shown);
        assert_eq!(html.matches("class=\"copy\"").count(), shown);
        assert!(
            !html.contains("class=\"open\""),
            "no Open without a proven link"
        );
        assert!(html.contains("more"), "+N more");
    }

    #[test]
    fn the_page_draws_the_claude_meters_with_their_age_the_sparkline_and_the_settled_drawer() {
        let sim = build(&Value::Null, &Probes::default(), Some("calm"));
        let html = render(sim, false);
        // one meter per Claude window, each with the reading's age
        assert!(
            html.contains("data-key=\"quota:claude:5h\"")
                && html.contains("data-key=\"quota:claude:7d\"")
        );
        assert_eq!(
            html.matches("SIMULATED fixture · 1m ago").count(),
            2,
            "{html}"
        );
        assert!(html.contains("class=\"spark\"") && html.matches("class=\"sb").count() == 12);
        // no reading: one grey meter that says unknown
        let html = render(real_model(), false);
        assert!(
            html.contains("data-key=\"quota:claude\"")
                && html.contains("class=\"gauge t-unknown\"")
        );
        assert!(html.contains(">unknown<"));
        assert!(
            !html.contains("settled"),
            "nothing settled: no settled button"
        );
        // a settled call: hidden from the list, counted on the settled button, in its drawer
        let mut m = real_model();
        let i = m.needs.iter().position(|n| n.id == "d1").unwrap();
        let mut n = m.needs.remove(i);
        n.settled = Some("Closed 1m ago (journal hold.close, status answered).".into());
        m.settled.push(n);
        let html = render(m.clone(), false);
        assert!(html.contains("1 settled"));
        assert!(
            !html.contains("data-id=\"d1\""),
            "the settled call left the list"
        );
        let html = dioxus_ssr::render_element(
            rsx! { Preview { model: m, hidden: false, drawer_key: "needs:settled" } },
        );
        assert!(
            html.contains("Settled · 1")
                && html.contains("class=\"evidence\"")
                && html.contains("hold.close")
        );
    }

    #[test]
    fn the_cockpit_toggle_and_its_hidden_state_render() {
        let html = render(real_model(), false);
        assert!(html.contains("id=\"cockpit-toggle\"") && html.contains("Hide cockpit"));
        assert!(html.contains("data-cockpit=\"shown\""));
        let html = render(real_model(), true);
        assert!(html.contains("data-cockpit=\"hidden\"") && html.contains("Show cockpit"));
        assert!(html.contains("inert"));
    }

    #[test]
    fn the_crew_heading_and_rows_open_a_diagram_of_cards_and_arrows() {
        use diagram::Kind;
        let sim = build(&Value::Null, &Probes::default(), Some("problem"));
        // closed: the heading is a button, the width input is there, no overlay
        let html = render(sim.clone(), false);
        assert!(html.contains("id=\"crew-open\"") && html.contains("id=\"dgm-vw\""));
        assert!(!html.contains("id=\"diagram\""));
        assert!(!html.contains(">?<"), "a bare ? in the cockpit");
        // open: one card per node, one arrow per edge, at the default 1280 px
        let d = diagram::of(&sim.crew, 1280.0);
        let kinds = |k: Kind| d.nodes.iter().filter(|n| n.card.kind == k).count();
        assert_eq!(kinds(Kind::L1), 1);
        assert!(kinds(Kind::L2) >= 2 && kinds(Kind::L3) >= 2, "{d:?}");
        let html = dioxus_ssr::render_element(
            rsx! { Preview { model: sim.clone(), diagram_key: Some(String::new()) } },
        );
        assert!(html.contains("id=\"diagram\"") && html.contains("aria-modal=\"true\""));
        let framed = kinds(Kind::Frame);
        assert_eq!(
            html.matches("class=\"node ").count(),
            d.nodes.len() - framed
        );
        assert_eq!(html.matches("class=\"dgm-frame\"").count(), framed);
        // no "?" anywhere: unknown model or effort is left out or said in words
        assert!(!html.contains(">?<"), "a bare ? in the diagram");
        assert_eq!(html.matches("class=\"edge").count(), d.edges.len());
        assert_eq!(
            html.matches("marker-end=\"url(#dgm-arrow").count(),
            d.edges.len()
        );
        assert!(
            !html.contains(" focus\""),
            "opened from the heading: no card singled out"
        );
        // each card is positioned, names level · project · harness, and opens its drawer
        for n in &d.nodes {
            let c = &n.card;
            assert!(
                html.contains(&format!(
                    "translate({:.1}px, {:.1}px); width: {:.1}px",
                    n.x, n.y, n.w
                )),
                "{} placed",
                c.key
            );
            if c.drawer.is_some() {
                assert!(
                    html.contains(&format!("title=\"{} · ", c.head)),
                    "{}",
                    c.head
                );
            }
        }
        // L1 sits above every lead, every lead above its workers
        let l1 = d.nodes.iter().find(|n| n.card.kind == Kind::L1).unwrap();
        assert!(
            d.nodes
                .iter()
                .all(|n| n.card.kind == Kind::L1 || n.y > l1.y)
        );
        // opened from a row: that card is singled out
        let w = d.nodes.iter().find(|n| n.card.kind == Kind::L3).unwrap();
        let html = dioxus_ssr::render_element(
            rsx! { Preview { model: sim, diagram_key: Some(w.card.key.clone()) } },
        );
        assert_eq!(html.matches(" focus\"").count(), 1);
        let at = html.find(" focus\"").unwrap();
        let tag = &html[at..at + html[at..].find('>').unwrap()];
        assert!(
            tag.contains(&format!("data-key=\"{}\"", w.card.key)),
            "{tag}"
        );
    }

    /// Every `border-radius` in a stylesheet but 0 and the 50% of a round status light
    /// (its circle is the "ok" shape; warn is a diamond, bad a square).
    fn radii(css: &str) -> Vec<&str> {
        css.match_indices("border-radius:")
            .map(|(i, m)| {
                css[i + m.len()..]
                    .split([';', '}'])
                    .next()
                    .unwrap_or("")
                    .trim()
            })
            .filter(|v| *v != "0" && *v != "50%")
            .collect()
    }

    #[test]
    fn no_rounded_corners_and_the_sky_fills_the_screen() {
        assert_eq!(radii(PREVIEW_CSS), Vec::<&str>::new(), "preview.css");
        assert_eq!(radii(SETTINGS_CSS), Vec::<&str>::new(), "settings.css");
        assert_eq!(
            radii(include_str!("../style.css")),
            Vec::<&str>::new(),
            "style.css"
        );
        let round: Vec<&str> = PREVIEW_CSS
            .lines()
            .filter(|l| l.contains("border-radius: 50%"))
            .map(|l| l.split('{').next().unwrap_or("").trim())
            .collect();
        assert_eq!(
            round,
            [".status .hb", ".dot"],
            "only the status lights are round"
        );
        assert!(PREVIEW_CSS.contains(".window { position: fixed; inset: 0;"));
        assert!(include_str!("../style.css").contains("--frame: 0px;"));
        // the renderer draws no window frame, hull or planet, and flies through stars
        let cosmos = include_str!("../../cosmos/cosmos.rs");
        assert!(!cosmos.contains("windowSdf") && !cosmos.contains("hull plating"));
        assert!(
            cosmos.contains("gl_u4f(l[10], 1.0, 0.4, 0.0, 0.0)"),
            "planet off"
        );
        assert!(cosmos.contains("const FLY_VS") && cosmos.contains("gl_draw(1, 0, 4, FLY_N)"));
    }

    /// Context ownership: the zone lists each checked task with its verdict and first
    /// leak, and its drawer shows what UNVRS holds (brief, record, deliverables,
    /// learnings) and each leak.
    #[test]
    fn context_zone_shows_each_check_and_its_drawer_what_unvrs_holds() {
        let now = now_ms();
        let host = Host {
            ownership: Some(vec![
                sim::ownership_check(now, 7, "codex", "accepted", &[]),
                sim::ownership_check(
                    now,
                    8,
                    "claude",
                    "bounced",
                    &["deliverable outside UNVRS: ~/Downloads/r.md"],
                ),
                sim::ownership_check(
                    now,
                    9,
                    "claude",
                    "blocked",
                    &["handoff missing: end with a HANDOFF block"],
                ),
            ]),
            ..Default::default()
        };
        let snap = serde_json::json!({"at": now, "kernel": {"version": "t", "uptime_s": 60, "os_pid": 1, "home": "/nowhere"}, "seats": [], "workers": [], "quota": []});
        let m = Model::build(Inputs {
            snap: &snap,
            host: &host,
            now_ms: now,
            sim: false,
        });
        // bounced counts every task the check ever sent back (the blocked one too)
        assert_eq!(
            (m.context.passed, m.context.bounced, m.context.blocked),
            (1, 2, 1)
        );
        let html = render(m.clone(), false);
        assert!(html.contains("id=\"h-context\""), "{html}");
        assert!(html.contains("1 passed · 2 bounced · 1 blocked"));
        assert!(
            html.contains("data-key=\"ctx:8\"")
                && html.contains("deliverable outside UNVRS: ~/Downloads/r.md")
        );
        let d = m.detail("ctx:7").unwrap();
        let labels: Vec<&str> = d.facts.iter().map(|f| f.label.as_str()).collect();
        for l in [
            "Check",
            "Brief",
            "Record",
            "Deliverables",
            "Decisions",
            "Learnings",
            "Notes filed",
        ] {
            assert!(labels.contains(&l), "{l} in {labels:?}");
        }
        assert_eq!(
            d.facts
                .iter()
                .find(|f| f.label == "Learnings")
                .unwrap()
                .value,
            "none (said explicitly)"
        );
        assert!(m.detail("ctx:9").unwrap().reason.contains("ended blocked"));
        let open = dioxus_ssr::render_element(
            rsx! { Preview { model: m, drawer_key: "ctx:8".to_string() } },
        );
        assert!(
            open.contains("PID 8 · context ownership")
                && open.contains("Leaks: deliverable outside UNVRS"),
            "{open}"
        );
        let empty = render(real_model(), false);
        assert!(empty.contains("Task folders unreadable"));
    }

    #[test]
    fn the_shell_mounts_the_galaxy_and_the_right_socket() {
        let p = page(None, "", "");
        assert!(p.contains("id=\"cosmos\"") && p.contains("class=\"scrim\""));
        assert!(p.contains("/preview/ws\"") && !p.contains("SIMULATED"));
        let p = page(Some("problem"), "", "");
        assert!(p.contains("/preview/ws?sim=problem") && p.contains("SIMULATED"));
        let p = page(Some("calm"), "", "");
        assert!(p.contains("/preview/ws?sim=calm") && p.contains("SIMULATED"));
    }
}
