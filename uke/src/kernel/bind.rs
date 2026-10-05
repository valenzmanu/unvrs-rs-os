//! Lifecycle channel (hooks). Threads bind only by command (D35): a thread the kernel
//! does not know is untouched. Seats hold knowledge, threads occupy seats; claiming an
//! occupied seat moves it (D34): the kernel writes the handoff package, the new thread
//! rehydrates, the old one is told once that it detached.
use super::{Inner, Kernel, hot, procinfo, state::ThreadRec, state::now_ms};
#[cfg(test)]
use crate::signals::CommandTracking;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};

#[derive(Debug, Default)]
pub struct HookReply {
    pub context: Option<String>,
    pub system: Option<String>,
    /// UserPromptSubmit: stop the prompt before the model and show this instead.
    pub block: Option<String>,
    /// Stop: keep the turn going with this input (re-wake at turn end).
    pub stop: Option<String>,
}

/// What a hook tells the kernel about its thread.
#[derive(Clone, Debug)]
pub struct ThreadInfo {
    pub key: String,
    pub harness: String,
    pub session: String,
    pub cwd: PathBuf,
    pub os_pid: Option<u32>,
    pub transcript: Option<String>,
}

const IDLE_UNKNOWN_MS: u64 = 30 * 60 * 1000;

/// A hook reported the thread's harness process: keep its OS pid and re-derive the app
/// from it (only while that process runs), so the label follows the live process.
fn refresh_os_pid(
    t: &mut ThreadRec,
    os_pid: Option<u32>,
    live_app: impl Fn(u32) -> Option<String>,
) {
    let Some(p) = os_pid else {
        return;
    };
    t.os_pid = Some(p);
    if let Some(app) = live_app(p) {
        t.app = app;
    }
}

impl Kernel {
    pub(crate) fn thread_live(&self, inner: &Inner, key: &str) -> bool {
        inner.st.threads.get(key).is_some_and(|t| {
            t.bound
                && match t.os_pid {
                    Some(p) => procinfo::alive(p),
                    None => now_ms().saturating_sub(t.last_seen) < IDLE_UNKNOWN_MS,
                }
        })
    }

    /// Puts the seat's unacknowledged wakes delivered to `key` back in its queue (C5):
    /// only a Stop from that thread acks them, and a thread that left the seat will not
    /// send one for it. The next thread (or a detached run) gets them again.
    pub(crate) fn requeue_delivered(
        &self,
        inner: &mut Inner,
        pid: usize,
        key: &str,
        reason: &str,
    ) -> Vec<u64> {
        let mut requeued = vec![];
        if let Ok(rec) = inner.st.pid_mut(pid) {
            for w in rec.wakes.iter_mut() {
                if !w.acked && w.delivered.as_deref() == Some(key) {
                    w.delivered = None;
                    requeued.push(w.id);
                }
            }
        }
        if !requeued.is_empty() {
            self.event(
                inner,
                "wake.requeue",
                json!({"pid": pid, "wakes": requeued, "from": key, "reason": reason}),
            );
        }
        requeued
    }

    /// Detaches a thread from its seat; returns the PID to fold. A notice is kept for
    /// the thread's next prompt (and its marker file stays until it is delivered). Wakes
    /// in flight to the thread go back to the seat's queue.
    pub(crate) fn detach(
        &self,
        inner: &mut Inner,
        key: &str,
        reason: &str,
        notice: Option<String>,
    ) -> Option<usize> {
        let t = inner.st.threads.get_mut(key)?;
        if !t.bound {
            if notice.is_some() {
                t.notice = notice;
            }
            return None;
        }
        t.bound = false;
        t.detached = Some(reason.into());
        t.notice = notice;
        let (harness, app, pid) = (t.harness.clone(), t.app.clone(), t.pid);
        let pid = pid?;
        self.requeue_delivered(inner, pid, key, reason);
        let mut rank = 0;
        if let Ok(rec) = inner.st.pid_mut(pid)
            && rec.thread.as_deref() == Some(key)
        {
            rec.thread = None;
            rec.state = "idle".into();
            rec.updated = now_ms();
            rank = rec.rank;
        }
        self.event(
            inner,
            "detach",
            json!({"pid": pid, "rank": rank, "harness": harness, "app": app, "thread": key, "reason": reason}),
        );
        Some(pid)
    }

