//! Expected events and repeated failures; worker stop classification lives elsewhere.
use super::{Inner, Kernel, TurnRequest, TurnResult, now_ms};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Watchdog {
    jobs: BTreeMap<String, Health>,
    /// Wall clock and monotonic clock at the last sweep (system sleep detection).
    clock: Option<(u64, std::time::Instant)>,
}

/// Suspensions shorter than this are scheduling noise, not system sleep.
const SUSPEND_MIN_MS: u64 = 2_000;
#[derive(Default)]
struct Health {
    failures: u32,
    deadline: Option<u64>,
    interval: u64,
    /// Maximum wait from completion to the next start (cadence + grace).
    next_start_after: Option<u64>,
    waiting_for_start: bool,
    alarmed: bool,
    reason: String,
}
impl Watchdog {
    /// Time the process spent suspended since the last sweep: wall-clock elapsed minus
    /// monotonic elapsed (`Instant` does not advance during macOS or Linux system sleep).
    /// Deadlines are wall-clock, but the job threads pace themselves with `Instant`, so
    /// after a sleep every deadline has passed although no job missed a start; each
    /// deadline moves by the suspension instead of raising a false alarm.
    fn resume_after_sleep(&mut self, wall: u64, mono: std::time::Instant) -> u64 {
        let suspended = match self.clock {
            Some((w, m)) => wall
                .saturating_sub(w)
                .saturating_sub(mono.saturating_duration_since(m).as_millis() as u64),
            None => 0,
        };
        self.clock = Some((wall, mono));
        if suspended < SUSPEND_MIN_MS {
            return 0;
        }
        for h in self.jobs.values_mut() {
            if let Some(d) = h.deadline.as_mut() {
                *d = d.saturating_add(suspended);
            }
        }
        suspended
    }
    pub(super) fn active(&self, job: &str) -> bool {
        self.jobs.get(job).is_some_and(|h| h.failures > 0)
    }
    fn periodic(&mut self, job: &str, now: u64, cadence: u64, grace: u64) {
        let h = self.jobs.entry(job.into()).or_default();
        h.next_start_after = Some(cadence.saturating_add(grace));
        h.deadline = Some(now.saturating_add(grace));
        h.interval = grace;
        h.waiting_for_start = true;
    }
    fn begin(&mut self, job: &str, now: u64, interval: u64) {
        let h = self.jobs.entry(job.into()).or_default();
        h.deadline = Some(now.saturating_add(interval));
        h.interval = interval;
        h.waiting_for_start = false;
    }
    fn progress(&mut self, job: &str, now: u64) {
        if let Some(h) = self.jobs.get_mut(job)
            && h.deadline.is_some()
        {
            h.deadline = Some(now.saturating_add(h.interval));
        }
    }
    pub(super) fn finish(
        &mut self,
        job: &str,
        error: Option<&str>,
        immediate: bool,
    ) -> Option<String> {
        self.finish_at(job, error, immediate, now_ms())
    }
    fn finish_at(
        &mut self,
        job: &str,
        error: Option<&str>,
        immediate: bool,
        now: u64,
    ) -> Option<String> {
        let h = self.jobs.entry(job.into()).or_default();
        h.deadline = h
            .next_start_after
            .map(|interval| now.saturating_add(interval));
        if let Some(interval) = h.next_start_after {
            h.interval = interval;
            h.waiting_for_start = true;
        }
        if let Some(error) = error {
            h.failures = h.failures.saturating_add(1);
            h.reason = error.into();
            if (h.failures >= 3 || immediate) && !h.alarmed {
                h.alarmed = true;
                return Some(format!("{job}: {error}"));
            }
        } else {
            h.failures = 0;
            h.alarmed = false;
            h.reason.clear();
        }
        None
    }
    fn stalled(&mut self, now: u64) -> Vec<(String, String)> {
        self.jobs
            .iter_mut()
            .filter_map(|(job, h)| {
                if h.deadline.is_some_and(|d| now >= d) && !h.alarmed {
                    h.alarmed = true;
                    Some((
                        job.clone(),
                        format!(
                            "{job}: no expected {} for {} ms; last error: {}",
                            if h.waiting_for_start {
                                "start"
                            } else {
                                "event/completion"
                            },
                            h.interval,
                            if h.reason.is_empty() {
                                "none recorded"
                            } else {
                                &h.reason
                            }
                        ),
                    ))
                } else {
                    None
                }
            })
            .collect()
    }
}
impl Kernel {
    pub(super) fn job_periodic(&self, job: &str, cadence: u64, grace: u64) {
        self.lock().watchdog.periodic(job, now_ms(), cadence, grace);
    }
    pub(super) fn job_begin(&self, job: &str, interval: u64) {
        self.lock().watchdog.begin(job, now_ms(), interval);
    }
    pub(super) fn job_finish(&self, job: &str, error: Option<String>, immediate: bool) {
        let mut inner = self.lock();
        self.event(&mut inner, "job", json!({"job":job,"error":error}));
        // event() records normal failures; startup failures alarm on their first occurrence.
        if immediate
            && error.is_some()
            && let Some(reason) = inner.watchdog.finish(job, error.as_deref(), true)
        {
            self.alarm(&mut inner, job, &reason);
        }
    }
    pub(super) fn observe_job(&self, inner: &mut Inner, kind: &str, fields: &Value) {
        if kind == "driver.event" {
            if let (Some(harness), Some(pid)) = (fields["harness"].as_str(), fields["pid"].as_u64())
            {
                inner
                    .watchdog
                    .progress(&format!("turn:{harness}:{pid}"), now_ms());
            }
            return;
        }
        if kind == "signal.refused" {
            let job = format!("signal:{}", fields["os_pid"]);
            self.alarm(
                inner,
                &job,
                fields["reason"]
                    .as_str()
                    .unwrap_or("process signal refused"),
            );
            return;
        }
        if matches!(kind, "watchdog" | "wake" | "fold" | "turn" | "outcome") {
            return;
        }
        let error = fields["error"].as_str();
        let explicit = fields["job"].as_str();
        if explicit.is_none() && error.is_none() {
            return;
        }
        let job = explicit.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "{kind}:{}",
                fields["harness"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| fields["pid"].to_string())
            )
        });
        if let Some(reason) = inner.watchdog.finish(&job, error, false) {
            self.alarm(inner, &job, &reason);
        }
    }
    fn alarm(&self, inner: &mut Inner, job: &str, reason: &str) {
        let reason = inner.mem.redact(reason);
        self.event(inner, "watchdog", json!({"job":job,"error":reason}));
        let pid = inner.st.l1().unwrap_or_else(|| self.create_l1(inner));
        if let Err(e) = self.wake_seat(inner, pid, "watchdog", &reason, None, Some(job.into())) {
            eprintln!("watchdog wake failed: {e:#}; {reason}");
        }
        self.save(inner);
    }
    pub(super) fn sweep_watchdog(&self) {
        let diagnostics = self.drivers.diagnostics();
        for d in diagnostics {
            let job = d["job"].as_str().unwrap_or("driver").to_owned();
            let error = d["error"].as_str().map(str::to_owned);
            let mut inner = self.lock();
            self.event(&mut inner, "summary.route", d.clone());
            if error.is_some()
                && let Some(reason) = inner.watchdog.finish(&job, error.as_deref(), true)
            {
                self.alarm(&mut inner, &job, &reason);
            }
        }
        let now = now_ms();
        let suspended = self
            .lock()
            .watchdog
            .resume_after_sleep(now, std::time::Instant::now());
        if suspended > 0 {
            let mut inner = self.lock();
            self.event(
                &mut inner,
                "watchdog.resume",
                json!({"suspended_ms": suspended, "note": "system sleep: job deadlines moved, no alarm"}),
            );
        }
        self.expire_jobs(now);
    }
    fn expire_jobs(&self, now: u64) {
        let mut inner = self.lock();
        for (job, reason) in inner.watchdog.stalled(now) {
            self.alarm(&mut inner, &job, &reason);
        }
    }
    pub(super) fn driver_turn(
        &self,
        pid: usize,
        req: &TurnRequest,
        started: &dyn Fn(u32),
    ) -> Result<TurnResult> {
        let _owner = crate::signals::owner_scope(self.u.root(), pid);
        let job = format!("turn:{}:{pid}", req.harness);
        self.job_begin(&job, 120_000);
        let result = self.drivers.turn_observed(req, started, &|event| {
            let mut inner = self.lock();
            if event.is_null() {
                inner.watchdog.progress(&job, now_ms());
                return;
            }
            if let Ok(rec) = inner.st.pid_mut(pid)
                && let Some(driven) = rec.driven.as_mut()
            {
                driven.last_activity = Some(now_ms());
                if let Some(session) = event["session"].as_str() {
                    driven.session = Some(session.into());
                }
                if event["kind"] == "accepted" {
                    driven.actual_model = event["model"].as_str().map(str::to_owned);
                    driven.actual_effort = event["effort"].as_str().map(str::to_owned);
                    driven.effort_evidence = event["effort_evidence"].as_str().map(str::to_owned);
                }
                if event["kind"] == "tool.started" {
                    driven.remember_tool(event["tool"].as_str().unwrap_or("tool"));
                }
            }
            self.event(
                &mut inner,
                "driver.event",
                json!({"pid": pid, "harness": req.harness, "event": event}),
            );
            if event["kind"] == "accepted" {
                self.save(&mut inner);
            }
        });
        let error = match &result {
            Err(e) => Some(format!("{e:#}")),
            Ok(out) => out.error.clone(),
        };
        self.lock().watchdog.finish(&job, None, false);
        self.job_finish(&format!("driver:{}", req.harness), error, false);
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Kernel {
        use super::super::{Drivers, SilentMapp, State, Universe, state::Journal};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicU64},
        };
        struct Fake;
        impl Drivers for Fake {
            fn fold(&self, _: &crate::FoldJob) -> Result<crate::BriefFold> {
                unreachable!()
            }
            fn turn(&self, _: &TurnRequest, _: &dyn Fn(u32)) -> Result<TurnResult> {
                unreachable!()
            }
            fn turn_observed(
                &self,
                req: &TurnRequest,
                _: &dyn Fn(u32),
                event: &dyn Fn(&Value),
            ) -> Result<TurnResult> {
                event(
                    &json!({"kind":"accepted", "session":"managed-session", "model":req.model,
                    "effort":req.effort, "effort_evidence":"scripted accepted config", "native_delegation":false}),
                );
                event(&json!({"kind":"tool.started", "tool":"shell", "session":"managed-session"}));
                event(&json!({"kind":"turn.completed", "session":"managed-session"}));
                Ok(TurnResult {
                    session: Some("managed-session".into()),
                    model: req.model.clone(),
                    effort: req.effort.clone(),
                    ..Default::default()
                })
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "unvrs-watchdog-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let u = Universe::at(&path).unwrap();
        u.init().unwrap();
        Kernel {
            journal: Journal::new(u.journal_path()),
            inner: Mutex::new(Inner {
                st: State::load(&u.state_path()).unwrap(),
                dirty: false,
                mem: crate::MemoryIndex::open(u.root()).unwrap(),
                watchdog: Default::default(),
                folding: Default::default(),
                running: Default::default(),
                watchers: Default::default(),
                recent: Default::default(),
                quota: Default::default(),
                seat_backoff: Default::default(),
            }),
            u,
            drivers: Arc::new(Fake),
            mapp: Arc::new(SilentMapp),
            stop: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            started: now_ms(),
            last_active: AtomicU64::new(now_ms()),
        }
    }
    #[test]
    fn managed_lifecycle_updates_pid_snapshot_and_persisted_configuration() {
        use super::super::{Driven, State};
        let k = fixture();
        let pid = {
            let mut inner = k.lock();
            let pid = inner
                .st
                .create(0, 3, "driven", "codex", "task", Default::default(), None);
            let rec = inner.st.pid_mut(pid).unwrap();
            rec.state = "working".into();
            rec.driven = Some(Driven {
                cwd: k.u.root().into(),
                model: Some("gpt-6.1-sol".into()),
                effort: Some("high".into()),
                ..Default::default()
            });
            pid
        };
        k.driver_turn(
            pid,
            &TurnRequest {
                harness: "codex".into(),
                cwd: k.u.root().into(),
                session: None,
                model: Some("gpt-6.1-sol".into()),
                effort: Some("high".into()),
                prompt: "task".into(),
                env: vec![],
            },
            &|_| {},
        )
        .unwrap();
        let saved = State::load(&k.u.state_path()).unwrap();
        let d = saved.pid(pid).unwrap().driven.as_ref().unwrap();
        assert_eq!(d.session.as_deref(), Some("managed-session"));
        assert_eq!(d.actual_model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(d.actual_effort.as_deref(), Some("high"));
        assert!(d.last_activity.is_some());
        let snap = k.snapshot();
        assert_eq!(snap["workers"][0]["session"], "managed-session");
        assert!(snap["workers"][0]["last_activity"].as_u64().is_some());
        assert_eq!(snap["workers"][0]["tools"], json!(["shell"]));
        assert_eq!(
            k.journal
                .tail(30)
                .iter()
                .filter(|e| e["kind"] == "driver.event")
                .count(),
            3
        );
        {
            let mut inner = k.lock();
            inner.st.pid_mut(pid).unwrap().project = Some("fixture".into());
            // A later turn's edit receipt retains the earlier shell linkage.
            inner
                .st
                .pid_mut(pid)
                .unwrap()
                .driven
                .as_mut()
                .unwrap()
                .remember_tool("edit: file change");
            k.finish(&mut inner, pid, "done", "done");
        }
        let result: Value = serde_json::from_slice(
            &std::fs::read(
                k.u.project_dir("fixture")
                    .join(format!("tasks/pid-{pid}/result.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (
                result["session"].as_str(),
                result["tools"].clone(),
                result["last_activity"].as_u64()
            ),
            (
                Some("managed-session"),
                json!(["edit", "shell"]),
                snap["workers"][0]["last_activity"].as_u64()
            )
        );
        let root = k.u.root().to_path_buf();
        drop(k);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn driver_stream_events_keep_only_their_active_turn_alive() {
        let k = fixture();
        let root = k.u.root().to_path_buf();
        let l1 = k.create_l1(&mut k.lock());
        let job = "turn:claude:162";
        let sibling = "turn:codex:162";
        k.job_begin(sibling, 5_000_000);
        let sibling_deadline = k.lock().watchdog.jobs[sibling].deadline;
        for kind in [
            "tool.started",
            "tool.completed",
            "progress",
            "delta",
            "protocol.dropped",
        ] {
            // Simulate a turn already beyond its old deadline, then receive a real journal event.
            k.lock().watchdog.begin(job, 0, 120_000);
            let emitted = now_ms();
            k.event(
                &mut k.lock(),
                "driver.event",
                json!({"pid":162, "harness":"claude", "event":{"kind":kind}}),
            );
            let deadline = k.lock().watchdog.jobs[job].deadline.unwrap();
            assert!(
                deadline >= emitted + 120_000,
                "{kind} did not refresh liveness"
            );
            assert_eq!(k.lock().watchdog.jobs[sibling].deadline, sibling_deadline);
            k.expire_jobs(deadline - 1);
            assert!(k.lock().st.pid(l1).unwrap().wakes.is_empty());
        }
        let deadline = k.lock().watchdog.jobs[job].deadline.unwrap();
        for fields in [
            json!({"pid":163, "harness":"claude", "event":{"kind":"progress"}}),
            json!({"pid":162, "event":{"kind":"progress"}}),
            json!({"harness":"claude", "event":{"kind":"progress"}}),
        ] {
            k.event(&mut k.lock(), "driver.event", fields);
            assert_eq!(k.lock().watchdog.jobs[job].deadline, Some(deadline));
        }
        k.expire_jobs(deadline);
        k.expire_jobs(deadline + 1);
        assert_eq!(k.lock().st.pid(l1).unwrap().wakes.len(), 1);
        assert!(k.lock().st.pid(l1).unwrap().wakes[0].text.contains(job));
        k.lock().watchdog.finish(job, None, false);
        k.event(
            &mut k.lock(),
            "driver.event",
            json!({"pid":162,"harness":"claude","event":{"kind":"progress"}}),
        );
        assert!(
            k.lock().watchdog.jobs[job].deadline.is_none(),
            "late event revived a finished turn"
        );
        drop(k);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exited_owned_group_creates_no_refusal_event_or_watchdog_wake() {
        let k = fixture();
        let root = k.u.root().to_path_buf();
        let store = crate::signals::LedgerStore::new(&root);
        let pid = crate::signals::fake_worker(&root, 0).unwrap();
        store.signal(i64::from(pid), libc::SIGKILL, true).unwrap();
        // The unit boundary returns EPERM once this owned fake child has exited.
        store.signal(i64::from(pid), libc::SIGKILL, true).unwrap();
        assert!(store.refusals().unwrap().is_empty());
        k.collect_signal_refusals();
        assert!(
            k.journal
                .tail(100)
                .iter()
                .all(|e| e["kind"] != "signal.refused" && e["kind"] != "watchdog")
        );
        assert!(
            k.lock()
                .st
                .pids
                .values()
                .all(|p| p.wakes.iter().all(|w| w.kind != "watchdog"))
        );
        crate::signals::wait_fake_worker(&root, pid).unwrap();
        assert!(store.snapshot().unwrap().entries().is_empty());
        drop(k);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn signal_refusal_journals_a_watchdog_warning_and_wakes_l1_immediately() {
        let k = fixture();
        let root = k.u.root().to_path_buf();
        let store = crate::signals::LedgerStore::new(&root);
        assert!(store.signal(-1, libc::SIGTERM, true).is_err());
        let reason = store.refusals().unwrap()[0].1.reason.clone();
        k.collect_signal_refusals();
        assert!(store.refusals().unwrap().is_empty());
        let saved = super::super::State::load(&k.u.state_path()).unwrap();
        let l1 = saved.l1().unwrap();
        assert!(
            saved
                .pid(l1)
                .unwrap()
                .wakes
                .iter()
                .any(|w| w.kind == "watchdog" && w.text.contains(&reason))
        );
        let events = k.journal.tail(100);
        assert!(events.iter().any(|e| e["kind"] == "signal.refused"));
        assert!(
            events
                .iter()
                .any(|e| e["kind"] == "watchdog" && e["error"] == reason)
        );
        drop(k);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kernel_journals_reasons_and_persists_pid1_wakes() {
        use super::super::State;
        let k = fixture();
        let path = k.u.root().to_path_buf();
        let pid = {
            let mut inner = k.lock();
            k.create_l1(&mut inner)
        };
        for _ in 0..3 {
            k.job_finish(
                "driver:test",
                Some("exact underlying refusal".into()),
                false,
            );
        }
        k.job_begin("stalled", 0);
        k.sweep_watchdog();
        k.sweep_watchdog();
        assert_eq!(k.lock().st.pid(pid).unwrap().wakes.len(), 2);
        let saved = State::load(&k.u.state_path()).unwrap();
        assert!(
            saved.pid(pid).unwrap().wakes[0]
                .text
                .contains("exact underlying refusal")
        );
        let journal = std::fs::read_to_string(k.u.journal_path()).unwrap();
        assert_eq!(journal.lines().filter(|l|l.contains("\"kind\":\"job\"") && l.contains("exact underlying refusal")).count(),3);
        assert_eq!(
            journal
                .lines()
                .filter(|l| l.contains("\"kind\":\"watchdog\""))
                .count(),
            2
        );
        drop(k);
        std::fs::remove_dir_all(path).unwrap();
    }

    /// System sleep freezes the job threads (monotonic clock) while wall-clock deadlines
    /// pass: the sweep after a wake moves the deadlines instead of alarming.
    #[test]
    fn system_sleep_moves_deadlines_instead_of_alarming() {
        use std::time::{Duration, Instant};
        let mut w = Watchdog::default();
        let mono = Instant::now();
        w.periodic("memory.sweep", 1_000, 60_000, 5_000);
        assert_eq!(w.resume_after_sleep(1_000, mono), 0);
        // 10 minutes of wall clock pass, 1 s of monotonic time: 599 s suspended.
        let suspended = w.resume_after_sleep(601_000, mono + Duration::from_secs(1));
        assert_eq!(suspended, 599_000);
        assert_eq!(
            w.jobs["memory.sweep"].deadline,
            Some(1_000 + 5_000 + 599_000)
        );
        assert!(w.stalled(601_000).is_empty(), "no false alarm after sleep");
        assert_eq!(w.stalled(605_000).len(), 1, "a real stall still alarms");
        // Normal ticks (no gap between clocks) leave deadlines alone.
        assert_eq!(
            w.resume_after_sleep(602_000, mono + Duration::from_secs(2)),
            0
        );
    }

    #[test]
    fn catalog_startup_waits_for_refresh_cadence_before_grace() {
        use super::super::PERIODIC_GRACE_MS;
        let k = std::sync::Arc::new(fixture());
        let path = k.u.root().to_path_buf();
        let pid = k.create_l1(&mut k.lock());
        // Startup and later refreshes own registration, including edited cadence.
        for (round, minutes) in [5, 7].into_iter().enumerate() {
            std::fs::write(
                path.join("econ.toml"),
                crate::drv_econ::DEFAULT_CONFIG.replace(
                    "refresh_minutes = 5",
                    &format!("refresh_minutes = {minutes}"),
                ),
            )
            .unwrap();
            let started = now_ms();
            k.refresh_econ();
            let cadence = k.econ_refresh_seconds() * 1000;
            assert_eq!(cadence, minutes * 60_000);
            let due = {
                let inner = k.lock();
                let health = &inner.watchdog.jobs["econ.catalog"];
                assert_eq!(health.interval, cadence + PERIODIC_GRACE_MS);
                health.deadline.unwrap()
            };
            assert!(due >= started + cadence + PERIODIC_GRACE_MS);
            k.expire_jobs(started + PERIODIC_GRACE_MS + 1);
            k.expire_jobs(due - 1);
            assert_eq!(k.lock().st.pid(pid).unwrap().wakes.len(), round);
            k.expire_jobs(due);
            k.expire_jobs(due + 1);
            assert_eq!(k.lock().st.pid(pid).unwrap().wakes.len(), round + 1);
        }
        drop(k);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missed_periodic_starts_and_completions_wake_pid1_within_grace() {
        use super::super::{
            MAINTENANCE_INTERVAL_MS, MEMORY_INTERVAL_SECS, PERIODIC_GRACE_MS, QUOTA_INTERVAL_SECS,
            State,
        };
        for (job, cadence, budget) in [
            ("quota", QUOTA_INTERVAL_SECS * 1000, 60_000),
            ("memory.sweep", MEMORY_INTERVAL_SECS * 1000, 120_000),
            ("maintenance", MAINTENANCE_INTERVAL_MS, PERIODIC_GRACE_MS),
            ("econ.catalog", 300_000, 300_000 + PERIODIC_GRACE_MS),
        ] {
            let k = fixture();
            let path = k.u.root().to_path_buf();
            let pid = {
                let mut inner = k.lock();
                k.create_l1(&mut inner)
            };
            let wakes = || k.lock().st.pid(pid).unwrap().wakes.clone();
            let now = 100;
            k.lock()
                .watchdog
                .periodic(job, now, cadence, PERIODIC_GRACE_MS);
            // Registered before thread spawn: even a missing first START is detected.
            k.expire_jobs(now + PERIODIC_GRACE_MS - 1);
            assert!(wakes().is_empty());
            k.expire_jobs(now + PERIODIC_GRACE_MS);
            k.expire_jobs(now + PERIODIC_GRACE_MS + 1);
            assert_eq!(wakes().len(), 1);
            assert!(wakes()[0].text.contains("no expected start"));
            let recovered = now + PERIODIC_GRACE_MS + 2;
            k.lock().watchdog.begin(job, recovered, budget);
            k.lock().watchdog.finish_at(job, None, false, recovered + 1);
            // After completion, a stopped scheduler cannot remove coverage.
            let due = recovered + 1 + cadence + PERIODIC_GRACE_MS;
            k.expire_jobs(due - 1);
            assert_eq!(wakes().len(), 1);
            k.expire_jobs(due);
            k.expire_jobs(due + 1);
            assert_eq!(wakes().len(), 2);
            assert!(wakes()[1].text.contains("no expected start"));
            k.lock().watchdog.begin(job, due + 2, budget);
            k.lock().watchdog.finish_at(job, None, false, due + 3);
            let started = due + 3 + cadence;
            k.lock().watchdog.begin(job, started, budget);
            k.expire_jobs(started + budget - 1);
            assert_eq!(wakes().len(), 2);
            k.expire_jobs(started + budget);
            k.expire_jobs(started + budget + 1);
            assert_eq!(wakes().len(), 3);
            assert!(wakes()[2].text.contains("no expected event/completion"));
            // A failed completion retains the reason and deduplication until success.
            k.lock().watchdog.finish_at(
                job,
                Some("exact periodic failure"),
                false,
                started + budget + 2,
            );
            k.expire_jobs(started + budget + 2 + cadence + PERIODIC_GRACE_MS);
            assert_eq!(wakes().len(), 3);
            assert_eq!(k.lock().watchdog.jobs[job].reason, "exact periodic failure");
            assert_eq!(
                State::load(&k.u.state_path())
                    .unwrap()
                    .pid(pid)
                    .unwrap()
                    .wakes
                    .len(),
                3
            );
            let journal = std::fs::read_to_string(k.u.journal_path()).unwrap();
            assert_eq!(
                journal
                    .lines()
                    .filter(|line| line.contains("\"kind\":\"watchdog\""))
                    .count(),
                3
            );
            drop(k);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn three_failures_and_silence_alarm_once_then_success_resets() {
        let mut w = Watchdog::default();
        assert!(w.finish("fold", Some("model missing"), false).is_none());
        assert!(w.finish("fold", Some("model missing"), false).is_none());
        assert_eq!(
            w.finish("fold", Some("model missing"), false).as_deref(),
            Some("fold: model missing")
        );
        assert!(w.finish("fold", Some("again"), false).is_none());
        w.finish("fold", None, false);
        w.begin("fold", 100, 120_000);
        assert!(w.stalled(120_099).is_empty());
        assert_eq!(w.stalled(120_100).len(), 1);
        assert!(w.stalled(999_999).is_empty());
        w.finish("fold", None, false);
        w.begin("fold", 0, 100);
        w.progress("fold", 50);
        assert!(w.stalled(149).is_empty());
        assert_eq!(w.stalled(150).len(), 1);
        assert_eq!(
            w.finish("startup", Some("unsupported model"), true)
                .as_deref(),
            Some("startup: unsupported model")
        );
    }
}
