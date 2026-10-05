//! G-L1push: L1 in Claude Desktop saw its wakes only when the captain typed. Claude Code
//! wakes an idle thread when an `asyncRewake` hook exits 2; `unvrs hook Watch` is that
//! hook and polls the kernel's `watch` op. An idle bound thread takes its wakes there,
//! delivered to the thread so the rewake turn's Stop acknowledges them.
use std::{
    fs,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use uke::{
    ActiveScope, BriefFold, FoldJob, MemoryIndex, State, ThreadRec, TurnRequest, TurnResult,
    Universe,
};

struct Drivers;
impl uke::Drivers for Drivers {
    fn fold(&self, job: &FoldJob) -> anyhow::Result<BriefFold> {
        Ok(BriefFold {
            brief: job.brief.clone(),
            notes: vec![],
        })
    }
    fn turn(&self, _: &TurnRequest, _: &dyn Fn(u32)) -> anyhow::Result<TurnResult> {
        Ok(TurnResult::default())
    }
}

#[test]
fn an_idle_bound_thread_is_rewoken_with_its_wakes_and_acks_them_at_stop() {
    unsafe { std::env::set_var("UNVRS_NOTIFY", "0") };
    let root = std::env::temp_dir().join(format!("unvrs-watch-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let u = Universe::at(&root).unwrap();
    u.init().unwrap();

    let key = "claude:s-l1".to_owned();
    let mut st = State::load(&u.state_path()).unwrap();
    let l1 = st.create(
        0,
        1,
        "attached",
        "claude",
        "L1",
        ActiveScope::default(),
        None,
    );
    st.pid_mut(l1).unwrap().thread = Some(key.clone());
    st.threads.insert(
        key.clone(),
        ThreadRec {
            key: key.clone(),
            harness: "claude".into(),
            session: "s-l1".into(),
            pid: Some(l1),
            bound: true,
            last_seen: uke::now_ms(),
            app: "claude-desktop".into(),
            ..Default::default()
        },
    );
    let w = st
        .wake(
            l1,
            "outcome",
            "bloom lead finished S3 and reports",
            None,
            None,
        )
        .unwrap();
    st.save(&u.state_path()).unwrap();

    let k = u.clone();
    thread::spawn(move || {
        uke::serve_kernel(k, Arc::new(Drivers), Arc::new(mapp_unvrs::UnvrsMapp), None)
    });
    let req = |v: serde_json::Value| uke::kernel_request(&u, &v, Duration::from_secs(5));
    let watch = |watcher: u32, arm: bool| {
        req(
            serde_json::json!({"op": "watch", "harness": "claude", "session": "s-l1",
            "watcher": watcher, "arm": arm}),
        )
    };
    let until = Instant::now() + Duration::from_secs(10);
    let first = loop {
        match watch(111, true) {
            Ok(v) => break v,
            Err(_) if Instant::now() < until => thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("kernel did not start: {e:#}"),
        }
    };
    let second = watch(111, false).unwrap();
    let stale = watch(222, false).unwrap();
    let newer = watch(222, true).unwrap();
    let retired = watch(111, false).unwrap();
    let unknown = req(serde_json::json!({"op": "watch", "harness": "claude",
        "session": "nobody", "watcher": 333, "arm": true}))
    .unwrap();
    let delivered = State::load(&u.state_path()).unwrap().pid(l1).unwrap().wakes[0].clone();
    // The rewake turn ends: its Stop (stop_hook_active, as Claude Code marks rewakes) acks.
    let stop = req(
        serde_json::json!({"op": "hook", "event": "Stop", "harness": "claude",
        "payload": {"session_id": "s-l1", "cwd": "/tmp", "stop_hook_active": true,
            "last_assistant_message": "Read the S3 report."}}),
    )
    .unwrap();
    let after = State::load(&u.state_path()).unwrap().pid(l1).unwrap().wakes[0].clone();
    let tail = MemoryIndex::open(u.root())
        .unwrap()
        .session(l1)
        .unwrap()
        .tail;
    let _ = req(serde_json::json!({"op": "stop"}));
    let _ = fs::remove_dir_all(&root);

    let text = first["wake"]
        .as_str()
        .expect("an idle bound thread gets its wakes");
    assert!(
        text.contains(&format!("[w{w}]")) && text.contains("finished S3"),
        "{text}"
    );
    assert_eq!(
        second,
        serde_json::json!({}),
        "nothing new: the watcher keeps waiting"
    );
    assert_eq!(stale["done"], true, "an older watcher retires");
    assert_eq!(
        newer,
        serde_json::json!({}),
        "the watcher armed at a later Stop takes over"
    );
    assert_eq!(retired["done"], true, "the displaced watcher retires");
    assert_eq!(
        unknown["done"], true,
        "a thread the kernel does not bind is not watched"
    );
    assert_eq!(delivered.delivered.as_deref(), Some(key.as_str()));
    assert!(!delivered.acked, "acked only when the rewake turn ends");
    assert!(
        stop["stop"].is_null(),
        "no second re-wake from the turn-end guard: {stop}"
    );
    assert!(after.acked, "the rewake turn's Stop acknowledges the wake");
    assert!(
        tail.iter()
            .any(|t| t.starts_with("Rewake:") && t.contains("finished S3")),
        "the wake is a turn in the seat's tail: {tail:?}"
    );
}
