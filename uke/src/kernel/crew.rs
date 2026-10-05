//! The crew's mechanics (mapp-unvrs §2, §12): projects and their seats, holds
//! (decisions and proposals, D46), wake queues and their delivery (D45), away mode
//! (D48), detached seat runs (C9) and the digest's facts (D51).
use super::{Inner, Kernel, TurnRequest, hot, now_ms, state::Hold, state::clip};
use crate::MemStore;
use crate::signals::CommandTracking;
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, sync::Arc, thread};

/// `projects/<id>/project.toml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub purpose: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub created_by: String,
}

pub fn valid_project_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 40
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !id.starts_with('-'),
        "Project id must be 1–40 characters of a-z, 0-9 and -"
    );
    Ok(())
}

impl Kernel {
    pub(crate) fn project_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(self.u.root().join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("project.toml").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }
    pub(crate) fn project(&self, id: &str) -> Option<Project> {
        let text = fs::read_to_string(self.u.project_dir(id).join("project.toml")).ok()?;
        toml::from_str(&text).ok()
    }

    /// Creates the project record and its L2 seat. Only the captain's path (a direct
    /// `$unvrs:project new`, or approving a proposal) reaches this.
    pub(crate) fn create_project(
        &self,
        inner: &mut Inner,
        id: &str,
        purpose: &str,
        sources: &[String],
        by: &str,
    ) -> Result<usize> {
        valid_project_id(id)?;
        ensure!(self.project(id).is_none(), "Project {id} already exists");
        ensure!(!purpose.trim().is_empty(), "A project needs a purpose");
        let registry = crate::Sources::load(self.u.root())?;
        for s in sources {
            ensure!(
                registry.get(s).is_some(),
                "Unknown source {s:?}; register it first (unvrs sources add …)"
            );
        }
        let dir = self.u.project_dir(id);
        fs::create_dir_all(dir.join("memory/notes"))?;
        fs::create_dir_all(dir.join("notes"))?;
        fs::create_dir_all(dir.join("tasks"))?;
        let p = Project {
            id: id.into(),
            purpose: purpose.trim().into(),
            sources: sources.to_vec(),
            created: now_ms(),
            created_by: by.into(),
            ..Default::default()
        };
        fs::write(dir.join("project.toml"), toml::to_string(&p)?)?;
        let l1 = match inner.st.l1() {
            Some(l1) => l1,
            None => self.create_l1(inner),
        };
        let seat = inner.st.create(
            l1,
            2,
            "attached",
            "",
            &format!("lead {id}"),
            Default::default(),
            None,
        );
        inner.st.pid_mut(seat)?.project = Some(id.into());
        self.event(
            inner,
            "project.create",
            json!({"project": id, "pid": seat, "purpose": clip(purpose, 200), "sources": sources, "by": by}),
        );
        self.save(inner);
        Ok(seat)
    }

    pub(crate) fn projects_text(&self, inner: &Inner) -> String {
        let ids = self.project_ids();
        if ids.is_empty() {
            return "No projects yet. L1 can propose one; you can create one with $unvrs:project new <id> <purpose>.".into();
        }
        let mut out = String::from("Projects (type $unvrs:l2 <id> to lead one here):\n");
        for id in ids {
            let p = self.project(&id).unwrap_or_default();
            let seat = inner.st.seat_of(&id);
            let state = seat
                .and_then(|s| inner.st.pids.get(&s))
                .map(|r| match &r.thread {
                    Some(t) => format!(
                        "led in a {} thread",
                        inner
                            .st
                            .threads
                            .get(t)
                            .map(|t| t.app.as_str())
                            .unwrap_or("")
                    ),
                    None => "seat free".into(),
                })
                .unwrap_or_else(|| "no seat".into());
            let calls = inner
                .st
                .holds
                .values()
                .filter(|h| h.status == "open" && h.project.as_deref() == Some(id.as_str()))
                .count();
            out.push_str(&format!(
                "- {id} · {} · {state}{}\n",
                clip(&p.purpose, 120),
                if calls > 0 {
                    format!(" · {calls} open call(s)")
                } else {
                    String::new()
                }
            ));
        }
        out
    }