    /// At kernel start: re-derive the app of every bound thread whose harness is still
    /// running, so records written by an older binary (before claude-desktop detection)
    /// report what the thread runs in now, without a rebind.
    pub(crate) fn refresh_apps(&self) {
        self.refresh_apps_with(procinfo::live_app);
    }

    fn refresh_apps_with(&self, live_app: impl Fn(u32) -> Option<String>) {
        let mut inner = self.lock();
        let mut changed = vec![];
        for t in inner.st.threads.values_mut().filter(|t| t.bound) {
            let Some(app) = t.os_pid.and_then(&live_app) else {
                continue;
            };
            if app != t.app {
                changed.push(json!({"pid": t.pid, "thread": t.key, "from": t.app, "app": app}));
                t.app = app;
            }
        }
        if changed.is_empty() {
            return;
        }
        for c in changed {
            self.event(&mut inner, "thread.app", c);
        }
        self.save(&mut inner);
    }

    /// Creates or refreshes the thread record (only for threads that bind).
    fn touch(&self, inner: &mut Inner, info: &ThreadInfo) {
        let now = now_ms();
        let app = info
            .os_pid
            .map(procinfo::app_of)
            .unwrap_or_else(|| "headless".into());
        let t = inner
            .st
            .threads
            .entry(info.key.clone())
            .or_insert_with(|| ThreadRec {
                key: info.key.clone(),
                harness: info.harness.clone(),
                session: info.session.clone(),
                attached_at: now,
                ..Default::default()
            });
        t.cwd = info.cwd.clone();
        t.last_seen = now;
        if info.os_pid.is_some() {
            t.os_pid = info.os_pid;
            t.app = app;
        }
        if let Some(tp) = &info.transcript {
            t.transcript = Some(tp.clone());
        }
    }

