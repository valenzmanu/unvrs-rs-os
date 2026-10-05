//! The Observatory snapshot (C10): "what is the system doing right now, and does
//! anything need me?" as one JSON value. Times are ms since the epoch, durations
//! seconds; unknown quota is null, never 0.
use super::{KERNEL_VERSION, Kernel, now_ms, state::clip};
use crate::signals::CommandTracking;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    sync::Mutex,
};

/// The live build and the running or last deploy, from `deploy/live.json` and
/// `deploy/status.json` (written by `unvrs deploy|rollback`). Missing files are null. A run
/// that is not done while its process is gone is `stale` (the deployer died mid-run).
pub(crate) fn deploy_view(home: &Path) -> Value {
    let read = |name: &str| -> Value {
        std::fs::read_to_string(home.join("deploy").join(name))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    };
    let live = read("live.json");
    let status = read("status.json");
    let live = if live.is_object() {
        json!({"slot": live["slot"], "commit": live["commit"], "branch": live["branch"],
            "ref": live["ref"], "since": live["since"]})
    } else {
        Value::Null
    };
    let run = if status.is_object() {
        let done = status["done"].as_bool().unwrap_or(false);
        let stale = !done
            && status["os_pid"]
                .as_u64()
                .is_none_or(|pid| !super::procinfo::alive(pid as u32));
        json!({"run": status["run"], "kind": status["kind"], "ref": status["ref"],
            "commit": status["commit"], "slot": status["slot"], "phase": status["phase"],
            "done": done, "stale": stale, "result": status["result"], "reason": status["reason"],
            "started": status["started"], "updated": status["updated"], "ended": status["ended"]})
    } else {
        Value::Null
    };
    json!({"live": live, "run": run})
}

type TitleCache = HashMap<String, (Option<String>, u64)>;
static TITLES: Mutex<Option<TitleCache>> = Mutex::new(None);

/// A T3 Code thread title for a harness session, read-only from T3's state.sqlite
/// through the system `sqlite3` (no new crate). Cached for a minute.
fn t3_title(session: &str) -> Option<String> {
    if session.is_empty()
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    let now = now_ms();
    if let Ok(mut g) = TITLES.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if let Some((t, at)) = map.get(session)
            && now.saturating_sub(*at) < 60_000
        {
            return t.clone();
        }
    }
    let db = std::env::var("UNVRS_T3_STATE").unwrap_or_else(|_| {
        format!(
            "{}/.t3/userdata/state.sqlite",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let title = std::path::Path::new(&db).is_file().then(|| {
        let sql = format!(
            "select t.title from provider_session_runtime r join projection_threads t using(thread_id) where r.resume_cursor_json like '%{session}%' limit 1;"
        );
        Command::new("sqlite3")
            .args(["-readonly", &db, &sql])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output_owned()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|t| !t.is_empty())
    });
    let title = title.flatten();
    if let Ok(mut g) = TITLES.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(session.into(), (title.clone(), now));
    }
    title
}

/// How much of a transcript's end is read for the model and effort of its last turn.
const TRANSCRIPT_TAIL: u64 = 256 * 1024;

/// The model and effort of a harness thread's last turn, from its transcript: a Claude
/// Code main-chain assistant record (`message.model`, `effort`) or a Codex rollout
/// `turn_context` (`payload.model`, `payload.effort`). None when the tail has neither.
pub(crate) fn transcript_facts(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL)))
        .ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let s = |v: &Value| v.as_str().filter(|t| !t.is_empty()).map(str::to_owned);
    text.lines().rev().find_map(|line| {
        let v: Value = serde_json::from_str(line).ok()?;
        match v["type"].as_str()? {
            "assistant" if v["isSidechain"] != true => {
                let model = s(&v["message"]["model"]).filter(|m| !m.starts_with('<'))?;
                Some((Some(model), s(&v["effort"])))
            }
            "turn_context" => {
                let (model, effort) = (s(&v["payload"]["model"]), s(&v["payload"]["effort"]));
                (model.is_some() || effort.is_some()).then_some((model, effort))
            }
            _ => None,
        }
    })
}