    // ───────────── holds ─────────────

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_hold(
        &self,
        inner: &mut Inner,
        kind: &str,
        question: &str,
        options: Vec<String>,
        until: Option<u64>,
        project: Option<String>,
        by: Option<usize>,
        action: Option<Value>,
    ) -> Result<String> {
        ensure!(
            !question.trim().is_empty() && question.len() <= 2000,
            "A decision needs a question (≤ 2000 bytes)"
        );
        inner.st.hold_seq += 1;
        let id = format!("d{}", inner.st.hold_seq);
        inner.st.holds.insert(
            id.clone(),
            Hold {
                id: id.clone(),
                kind: kind.into(),
                question: question.trim().into(),
                options,
                until,
                project: project.clone(),
                by,
                created: now_ms(),
                status: "open".into(),
                answer: None,
                closed_at: None,
                action,
                ..Default::default()
            },
        );
        self.event(
            inner,
            "hold.open",
            json!({"hold": id, "kind": kind, "question": clip(question, 300), "project": project, "pid": by}),
        );
        self.notify(inner, &format!("Your call: {}", clip(question, 120)), None);
        Ok(id)
    }

    /// Closes a hold with the captain's exact words; the asking seat gets them as a wake.
    /// The captain's words also overrule a moot close (the call was mooted by evidence,
    /// not decided).
    pub(crate) fn close_hold(
        &self,
        inner: &mut Inner,
        id: &str,
        status: &str,
        words: &str,
    ) -> Result<String> {
        let h = inner
            .st
            .holds
            .get_mut(id)
            .with_context(|| format!("No decision {id}"))?;
        ensure!(
            matches!(h.status.as_str(), "open" | "moot"),
            "Decision {id} is already {}",
            h.status
        );
        ensure!(!words.trim().is_empty(), "An answer needs your words");
        let was_moot = h.status == "moot";
        h.status = status.into();
        h.answer = Some(words.into());
        h.closed_at = Some(now_ms());
        h.closed_by = None;
        let h = h.clone();
        if let Some(by) = h.by
            && inner.st.pids.contains_key(&by)
        {
            let _ = inner.st.wake(
                by,
                "answered",
                &format!(
                    "The captain answered {id} ({}): \"{words}\"",
                    clip(&h.question, 200)
                ),
                None,
                Some(id.into()),
            );
        }
        self.event(
            inner,
            "hold.close",
            json!({"hold": id, "status": status, "words": words, "project": h.project, "question": clip(&h.question, 200),
                "overrules_moot": was_moot}),
        );
        Ok(format!(
            "Recorded your words on {id} ({}): \"{words}\"",
            clip(&h.question, 200)
        ))
    }

    /// Who may close a call as moot (D46 "or evidence it became moot"): the seat that
    /// raised it, L1, or the captain (`by == None`).
    pub(crate) fn may_moot(inner: &Inner, h: &Hold, by: Option<usize>) -> bool {
        match by {
            None => true,
            Some(p) => h.by == Some(p) || inner.st.pids.get(&p).is_some_and(|r| r.rank == 1),
        }
    }

    /// Closes an open call as moot with cited evidence. Nothing is approved or run: a
    /// moot close retires a question, it grants no authority (D48). The raiser gets a
    /// wake when someone else closed its call; the digest shows the close and how to
    /// reopen it.
    pub(crate) fn moot_hold(
        &self,
        inner: &mut Inner,
        id: &str,
        evidence: &str,
        by: Option<usize>,
    ) -> Result<String> {
        let evidence = evidence.trim();
        ensure!(
            !evidence.is_empty() && evidence.len() <= 2000,
            "A moot close needs evidence (a ref, commit or fact, ≤ 2000 bytes): moot {id} --evidence \"…\""
        );
        let h = inner
            .st
            .holds
            .get(id)
            .with_context(|| format!("No decision {id}"))?;
        ensure!(h.status == "open", "Decision {id} is already {}", h.status);
        if !Self::may_moot(inner, h, by) {
            let raiser = h.by;
            self.event(
                inner,
                "refused",
                json!({"verb": "moot", "hold": id, "pid": by, "raiser": raiser,
                    "reason": "only the seat that raised a call, L1 or the captain may close it as moot"}),
            );
            bail!(
                "Refused: only the seat that raised {id}{}, L1 or the captain may close it as moot. Send the evidence to L1 instead.",
                raiser.map(|r| format!(" (PID {r})")).unwrap_or_default()
            );
        }
        let h = inner.st.holds.get_mut(id).expect("checked above");
        h.status = "moot".into();
        h.evidence = Some(evidence.into());
        h.closed_by = by;
        h.closed_at = Some(now_ms());
        let h = h.clone();
        let who = by.map_or_else(|| "the captain".to_owned(), |p| format!("PID {p}"));
        if let Some(raiser) = h.by
            && by != Some(raiser)
            && inner.st.pids.contains_key(&raiser)
        {
            let _ = inner.st.wake(
                raiser,
                "answered",
                &format!(
                    "{who} closed your call {id} ({}) as moot: {evidence}",
                    clip(&h.question, 200)
                ),
                None,
                Some(id.into()),
            );
        }
        self.event(
            inner,
            "hold.close",
            json!({"hold": id, "status": "moot", "evidence": evidence, "by": by,
                "project": h.project, "question": clip(&h.question, 200)}),
        );
        Ok(format!(
            "Closed {id} ({}) as moot: {evidence}. It leaves the open calls; the next digest shows it and the captain can reopen it with $unvrs:reopen {id}.",
            clip(&h.question, 200)
        ))
    }