    /// Claims `seat` for the thread: a move (handoff into the seat) when another thread
    /// held it. Returns the rehydrate context.
    pub(crate) fn claim(
        &self,
        inner: &mut Inner,
        info: &ThreadInfo,
        seat: usize,
        why: &str,
    ) -> Result<String> {
        self.touch(inner, info);
        let key = info.key.as_str();
        let t = inner.st.threads[key].clone();
        // Leaving another seat.
        if let Some(q) = t.pid
            && q != seat
            && inner.st.pids.get(&q).and_then(|r| r.thread.as_deref()) == Some(key)
        {
            let rec = inner.st.pid_mut(q)?;
            rec.thread = None;
            rec.state = "idle".into();
            self.requeue_delivered(inner, q, key, "took another seat");
            self.event(
                inner,
                "detach",
                json!({"pid": q, "thread": key, "reason": "took another seat"}),
            );
        }
        let rec = inner.st.pid(seat)?.clone();
        let (rank, project) = (rec.rank, rec.project.clone());
        if rec.thread.as_deref() == Some(key) && t.bound {
            let body = hot::hot_text(self, inner, seat)?;
            hot::mark_seen(inner, key, seat);
            return Ok(format!(
                "{}\n{body}",
                self.mapp.seated(rank, project.as_deref(), "rebound", None)
            ));
        }
        let previous = rec.sessions.last().cloned().filter(|s| s != key);
        let from_app = previous
            .as_ref()
            .and_then(|p| inner.st.threads.get(p))
            .map(|t| t.app.clone())
            .unwrap_or_else(|| rec.harness.clone());
        let mid_turn = rec
            .thread
            .as_ref()
            .and_then(|h| inner.st.threads.get(h))
            .is_some_and(|h| h.turn_open);
        if let Some(h) = rec.thread.clone().filter(|h| h != key) {
            let notice = self.mapp.detached(rank, project.as_deref(), &t.app);
            self.detach(
                inner,
                &h,
                &format!("seat moved to a {} thread", t.app),
                Some(notice),
            );
        }
        let mut how = if why == "rebind" { "rebound" } else { "bound" };
        if let Some(prev) = &previous {
            // The seat moves: its handoff package is written from the brief, memory and
            // the recent turns; the new thread rehydrates from it.
            let package = hot::hot_text(self, inner, seat)?;
            let brief = inner.mem.session(seat).map(|s| s.brief).unwrap_or_default();
            let dir = self.u.root().join("handoffs");
            fs::create_dir_all(&dir)?;
            let path = dir.join(format!("seat-{seat}-{}.json", now_ms()));
            let value = json!({
                "pid": seat, "rank": rank, "project": project, "from": prev, "to": key,
                "from_harness": rec.harness, "to_harness": t.harness, "created": now_ms(),
                "brief": brief, "package": inner.mem.redact(&package),
            });
            let bytes = serde_json::to_vec_pretty(&value)?;
            fs::write(&path, &bytes)?;
            let kind = if rec.harness != t.harness {
                "swap"
            } else {
                "move"
            };
            how = if kind == "swap" { "swapped" } else { "moved" };
            self.event(
                inner,
                "move",
                json!({"pid": seat, "rank": rank, "move": kind, "from": prev, "to": key,
                    "from_app": from_app, "to_app": t.app, "from_harness": rec.harness,
                    "to_harness": t.harness, "bytes": bytes.len(), "package": path,
                    "open": brief.open.len(), "mid_turn": mid_turn, "why": why}),
            );
        }
        let r = inner.st.pid_mut(seat)?;
        r.thread = Some(key.into());
        r.state = "live".into();
        r.harness = t.harness.clone();
        r.updated = now_ms();
        if !r.sessions.contains(&t.key) {
            r.sessions.push(t.key.clone());
        } else {
            r.sessions.retain(|s| s != key);
            r.sessions.push(key.into());
        }
        // A thread gives the seat a known harness: refusals and failures from before
        // (C7, S2) no longer hold its next detached run back.
        inner.seat_backoff.remove(&seat);
        if let Some(tr) = inner.st.threads.get_mut(key) {
            tr.pid = Some(seat);
            tr.bound = true;
            tr.detached = None;
            tr.notice = None;
            tr.rehydrate = false;
        }
        if previous.is_none() {
            self.event(
                inner,
                "bind",
                json!({"pid": seat, "rank": rank, "harness": t.harness, "app": t.app, "thread": key, "why": why}),
            );
        }
        let header = self.mapp.seated(
            rank,
            project.as_deref(),
            how,
            previous.as_ref().map(|_| from_app.as_str()),
        );
        let body = hot::hot_text(self, inner, seat)?;
        hot::mark_seen(inner, key, seat);
        Ok(format!("{header}\n{body}"))
    }

    /// A known thread that is not bound: deliver its notice once (and forget it), or
    /// rebind it silently when its seat is free.
    fn resume(&self, inner: &mut Inner, info: &ThreadInfo) -> Result<HookReply> {
        let key = info.key.as_str();
        let t = inner.st.threads[key].clone();
        if let Some(notice) = t.notice.clone() {
            inner.st.threads.remove(key);
            return Ok(HookReply {
                context: Some(notice.clone()),
                system: Some(notice),
                ..Default::default()
            });
        }
        if let Some(pid) = t.pid
            && let Ok(rec) = inner.st.pid(pid)
            && !matches!(rec.state.as_str(), "ended" | "handed-off")
        {
            let holder = rec.thread.clone();
            let free = holder.as_ref().is_none_or(|h| !self.thread_live(inner, h));
            // Only the thread that held the seat last resumes it silently.
            let last = rec.sessions.last().map(String::as_str) == Some(key);
            if free && last {
                if let Some(h) = holder {
                    self.detach(inner, &h, "harness exited", None);
                }
                return Ok(HookReply {
                    context: Some(self.claim(inner, info, pid, "rebind")?),
                    ..Default::default()
                });
            }
            let (rank, project) = (rec.rank, rec.project.clone());
            let to = holder
                .as_ref()
                .and_then(|h| inner.st.threads.get(h))
                .map(|h| h.app.clone())
                .unwrap_or_else(|| "another".into());
            let notice = self.mapp.detached(rank, project.as_deref(), &to);
            inner.st.threads.remove(key);
            return Ok(HookReply {
                context: Some(notice.clone()),
                system: Some(notice),
                ..Default::default()
            });
        }
        inner.st.threads.remove(key);
        Ok(HookReply::default())
    }