/// A seat's model and effort from its seat run: what the harness accepted, never what was
/// asked for. None when the run has not reported either yet.
fn seat_run_facts(d: &super::Driven) -> Option<(Option<String>, Option<String>)> {
    (d.cpu == "seat-run" && (d.actual_model.is_some() || d.actual_effort.is_some()))
        .then(|| (d.actual_model.clone(), d.actual_effort.clone()))
}

/// A seat's or worker's brief line (what it is working on): `brief.now` and the first
/// `brief.next` from its session file (written atomically), clipped. Read after the kernel
/// lock is released; (null, null) when there is no brief yet.
fn brief_line(root: &Path, pid: u64) -> (Value, Value) {
    let path = root.join(format!("sessions/pid-{pid}/session.json"));
    let Some(v) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return (Value::Null, Value::Null);
    };
    let line = |x: &Value| {
        x.as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| json!(clip(t, 160)))
            .unwrap_or(Value::Null)
    };
    (line(&v["brief"]["now"]), line(&v["brief"]["next"][0]))
}

impl Kernel {
    pub(crate) fn snapshot(&self) -> Value {
        let inner = self.lock();
        let now = now_ms();
        let mut holds: Vec<_> = inner
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
                    "age_s": now.saturating_sub(h.created) / 1000, "project": h.project})
            })
            .collect();
        // seat index → transcript to read once the lock is released
        let mut transcripts: Vec<(usize, String)> = vec![];
        let seats: Vec<Value> = inner
            .st
            .seats()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let occupied = s
                    .thread
                    .as_ref()
                    .and_then(|k| inner.st.threads.get(k))
                    .map(|t| {
                        let title = (t.app == "t3").then(|| t3_title(&t.session)).flatten();
                        let link = (t.harness == "codex")
                            .then(|| format!("codex://threads/{}", t.session));
                        json!({"app": if t.app.is_empty() { "headless" } else { t.app.as_str() },
                        "harness": t.harness, "thread_id": t.session, "title": title, "link": link})
                    });
                let state = if inner.running.contains(&s.pid) {
                    "running"
                } else if s.thread.is_some()
                    && s.thread
                        .as_ref()
                        .is_some_and(|k| self.thread_live(&inner, k))
                {
                    "live"
                } else if s.thread.is_some() {
                    "away"
                } else if s.state == "held" {
                    "held"
                } else {
                    "free"
                };
                // model · effort · source (C9, I8): a seat run in flight, else the bound
                // thread's transcript, else the last seat run; null when unknown.
                let run = s.driven.as_ref();
                let transcript = s
                    .thread
                    .as_ref()
                    .and_then(|k| inner.st.threads.get(k))
                    .and_then(|t| t.transcript.clone());
                let (facts, source) =
                    match (run.filter(|d| d.busy).and_then(seat_run_facts), transcript) {
                        (Some(f), _) => (Some(f), Some("seat-run")),
                        (None, Some(path)) => {
                            transcripts.push((i, path));
                            (None, None)
                        }
                        (None, None) => match run.and_then(seat_run_facts) {
                            Some(f) => (Some(f), Some("last seat-run")),
                            None => (None, None),
                        },
                    };
                let (model, effort) = facts.unwrap_or_default();
                json!({"rank": s.rank, "project": s.project, "pid": s.pid, "state": state,
                    "wakes": s.wakes.iter().filter(|w| !w.acked).count(), "occupied_by": occupied,
                    "model": model, "effort": effort, "source": source,
                    "econ": s.contract.as_ref().map(|c| c["econ"].clone()), "harness":s.harness})
            })
            .collect();
        let workers: Vec<Value> = inner
            .st
            .pids
            .values()
            .filter(|p| p.rank == 3 && matches!(p.state.as_str(), "working" | "idle" | "held"))
            .map(|p| {
                let c = p.contract.clone().unwrap_or(Value::Null);
                json!({"pid": p.pid, "parent": p.parent, "project": p.project,
                    "intent": clip(c["intent"].as_str().unwrap_or(&p.task), 140),
                    "shape": c["shape"].as_str().unwrap_or("report"), "harness": p.harness,
                    "econ": c["econ"],
                    "go_quote": c["go_quote"].as_str().or_else(|| c["go"].as_str()),
                    "go_source": c.get("go_source").or_else(|| c.get("go_evidence")),
                    "done_when": c["done_when"],
                    "model": p.driven.as_ref().and_then(|d| d.model.clone()),
                    "actual_model": p.driven.as_ref().and_then(|d| d.actual_model.clone()),
                    "effort": p.driven.as_ref().and_then(|d| d.effort.clone()),
                    "actual_effort": p.driven.as_ref().and_then(|d| d.actual_effort.clone()),
                    "effort_evidence": p.driven.as_ref().and_then(|d| d.effort_evidence.clone()),
                    "session": p.driven.as_ref().and_then(|d| d.session.clone()),
                    "last_activity": p.driven.as_ref().and_then(|d| d.last_activity),
                    "os_pid": p.driven.as_ref().and_then(|d| d.os_pid),
                    "busy": p.driven.as_ref().is_some_and(|d| d.busy),
                    "recovering": p.driven.as_ref().is_some_and(|d| d.recovering),
                    "tools": p.driven.as_ref().map(|d| d.last_tools.iter().map(|t| t.split(':').next().unwrap_or("tool")).collect::<Vec<_>>()),
                    "attempt": p.driven.as_ref().map(|d| d.attempt),
                    "lineage": p.driven.as_ref().map(|d| d.lineage.unwrap_or(p.pid)),
                    "last_stop": p.driven.as_ref().and_then(|d| d.last_stop.clone()),
                    "handed_from": p.handed_from,
                    "elapsed_s": now.saturating_sub(p.created) / 1000, "state": p.state})
            })
            .collect();
        let mut quota: Vec<Value> = inner
            .quota
            .values()
            .map(|q| {
                json!({"account": q["account"].as_str().unwrap_or("default"),
                    "harness": q["harness"], "remaining_pct": q.get("remaining_pct").cloned().unwrap_or(Value::Null),
                    "resets_at": q.get("resets_at").cloned().unwrap_or(Value::Null),
                    "source": q["source"].as_str().unwrap_or("unknown")})
            })
            .collect();
        for h in ["claude", "codex"] {
            if !quota.iter().any(|q| q["harness"] == h) {
                quota.push(json!({"account": "default", "harness": h, "remaining_pct": null, "resets_at": null, "source": "unknown"}));
            }
        }
        let moves: Vec<Value> = inner
            .recent
            .iter()
            .rev()
            .filter(|e| matches!(e["kind"].as_str(), Some("move" | "handoff")))
            .take(12)
            .map(|e| {
                if e["kind"] == "move" {
                    json!({"at": e["at"], "kind": e["move"], "from": e["from_app"], "to": e["to_app"], "pid": e["pid"], "bytes": e["bytes"]})
                } else {
                    json!({"at": e["at"], "kind": "handoff",
                        "from": format!("PID {} ({})", e["from_pid"], e["from_harness"].as_str().unwrap_or("")),
                        "to": format!("PID {} ({})", e["to_pid"], e["to_harness"].as_str().unwrap_or("")),
                        "pid": e["from_pid"], "bytes": e["bytes"]})
                }
            })
            .collect();
        let recent: Vec<Value> = inner
            .recent
            .iter()
            .rev()
            .filter_map(|e| {
                let kind = e["kind"].as_str().unwrap_or("");
                self.mapp
                    .event_text(kind, e)
                    .map(|t| json!({"at": e["at"], "kind": kind, "text": t}))
            })
            .take(30)
            .collect();
        let context_reads: Vec<Value> = inner
            .recent
            .iter()
            .rev()
            .filter(|e| e["kind"] == "ctx.read")
            .take(20)
            .map(|e| json!({"at": e["at"], "pid": e["pid"], "source": e["source"], "ref": e["ref"], "bytes": e["bytes"]}))
            .collect();
        let projects: Vec<Value> = self
            .project_ids()
            .into_iter()
            .map(|id| {
                let p = self.project(&id).unwrap_or_default();
                json!({"id": id, "purpose": p.purpose, "sources": p.sources, "seat_pid": inner.st.seat_of(&id)})
            })
            .collect();
        let econ_catalogs = inner.st.econ_catalogs.clone();
        drop(inner);
        let mut seats = seats;
        for (i, path) in transcripts {
            if let Some((model, effort)) = transcript_facts(Path::new(&path)) {
                seats[i]["model"] = json!(model);
                seats[i]["effort"] = json!(effort);
                seats[i]["source"] = json!("transcript");
            }
        }
        let mut workers = workers;
        for v in seats.iter_mut().chain(workers.iter_mut()) {
            let (now, next) = brief_line(self.u.root(), v["pid"].as_u64().unwrap_or(0));
            v["now"] = now;
            v["next"] = next;
        }
        json!({
            "at": now,
            "kernel": {"version": KERNEL_VERSION, "commit": super::BUILD_COMMIT, "ref": super::BUILD_REF,
                "draining": self.draining.load(std::sync::atomic::Ordering::SeqCst), "os_pid": std::process::id(),
                "uptime_s": now.saturating_sub(self.started) / 1000, "home": self.u.root()},
            "deploy": deploy_view(self.u.root()),
            "calls": calls, "seats": seats, "workers": workers, "quota": quota,
            "econ_catalogs": econ_catalogs,
            "moves": moves, "recent": recent, "context_reads": context_reads, "projects": projects,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::deploy_view;
    use serde_json::json;

    #[test]
    fn deploy_view_reads_live_and_status() {
        let home = std::env::temp_dir().join(format!("uke-deploy-view-{}", std::process::id()));
        let d = home.join("deploy");
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(deploy_view(&home), json!({"live": null, "run": null}));
        std::fs::write(
            d.join("live.json"),
            r#"{"slot":"0.8.0+gabc","commit":"abc","branch":"main","ref":"main","since":5,"previous":"x"}"#,
        )
        .unwrap();
        // a run that is not done and whose process is gone is stale
        std::fs::write(
            d.join("status.json"),
            r#"{"run":"deploy-1","kind":"deploy","ref":"main","phase":"build","done":false,"os_pid":999999999,"log":[]}"#,
        )
        .unwrap();
        let v = deploy_view(&home);
        assert_eq!(v["live"]["commit"], "abc");
        assert_eq!(v["live"]["previous"], json!(null));
        assert_eq!(v["run"]["phase"], "build");
        assert_eq!(v["run"]["stale"], true);
        // a running deploy (this process) is not stale; a finished one never is
        let me = std::process::id();
        std::fs::write(
            d.join("status.json"),
            format!(r#"{{"run":"deploy-2","phase":"drain","done":false,"os_pid":{me}}}"#),
        )
        .unwrap();
        assert_eq!(deploy_view(&home)["run"]["stale"], false);
        std::fs::write(
            d.join("status.json"),
            r#"{"run":"deploy-3","phase":"live","done":true,"result":"live","os_pid":999999999}"#,
        )
        .unwrap();
        let v = deploy_view(&home);
        assert_eq!(
            (v["run"]["stale"].clone(), v["run"]["result"].clone()),
            (json!(false), json!("live"))
        );
        std::fs::remove_dir_all(&home).unwrap();
    }
}
