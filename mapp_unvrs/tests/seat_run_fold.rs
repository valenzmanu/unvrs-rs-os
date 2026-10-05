//! G-hold: a threadless lead runs only detached (C9), so no thread hook ever folds
//! its brief. Live, brief v2 said "all dispatches are held; wait for L1 follow-up";
//! the captain's go (w263) was handled in a detached run, never folded, and every
//! later run read the stale hold and idled (w283, w285) until L1 overrode it (w287).
//! A detached run that handles an instruction must record it and fold the brief.
use std::{
    fs,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use uke::{
    ActiveScope, Brief, BriefFold, FoldJob, MemoryIndex, State, TurnRequest, TurnResult, Universe,
};

struct Drivers(Mutex<mpsc::Sender<FoldJob>>);
impl uke::Drivers for Drivers {
    fn catalogs(
        &self,
        _: &std::path::Path,
    ) -> std::collections::BTreeMap<String, uke::drv_econ::Catalog> {
        let at = uke::now_ms();
        [(
            "claude".into(),
            uke::drv_econ::Catalog {
                observed_at: at,
                signed_in: Some(true),
                quota_available: Some(true),
                quota_observed_at: at,
                models: vec![uke::drv_econ::Model {
                    id: "claude-opus-fixture".into(),
                    efforts: vec!["low".into(), "high".into()],
                    ..Default::default()
                }],
                ..Default::default()
            },
        )]
        .into()
    }
    fn fold(&self, job: &FoldJob) -> anyhow::Result<BriefFold> {
        let _ = self.0.lock().unwrap().send(job.clone());
        let mut brief = job.brief.clone();
        brief.now = "Hold lifted by the captain's go; dispatching the slices.".into();
        brief.next = vec!["Dispatch S3".into()];
        Ok(BriefFold {
            brief,
            notes: vec![],
        })
    }
    fn turn(&self, req: &TurnRequest, _: &dyn Fn(u32)) -> anyhow::Result<TurnResult> {
        Ok(TurnResult {
            model: req.model.clone(),
            effort: req.effort.clone(),
            text: "Lifted the hold and dispatched S1.".into(),
            ..Default::default()
        })
    }
}

#[test]
fn a_go_handled_in_a_detached_run_is_recorded_and_folds_the_stale_hold() {
    unsafe { std::env::set_var("UNVRS_NOTIFY", "0") };
    let root = std::env::temp_dir().join(format!("unvrs-seat-fold-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let u = Universe::at(&root).unwrap();
    u.init().unwrap();

    let mut st = State::load(&u.state_path()).unwrap();
    let l1 = st.create(
        0,
        1,
        "attached",
        "codex",
        "L1",
        ActiveScope::default(),
        None,
    );
    let l2 = st.create(
        l1,
        2,
        "attached",
        "codex",
        "lead",
        ActiveScope::default(),
        None,
    );
    st.pid_mut(l2).unwrap().project = Some("bloom".into());
    st.wake(
        l2,
        "note",
        &format!("from PID {l1}: Captain go: lift the hold and implement S1-S8."),
        Some(l1),
        None,
    )
    .unwrap();
    st.save(&u.state_path()).unwrap();
    let held = Brief {
        goal: "Build the delegation slices".into(),
        now: "All other dispatches are held.".into(),
        next: vec!["Wait for L1 follow-up before any further dispatches".into()],
        ..Default::default()
    };
    let v0 = {
        let mut m = MemoryIndex::open(u.root()).unwrap();
        m.write_brief(l2, "seed", held).unwrap();
        m.session(l2).unwrap().brief_version
    };

    let (tx, rx) = mpsc::channel();
    let k = u.clone();
    thread::spawn(move || {
        uke::serve_kernel(
            k,
            Arc::new(Drivers(Mutex::new(tx))),
            Arc::new(mapp_unvrs::UnvrsMapp),
            None,
        )
    });
    let job = rx.recv_timeout(Duration::from_secs(15));
    // The fold applies after the driver returns; wait for the new version on disk.
    let until = Instant::now() + Duration::from_secs(5);
    let mut session = MemoryIndex::open(u.root()).unwrap().session(l2).unwrap();
    while session.brief_version == v0 && Instant::now() < until {
        thread::sleep(Duration::from_millis(50));
        session = MemoryIndex::open(u.root()).unwrap().session(l2).unwrap();
    }
    let until = Instant::now() + Duration::from_secs(5);
    while uke::kernel_running(&u) && Instant::now() < until {
        let _ = uke::kernel_request(
            &u,
            &serde_json::json!({"op": "stop"}),
            Duration::from_secs(2),
        );
        thread::sleep(Duration::from_millis(50));
    }
    let _ = fs::remove_dir_all(&root);

    let job = job.expect("a detached run that handled a note folds the seat's brief");
    assert_eq!(job.reason, "seat run");
    assert!(
        job.turns.iter().any(|t| t.contains("lift the hold")),
        "the fold must see the instruction itself, not only the run's reply: {:?}",
        job.turns
    );
    assert!(
        session
            .tail
            .iter()
            .any(|t| t.contains("[w") && t.contains("lift the hold")),
        "the handled wake is a turn in the seat's tail: {:?}",
        session.tail
    );
    assert!(session.brief_version > v0, "the fold applied");
    assert!(!session.brief.now.contains("held"), "{:?}", session.brief);
}
