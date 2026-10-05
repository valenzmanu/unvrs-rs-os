//! `hot_set(pid, rank)` (memory-layer §13 R1, mapp-unvrs §7): one function serves a
//! seat's rehydrate payload, the seat-move package and an L3's task package. After
//! that, each prompt gets only the delta: new wakes and changed decisions.
use super::{Inner, Kernel, now_ms, state::PidRec, state::clip};
use crate::MemStore;
use anyhow::Result;

/// Rehydrate budget in bytes. Claude Code keeps hook context near 10k characters and
/// Codex spills past its per-hook limit; the payload stays under both.
pub const HOT_BYTES: usize = 9000;
const BRIEF_CAP: usize = 2400;
const MEMORY_CAP: usize = 2200;
const CALLS_CAP: usize = 1200;
const WAKES_CAP: usize = 1600;
const PROJECTS_CAP: usize = 1200;
const SOURCES_CAP: usize = 900;
const WORK_CAP: usize = 700;

struct Section {
    name: &'static str,
    body: String,
}

fn brief_body(inner: &Inner, pid: usize) -> String {
    let Ok(s) = inner.mem.session(pid) else {
        return String::new();
    };
    let text = if s.brief.is_empty() {
        s.summary.clone()
    } else {
        let mut t = format!("(brief v{}", s.brief_version);
        if s.brief_mark < s.tail.len() {
            t.push_str("; the recent turns below are newer and win where they differ");
        }
        t.push_str(")\n");
        t.push_str(&s.brief.text());
        t
    };
    clip(&text, BRIEF_CAP)
}

fn calls_body(inner: &Inner, rec: &PidRec) -> String {
    let now = now_ms();
    let mut holds: Vec<_> = inner
        .st
        .holds
        .values()
        .filter(|h| h.status == "open")
        .filter(|h| rec.rank == 1 || h.project == rec.project)
        .collect();
    holds.sort_by_key(|h| h.created);
    let mut out = String::new();
    for h in holds {
        out.push_str(&format!(
            "- {} ({}{}, open {}): {}{}\n",
            h.id,
            h.kind,
            h.project
                .as_ref()
                .map(|p| format!(", {p}"))
                .unwrap_or_default(),
            age(now.saturating_sub(h.created)),
            h.question,
            if h.options.is_empty() {
                String::new()
            } else {
                format!(" [{}]", h.options.join(" / "))
            }
        ));
    }
    clip(&out, CALLS_CAP)
}

pub fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn wakes_body(rec: &PidRec) -> String {
    let mut out = String::new();
    for w in rec.wakes.iter().filter(|w| !w.acked) {
        out.push_str(&format!(
            "- [w{}] {}: {}{}\n",
            w.id,
            w.kind,
            w.text,
            w.reference
                .as_ref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ));
    }
    clip(&out, WAKES_CAP)
}