    pub(crate) fn hook(
        self: &Arc<Self>,
        event: &str,
        harness: &str,
        p: &Value,
        os_pid: Option<u32>,
    ) -> Result<HookReply> {
        anyhow::ensure!(
            matches!(harness, "claude" | "codex"),
            "Unknown harness {harness:?}"
        );
        let session = p["session_id"]
            .as_str()
            .context("hook payload has no session_id")?
            .to_owned();
        let info = ThreadInfo {
            key: format!("{harness}:{session}"),
            harness: harness.into(),
            session,
            cwd: PathBuf::from(p["cwd"].as_str().unwrap_or("")),
            os_pid,
            transcript: p["transcript_path"].as_str().map(str::to_owned),
        };
        let key = info.key.clone();
        let mut fold = None;
        let mut fold_drain = None;
        let reply = {
            let mut inner = self.lock();
            let known = inner.st.threads.contains_key(&key);
            let reply = match event {
                "UserPromptSubmit" => {
                    let prompt = p["prompt"].as_str().unwrap_or("");
                    let reply = if let Some(cmd) = super::captain::parse(prompt) {
                        self.captain(&mut inner, &info, cmd)?
                    } else if !known {
                        HookReply::default()
                    } else if inner.st.threads[&key].bound {
                        self.prompt(&mut inner, &info, prompt)?
                    } else {
                        let r = self.resume(&mut inner, &info)?;
                        if inner.st.threads.get(&key).is_some_and(|t| t.bound)
                            && let Some(t) = inner.st.threads.get_mut(&key)
                        {
                            t.pending_prompt = Some(prompt.chars().take(16000).collect());
                            t.turn_open = true;
                        }
                        r
                    };
                    self.record_captain_prompt(&mut inner, &key, prompt);
                    reply
                }
                _ if !known => HookReply::default(),
                "SessionStart" => {
                    let t = inner.st.threads[&key].clone();
                    if t.bound {
                        if let Some(tr) = inner.st.threads.get_mut(&key) {
                            refresh_os_pid(tr, os_pid, procinfo::live_app);
                            tr.last_seen = now_ms();
                        }
                        let pid = t.pid.context("bound thread without seat")?;
                        let body = hot::hot_text(self, &inner, pid)?;
                        hot::mark_seen(&mut inner, &key, pid);
                        HookReply {
                            context: Some(body),
                            ..Default::default()
                        }
                    } else {
                        self.resume(&mut inner, &info)?
                    }
                }
                "Stop" => {
                    let (drain, stop) = self.capture(&mut inner, &key, p)?;
                    fold_drain = drain;
                    HookReply {
                        stop,
                        ..Default::default()
                    }
                }
                "PreCompact" => {
                    if let Some(t) = inner.st.threads.get_mut(&key) {
                        t.rehydrate = true;
                        if t.bound {
                            fold = t.pid;
                        }
                    }
                    HookReply::default()
                }
                "SessionEnd" => {
                    fold = self.detach(&mut inner, &key, "session end", None);
                    HookReply::default()
                }
                _ => HookReply::default(),
            };
            self.save(&mut inner);
            reply
        };
        if let Some(pid) = fold {
            self.schedule_fold(
                pid,
                if event == "SessionEnd" {
                    "detach"
                } else {
                    "pre-compact"
                },
                false,
            );
        }
        if let Some(pid) = fold_drain {
            self.schedule_fold(pid, "tail threshold", true);
        }
        Ok(reply)
    }

