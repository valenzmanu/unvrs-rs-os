//! An idle L2 lead with no thread, woken by L1's `send`, runs detached (C9). The
//! kernel must hand the harness a prompt it acts on: the send is work, and no
//! `$unvrs:<verb>` in it may pull an entry skill into the run (Codex expands `$name`
//! mentions in any prompt; the `answer` skill then replied "no `$unvrs:answer` result
//! in this turn… run `unvrs doctor`" and did no work, wakes w239 and w241).
use std::{
    fs,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use uke::{ActiveScope, State, TurnRequest, TurnResult, Universe};

struct Capture(Mutex<mpsc::Sender<TurnRequest>>);
impl uke::Drivers for Capture {
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
    fn fold(&self, _: &uke::FoldJob) -> anyhow::Result<uke::BriefFold> {
        anyhow::bail!("no fold worker here")
    }
    fn turn(&self, req: &TurnRequest, _: &dyn Fn(u32)) -> anyhow::Result<TurnResult> {
        let _ = self.0.lock().unwrap().send(req.clone());
        Ok(TurnResult {
            model: req.model.clone(),
            effort: req.effort.clone(),
            text: "Dispatched the audit.".into(),
            ..Default::default()
        })
    }
}

#[test]
fn a_send_to_an_idle_threadless_lead_is_a_work_request() {
    unsafe { std::env::set_var("UNVRS_NOTIFY", "0") };
    let root = std::env::temp_dir().join(format!("unvrs-seat-run-{}", std::process::id()));
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
    // Exactly what `send bloom "…"` from L1 queues (ops.rs: kind note, "from PID n: …").
    st.wake(
        l2,
        "note",
        &format!(
            "from PID {l1}: The captain wants the onboarding flow audited end to end. They answered d3 with $unvrs:answer d3 \"ship it\"; plan it and report back."
        ),
        Some(l1),
        None,
    )
    .unwrap();
    st.save(&u.state_path()).unwrap();

    let (tx, rx) = mpsc::channel();
    let k = u.clone();
    thread::spawn(move || {
        uke::serve_kernel(
            k,
            Arc::new(Capture(Mutex::new(tx))),
            Arc::new(mapp_unvrs::UnvrsMapp),
            None,
        )
    });
    let req = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("the kernel ran the woken lead");
    for flag in ["--kind", "--judgment", "--thoroughness"] {
        assert!(
            req.prompt.contains(flag),
            "delegation guidance must name {flag}"
        );
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

    assert_eq!(req.harness, "claude");
    assert_eq!(req.model.as_deref(), Some("claude-opus-fixture"));
    assert_eq!(req.effort.as_deref(), Some("high"));
    let p = &req.prompt;
    assert!(p.contains("onboarding flow audited"), "{p}");
    assert!(
        !p.contains("$unvrs:"),
        "a `$unvrs:` mention in a driven prompt triggers the entry skill:\n{p}"
    );
    assert!(
        p.contains("request for work") && p.contains("ctl task"),
        "a send must read as work to do, with the way to dispatch it:\n{p}"
    );
    assert!(!p.contains("Do not start new tasks"), "{p}");
    // A detached run is driven: its hot set names `unvrs ctl`, not the MCP tool.
    assert!(!p.contains("MCP tool"), "{p}");
}