fn projects_body(k: &Kernel, inner: &Inner) -> String {
    let mut out = String::new();
    for id in k.project_ids() {
        let p = k.project(&id).unwrap_or_default();
        let lead = inner
            .st
            .seat_of(&id)
            .and_then(|s| inner.st.pids.get(&s))
            .map(|r| {
                let wakes = r.wakes.iter().filter(|w| !w.acked).count();
                format!(
                    "L2 PID {} {}{}",
                    r.pid,
                    if r.thread.is_some() {
                        "in a thread"
                    } else {
                        "free"
                    },
                    if wakes > 0 {
                        format!(", {wakes} wake(s)")
                    } else {
                        String::new()
                    }
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "- {id}: {} · {lead}{}\n",
            clip(&p.purpose, 140),
            if p.sources.is_empty() {
                String::new()
            } else {
                format!(" · sources {}", p.sources.join(", "))
            }
        ));
    }
    clip(&out, PROJECTS_CAP)
}

/// A short map of sources: id, purpose and, for local folders, the entry points.
fn sources_body(k: &Kernel, ids: Option<&[String]>) -> String {
    let Ok(reg) = crate::Sources::load(k.u.root()) else {
        return String::new();
    };
    let mut out = String::new();
    for s in reg.list() {
        if let Some(ids) = ids
            && !ids.contains(&s.id)
            && s.id != "memory"
        {
            continue;
        }
        let mut entries = vec![];
        if s.kind == "path"
            && let Ok(root) = reg.root(&s.id)
        {
            for f in ["CONTEXT-MAP.md", "CONTEXT.md", "AGENTS.md", "README.md"] {
                if root.join(f).is_file() {
                    entries.push(f.to_owned());
                }
            }
        }
        out.push_str(&format!(
            "- {} ({}): {}{}\n",
            s.id,
            s.kind,
            clip(&s.purpose, 120),
            if entries.is_empty() {
                String::new()
            } else {
                format!(" · start at {}", entries.join(", "))
            }
        ));
    }
    clip(&out, SOURCES_CAP)
}

fn work_body(inner: &Inner, rec: &PidRec) -> String {
    let now = now_ms();
    let mut out = String::new();
    for p in
        inner.st.pids.values().filter(|p| {
            p.rank == 3 && p.state == "working" && (rec.rank == 1 || p.parent == rec.pid)
        })
    {
        let intent = p
            .contract
            .as_ref()
            .and_then(|c| c["intent"].as_str())
            .unwrap_or(&p.task);
        out.push_str(&format!(
            "- L3 PID {} on {} for {} · {} · {}\n",
            p.pid,
            p.harness,
            p.project.as_deref().unwrap_or("-"),
            clip(intent, 100),
            age(now.saturating_sub(p.created))
        ));
    }
    clip(&out, WORK_CAP)
}

fn tail_body(inner: &Inner, pid: usize, budget: usize) -> String {
    let Ok(s) = inner.mem.session(pid) else {
        return String::new();
    };
    let mut picked = vec![];
    let mut used = 0;
    for turn in s.tail.iter().rev() {
        let cost = turn.len() + 1;
        if used + cost > budget {
            if picked.is_empty() && budget > 200 {
                let mut start = turn.len() - (budget - 20).min(turn.len());
                while !turn.is_char_boundary(start) {
                    start += 1;
                }
                picked.push(format!("…{}", &turn[start..]));
            }
            break;
        }
        used += cost;
        picked.push(turn.clone());
    }
    picked.reverse();
    picked.join("\n")
}

fn sections(k: &Kernel, inner: &Inner, pid: usize) -> Result<Vec<Section>> {
    let rec = inner.st.pid(pid)?.clone();
    let project = rec.project.as_ref().and_then(|p| k.project(p));
    let mut out = vec![];
    let seat = match (rec.rank, &rec.project) {
        (1, _) => "L1".to_owned(),
        (2, Some(p)) => format!("L2 · project {p}"),
        (r, p) => format!(
            "L{r}{}",
            p.as_ref()
                .map(|p| format!(" · project {p}"))
                .unwrap_or_default()
        ),
    };
    let driven = rec.kind == "driven" || rec.driven.as_ref().is_some_and(|d| d.busy);
    out.push(Section {
        name: "Identity",
        body: format!(
            "UNVRS · PID {} · {seat} · mapp {}\n{}\n{}",
            rec.pid,
            rec.mapp,
            k.mapp.role(
                rec.rank,
                rec.project.as_deref(),
                project.as_ref().map(|p| p.purpose.as_str())
            ),
            k.mapp.ops(rec.rank, &super::unvrs_cmd(), driven)
        ),
    });
    if rec.rank == 3 {
        if let Some(c) = &rec.contract {
            out.push(Section {
                name: "Task contract",
                body: format!(
                    "authority: {}\nintent (the captain's words): {}\ngo (captain quote): {}\ndone when: {}\ninherited scope: {}\nspec: {}\nshape: {}\nsources you may read: {}",
                    c["authority"].as_str().unwrap_or("implement"),
                    c["intent"].as_str().unwrap_or(""),
                    c["go_quote"].as_str().or_else(|| c["go"].as_str()).unwrap_or(""),
                    c["done_when"].as_str().unwrap_or(""),
                    c["go_scope"].as_str().unwrap_or("this task's done-when"),
                    c["spec"].as_str().unwrap_or(""),
                    c["shape"].as_str().unwrap_or("report"),
                    c["sources"]
                        .as_array()
                        .map(|a| a
                            .iter()
                            .filter_map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", "))
                        .unwrap_or_default()
                ),
            });
        }
        let brief = brief_body(inner, pid);
        out.push(Section {
            name: "Task package",
            body: if brief.trim().is_empty() {
                format!("goal: {}", rec.task)
            } else {
                brief
            },
        });
        let ids: Vec<String> = rec
            .contract
            .as_ref()
            .and_then(|c| c["sources"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect();
        let src = sources_body(k, Some(&ids));
        if !src.is_empty() {
            out.push(Section {
                name: "Sources",
                body: src,
            });
        }
        let used: usize = out.iter().map(|s| s.body.len() + s.name.len() + 8).sum();
        let tail = tail_body(inner, pid, HOT_BYTES.saturating_sub(used + 300).min(3500));
        if !tail.is_empty() {
            out.push(Section {
                name: "Recent turns",
                body: tail,
            });
        }
        return Ok(out);
    }
    let calls = calls_body(inner, &rec);
    if !calls.is_empty() {
        out.push(Section {
            name: "Captain's calls (open decisions)",
            body: calls,
        });
    }
    let wakes = wakes_body(&rec);
    if !wakes.is_empty() {
        out.push(Section {
            name: "Wakes (handle these)",
            body: wakes,
        });
    }
    let memory = match &rec.project {
        Some(p) if rec.rank == 2 => MemStore::project(k.u.root(), p).hot(MEMORY_CAP, now_ms()),
        _ => MemStore::captain(k.u.root()).hot(MEMORY_CAP, now_ms()),
    };
    out.push(Section {
        name: if rec.rank == 1 {
            "Captain memory (never make the captain repeat these)"
        } else {
            "Project memory"
        },
        body: if memory.is_empty() {
            "(none yet)".into()
        } else {
            memory
        },
    });
    if rec.rank == 1 {
        let projects = projects_body(k, inner);
        out.push(Section {
            name: "Projects",
            body: if projects.is_empty() {
                "(none yet)".into()
            } else {
                projects
            },
        });
        if let Some(a) = &inner.st.away {
            out.push(Section {
                name: "Away",
                body: format!(
                    "The captain is away: \"{}\" (runs {} of {}).",
                    a.words, a.runs, a.cap
                ),
            });
        }
    }
    let src = match &project {
        Some(p) => sources_body(k, Some(&p.sources)),
        None => sources_body(k, None),
    };
    if !src.is_empty() {
        out.push(Section {
            name: "Sources (search before saying you do not know)",
            body: src,
        });
    }
    let work = work_body(inner, &rec);
    if !work.is_empty() {
        out.push(Section {
            name: "Under way",
            body: work,
        });
    }
    let brief = brief_body(inner, pid);
    if !brief.trim().is_empty() {
        out.push(Section {
            name: "Brief",
            body: brief,
        });
    }
    let used: usize = out.iter().map(|s| s.body.len() + s.name.len() + 8).sum();
    let budget = HOT_BYTES.saturating_sub(used + 300).min(3500);
    let tail = tail_body(inner, pid, budget);
    if !tail.trim().is_empty() {
        out.push(Section {
            name: "Recent turns",
            body: tail,
        });
    }
    Ok(out)
}

fn render(sections: &[Section], inner: &Inner) -> String {
    let mut text = String::new();
    for s in sections {
        if s.name == "Identity" {
            text.push_str(&s.body);
            text.push('\n');
        } else {
            text.push_str(&format!("## {}\n{}\n", s.name, s.body.trim_end()));
        }
    }
    clip(&inner.mem.redact(&text), HOT_BYTES)
}

/// Full hot set text for a PID.
pub fn hot_text(k: &Kernel, inner: &Inner, pid: usize) -> Result<String> {
    Ok(render(&sections(k, inner, pid)?, inner))
}

fn holds_signature(inner: &Inner, rec: &PidRec) -> String {
    calls_body(inner, rec)
}

/// The thread has seen everything as of now: wakes count as delivered to it.
pub fn mark_seen(inner: &mut Inner, key: &str, pid: usize) {
    let Ok(rec) = inner.st.pid(pid).cloned() else {
        return;
    };
    let sig = holds_signature(inner, &rec);
    if let Some(t) = inner.st.threads.get_mut(key) {
        t.seen_notes = sig;
    }
    if let Ok(r) = inner.st.pid_mut(pid) {
        for w in r.wakes.iter_mut().filter(|w| !w.acked) {
            w.delivered = Some(key.into());
        }
        for m in &mut r.mailbox {
            m.read = true;
        }
    }
}

/// Text for a kernel-driven run with `$unvrs:<verb>` mentions quieted to `unvrs:<verb>`.
/// Codex expands a `$name` mention anywhere in a prompt into that skill, so a wake
/// that quotes a captain command (e.g. `$unvrs:answer d3 …`) pulled the entry skill
/// into a detached run, which then answered about the skill instead of the work.
pub fn quiet_mentions(text: &str) -> String {
    text.replace("$unvrs:", "unvrs:")
}

/// New wakes (marked delivered to this thread) as text, or None.
pub fn wake_text(k: &Kernel, inner: &mut Inner, key: &str, pid: usize) -> Option<String> {
    let rec = inner.st.pid_mut(pid).ok()?;
    let mut lines = vec![];
    for w in rec
        .wakes
        .iter_mut()
        .filter(|w| !w.acked && w.delivered.is_none())
    {
        w.delivered = Some(key.into());
        lines.push(format!(
            "- [w{}] {}: {}{}",
            w.id,
            w.kind,
            w.text,
            w.reference
                .as_ref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ));
    }
    if lines.is_empty() {
        return None;
    }
    let ids: Vec<u64> = rec
        .wakes
        .iter()
        .filter(|w| w.delivered.as_deref() == Some(key) && !w.acked)
        .map(|w| w.id)
        .collect();
    k.event(
        inner,
        "wake.deliver",
        serde_json::json!({"pid": pid, "wakes": ids, "to": key}),
    );
    Some(format!(
        "UNVRS wakes for PID {pid} (handle them; they are acknowledged when this turn ends):\n{}",
        lines.join("\n")
    ))
}

/// Only what changed since the thread's last turn: new wakes and changed decisions.
pub fn delta(k: &Kernel, inner: &mut Inner, key: &str, pid: usize) -> Option<String> {
    let rec = inner.st.pid(pid).ok()?.clone();
    let mut parts = vec![];
    if let Some(w) = wake_text(k, inner, key, pid) {
        parts.push(w);
    }
    let sig = holds_signature(inner, &rec);
    let seen = inner
        .st
        .threads
        .get(key)
        .map(|t| t.seen_notes.clone())
        .unwrap_or_default();
    if sig != seen {
        parts.push(format!(
            "## Captain's calls (updated)\n{}",
            if sig.is_empty() {
                "(none open)".into()
            } else {
                sig.clone()
            }
        ));
        if let Some(t) = inner.st.threads.get_mut(key) {
            t.seen_notes = sig;
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(inner.mem.redact(&parts.join("\n")))
}