    /// Captain only: a moot call becomes open again (a wrong moot is one command away
    /// from undone). Answered and approved calls stay closed.
    pub(crate) fn reopen_hold(&self, inner: &mut Inner, id: &str) -> Result<String> {
        let h = inner
            .st
            .holds
            .get_mut(id)
            .with_context(|| format!("No decision {id}"))?;
        ensure!(
            h.status == "moot",
            "Only a moot call reopens; {id} is {}",
            h.status
        );
        let evidence = h.evidence.take();
        h.status = "open".into();
        h.closed_by = None;
        h.closed_at = None;
        let h = h.clone();
        if let Some(by) = h.by
            && inner.st.pids.contains_key(&by)
        {
            let _ = inner.st.wake(
                by,
                "note",
                &format!(
                    "The captain reopened {id} ({}); it is open again.",
                    clip(&h.question, 200)
                ),
                None,
                Some(id.into()),
            );
        }
        self.event(
            inner,
            "hold.reopen",
            json!({"hold": id, "was": evidence, "project": h.project, "question": clip(&h.question, 200)}),
        );
        Ok(format!(
            "Reopened {id} ({}); it is back among your open calls.",
            clip(&h.question, 200)
        ))
    }

    pub(crate) fn approve(&self, inner: &mut Inner, id: &str) -> Result<String> {
        let h = inner
            .st
            .holds
            .get(id)
            .with_context(|| format!("No decision {id}"))?
            .clone();
        ensure!(
            matches!(h.status.as_str(), "open" | "moot"),
            "Decision {id} is already {}",
            h.status
        );
        let mut done = String::new();
        if let Some(action) = &h.action
            && let Some(p) = action.get("create_project")
        {
            let pid = p["id"].as_str().unwrap_or("");
            let sources: Vec<String> = p["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect();
            let seat = self.create_project(
                inner,
                pid,
                p["purpose"].as_str().unwrap_or(""),
                &sources,
                &format!("captain approved {id}"),
            )?;
            done = format!(
                " Project {pid} created with its lead seat (PID {seat}); type $unvrs:l2 {pid} to lead it."
            );
        }
        let text = self.close_hold(inner, id, "approved", "approve")?;
        Ok(format!("Approved {id}.{done}\n{text}"))
    }

    // ───────────── away ─────────────

    pub(crate) fn set_away(
        &self,
        inner: &mut Inner,
        words: Option<(String, u32)>,
    ) -> Result<String> {
        match words {
            Some((w, cap)) => {
                ensure!(
                    !w.trim().is_empty(),
                    "Away needs your words (what L1 may do while you are away)"
                );
                inner.st.away = Some(super::Away {
                    words: w.clone(),
                    cap,
                    runs: 0,
                    since: now_ms(),
                });
                self.event(inner, "away", json!({"words": w, "cap": cap}));
                Ok(format!(
                    "Away recorded in your words: \"{w}\". L1 may run on its own at most {cap} time(s); authority does not widen. $unvrs:away off when you are back."
                ))
            }
            None => {
                let Some(a) = inner.st.away.take() else {
                    return Ok("You were not away.".into());
                };
                self.event(inner, "back", json!({"runs": a.runs}));
                let facts = self.digest_facts(inner, a.since);
                Ok(format!(
                    "Welcome back. While you were away L1 ran {} time(s).\n{}",
                    a.runs,
                    self.mapp.digest(&facts)
                ))
            }
        }
    }

    // ───────────── digest ─────────────

    pub(crate) fn digest_facts(&self, inner: &Inner, since: u64) -> Value {
        let now = now_ms();
        let mut holds: Vec<&Hold> = inner
            .st
            .holds
            .values()
            .filter(|h| h.status == "open")
            .collect();
        holds.sort_by_key(|h| h.created);
        let calls: Vec<Value> = holds
            .iter()
            .map(|h| {
                json!({"id": h.id, "kind": h.kind, "question": h.question, "options": h.options,
                    "age_s": now.saturating_sub(h.created) / 1000, "project": h.project,
                    "overdue": h.until.is_some_and(|u| now > u)})
            })
            .collect();
        let delivered: Vec<Value> = inner
            .recent
            .iter()
            .filter(|e| e["at"].as_u64().unwrap_or(0) >= since)
            .filter(|e| {
                matches!(
                    e["kind"].as_str().unwrap_or(""),
                    "result"
                        | "outcome"
                        | "hold.close"
                        | "hold.reopen"
                        | "project.create"
                        | "remember"
                )
            })
            .filter_map(|e| {
                self.mapp
                    .event_text(e["kind"].as_str().unwrap_or(""), e)
                    .map(|t| json!({"at": e["at"], "project": e["project"], "text": t}))
            })
            .collect();
        let under_way: Vec<Value> = inner
            .st
            .pids
            .values()
            .filter(|p| p.state == "working" || inner.running.contains(&p.pid))
            .map(|p| {
                json!({"pid": p.pid, "rank": p.rank, "project": p.project,
                    "intent": p.contract.as_ref().and_then(|c| c["intent"].as_str()).unwrap_or(&p.task),
                    "harness": p.harness, "elapsed_s": now.saturating_sub(p.created) / 1000})
            })
            .collect();
        let next: Vec<Value> = inner
            .st
            .seats()
            .iter()
            .filter_map(|s| {
                let brief = inner.mem.session(s.pid).ok()?.brief;
                let n = brief.next.first()?.clone();
                Some(json!({"project": s.project, "rank": s.rank, "next": n}))
            })
            .collect();
        json!({"calls": calls, "delivered": delivered, "under_way": under_way, "next": next,
            "away": inner.st.away, "since": since})
    }

    pub(crate) fn digest_text(&self, inner: &Inner, _captain: bool) -> String {
        let since = now_ms().saturating_sub(24 * 3600 * 1000);
        self.mapp.digest(&self.digest_facts(inner, since))
    }

    // ───────────── Observatory ─────────────

    pub(crate) fn observatory_url(&self) -> String {
        let port = std::env::var("UNVRS_OBSERVATORY_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(7576);
        if port == 80 {
            "http://unvrs.localhost".into()
        } else {
            format!("http://unvrs.localhost:{port}")
        }
    }

    /// Opens the Observatory beside the thread where the app allows it.
    pub(crate) fn open_observatory(&self, app: &str, session: &str, url: &str) -> String {
        let (link, text) = observatory_hint(app, session, url);
        if let Some(link) = link
            && std::env::var("UNVRS_NO_OPEN").is_err()
        {
            let _ = std::process::Command::new("open").arg(&link).status_owned();
        }
        text
    }

    // ───────────── wakes ─────────────

    /// Queues a wake and tells the captain when its seat sits idle in an app.
    pub(crate) fn wake_seat(
        &self,
        inner: &mut Inner,
        to: usize,
        kind: &str,
        text: &str,
        from: Option<usize>,
        reference: Option<String>,
    ) -> Result<u64> {
        let id = inner.st.wake(to, kind, text, from, reference)?;
        let rec = inner.st.pid(to)?.clone();
        self.event(
            inner,
            "wake",
            json!({"pid": to, "wake": id, "wake_kind": kind, "from": from, "text": clip(text, 200)}),
        );
        // A live rewake watcher wakes the thread itself; the notification is the fallback.
        let watched = rec
            .thread
            .as_ref()
            .and_then(|t| inner.watchers.get(t))
            .is_some_and(|w| super::procinfo::alive(*w));
        if let Some(t) = rec.thread.as_ref().and_then(|t| inner.st.threads.get(t))
            && t.bound
            && !t.turn_open
            && !watched
        {
            let seat = match &rec.project {
                Some(p) => format!("{p} lead"),
                None => "L1".into(),
            };
            let link = (t.app == "codex-app").then(|| {
                format!(
                    "codex://threads/{}?prompt={}",
                    t.session,
                    urlencode("$unvrs:digest")
                )
            });
            self.notify(
                inner,
                &format!("{seat}: {}", clip(text, 140)),
                link.as_deref(),
            );
        }
        Ok(id)
    }

    /// macOS notification (osascript). A click cannot carry a link without a helper;
    /// the link is in the journal and the text says where to look.
    pub(crate) fn notify(&self, inner: &mut Inner, text: &str, link: Option<&str>) {
        self.event(inner, "notify", json!({"text": text, "link": link}));
        if std::env::var("UNVRS_NOTIFY").is_ok_and(|v| v == "0") || cfg!(test) {
            return;
        }
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"UNVRS\"",
            esc(&clip(text, 200))
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn_owned()
            .map(|child| child.detach());
    }

    // ───────────── detached seats (C9) ─────────────

    /// A seat with unhandled wakes and no live thread runs driven: an L2 always, L1
    /// only in away mode and within its cap. A seat whose last run failed waits out its
    /// backoff, so a failing driver cannot spend model turns every sweep (I9).
    pub(crate) fn run_detached_seats(self: &Arc<Self>) {
        let mut start = vec![];
        {
            let mut inner = self.lock();
            let now = now_ms();
            let seats: Vec<(usize, u8)> = inner
                .st
                .seats()
                .iter()
                .filter(|s| s.wakes.iter().any(|w| !w.acked && w.delivered.is_none()))
                .filter(|s| !inner.running.contains(&s.pid))
                .filter(|s| {
                    inner
                        .seat_backoff
                        .get(&s.pid)
                        .is_none_or(|b| b.until <= now)
                })
                .filter(|s| {
                    s.thread
                        .as_ref()
                        .is_none_or(|t| !self.thread_live(&inner, t))
                })
                .map(|s| (s.pid, s.rank))
                .collect();
            let mut refused = false;
            for (pid, rank) in seats {
                // L1 runs detached only in away mode and within its cap.
                if rank == 1 && inner.st.away.as_ref().is_none_or(|a| a.runs >= a.cap) {
                    continue;
                }
                match self.admit_econ_seat(&mut inner, pid) {
                    Ok(true) => {}
                    Ok(false) => {
                        arm_seat_backoff(&mut inner, pid, now);
                        refused = true;
                        continue;
                    }
                    Err(error) => {
                        let (fails, wait) = arm_seat_backoff(&mut inner, pid, now);
                        self.event(&mut inner, "econ.error", json!({"pid":pid,"error":format!("{error:#}"),"fails":fails,"retry_in_ms":wait}));
                        refused = true;
                        continue;
                    }
                }
                if rank == 1
                    && let Some(a) = inner.st.away.as_mut()
                {
                    a.runs += 1;
                }
                inner.running.insert(pid);
                start.push(pid);
            }
            if !start.is_empty() || refused {
                self.save(&mut inner);
                if inner.dirty {
                    for pid in start.drain(..) {
                        inner.running.remove(&pid);
                    }
                }
            }
        }
        for pid in start {
            let k = Arc::clone(self);
            thread::spawn(move || {
                k.seat_run(pid);
                let mut inner = k.lock();
                inner.running.remove(&pid);
                if let Ok(r) = inner.st.pid_mut(pid) {
                    settle_seat_run(r);
                }
                k.save(&mut inner);
            });
        }
    }

    /// No seat run survives a kernel restart: settle the ones a restart interrupted (C8).
    pub(crate) fn settle_seat_runs(&self, inner: &mut Inner) -> bool {
        let stale: Vec<usize> = inner
            .st
            .pids
            .values()
            .filter(|p| p.rank <= 2)
            .filter(|p| {
                p.driven
                    .as_ref()
                    .is_some_and(|d| d.cpu == "seat-run" && (d.busy || p.thread.is_some()))
            })
            .map(|p| p.pid)
            .collect();
        for pid in &stale {
            if let Ok(r) = inner.st.pid_mut(*pid) {
                settle_seat_run(r);
            }
        }
        !stale.is_empty()
    }

    /// One detached run. A failed run (driver error or a reported error) acknowledges
    /// nothing: the wakes it took go back to the queue and the seat backs off.
    fn seat_run(self: &Arc<Self>, pid: usize) {
        let mut taken = vec![];
        let res = self.seat_turn(pid, &mut taken);
        let mut inner = self.lock();
        match res {
            Ok(()) => {
                inner.seat_backoff.remove(&pid);
            }
            Err(e) => {
                let reason = format!("{e:#}");
                if reason.to_ascii_lowercase().contains("model")
                    || reason.to_ascii_lowercase().contains("effort")
                {
                    let harness = inner
                        .st
                        .pid(pid)
                        .map(|r| r.harness.clone())
                        .unwrap_or_default();
                    self.invalidate_econ(&mut inner, &harness, &reason);
                }
                let mut requeued = vec![];
                if let Ok(r) = inner.st.pid_mut(pid) {
                    for w in r.wakes.iter_mut().filter(|w| taken.contains(&w.id)) {
                        if !w.acked && w.delivered.as_deref() == Some("driven") {
                            w.delivered = None;
                            requeued.push(w.id);
                        }
                    }
                }
                let (fails, wait) = arm_seat_backoff(&mut inner, pid, now_ms());
                self.event(
                    &mut inner,
                    "seat.run",
                    json!({"pid": pid, "result": "failed", "error": format!("{e:#}"),
                        "requeued": requeued, "fails": fails, "retry_in_ms": wait}),
                );
            }
        }
        self.save(&mut inner);
    }

    fn seat_turn(self: &Arc<Self>, pid: usize, taken: &mut Vec<u64>) -> Result<()> {
        let req = {
            let mut inner = self.lock();
            let rec = inner.st.pid(pid)?.clone();
            let decision: crate::drv_econ::Decision = serde_json::from_value(
                rec.contract
                    .as_ref()
                    .context("seat route receipt missing")?["econ"]
                    .clone(),
            )?;
            let chosen = decision.chosen.context("detached seat is held")?;
            let wakes: Vec<String> = rec
                .wakes
                .iter()
                .filter(|w| !w.acked && w.delivered.is_none())
                .map(|w| format!("- [{}] {}: {}", w.id, w.kind, w.text))
                .collect();
            if let Ok(r) = inner.st.pid_mut(pid) {
                for w in r.wakes.iter_mut() {
                    if !w.acked && w.delivered.is_none() {
                        w.delivered = Some("driven".into());
                        taken.push(w.id);
                    }
                }
            }
            // Marked driven first, so the hot set tells the run to use `unvrs ctl`.
            if let Ok(r) = inner.st.pid_mut(pid) {
                r.driven = Some(super::Driven {
                    cpu: "seat-run".into(),
                    busy: true,
                    ..Default::default()
                });
            }
            let hot = hot::hot_text(self, &inner, pid)?;
            let prompt = hot::quiet_mentions(&self.mapp.seat_run(
                rec.rank,
                rec.project.as_deref(),
                &hot,
                &wakes.join("\n"),
                &super::unvrs_cmd(),
            ));
            let harness = chosen.harness;
            let model = Some(chosen.model);
            let effort = Some(chosen.effort);
            let cwd = match &rec.project {
                Some(p) => self.u.project_dir(p),
                None => self.u.root().join("captain"),
            };
            fs::create_dir_all(&cwd)?;
            let r = inner.st.pid_mut(pid)?;
            r.state = "running".into();
            r.driven = Some(super::Driven {
                cpu: "seat-run".into(),
                cwd: cwd.clone(),
                busy: true,
                model: model.clone(),
                effort: effort.clone(),
                ..Default::default()
            });
            self.event(
                &mut inner,
                "seat.run",
                json!({"pid": pid, "rank": rec.rank, "harness": harness, "wakes": wakes.len(), "result": "started",
                    "model": model, "effort": effort}),
            );
            self.save(&mut inner);
            ensure!(!inner.dirty, "seat state persistence failed before turn");
            TurnRequest {
                harness: harness.clone(),
                cwd,
                session: None,
                prompt,
                env: self.driven_env(pid),
                model,
                effort,
            }
        };
        let k = Arc::clone(self);
        let result = self.driver_turn(pid, &req, &move |os| {
            let mut inner = k.lock();
            if let Ok(r) = inner.st.pid_mut(pid)
                && let Some(d) = r.driven.as_mut()
            {
                d.os_pid = Some(os);
            }
        });
        let mut inner = self.lock();
        let rec = inner.st.pid(pid)?.clone();
        if let Ok(r) = inner.st.pid_mut(pid) {
            r.state = if r.thread.is_some() { "live" } else { "idle" }.into();
        }
        let out = result?;
        // What the harness accepted, even for a run that then reports an error.
        if let Ok(r) = inner.st.pid_mut(pid)
            && let Some(d) = r.driven.as_mut()
        {
            d.actual_model = out.model.clone();
            d.actual_effort = out.effort.clone();
            d.effort_evidence = out.effort_evidence.clone();
        }
        ensure!(
            out.model == req.model && out.effort == req.effort,
            "seat model or effort mismatch: requested {:?}/{:?}, accepted {:?}/{:?}",
            req.model,
            req.effort,
            out.model,
            out.effort
        );
        if let Some(rate) = &out.rate {
            let key = format!("{}:rate", req.harness);
            inner.quota.insert(key, rate.clone());
        }
        let text = out.text.trim().to_owned();
        let handled: Vec<(String, String)> = rec
            .wakes
            .iter()
            .filter(|w| !w.acked && w.delivered.as_deref() == Some("driven"))
            .map(|w| {
                (
                    w.kind.clone(),
                    format!("- [w{}] {}: {}", w.id, w.kind, w.text),
                )
            })
            .collect();
        // The wakes are the run's prompt: a threadless seat has no other record of an
        // instruction (a note, the captain's answer) that changed its direction.
        if !handled.is_empty() {
            let lines: Vec<&str> = handled.iter().map(|(_, l)| l.as_str()).collect();
            inner
                .mem
                .append_turn(pid, &format!("Wakes (detached run):\n{}", lines.join("\n")))?;
        }
        if !text.is_empty() {
            inner
                .mem
                .append_turn(pid, &format!("{} (detached run): {text}", req.harness))?;
        }
        if let Some(e) = &out.error {
            return Err(anyhow!("the {} run reported an error: {e}", req.harness));
        }
        let mut acked = vec![];
        if let Ok(r) = inner.st.pid_mut(pid) {
            for w in r.wakes.iter_mut() {
                if !w.acked && w.delivered.as_deref() == Some("driven") {
                    w.acked = true;
                    acked.push(w.id);
                }
            }
        }
        // A detached seat has no thread hooks (Stop, PreCompact, SessionEnd) to fold
        // its brief, so the run folds it: after an instruction, or past the tail budget.
        // Without this a brief that says "hold" outlives the go that lifted it.
        let big = inner
            .mem
            .session(pid)
            .map(|s| s.tail.iter().map(String::len).sum::<usize>() > crate::TAIL_BYTES)
            .unwrap_or(false);
        let steered = handled
            .iter()
            .any(|(kind, _)| matches!(kind.as_str(), "note" | "answered"));
        // An instruction folds the whole unfolded tail (a drain keeps the newest turns,
        // the go among them, out of the brief); size alone drains the old part.
        let fold = if steered {
            Some(("seat run", false))
        } else if big {
            Some(("tail threshold", true))
        } else {
            None
        };
        self.event(
            &mut inner,
            "outcome",
            json!({"pid": pid, "rank": rec.rank, "project": rec.project, "harness": req.harness,
                "model": req.model, "actual_model": out.model, "effort": req.effort,
                "actual_effort": out.effort, "effort_evidence": out.effort_evidence,
                "text": clip(&text, 600), "wakes": acked}),
        );
        if rec.rank == 2
            && let Some(l1) = inner.st.l1()
        {
            let seat = rec.project.clone().unwrap_or_default();
            let _ = self.wake_seat(
                &mut inner,
                l1,
                "outcome",
                &format!(
                    "{seat} lead handled {} wake(s) on its own: {}",
                    acked.len(),
                    clip(&text, 600)
                ),
                Some(pid),
                None,
            );
        }
        self.save(&mut inner);
        drop(inner);
        if let Some((reason, drain)) = fold {
            self.schedule_fold(pid, reason, drain);
        }
        Ok(())
    }

    /// Environment for driven runs: the stable binary first on PATH, the home, and
    /// the PID so hooks stay quiet.
    pub(crate) fn driven_env(&self, pid: usize) -> Vec<(String, String)> {
        let bin = super::unvrs_cmd();
        let dir = std::path::Path::new(bin.trim_matches('\''))
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        vec![
            (
                "PATH".into(),
                format!("{dir}:{}", std::env::var("PATH").unwrap_or_default()),
            ),
            ("UNVRS_DRIVEN_PID".into(), pid.to_string()),
            ("UNVRS_HOME".into(), self.u.root().display().to_string()),
        ]
    }

    /// Memory decay (D49): runs at start and about hourly.
    pub(crate) fn sweep_memory(&self) {
        self.job_begin("memory.sweep", 120_000);
        let now = now_ms();
        let mut errors = vec![];
        let mut stores = vec![MemStore::captain(self.u.root())];
        for p in self.project_ids() {
            stores.push(MemStore::project(self.u.root(), &p));
        }
        let mut changed = vec![];
        for s in stores {
            match s.sweep(now) {
                Ok(c) => changed.extend(c),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        if !changed.is_empty() {
            let mut inner = self.lock();
            self.event(&mut inner, "memory.sweep", json!({"changed": changed}));
        }
        self.job_finish(
            "memory.sweep",
            (!errors.is_empty()).then(|| errors.join("\n")),
            false,
        );
    }

    /// Validates a seat's project proposal and holds it for the captain.
    pub(crate) fn propose_project(
        &self,
        inner: &mut Inner,
        by: usize,
        id: &str,
        purpose: &str,
        sources: &[String],
    ) -> Result<String> {
        valid_project_id(id)?;
        if self.project(id).is_some() {
            bail!("Project {id} already exists");
        }
        let registry = crate::Sources::load(self.u.root())?;
        for s in sources {
            ensure!(registry.get(s).is_some(), "Unknown source {s:?}");
        }
        let q = format!(
            "Create project {id}: {purpose}{}",
            if sources.is_empty() {
                String::new()
            } else {
                format!(" (sources: {})", sources.join(", "))
            }
        );
        let hold = self.open_hold(
            inner,
            "proposal",
            &q,
            vec!["approve".into(), "no".into()],
            None,
            None,
            Some(by),
            Some(json!({"create_project": {"id": id, "purpose": purpose, "sources": sources}})),
        )?;
        Ok(hold)
    }
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Where the Observatory opens for a thread in `app`: a link the kernel opens (if
/// any) and the words for the seat.
/// First wait after a failed detached seat run; doubles per consecutive failure.
const SEAT_RETRY_BASE_MS: u64 = 30_000;
/// Longest wait between detached runs of a failing seat.
const SEAT_RETRY_MAX_MS: u64 = 15 * 60_000;

/// Counts one more failed (or refused) detached run of `pid` and holds its next run
/// back; returns the consecutive failures and the wait (ms).
fn arm_seat_backoff(inner: &mut Inner, pid: usize, now: u64) -> (u32, u64) {
    let b = inner.seat_backoff.entry(pid).or_default();
    b.fails += 1;
    let wait = seat_retry_ms(b.fails);
    b.until = now + wait;
    (b.fails, wait)
}

/// Backoff before the next detached run of a seat after `fails` failed runs in a row.
pub(crate) fn seat_retry_ms(fails: u32) -> u64 {
    SEAT_RETRY_BASE_MS
        .saturating_mul(1 << fails.saturating_sub(1).min(10))
        .min(SEAT_RETRY_MAX_MS)
}

/// A finished seat run (C8): on a seat with a thread the thread is the CPU again, so the
/// run's Driven is cleared; a threadless seat keeps it, idle, as the record of its last run.
pub(crate) fn settle_seat_run(r: &mut super::PidRec) {
    if r.driven.as_ref().is_none_or(|d| d.cpu != "seat-run") {
        return;
    }
    if r.thread.is_some() {
        r.driven = None;
    } else if let Some(d) = r.driven.as_mut() {
        d.busy = false;
        d.os_pid = None;
    }
}

pub(crate) fn observatory_hint(app: &str, session: &str, url: &str) -> (Option<String>, String) {
    match app {
        "codex-app" => {
            let link = format!("codex://threads/{session}?browserUrl={}", urlencode(url));
            let text = format!(
                "Opened beside this thread in the Codex app ({link}). If it did not open: Cmd+Shift+B and paste the link."
            );
            (Some(link), text)
        }
        "t3" => (
            None,
            format!(
                "In T3 Code the seat opens it in the preview panel (tool mcp__t3-code__preview_open with url {url})."
            ),
        ),
        "claude-desktop" => (
            None,
            format!(
                "In Claude Desktop the seat opens it in the Browser pane beside this thread (Browser tool preview_start with url {url})."
            ),
        ),
        _ => (
            None,
            "Open the link in any Chromium browser (Safari needs `unvrs observe --short-url`)."
                .into(),
        ),
    }
}

#[cfg(test)]
mod observatory_tests {
    use super::observatory_hint;

    #[test]
    fn each_app_gets_its_own_way_to_the_observatory() {
        let url = "http://unvrs.localhost:7576";
        let (link, text) = observatory_hint("claude-desktop", "s1", url);
        assert!(link.is_none());
        assert!(
            text.contains("Claude Desktop") && text.contains(url),
            "{text}"
        );
        let (link, _) = observatory_hint("codex-app", "s1", url);
        assert!(link.unwrap().starts_with("codex://threads/s1?browserUrl="));
        assert!(observatory_hint("t3", "s1", url).1.contains("T3 Code"));
        assert!(
            observatory_hint("headless", "s1", url)
                .1
                .contains("Chromium")
        );
    }
}