    /// An ordinary prompt in a bound thread: the delta since its last turn (wakes, open
    /// decisions, mail), or the full hot set after a compaction.
    fn prompt(
        self: &Arc<Self>,
        inner: &mut Inner,
        info: &ThreadInfo,
        prompt: &str,
    ) -> Result<HookReply> {
        let key = info.key.as_str();
        let t = inner.st.threads.get_mut(key).context("Unknown thread")?;
        t.last_seen = now_ms();
        refresh_os_pid(t, info.os_pid, procinfo::live_app);
        t.pending_prompt = Some(prompt.chars().take(16000).collect());
        t.turn_open = true;
        let rehydrate = std::mem::take(&mut t.rehydrate);
        let pid = t.pid.context("Bound thread without seat")?;
        if rehydrate {
            let body = hot::hot_text(self, inner, pid)?;
            hot::mark_seen(inner, key, pid);
            return Ok(HookReply {
                context: Some(body),
                ..Default::default()
            });
        }
        Ok(HookReply {
            context: hot::delta(self, inner, key, pid),
            ..Default::default()
        })
    }

    /// V1 trusts the hook's human provenance; a local process can forge its payload.
    fn record_captain_prompt(&self, inner: &mut Inner, key: &str, text: &str) {
        let Some(rec) = inner
            .st
            .threads
            .get(key)
            .filter(|t| t.bound)
            .and_then(|t| t.pid)
            .and_then(|pid| inner.st.pids.get(&pid))
            .filter(|r| matches!(r.rank, 1 | 2))
        else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        let at = now_ms();
        let prompt = super::state::CaptainPrompt {
            id: inner.st.seq + 1,
            pid: rec.pid,
            thread: key.into(),
            project: rec.project.clone(),
            at,
            text: text.into(),
        };
        inner
            .st
            .captain_prompts
            .retain(|p| p.at <= at && at - p.at <= super::driven::GO_WINDOW_MS);
        self.event(inner, "captain.prompt", json!({"prompt_id":prompt.id,"pid":prompt.pid,"thread":prompt.thread,"project":prompt.project,"at":at,"text":text}));
        inner.st.captain_prompts.push(prompt);
    }

    /// The rewake watcher's poll (G-L1push). Claude Code runs `unvrs hook Watch` as an
    /// `asyncRewake` hook at SessionStart and Stop; when it exits 2 the idle model wakes
    /// with its output. An idle bound thread takes its queued wakes here, delivered to
    /// the thread so the rewake turn's Stop acknowledges them. During a turn it waits:
    /// the prompt and the Stop guard deliver. `done` retires the watcher: its thread
    /// is gone or unbound, or a newer watcher (armed at a later Stop) took over.
    pub(crate) fn watch(&self, inner: &mut Inner, key: &str, watcher: u32, arm: bool) -> Value {
        let seat = inner
            .st
            .threads
            .get(key)
            .filter(|t| t.bound)
            .and_then(|t| t.pid.map(|pid| (pid, t.turn_open)));
        let Some((pid, turn_open)) = seat else {
            if inner.watchers.get(key) == Some(&watcher) {
                inner.watchers.remove(key);
            }
            return json!({"done": true});
        };
        match inner.watchers.get(key) {
            Some(w) if *w != watcher && !arm => return json!({"done": true}),
            _ => {
                inner.watchers.insert(key.into(), watcher);
            }
        }
        if turn_open {
            return json!({});
        }
        let Some(text) = hot::wake_text(self, inner, key, pid) else {
            return json!({});
        };
        if let Some(t) = inner.st.threads.get_mut(key) {
            t.last_seen = now_ms();
        }
        // The rewake turn has no prompt: the wakes are its only record of what it answered.
        let _ = inner.mem.append_turn(pid, &format!("Rewake: {text}"));
        self.event(
            inner,
            "wake.rewake",
            json!({"pid": pid, "to": key, "watcher": watcher}),
        );
        self.save(inner);
        json!({"wake": text})
    }

