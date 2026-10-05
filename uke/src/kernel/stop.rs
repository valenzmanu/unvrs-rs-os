//! `stop <pid> [reason…]` (aliases `cancel`, `kill`; uke-design §12 `kill`): the owner of a
//! driven L3 (its lead, L1 or the captain; `may_signal`) ends it. A busy worker's process
//! group gets SIGTERM, then SIGKILL after a grace period; the worker loop ends the task
//! only once the driver has returned (group reaped), after the usual checkpoint
//! (worker-recovery.md). The result package says `cancelled` and wakes the lead. Every
//! refusal is a `refused` journal event.
use super::super::{Inner, Kernel, recover::Stop, state::clip};
use super::Caller;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{sync::Arc, thread, time::Duration};

const KILL_GRACE: Duration = Duration::from_secs(5);

impl Kernel {
    pub(super) fn stop_verb(
        self: &Arc<Self>,
        inner: &mut Inner,
        who: &Caller,
        args: &[String],
    ) -> Result<String> {
        let by = match who {
            Caller::Seat(p, _) | Caller::Driven(p) => Some(*p),
            _ => None,
        };
        let words: Vec<&String> = args.iter().filter(|a| *a != "--pid").collect();
        let asked: usize = words
            .first()
            .and_then(|a| a.parse().ok())
            .context("usage: stop <pid> [reason]")?;
        let reason = words[1..]
            .iter()
            .map(|w| w.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        // A handed-off PID names its lineage: stop the live successor.
        let mut target = asked;
        while let Ok(r) = inner.st.pid(target)
            && r.state == "handed-off"
            && let Some(next) = r.handed_to
        {
            target = next;
        }
        let refuse = |k: &Self, inner: &mut Inner, why: String| -> Result<String> {
            k.event(
                inner,
                "refused",
                json!({"verb": "stop", "pid": by, "target": target, "asked": asked, "caller": format!("{who:?}"), "reason": clip(&why, 300)}),
            );
            bail!("{why}")
        };
        let Ok(rec) = inner.st.pid(target).cloned() else {
            return refuse(self, inner, format!("Refused: no PID {target}"));
        };
        if matches!(who, Caller::Driven(p) if *p == target) {
            return refuse(
                self,
                inner,
                "Refused: a worker ends itself with UNVRS-RESULT, not stop".into(),
            );
        }
        if let Err(e) = self.may_signal(inner, who, target) {
            return refuse(self, inner, format!("{e:#}"));
        }
        if rec.kind != "driven" || rec.rank != 3 {
            return refuse(
                self,
                inner,
                format!(
                    "Refused: PID {target} is an L{} seat; stop ends driven L3 workers (the captain closes seats)",
                    rec.rank
                ),
            );
        }
        match rec.state.as_str() {
            "stopping" => return Ok(format!("PID {target} is already stopping")),
            "working" | "idle" => {}
            other => {
                return refuse(
                    self,
                    inner,
                    format!("Refused: PID {target} is {other}; nothing to stop"),
                );
            }
        }
        let by_label = match by {
            Some(p) => format!("PID {p}"),
            None => "the captain".into(),
        };
        let why = if reason.trim().is_empty() {
            format!("stopped by {by_label}")
        } else {
            format!("stopped by {by_label}: {}", clip(reason.trim(), 300))
        };
        let d = rec.driven.clone().unwrap_or_default();
        {
            let r = inner.st.pid_mut(target)?;
            r.state = "stopping".into();
            if let Some(dd) = r.driven.as_mut() {
                dd.stop_request = Some(why.clone());
            }
        }
        self.event(
            inner,
            "stop.requested",
            json!({"pid": target, "asked": asked, "by": by, "caller": format!("{who:?}"), "reason": clip(&why, 300), "busy": d.busy, "recovering": d.recovering, "os_pid": d.os_pid}),
        );
        let origin = if asked == target {
            String::new()
        } else {
            format!(" (PID {asked} was handed off to it)")
        };
        if d.busy {
            if let Some(os) = d.os_pid {
                let _ = crate::signals::signal_group(i64::from(os), libc::SIGTERM);
                let k = Arc::clone(self);
                thread::spawn(move || {
                    thread::sleep(KILL_GRACE);
                    let mut inner = k.lock();
                    let alive = inner.st.pid(target).is_ok_and(|r| {
                        r.state == "stopping"
                            && r.driven
                                .as_ref()
                                .is_some_and(|d| d.busy && d.os_pid == Some(os))
                    });
                    if alive {
                        let _ = crate::signals::signal_group(i64::from(os), libc::SIGKILL);
                        k.event(
                            &mut inner,
                            "stop.escalated",
                            json!({"pid": target, "os_pid": os, "signal": "SIGKILL"}),
                        );
                        k.save(&mut inner);
                    }
                });
            }
            return Ok(format!(
                "PID {target}{origin}: its turn was signalled; the task ends as cancelled once its process group is reaped, and its lead gets the result"
            ));
        }
        if d.recovering {
            return Ok(format!(
                "PID {target}{origin} is checkpointing; it ends as cancelled when the checkpoint finishes, not continued"
            ));
        }
        // Between turns (or never started): checkpoint and end now, off this lock.
        inner.running.insert(target);
        let k = Arc::clone(self);
        thread::spawn(move || {
            let inner = k.lock();
            k.recover(inner, target, Stop::Cancelled(why));
        });
        Ok(format!(
            "PID {target}{origin} stopped between turns; it ends as cancelled after its checkpoint, and its lead gets the result"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{Driven, now_ms};
    use super::*;
    use crate::{
        Brief, BriefFold, Drivers, FoldJob, SilentMapp, TurnRequest, TurnResult, Universe,
    };
    use serde_json::Value;
    use std::{fs, path::PathBuf, sync::Mutex, time::Instant};

    /// Fake worker ends when the unit Signaller records a stop request.
    struct Sleeper {
        turns: Mutex<u32>,
    }
    impl Drivers for Sleeper {
        fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
            bail!("no fold worker")
        }
        fn turn(&self, req: &TurnRequest, started: &dyn Fn(u32)) -> Result<TurnResult> {
            *self.turns.lock().unwrap() += 1;
            let home = req
                .env
                .iter()
                .find(|(k, _)| k == "UNVRS_HOME")
                .context("unit home")?
                .1
                .clone();
            let owner = req
                .env
                .iter()
                .find(|(k, _)| k == "UNVRS_DRIVEN_PID")
                .context("unit owner")?
                .1
                .parse()?;
            let os = crate::signals::fake_worker(std::path::Path::new(&home), owner)?;
            started(os);
            crate::signals::wait_fake_worker(std::path::Path::new(&home), os)?;
            bail!("{} fake worker stopped", req.harness)
        }
    }

    struct Rig {
        k: Arc<Kernel>,
        drv: Arc<Sleeper>,
        root: PathBuf,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// L1 = PID 1; L2 = PID 2 (project proj); L2 = PID 3 (project other).
    fn rig(tag: &str) -> Rig {
        let root = std::env::temp_dir().join(format!(
            "unvrs-stop-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        let u = Universe::at(&root).unwrap();
        u.init().unwrap();
        let drv = Arc::new(Sleeper {
            turns: Mutex::new(0),
        });
        let k = Kernel::open(u, drv.clone(), Arc::new(SilentMapp), now_ms()).unwrap();
        {
            let mut inner = k.lock();
            let l1 = inner
                .st
                .create(0, 1, "attached", "claude", "", Default::default(), None);
            for project in ["proj", "other"] {
                let l2 = inner
                    .st
                    .create(l1, 2, "attached", "claude", "", Default::default(), None);
                inner.st.pid_mut(l2).unwrap().project = Some(project.into());
                inner.st.pid_mut(l2).unwrap().state = "idle".into();
            }
        }
        Rig { k, drv, root }
    }

    /// A working L3 under PID 2 with a task dir, not yet running.
    fn worker(r: &Rig) -> usize {
        let mut inner = r.k.lock();
        let pid = inner.st.create(
            2,
            3,
            "driven",
            "codex",
            "the task",
            Default::default(),
            None,
        );
        inner
            .mem
            .write_brief(
                pid,
                "spawn",
                Brief {
                    goal: "the task".into(),
                    now: "not started".into(),
                    open: vec!["[o1] the task".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let dir = r.k.u.project_dir("proj").join(format!("tasks/pid-{pid}"));
        fs::create_dir_all(&dir).unwrap();
        let rec = inner.st.pid_mut(pid).unwrap();
        rec.state = "working".into();
        rec.project = Some("proj".into());
        rec.contract = Some(
            json!({"intent": "the task", "spec": "do it", "shape": "report",
            "go_quote":"Ok, go", "done_when":"result delivered",
            "go_source":{"kind":"captain_prompt", "prompt_id":1, "thread":"claude:fixture", "at":0}}),
        );
        rec.driven = Some(Driven {
            cpu: "headless".into(),
            cwd: dir,
            ..Default::default()
        });
        pid
    }

    fn stop(r: &Rig, who: Caller, args: &[&str]) -> Result<String> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let mut inner = r.k.lock();
        r.k.stop_verb(&mut inner, &who, &args)
    }

    fn wait_ended(r: &Rig, pid: usize) {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            {
                let inner = r.k.lock();
                if inner.st.pid(pid).unwrap().state == "ended" && !inner.running.contains(&pid) {
                    return;
                }
            }
            assert!(Instant::now() < until, "PID {pid} did not end");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn events(r: &Rig, kind: &str) -> Vec<Value> {
        r.k.journal
            .tail(2000)
            .into_iter()
            .filter(|e| e["kind"] == kind || e["event"] == kind)
            .collect()
    }

    fn result_json(r: &Rig, pid: usize) -> Value {
        let p =
            r.k.u
                .project_dir("proj")
                .join(format!("tasks/pid-{pid}/result.json"));
        serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn only_the_owner_may_stop_and_every_refusal_is_journaled() {
        let r = rig("authority");
        let pid = worker(&r);
        let cases = [
            (Caller::Seat(3, "codex:other".into()), "belongs to PID 2"),
            (Caller::Unbound, "not an UNVRS seat"),
            (Caller::Driven(pid), "UNVRS-RESULT"),
        ];
        for (who, why) in cases {
            let err = stop(&r, who, &[&pid.to_string()]).unwrap_err().to_string();
            assert!(err.contains(why), "{err}");
        }
        let err = stop(&r, Caller::Seat(1, "claude:l1".into()), &["2"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("stop ends driven L3 workers"), "{err}");
        let refused = events(&r, "refused");
        assert_eq!(refused.len(), 4, "{refused:?}");
        assert!(refused.iter().all(|e| e["verb"] == "stop"));
        let inner = r.k.lock();
        assert_eq!(inner.st.pid(pid).unwrap().state, "working");
        assert!(inner.st.pid(2).unwrap().wakes.is_empty());
        drop(inner);
        assert!(events(&r, "stop.requested").is_empty());
        assert_eq!(*r.drv.turns.lock().unwrap(), 0);
    }

    #[test]
    fn a_lead_l1_or_captain_stop_ends_an_idle_worker_as_cancelled() {
        for (tag, who, label) in [
            ("lead", Caller::Seat(2, "claude:l2".into()), "PID 2"),
            ("l1", Caller::Seat(1, "claude:l1".into()), "PID 1"),
            ("captain", Caller::Captain, "the captain"),
        ] {
            let r = rig(tag);
            let pid = worker(&r);
            let out = stop(&r, who, &["--pid", &pid.to_string(), "wrong", "spec"]).unwrap();
            assert!(out.contains("ends as cancelled"), "{out}");
            wait_ended(&r, pid);
            let res = result_json(&r, pid);
            assert_eq!(res["how"], "cancelled");
            let text = res["result"].as_str().unwrap();
            assert!(
                text.contains(&format!("stopped by {label}: wrong spec")),
                "{text}"
            );
            let inner = r.k.lock();
            let wake = &inner.st.pid(2).unwrap().wakes[0];
            assert!(wake.text.contains("(cancelled)"), "{}", wake.text);
            drop(inner);
            let requested = events(&r, "stop.requested");
            assert_eq!(requested.len(), 1);
            assert_eq!(requested[0]["busy"], false);
            assert_eq!(events(&r, "stop")[0]["stop"], "cancelled");
            assert_eq!(events(&r, "result")[0]["how"], "cancelled");
            // No turn ran and no successor was created.
            assert_eq!(*r.drv.turns.lock().unwrap(), 0);
            assert_eq!(r.k.lock().st.pids.len(), 4);
            let again = stop(&r, Caller::Captain, &[&pid.to_string()]).unwrap_err();
            assert!(again.to_string().contains("is ended"), "{again}");
        }
    }

    #[test]
    fn a_busy_worker_is_signalled_reaped_and_never_continued() {
        let r = rig("busy");
        let pid = worker(&r);
        r.k.start_worker(pid);
        let until = Instant::now() + Duration::from_secs(10);
        let os = loop {
            {
                let inner = r.k.lock();
                let d = inner.st.pid(pid).unwrap().driven.clone().unwrap();
                if d.busy
                    && let Some(os) = d.os_pid
                {
                    break os;
                }
            }
            assert!(Instant::now() < until, "worker never started its turn");
            thread::sleep(Duration::from_millis(10));
        };
        let t0 = Instant::now();
        let out = stop(&r, Caller::Seat(2, "claude:l2".into()), &[&pid.to_string()]).unwrap();
        assert!(out.contains("process group is reaped"), "{out}");
        wait_ended(&r, pid);
        assert!(t0.elapsed() < KILL_GRACE, "SIGTERM alone should end sleep");
        // Probe the whole group, even after the leader has been reaped.
        assert_eq!(
            crate::signals::probe_group(i64::from(os))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ESRCH),
            "process group {os} still exists"
        );
        assert_eq!(result_json(&r, pid)["how"], "cancelled");
        assert_eq!(*r.drv.turns.lock().unwrap(), 1);
        let inner = r.k.lock();
        assert_eq!(inner.st.pids.len(), 4, "a stopped worker got a successor");
        assert!(!inner.st.pid(pid).unwrap().driven.as_ref().unwrap().busy);
        drop(inner);
        assert_eq!(events(&r, "stop.requested")[0]["busy"], true);
        assert!(events(&r, "continue").is_empty());
    }

    #[test]
    fn a_handed_off_pid_stops_its_live_successor() {
        let r = rig("lineage");
        let old = worker(&r);
        let new = worker(&r);
        {
            let mut inner = r.k.lock();
            inner.st.pid_mut(old).unwrap().state = "handed-off".into();
            inner.st.pid_mut(old).unwrap().handed_to = Some(new);
            inner.st.pid_mut(new).unwrap().handed_from = Some(old);
        }
        let out = stop(&r, Caller::Seat(2, "claude:l2".into()), &[&old.to_string()]).unwrap();
        assert!(
            out.contains(&format!("PID {old} was handed off to it")),
            "{out}"
        );
        wait_ended(&r, new);
        assert_eq!(result_json(&r, new)["how"], "cancelled");
        assert_eq!(r.k.lock().st.pid(old).unwrap().state, "handed-off");
    }

    #[test]
    fn stop_from_a_thread_that_is_not_a_seat_is_refused_through_ctl() {
        let r = rig("ctl");
        let pid = worker(&r);
        let err =
            r.k.ctl(&json!({"argv": ["cancel", pid.to_string()]}), None)
                .unwrap_err();
        assert!(err.to_string().contains("not an UNVRS seat"), "{err}");
        assert_eq!(events(&r, "refused").len(), 1);
        assert_eq!(r.k.lock().st.pid(pid).unwrap().state, "working");
    }
}
