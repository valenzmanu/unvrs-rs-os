//! PID 39 audit reproductions (delegation and handoff), tracked in
//! docs/design/kernel-gaps.md. Each `repro_*` test asserts the designed behaviour; one
//! whose slice has not landed is `#[ignore]`d with the slice that fixes it, and that
//! slice removes the ignore. Run: cargo test -p uke audit_repro -- --include-ignored
use super::*;
use crate::kernel::{hot, state, state::ThreadRec};
use crate::{BriefFold, Driven, Drivers, FoldJob, SilentMapp, TurnRequest, TurnResult, Universe};
use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

enum Step {
    Fail(&'static str),
    Reported(&'static str),
    Final,
    /// A clean turn; the harness reports this model when none was requested.
    Done(&'static str),
}

struct Scripted(Mutex<VecDeque<Step>>);
impl Drivers for Scripted {
    fn catalogs(
        &self,
        _: &std::path::Path,
    ) -> std::collections::BTreeMap<String, crate::drv_econ::Catalog> {
        crate::kernel::econ::test_catalogs(&["claude", "codex"])
    }
    fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
        bail!("no fold worker")
    }
    fn turn(&self, req: &TurnRequest, started: &dyn Fn(u32)) -> Result<TurnResult> {
        started(1);
        match self.0.lock().unwrap().pop_front() {
            Some(Step::Fail(e)) => bail!("{e}"),
            Some(Step::Reported(e)) => Ok(TurnResult {
                text: "partial".into(),
                error: Some(e.into()),
                model: req.model.clone(),
                ..Default::default()
            }),
            Some(Step::Final) => Ok(TurnResult {
                text: "HANDOFF\ndeliverables:\n- none\ndecisions:\n- none\nlearnings:\n- none\nEND-HANDOFF\nFIELD-NOTES\n- none\nEND-NOTES\nUNVRS-RESULT: complete".into(),
                model: req.model.clone(),
                effort: req.effort.clone(),
                ..Default::default()
            }),
            Some(Step::Done(model)) => Ok(TurnResult {
                text: "handled".into(),
                model: req.model.clone().or_else(|| Some(model.into())),
                effort: req.effort.clone(),
                ..Default::default()
            }),
            None => bail!("{} is not installed or not on PATH", req.harness),
        }
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

/// PID 1 = L1, PID 2 = L2 of `proj` (threadless), PID 3 = a busy driven L3 under PID 2.
fn rig(tag: &str, steps: Vec<Step>) -> Rig {
    let root = std::env::temp_dir().join(format!(
        "unvrs-audit-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    fs::create_dir_all(&root).unwrap();
    let u = Universe::at(&root).unwrap();
    u.init().unwrap();
    let drv = Arc::new(Scripted(Mutex::new(steps.into())));
    let k = Kernel::open(u, drv, Arc::new(SilentMapp), now_ms()).unwrap();
    {
        let mut inner = k.lock();
        let l1 = inner
            .st
            .create(0, 1, "attached", "claude", "", Default::default(), None);
        let l2 = inner
            .st
            .create(l1, 2, "attached", "claude", "", Default::default(), None);
        inner.st.pid_mut(l2).unwrap().project = Some("proj".into());
        let l3 = inner
            .st
            .create(l2, 3, "driven", "claude", "work", Default::default(), None);
        let r = inner.st.pid_mut(l3).unwrap();
        r.project = Some("proj".into());
        r.state = "working".into();
        r.driven = Some(Driven {
            cpu: "headless".into(),
            busy: true,
            model: Some("claude-opus-5-5".into()),
            effort: Some("high".into()),
            ..Default::default()
        });
    }
    k.refresh_econ();
    Rig { k, root }
}

fn settle(r: &Rig) {
    let until = Instant::now() + Duration::from_secs(10);
    while !r.k.lock().running.is_empty() {
        assert!(Instant::now() < until, "seat run did not settle");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wake_state(r: &Rig, pid: usize, id: u64) -> (Option<String>, bool) {
    let inner = r.k.lock();
    let w = inner
        .st
        .pid(pid)
        .unwrap()
        .wakes
        .iter()
        .find(|w| w.id == id)
        .unwrap()
        .clone();
    (w.delivered, w.acked)
}

/// C1: `quota-low` is handled before the caller is resolved (ops.rs:97-106), so a
/// process that is no UNVRS seat can stop and hand off any worker.
#[test]
fn repro_c1_quota_low_requires_an_authorized_caller() {
    let r = rig("c1", vec![]);
    let out = r.k.ctl(&json!({"argv": ["quota-low", "--pid", "3"]}), None);
    let flagged = r.k.lock().st.pid(3).unwrap().quota_low;
    assert!(
        out.is_err() && !flagged,
        "an unbound caller handed off PID 3: {out:?}, quota_low={flagged}"
    );
    let journal = fs::read_to_string(r.k.u.journal_path()).unwrap_or_default();
    assert!(
        journal.lines().any(|l| l.contains("\"refused\"")
            && l.contains("quota-low")
            && l.contains("\"target\":3")),
        "no refused event for quota-low: {journal}"
    );
}

/// S1: only the target's owner signals it: its parent seat, L1 or the captain.
#[test]
fn s1_quota_low_owner_rule() {
    let r = rig("s1-owner", vec![]);
    let other = {
        let mut inner = r.k.lock();
        let o = inner
            .st
            .create(1, 2, "attached", "codex", "", Default::default(), None);
        inner.st.pid_mut(o).unwrap().project = Some("elsewhere".into());
        o
    };
    let inner = r.k.lock();
    let may = |who: Caller| r.k.may_signal(&inner, &who, 3);
    assert!(
        may(Caller::Seat(2, "claude:s2".into())).is_ok(),
        "parent seat"
    );
    assert!(may(Caller::Driven(2)).is_ok(), "parent seat, detached run");
    assert!(may(Caller::Seat(1, "codex:s1".into())).is_ok(), "L1");
    assert!(may(Caller::Captain).is_ok(), "captain");
    let err = may(Caller::Seat(other, "codex:so".into()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("belongs to PID 2"), "{err}");
    assert!(may(Caller::Driven(3)).is_err(), "a worker signals itself");
    assert!(may(Caller::Unbound).is_err(), "unbound thread");
}

/// S1: a pinned worker cannot move harness on low quota; the reply says it ends.
#[test]
fn s1_quota_low_pinned_wording() {
    let r = rig("s1-pin", vec![]);
    let mut inner = r.k.lock();
    let text = r.k.quota_low(&mut inner, 3).unwrap();
    assert!(
        text.contains("being handed off to codex"),
        "unpinned: {text}"
    );
    let rec = inner.st.pid_mut(3).unwrap();
    rec.quota_low = false;
    rec.contract = Some(json!({"model": "claude-opus-5-5", "effort": "high"}));
    let text = r.k.quota_low(&mut inner, 3).unwrap();
    assert!(
        text.contains("pinned to model claude-opus-5-5 on claude")
            && text.contains("will not move to codex")
            && text.contains("ends as failed")
            && !text.contains("handed off"),
        "pinned busy: {text}"
    );
    let rec = inner.st.pid_mut(3).unwrap();
    rec.quota_low = false;
    rec.contract = Some(json!({"effort": "high"}));
    rec.driven.as_mut().unwrap().busy = false;
    let text = r.k.quota_low(&mut inner, 3).unwrap();
    assert!(
        text.contains("pinned to effort high") && text.contains("ended as failed"),
        "pinned idle: {text}"
    );
    assert_ne!(
        inner.st.pid(3).unwrap().state,
        "working",
        "pinned idle task did not end"
    );
}

/// C2: the lead cannot reach its running worker: `send` refuses L3 targets
/// (ops.rs:414-417) and nothing writes the PID mailbox the worker reads (driven.rs:290).
#[test]
fn repro_c2_lead_can_message_its_worker() {
    let r = rig("c2", vec![]);
    let mut inner = r.k.lock();
    let who = Caller::Seat(2, "claude:s2".into());
    let out = r.k.op(
        &mut inner,
        &who,
        Some(2),
        "send",
        &["3".into(), "steer: stop at step 2".into()],
    );
    let unread = inner
        .st
        .pid(3)
        .unwrap()
        .mailbox
        .iter()
        .filter(|m| !m.read)
        .count();
    assert!(
        out.is_ok() && unread == 1,
        "lead → worker message not delivered: {out:?}, unread={unread}"
    );
}

fn unread(inner: &Inner, pid: usize) -> usize {
    inner
        .st
        .pid(pid)
        .unwrap()
        .mailbox
        .iter()
        .filter(|m| !m.read)
        .count()
}

/// S4: only the worker's lead or L1 writes to its mailbox; every refusal is journaled
/// and every accepted message is journaled as `mail`.
#[test]
fn s4_send_to_a_worker_owner_rule() {
    let r = rig("s4-owner", vec![]);
    let mut inner = r.k.lock();
    let other = inner
        .st
        .create(1, 2, "attached", "codex", "", Default::default(), None);
    inner.st.pid_mut(other).unwrap().project = Some("elsewhere".into());
    let sibling = inner
        .st
        .create(2, 3, "driven", "claude", "other", Default::default(), None);
    let mut send = |who: Caller, by: usize| {
        r.k.op(
            &mut inner,
            &who,
            Some(by),
            "send",
            &["3".into(), "steer".into(), "now".into()],
        )
    };
    let err = send(Caller::Seat(other, "codex:so".into()), other)
        .unwrap_err()
        .to_string();
    assert!(err.contains("belongs to PID 2"), "other lead: {err}");
    assert!(
        send(Caller::Driven(sibling), sibling).is_err(),
        "a sibling worker"
    );
    assert!(send(Caller::Driven(3), 3).is_err(), "the worker itself");
    assert_eq!(unread(&inner, 3), 0, "a refused message was queued");
    let out =
        r.k.op(
            &mut inner,
            &Caller::Seat(1, "codex:s1".into()),
            Some(1),
            "send",
            &["3".into(), "from L1".into()],
        )
        .unwrap();
    assert!(out.contains("Queued for PID 3"), "{out}");
    r.k.op(
        &mut inner,
        &Caller::Driven(2),
        Some(2),
        "send",
        &["3".into(), "from the lead's detached run".into()],
    )
    .unwrap();
    assert_eq!(unread(&inner, 3), 2);
    let m = &inner.st.pid(3).unwrap().mailbox;
    assert_eq!(
        (m[0].from.as_str(), m[0].text.as_str()),
        ("PID 1", "from L1")
    );
    drop(inner);
    let journal = fs::read_to_string(r.k.u.journal_path()).unwrap_or_default();
    let refused = journal
        .lines()
        .filter(|l| l.contains("\"refused\"") && l.contains("\"send\""))
        .count();
    assert_eq!(refused, 3, "refusals not journaled: {journal}");
    let mails = journal
        .lines()
        .filter(|l| l.contains("\"mail\"") && l.contains("\"to\":3"))
        .count();
    assert_eq!(mails, 2, "mail not journaled: {journal}");
}

/// S4: a message to a handed-off worker reaches the live successor; an ended worker
/// takes none.
#[test]
fn s4_send_follows_the_lineage_and_refuses_an_ended_worker() {
    let r = rig("s4-lineage", vec![]);
    let mut inner = r.k.lock();
    let next = inner
        .st
        .create(2, 3, "driven", "claude", "work", Default::default(), None);
    inner.st.pid_mut(next).unwrap().state = "working".into();
    let old = inner.st.pid_mut(3).unwrap();
    old.state = "handed-off".into();
    old.handed_to = Some(next);
    let lead = Caller::Seat(2, "claude:s2".into());
    let out =
        r.k.op(
            &mut inner,
            &lead,
            Some(2),
            "send",
            &["3".into(), "steer".into()],
        )
        .unwrap();
    assert!(
        out.contains(&format!("Queued for PID {next}")) && out.contains("handed off"),
        "{out}"
    );
    assert_eq!((unread(&inner, 3), unread(&inner, next)), (0, 1));
    inner.st.pid_mut(next).unwrap().state = "ended".into();
    let err =
        r.k.op(
            &mut inner,
            &lead,
            Some(2),
            "send",
            &["3".into(), "too late".into()],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("is ended"), "{err}");
    assert_eq!(unread(&inner, next), 1);
}

/// C3: a detached seat run whose driver fails leaves the handback wake marked
/// delivered="driven" and unacked forever (6bb7bcf crew.rs:543-549, 588); nothing redelivers it.
#[test]
fn repro_c3_failed_seat_run_keeps_its_wakes() {
    let r = rig("c3", vec![Step::Fail("claude exited with signal 9: boom")]);
    let id = {
        let mut inner = r.k.lock();
        r.k.wake_seat(
            &mut inner,
            2,
            "done",
            "L3 PID 3 finished (done): result",
            Some(3),
            None,
        )
        .unwrap()
    };
    r.k.run_detached_seats();
    settle(&r);
    let (delivered, acked) = wake_state(&r, 2, id);
    assert!(
        !acked && delivered.is_none(),
        "the result wake is stranded: delivered={delivered:?} acked={acked}"
    );
}

/// C4: a detached seat run whose driver reports an error still acknowledges the wakes
/// (6bb7bcf crew.rs:625-633), so the handback is consumed without being handled.
#[test]
fn repro_c4_seat_run_error_does_not_ack() {
    let r = rig(
        "c4",
        vec![Step::Reported("rate limited: usage cap reached")],
    );
    let id = {
        let mut inner = r.k.lock();
        r.k.wake_seat(
            &mut inner,
            2,
            "done",
            "L3 PID 3 finished (done): result",
            Some(3),
            None,
        )
        .unwrap()
    };
    r.k.run_detached_seats();
    settle(&r);
    let (delivered, acked) = wake_state(&r, 2, id);
    assert!(
        !acked,
        "an errored run acknowledged the result wake: delivered={delivered:?} acked={acked}"
    );
}

/// S2 (I9): a failed seat run backs the seat off; sweeps inside the backoff start no
/// model turn, and the requeued wake runs again once the backoff has passed.
#[test]
fn s2_failed_seat_run_backs_off() {
    let r = rig(
        "s2-backoff",
        vec![
            Step::Fail("claude exited with signal 9: boom"),
            Step::Reported("again"),
        ],
    );
    let id = {
        let mut inner = r.k.lock();
        r.k.wake_seat(
            &mut inner,
            2,
            "done",
            "L3 PID 3 finished (done): result",
            Some(3),
            None,
        )
        .unwrap()
    };
    let starts = || {
        fs::read_to_string(r.k.u.journal_path())
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"seat.run\"") && l.contains("\"started\""))
            .count()
    };
    r.k.run_detached_seats();
    settle(&r);
    let b =
        r.k.lock()
            .seat_backoff
            .get(&2)
            .copied()
            .expect("no backoff after a failed run");
    assert!(
        b.fails == 1 && b.until > now_ms(),
        "backoff not armed: {b:?}"
    );
    for _ in 0..3 {
        r.k.run_detached_seats();
        settle(&r);
    }
    assert_eq!(starts(), 1, "a seat run started inside the backoff");
    assert_eq!(
        wake_state(&r, 2, id),
        (None, false),
        "the wake left the queue"
    );
    r.k.lock().seat_backoff.get_mut(&2).unwrap().until = 0;
    r.k.run_detached_seats();
    settle(&r);
    assert_eq!(starts(), 2, "the seat did not run again after its backoff");
    let b = r.k.lock().seat_backoff.get(&2).copied().unwrap();
    assert_eq!(b.fails, 2, "a second failure does not count: {b:?}");
    assert!(
        b.until - now_ms() > crate::kernel::crew::seat_retry_ms(1),
        "the backoff did not grow: {b:?}"
    );
    assert_eq!(wake_state(&r, 2, id), (None, false));
}

fn recent(r: &Rig, kind: &str, pid: usize) -> Vec<Value> {
    r.k.lock()
        .recent
        .iter()
        .filter(|e| e["kind"] == kind && e["pid"] == pid)
        .cloned()
        .collect()
}

/// DrvEcon replaces the old guessed-harness refusal with catalog-based seat admission.
#[test]
fn seat_without_a_prior_harness_routes_as_judge_from_the_live_catalog() {
    let r = rig("seat-policy", vec![Step::Done("ignored default")]);
    let id = {
        let mut inner = r.k.lock();
        inner.st.pid_mut(2).unwrap().harness.clear();
        r.k.wake_seat(&mut inner, 2, "done", "read result", Some(3), None)
            .unwrap()
    };
    r.k.run_detached_seats();
    settle(&r);
    assert!(wake_state(&r, 2, id).1);
    let inner = r.k.lock();
    let rec = inner.st.pid(2).unwrap();
    assert_eq!(rec.harness, "claude");
    assert_eq!(rec.contract.as_ref().unwrap()["econ"]["role"], "judge");
    assert!(!inner.seat_backoff.contains_key(&2));
}

/// S6 (C7): an L1 outside away mode never runs detached, so a harness-less L1 with a
/// wake is neither refused nor backed off; it just waits for its thread.
#[test]
fn s6_l1_outside_away_is_not_refused() {
    let r = rig("s6-l1", vec![]);
    let id = {
        let mut inner = r.k.lock();
        inner.st.pid_mut(1).unwrap().harness = String::new();
        r.k.wake_seat(
            &mut inner,
            1,
            "outcome",
            "proj lead handled 1 wake(s)",
            Some(2),
            None,
        )
        .unwrap()
    };
    r.k.run_detached_seats();
    settle(&r);
    assert!(
        recent(&r, "seat.run", 1).is_empty(),
        "L1 was refused or run"
    );
    assert!(!r.k.lock().seat_backoff.contains_key(&1), "L1 backed off");
    assert_eq!(wake_state(&r, 1, id), (None, false));
}

/// S6 (C7): a bound seat's detached run records the requested and the accepted model
/// and effort on the seat and in its outcome event.
#[test]
fn s6_seat_run_outcome_carries_the_model() {
    let r = rig("s6-model", vec![Step::Done("claude-sonnet-5-5")]);
    let id = {
        let mut inner = r.k.lock();
        r.k.wake_seat(
            &mut inner,
            2,
            "done",
            "L3 PID 3 finished (done): result",
            Some(3),
            None,
        )
        .unwrap()
    };
    r.k.run_detached_seats();
    settle(&r);
    assert!(wake_state(&r, 2, id).1, "the handled wake was not acked");
    let out = recent(&r, "outcome", 2);
    assert_eq!(out.len(), 1, "{out:?}");
    let o = &out[0];
    for key in [
        "model",
        "actual_model",
        "effort",
        "actual_effort",
        "effort_evidence",
    ] {
        assert!(o.get(key).is_some(), "outcome lacks {key}: {o}");
    }
    assert_eq!(o["actual_model"], "claude-opus-5-5", "{o}");
    assert_eq!(o["harness"], "claude", "{o}");
    let started = recent(&r, "seat.run", 2);
    assert!(
        started
            .iter()
            .any(|e| e["result"] == "started" && e.get("model").is_some()),
        "seat.run started lacks the requested model: {started:?}"
    );
    let inner = r.k.lock();
    let d = inner
        .st
        .pid(2)
        .unwrap()
        .driven
        .clone()
        .expect("no seat-run record");
    assert_eq!(d.actual_model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(d.model, o["model"].as_str().map(str::to_owned));
}

/// C5: a wake delivered to a thread that detaches before its Stop stays delivered to
/// the dead key (hot.rs:445-456, bind.rs:46-80, 471); no thread or seat run gets it again.
#[test]
fn repro_c5_detach_requeues_in_flight_wakes() {
    let r = rig("c5", vec![]);
    let key = "claude:s-old".to_string();
    let mut inner = r.k.lock();
    inner.st.threads.insert(
        key.clone(),
        ThreadRec {
            key: key.clone(),
            harness: "claude".into(),
            session: "s-old".into(),
            pid: Some(2),
            bound: true,
            ..Default::default()
        },
    );
    inner.st.pid_mut(2).unwrap().thread = Some(key.clone());
    let id =
        r.k.wake_seat(
            &mut inner,
            2,
            "done",
            "L3 PID 3 finished (done): result",
            Some(3),
            None,
        )
        .unwrap();
    assert!(hot::wake_text(&r.k, &mut inner, &key, 2).is_some());
    r.k.detach(&mut inner, &key, "seat moved", None);
    drop(inner);
    let (delivered, acked) = wake_state(&r, 2, id);
    assert!(
        !acked && delivered.is_none(),
        "in-flight wake lost on detach: delivered={delivered:?} acked={acked}"
    );
}

/// S3 (C5): a thread that claims another seat leaves its old seat without a detach
/// (bind.rs claim); the old seat's wakes in flight to it go back to that seat's queue.
#[test]
fn s3_claim_leaving_a_seat_requeues_its_wakes() {
    let r = rig("s3claim", vec![]);
    let key = "claude:s-mover".to_string();
    let mut inner = r.k.lock();
    inner.st.threads.insert(
        key.clone(),
        ThreadRec {
            key: key.clone(),
            harness: "claude".into(),
            session: "s-mover".into(),
            pid: Some(2),
            bound: true,
            ..Default::default()
        },
    );
    inner.st.pid_mut(2).unwrap().thread = Some(key.clone());
    let id =
        r.k.wake_seat(&mut inner, 2, "done", "L3 PID 3 finished", Some(3), None)
            .unwrap();
    assert!(hot::wake_text(&r.k, &mut inner, &key, 2).is_some());
    let info = crate::kernel::bind::ThreadInfo {
        key: key.clone(),
        harness: "claude".into(),
        session: "s-mover".into(),
        cwd: r.root.clone(),
        os_pid: None,
        transcript: None,
    };
    r.k.claim(&mut inner, &info, 1, "bind").unwrap();
    assert_eq!(inner.st.pid(2).unwrap().thread, None);
    drop(inner);
    assert_eq!(
        wake_state(&r, 2, id),
        (None, false),
        "the old seat's in-flight wake was not requeued"
    );
}

/// S3: a kernel restart during a detached seat run leaves the run's wakes "driven";
/// the new kernel runs nothing, so they go back to the queue and the seat is not busy.
/// A wake delivered to the seat's own thread stays with it (its Stop acks it).
#[test]
fn s3_restart_requeues_driven_wakes() {
    let r = rig("s3restart", vec![]);
    let live = "claude:s-live".to_string();
    let (driven, own, stray) = {
        let mut inner = r.k.lock();
        inner.st.threads.insert(
            live.clone(),
            ThreadRec {
                key: live.clone(),
                harness: "claude".into(),
                session: "s-live".into(),
                pid: Some(1),
                bound: true,
                ..Default::default()
            },
        );
        inner.st.pid_mut(1).unwrap().thread = Some(live.clone());
        let driven =
            r.k.wake_seat(&mut inner, 2, "done", "L3 PID 3 finished", Some(3), None)
                .unwrap();
        let own =
            r.k.wake_seat(
                &mut inner,
                1,
                "outcome",
                "proj lead reported",
                Some(2),
                None,
            )
            .unwrap();
        let stray =
            r.k.wake_seat(
                &mut inner,
                1,
                "note",
                "delivered to a gone thread",
                None,
                None,
            )
            .unwrap();
        let rec = inner.st.pid_mut(2).unwrap();
        rec.wakes
            .iter_mut()
            .find(|w| w.id == driven)
            .unwrap()
            .delivered = Some("driven".into());
        rec.state = "running".into();
        rec.driven = Some(Driven {
            cpu: "seat-run".into(),
            busy: true,
            os_pid: Some(4_000_000),
            ..Default::default()
        });
        let rec = inner.st.pid_mut(1).unwrap();
        rec.wakes
            .iter_mut()
            .find(|w| w.id == own)
            .unwrap()
            .delivered = Some(live.clone());
        rec.wakes
            .iter_mut()
            .find(|w| w.id == stray)
            .unwrap()
            .delivered = Some("claude:s-gone".into());
        r.k.save(&mut inner);
        (driven, own, stray)
    };
    let u = Universe::at(&r.root).unwrap();
    let drv = Arc::new(Scripted(Mutex::new(VecDeque::new())));
    let k2 = Kernel::open(u, drv, Arc::new(SilentMapp), now_ms()).unwrap();
    let r2 = Rig {
        k: k2,
        root: r.root.clone(),
    };
    assert_eq!(
        wake_state(&r2, 2, driven),
        (None, false),
        "a driven wake survived the restart"
    );
    assert_eq!(wake_state(&r2, 1, stray), (None, false));
    assert_eq!(wake_state(&r2, 1, own), (Some(live.clone()), false));
    let inner = r2.k.lock();
    let seat = inner.st.pid(2).unwrap();
    assert_eq!(seat.state, "idle");
    let d = seat.driven.as_ref().unwrap();
    assert!(!d.busy && d.os_pid.is_none(), "{d:?}");
    assert!(
        inner.recent.iter().any(|e| e["kind"] == "wake.requeue"
            && e["pid"] == 2
            && e["reason"] == "kernel restart"),
        "no wake.requeue event"
    );
}

/// C6: a bound L1 dispatches a report task through the public ctl path and
/// receives the result directly; its project must be explicit and valid.
#[test]
fn c6_l1_dispatches_l3_report_through_ctl() {
    let r = rig("c6", vec![Step::Final]);
    r.k.lock().st.econ_catalogs = crate::kernel::econ::test_catalogs(&["codex", "claude"]);
    let lead = {
        let mut inner = r.k.lock();
        let lead =
            r.k.create_project(&mut inner, "proj", "test", &[], "captain")
                .unwrap();
        let thread = ThreadRec {
            key: "claude:c6".into(),
            harness: "claude".into(),
            session: "c6".into(),
            os_pid: Some(std::process::id()),
            pid: Some(1),
            bound: true,
            ..Default::default()
        };
        inner.st.threads.insert(thread.key.clone(), thread);
        inner
            .st
            .captain_prompts
            .push(crate::kernel::state::CaptainPrompt {
                id: 1,
                pid: 1,
                thread: "claude:c6".into(),
                project: None,
                at: now_ms(),
                text: "Ok, go".into(),
            });
        lead
    };
    let call = |project: Option<&str>| {
        let mut argv = vec!["task", "--intent", "check", "--spec", "report"];
        argv.extend(["--go", "Ok, go", "--done-when", "report delivered"]);
        if let Some(project) = project {
            argv.extend(["--project", project]);
        }
        r.k.ctl(
            &json!({"argv": argv, "hint": {"harness": "claude", "session": "c6"}}),
            Some(std::process::id()),
        )
    };
    assert!(
        call(None)
            .unwrap_err()
            .to_string()
            .contains("needs --project")
    );
    assert!(
        call(Some("missing"))
            .unwrap_err()
            .to_string()
            .contains("Unknown project")
    );
    assert!(
        call(Some("../proj")).is_err(),
        "invalid project id accepted"
    );
    let text = call(Some("proj")).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let pid: usize = text
        .split("L3 PID ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    settle(&r);
    let inner = r.k.lock();
    let worker = inner.st.pid(pid).unwrap();
    assert_eq!(
        (
            worker.parent,
            worker.project.as_deref(),
            worker.result.as_deref()
        ),
        (1, Some("proj"), Some("complete"))
    );
    assert!(
        inner
            .st
            .pid(1)
            .unwrap()
            .wakes
            .iter()
            .any(|w| w.from == Some(pid) && w.kind == "done")
    );
    assert!(
        inner
            .st
            .pid(lead)
            .unwrap()
            .wakes
            .iter()
            .all(|w| w.from != Some(pid))
    );
    let package: Value = serde_json::from_slice(
        &fs::read(
            r.k.u
                .project_dir("proj")
                .join(format!("tasks/pid-{pid}/result.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (package["parent"].as_u64(), package["project"].as_str()),
        (Some(1), Some("proj"))
    );
}

/// C9: CREW cannot show harness · model · effort for seats (snapshot.rs:108-109) or nest
/// workers under their owner without reading state.json (snapshot.rs:119-137).
#[test]
fn repro_c9_snapshot_carries_seat_model_effort_and_worker_parent() {
    let r = rig("c9", vec![]);
    let snap = r.k.snapshot();
    let seat = &snap["seats"][0];
    let worker = &snap["workers"][0];
    let missing: Vec<&str> = [
        ("seats[].model", seat.get("model").is_none()),
        ("seats[].effort", seat.get("effort").is_none()),
        ("workers[].parent", worker.get("parent").is_none()),
    ]
    .into_iter()
    .filter(|(_, m)| *m)
    .map(|(n, _)| n)
    .collect();
    assert!(missing.is_empty(), "snapshot lacks {missing:?}");
}

/// Binds a thread with `transcript` to seat `pid` (not live: no os_pid, never seen).
fn bind_transcript(r: &Rig, pid: usize, harness: &str, transcript: Option<&std::path::Path>) {
    let key = format!("{harness}:s-{pid}");
    let mut inner = r.k.lock();
    inner.st.threads.insert(
        key.clone(),
        ThreadRec {
            key: key.clone(),
            harness: harness.into(),
            session: format!("s-{pid}"),
            pid: Some(pid),
            bound: true,
            transcript: transcript.map(|p| p.display().to_string()),
            ..Default::default()
        },
    );
    inner.st.pid_mut(pid).unwrap().thread = Some(key);
}

fn seat(snap: &Value, pid: u64) -> Value {
    snap["seats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["pid"] == pid)
        .unwrap()
        .clone()
}

/// S5 (C9, I8): a bound thread's model and effort come from its transcript's last turn:
/// Claude main-chain assistant records (sidechains and synthetic records skipped) and
/// Codex turn_context records. Workers carry their parent.
#[test]
fn s5_seat_model_effort_from_transcripts() {
    let r = rig("s5-transcript", vec![]);
    let claude = r.root.join("claude.jsonl");
    fs::write(
        &claude,
        [
            r#"{"type":"assistant","message":{"model":"claude-sonnet-5-5"},"effort":"low"}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5"},"effort":"high"}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-haiku-4-5"},"effort":"low"}"#,
            r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let codex = r.root.join("rollout.jsonl");
    fs::write(
        &codex,
        [
            r#"{"type":"turn_context","payload":{"model":"gpt-6-sol","effort":"medium"}}"#,
            r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol","effort":"high"}}"#,
            r#"{"type":"response_item","payload":{"type":"message"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    bind_transcript(&r, 1, "claude", Some(&claude));
    bind_transcript(&r, 2, "codex", Some(&codex));
    let snap = r.k.snapshot();
    let l1 = seat(&snap, 1);
    assert_eq!(
        (&l1["model"], &l1["effort"], &l1["source"]),
        (
            &json!("claude-opus-5-5"),
            &json!("high"),
            &json!("transcript")
        )
    );
    let l2 = seat(&snap, 2);
    assert_eq!(
        (&l2["model"], &l2["effort"], &l2["source"]),
        (&json!("gpt-6.1-sol"), &json!("high"), &json!("transcript"))
    );
    assert_eq!(snap["workers"][0]["parent"], 2);
}

/// PID 132: seats and workers carry their brief's `now` and first `next` (what the crew
/// diagram shows as "working on"); null until a brief exists.
#[test]
fn seats_and_workers_carry_their_brief_line() {
    let r = rig("brief-line", vec![]);
    let snap = r.k.snapshot();
    assert_eq!(
        (&seat(&snap, 2)["now"], &seat(&snap, 2)["next"]),
        (&json!(null), &json!(null))
    );
    {
        let mut inner = r.k.lock();
        for (pid, now) in [
            (2, "  routing Acme work "),
            (3, "writing the layout tests"),
        ] {
            let brief = crate::Brief {
                now: now.into(),
                next: vec!["build wasm".into(), "deploy".into()],
                ..Default::default()
            };
            inner.mem.write_brief(pid, "test", brief).unwrap();
        }
    }
    let snap = r.k.snapshot();
    let l2 = seat(&snap, 2);
    assert_eq!(
        (&l2["now"], &l2["next"]),
        (&json!("routing Acme work"), &json!("build wasm"))
    );
    let w = &snap["workers"][0];
    assert_eq!(
        (w["pid"].as_u64(), &w["now"]),
        (Some(3), &json!("writing the layout tests"))
    );
    assert_eq!(seat(&snap, 1)["now"], json!(null), "no brief, no line");
}

/// S5 (C9): a seat's model is what a harness accepted, never what was asked for; unknown
/// stays null (a vacant seat, a transcript with no turn yet, a run not yet accepted).
#[test]
fn s5_seat_facts_are_null_until_known() {
    let r = rig("s5-null", vec![]);
    let empty = r.root.join("empty.jsonl");
    fs::write(&empty, "{\"type\":\"user\"}\n").unwrap();
    bind_transcript(&r, 1, "claude", Some(&empty));
    {
        let mut inner = r.k.lock();
        inner.st.pid_mut(2).unwrap().driven = Some(Driven {
            cpu: "seat-run".into(),
            busy: true,
            model: Some("asked-model".into()),
            effort: Some("high".into()),
            ..Default::default()
        });
    }
    let null3 = (json!(null), json!(null), json!(null));
    let facts = |s: Value| (s["model"].clone(), s["effort"].clone(), s["source"].clone());
    let snap = r.k.snapshot();
    assert_eq!(facts(seat(&snap, 1)), null3, "a transcript without a turn");
    assert_eq!(facts(seat(&snap, 2)), null3, "a run not yet accepted");
    {
        let mut inner = r.k.lock();
        let d = inner.st.pid_mut(2).unwrap().driven.as_mut().unwrap();
        d.actual_model = Some("gpt-6-sol".into());
    }
    let s2 = seat(&r.k.snapshot(), 2);
    assert_eq!(
        facts(s2),
        (json!("gpt-6-sol"), json!(null), json!("seat-run"))
    );
    {
        let mut inner = r.k.lock();
        inner.st.pid_mut(2).unwrap().driven.as_mut().unwrap().busy = false;
    }
    let s2 = seat(&r.k.snapshot(), 2);
    assert_eq!(s2["source"], "last seat-run");
    {
        let mut inner = r.k.lock();
        let l3 = inner
            .st
            .create(1, 2, "attached", "", "", Default::default(), None);
        inner.st.pid_mut(l3).unwrap().project = Some("vacant".into());
    }
    let snap = r.k.snapshot();
    let vacant = snap["seats"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(facts(vacant), null3, "a vacant seat");
}

/// S5 (C8): a finished seat run on a seat with a thread leaves no Driven behind, so the
/// tree does not read "model harness default (not yet accepted)" for a thread's seat; a
/// threadless seat keeps the run, idle and labelled as its last run.
#[test]
fn s5_finished_seat_run_is_not_a_stale_pin() {
    let r = rig("s5-c8", vec![Step::Reported("one"), Step::Reported("two")]);
    bind_transcript(&r, 1, "claude", None);
    {
        let mut inner = r.k.lock();
        inner.st.away = Some(Default::default());
        inner.st.away.as_mut().unwrap().cap = 5;
        for pid in [1, 2] {
            r.k.wake_seat(&mut inner, pid, "done", "result", Some(3), None)
                .unwrap();
        }
    }
    r.k.run_detached_seats();
    settle(&r);
    let inner = r.k.lock();
    let l1 = inner.st.pid(1).unwrap();
    assert!(
        l1.driven.is_none(),
        "stale seat-run Driven: {:?}",
        l1.driven
    );
    assert!(!state::pid_line(l1).contains("not yet accepted"));
    let l2 = inner.st.pid(2).unwrap();
    let d = l2
        .driven
        .as_ref()
        .expect("a threadless seat keeps its last run");
    assert!(!d.busy && d.os_pid.is_none());
    assert!(
        state::pid_line(l2).contains(" · last seat run: model claude-opus-5-5"),
        "{}",
        state::pid_line(l2)
    );
}

/// S5 (C8): no seat run survives a kernel restart; its Driven is settled at open.
#[test]
fn s5_restart_settles_seat_runs() {
    let r = rig("s5-restart", vec![]);
    bind_transcript(&r, 1, "claude", None);
    {
        let mut inner = r.k.lock();
        for pid in [1, 2] {
            inner.st.pid_mut(pid).unwrap().driven = Some(Driven {
                cpu: "seat-run".into(),
                busy: true,
                os_pid: None,
                ..Default::default()
            });
        }
        r.k.save(&mut inner);
    }
    let k = Kernel::open(
        Universe::at(&r.root).unwrap(),
        Arc::new(Scripted(Mutex::new(VecDeque::new()))),
        Arc::new(SilentMapp),
        now_ms(),
    )
    .unwrap();
    let inner = k.lock();
    assert!(inner.st.pid(1).unwrap().driven.is_none());
    let d = inner.st.pid(2).unwrap().driven.clone().unwrap();
    assert!(!d.busy && d.os_pid.is_none());
}

/// S8 (C11): an env default model is enforced as an exact pin by the driver, so the task
/// reply and the mismatch stop say so instead of "not pinned".
#[test]
fn s8_env_default_model_is_reported_as_a_pin() {
    use super::super::driven::model_terms;
    let env = model_terms("claude", None, Some("opus"));
    assert!(
        env.contains("UNVRS_L3_CLAUDE_MODEL") && env.contains("exact pin"),
        "{env}"
    );
    assert!(!env.contains("not pinned"), "{env}");
    assert!(model_terms("codex", None, None).contains("not pinned"));
    let pin = model_terms("claude", Some("claude-opus-5-5"), Some("claude-opus-5-5"));
    assert!(pin.starts_with("model: claude-opus-5-5, pinned"), "{pin}");
}

/// S8 (C12): `send` reaches L1, the caller's lead and the caller's project seat; another
/// project's seat is refused (journaled) unless the caller is L1.
#[test]
fn s8_send_is_scoped_to_lead_l1_and_project() {
    let r = rig("s8-send", vec![]);
    let mut inner = r.k.lock();
    let other = inner
        .st
        .create(1, 2, "attached", "claude", "", Default::default(), None);
    inner.st.pid_mut(other).unwrap().project = Some("other".into());
    let send = |inner: &mut Inner, by: usize, to: &str| {
        r.k.op(
            inner,
            &Caller::Driven(by),
            Some(by),
            "send",
            &[to.into(), "hello".into()],
        )
    };
    assert!(send(&mut inner, 3, "2").is_ok(), "worker to its lead");
    assert!(send(&mut inner, 3, "l1").is_ok(), "worker to L1");
    assert!(send(&mut inner, 3, "proj").is_ok(), "worker to its project");
    let err = send(&mut inner, 3, &other.to_string()).unwrap_err();
    assert!(err.to_string().contains("Refused"), "{err}");
    assert!(
        send(&mut inner, 2, &other.to_string()).is_err(),
        "L2 to another project"
    );
    assert!(
        send(&mut inner, 1, &other.to_string()).is_ok(),
        "L1 writes anywhere"
    );
    drop(inner);
    assert!(
        recent(&r, "refused", 3)
            .iter()
            .any(|e| e["verb"] == "send" && e["target"] == other),
        "no refused event for send"
    );
}

/// S8 (C13): a hint without a harness no longer means codex; it resolves the session on
/// whichever harness binds it in the caller's process tree.
#[test]
fn s8_hint_without_harness_is_resolved_from_the_process_tree() {
    let r = rig("s8-hint", vec![]);
    let me = std::process::id();
    let mut inner = r.k.lock();
    for (h, s, pid) in [("claude", "s-claude", 2), ("codex", "s-codex", 1)] {
        let key = format!("{h}:{s}");
        inner.st.threads.insert(
            key.clone(),
            ThreadRec {
                key,
                harness: h.into(),
                session: s.into(),
                os_pid: Some(me),
                pid: Some(pid),
                bound: true,
                ..Default::default()
            },
        );
    }
    let who =
        r.k.caller(&inner, Some(me), Some((None, "s-claude")))
            .unwrap();
    assert_eq!(who, Caller::Seat(2, "claude:s-claude".into()));
    let who =
        r.k.caller(&inner, Some(me), Some((Some("codex"), "s-codex")))
            .unwrap();
    assert_eq!(who, Caller::Seat(1, "codex:s-codex".into()));
    assert!(
        r.k.caller(&inner, Some(me), Some((Some("codex"), "s-claude")))
            .is_err(),
        "a named harness must match"
    );
    drop(inner);
}