    /// Stop hook: the turn goes into the seat's tail; wakes delivered in this turn are
    /// acknowledged (handled); wakes that arrived during the turn re-wake it once.
    fn capture(
        &self,
        inner: &mut Inner,
        key: &str,
        p: &Value,
    ) -> Result<(Option<usize>, Option<String>)> {
        let Some(t) = inner.st.threads.get_mut(key) else {
            return Ok((None, None));
        };
        let was_open = std::mem::take(&mut t.turn_open);
        if !t.bound && !was_open {
            return Ok((None, None));
        }
        let Some(pid) = t.pid else {
            return Ok((None, None));
        };
        let bound = t.bound;
        let prompt = t.pending_prompt.take();
        let harness = t.harness.clone();
        t.last_seen = now_ms();
        let reply = p["last_assistant_message"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        let label = if bound {
            harness.clone()
        } else {
            format!("{harness} (the previous thread, after the seat moved)")
        };
        if let Some(prompt) = prompt.filter(|s| !s.trim().is_empty()) {
            inner
                .mem
                .append_turn(pid, &format!("Captain: {}", prompt.trim()))?;
        }
        if !reply.is_empty() {
            inner.mem.append_turn(pid, &format!("{label}: {reply}"))?;
        }
        let mut acked = vec![];
        if let Ok(rec) = inner.st.pid_mut(pid) {
            rec.updated = now_ms();
            for w in rec.wakes.iter_mut() {
                if !w.acked && w.delivered.as_deref() == Some(key) {
                    w.acked = true;
                    acked.push(w.id);
                }
            }
        }
        if !acked.is_empty() {
            self.event(
                inner,
                "wake.ack",
                json!({"pid": pid, "wakes": acked, "by": key}),
            );
        }
        let big = inner
            .mem
            .session(pid)
            .map(|s| s.tail.iter().map(String::len).sum::<usize>() > crate::TAIL_BYTES)
            .unwrap_or(false);
        // Bounded turn-end guard (D45): once per turn, a wake that arrived while the
        // seat was working keeps it going instead of ending blind.
        let mut stop = None;
        if bound && p["stop_hook_active"] != true {
            stop = hot::wake_text(self, inner, key, pid);
        }
        Ok((big.then_some(pid), stop))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BriefFold, Drivers, FoldJob, SilentMapp, TurnRequest, TurnResult, Universe};
    use std::process::Command;

    struct NoDrivers;
    impl Drivers for NoDrivers {
        fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
            anyhow::bail!("no folds in these tests")
        }
        fn turn(&self, _: &TurnRequest, _: &dyn Fn(u32)) -> Result<TurnResult> {
            anyhow::bail!("no turns in these tests")
        }
    }

    struct Rig {
        k: Arc<Kernel>,
        root: PathBuf,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn open(root: &std::path::Path) -> Arc<Kernel> {
        Kernel::open(
            Universe::at(root).unwrap(),
            Arc::new(NoDrivers),
            Arc::new(SilentMapp),
            now_ms(),
        )
        .unwrap()
    }

    /// L1 (PID 1) bound to thread `claude:s1` (harness `os_pid`, app "headless", as
    /// written before claude-desktop detection) and PID 2 bound to `claude:s2`.
    fn rig(tag: &str, os_pid: u32, other_os_pid: u32) -> Rig {
        let root = std::env::temp_dir().join(format!(
            "unvrs-bind-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        Universe::at(&root).unwrap().init().unwrap();
        let k = open(&root);
        {
            let mut inner = k.lock();
            let l1 = inner
                .st
                .create(0, 1, "attached", "claude", "", Default::default(), None);
            let l2 = inner
                .st
                .create(l1, 2, "attached", "claude", "", Default::default(), None);
            for (pid, session, os) in [(l1, "s1", os_pid), (l2, "s2", other_os_pid)] {
                let key = format!("claude:{session}");
                inner.st.threads.insert(
                    key.clone(),
                    ThreadRec {
                        key: key.clone(),
                        harness: "claude".into(),
                        session: session.into(),
                        os_pid: Some(os),
                        pid: Some(pid),
                        bound: true,
                        app: "headless".into(),
                        last_seen: now_ms(),
                        ..Default::default()
                    },
                );
                let rec = inner.st.pid_mut(pid).unwrap();
                rec.thread = Some(key);
                rec.state = "live".into();
            }
            k.save(&mut inner);
        }
        Rig { k, root }
    }

    fn app(k: &Kernel, key: &str) -> String {
        k.lock().st.threads[key].app.clone()
    }

    /// A PID that has exited (reaped), for "the harness is gone".
    fn dead_pid() -> u32 {
        let mut c = Command::new("true").spawn_owned().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn os_pid_refresh_rederives_the_app_only_while_the_process_runs() {
        let mut t = ThreadRec {
            app: "headless".into(),
            os_pid: None,
            ..Default::default()
        };
        refresh_os_pid(&mut t, Some(42), |p| {
            (p == 42).then(|| "claude-desktop".into())
        });
        assert_eq!((t.os_pid, t.app.as_str()), (Some(42), "claude-desktop"));
        // The process is gone: the pid is kept, the label is not overwritten.
        refresh_os_pid(&mut t, Some(43), |_| None);
        assert_eq!((t.os_pid, t.app.as_str()), (Some(43), "claude-desktop"));
        // No pid in the hook: nothing changes.
        refresh_os_pid(&mut t, None, |_| Some("t3".into()));
        assert_eq!((t.os_pid, t.app.as_str()), (Some(43), "claude-desktop"));
    }

    #[test]
    fn startup_heals_stale_apps_of_bound_threads_and_journals_it() {
        let live = std::process::id();
        let dead = dead_pid();
        let r = rig("startup-fake", live, dead);
        r.k.refresh_apps_with(|p| (p == live).then(|| "claude-desktop".into()));
        assert_eq!(app(&r.k, "claude:s1"), "claude-desktop");
        assert_eq!(
            app(&r.k, "claude:s2"),
            "headless",
            "dead harness relabelled"
        );
        let saved = super::super::State::load(&r.k.u.state_path()).unwrap();
        assert_eq!(saved.threads["claude:s1"].app, "claude-desktop");
        let ev: Vec<Value> =
            r.k.journal
                .tail(200)
                .into_iter()
                .filter(|e| e["kind"] == "thread.app")
                .collect();
        assert_eq!(ev.len(), 1);
        assert_eq!(
            (&ev[0]["pid"], &ev[0]["from"], &ev[0]["app"]),
            (&json!(1), &json!("headless"), &json!("claude-desktop"))
        );
        // Nothing changed: a second pass journals nothing.
        r.k.refresh_apps_with(|p| (p == live).then(|| "claude-desktop".into()));
        let n =
            r.k.journal
                .tail(200)
                .into_iter()
                .filter(|e| e["kind"] == "thread.app")
                .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn kernel_open_rederives_apps_from_live_processes() {
        let live = std::process::id();
        let dead = dead_pid();
        let r = rig("startup-real", live, dead);
        {
            let mut inner = r.k.lock();
            for t in inner.st.threads.values_mut() {
                t.app = "stale".into();
            }
            r.k.save(&mut inner);
        }
        let k = open(&r.root);
        assert_eq!(app(&k, "claude:s1"), procinfo::app_of(live));
        assert_eq!(app(&k, "claude:s2"), "stale");
    }

    #[test]
    fn prompt_and_session_start_rederive_the_app() {
        let live = std::process::id();
        let r = rig("hooks", dead_pid(), dead_pid());
        let want = procinfo::app_of(live);
        for (event, key, session) in [
            ("UserPromptSubmit", "claude:s1", "s1"),
            ("SessionStart", "claude:s2", "s2"),
        ] {
            r.k.lock().st.threads.get_mut(key).unwrap().app = "stale".into();
            r.k.hook(
                event,
                "claude",
                &json!({"session_id": session, "cwd": "/tmp", "prompt": "hello"}),
                Some(live),
            )
            .unwrap();
            let inner = r.k.lock();
            let t = &inner.st.threads[key];
            assert_eq!(
                (t.os_pid, t.app.as_str()),
                (Some(live), want.as_str()),
                "{event}"
            );
        }
    }
}
