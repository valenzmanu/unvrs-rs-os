//! Deploy drain (docs/design/deploy-loop.md): a real kernel on a throwaway home with fake
//! drivers. A turn cut by the previous kernel is blocked for its lead (worker-recovery.md
//! startup rule), a parked worker resumes; a drain lets the in-flight turn finish, parks
//! the worker `working`, and starts nothing new.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uke::{
    ActiveScope, BriefFold, Driven, Drivers, FoldJob, SilentMapp, State, TurnRequest, TurnResult,
    Universe, kernel_request,
};

/// Turns block until the gate opens; `calls` counts started turns.
#[derive(Default)]
struct Fake {
    calls: AtomicU32,
    gate: Mutex<bool>,
    cv: Condvar,
}
impl Fake {
    fn open(&self) {
        *self.gate.lock().unwrap() = true;
        self.cv.notify_all();
    }
}
struct FakeDrivers(Arc<Fake>);
impl Drivers for FakeDrivers {
    fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
        bail!("no folds in this test")
    }
    fn turn(&self, _: &TurnRequest, _: &dyn Fn(u32)) -> Result<TurnResult> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let mut open = self.0.gate.lock().unwrap();
        while !*open {
            open = self.0.cv.wait(open).unwrap();
        }
        Ok(TurnResult {
            text: "working".into(),
            ..Default::default()
        })
    }
    fn quota(&self) -> Vec<Value> {
        vec![]
    }
}

fn home() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("uke-drain-{}-{nonce}", std::process::id()))
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn req(u: &Universe, v: Value) -> Value {
    kernel_request(u, &v, Duration::from_secs(5)).unwrap()
}

fn worker(u: &Universe, pid: usize) -> Value {
    req(u, json!({"op": "table"}))["pids"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pid"] == pid)
        .cloned()
        .unwrap()
}

#[test]
fn interrupted_turn_is_blocked_and_drain_parks_workers() {
    let dir = home();
    let u = Universe::at(&dir).unwrap();
    u.init().unwrap();
    // The previous kernel died mid-turn for `cut` (still `busy`); `parked` was drained
    // between turns (`working`, not busy).
    let mut st = State::load(&u.state_path()).unwrap();
    let mut make = |turns: u32, busy: bool, session: &str| {
        let pid = st.create(
            0,
            3,
            "driven",
            "claude",
            "a task",
            ActiveScope::default(),
            None,
        );
        let rec = st.pid_mut(pid).unwrap();
        rec.state = "working".into();
        rec.contract = Some(json!({"go_quote":"a task", "done_when":"task completes",
            "go_source":{"kind":"captain_terminal", "at":0}, "authority":"implement"}));
        rec.driven = Some(Driven {
            cpu: "headless".into(),
            turns,
            busy,
            os_pid: busy.then_some(u32::MAX - 1),
            session: Some(session.into()),
            cwd: dir.clone(),
            ..Default::default()
        });
        pid
    };
    let cut = make(2, true, "s-cut");
    let pid = make(2, false, "s-1");
    st.save(&u.state_path()).unwrap();

    let fake = Arc::new(Fake::default());
    let drivers = Arc::new(FakeDrivers(Arc::clone(&fake)));
    let kernel_home = u.root().to_path_buf();
    std::thread::spawn(move || {
        uke::serve_kernel(
            Universe::at(&kernel_home).unwrap(),
            drivers,
            Arc::new(SilentMapp),
            None,
        )
    });
    wait_for("the kernel", 10, || uke::kernel_running(&u));

    // The cut turn is blocked (no successor, no kill); the parked worker resumes on its session.
    // The socket can answer before the journal is written: wait for the event itself.
    let stop_event = || {
        fs::read_to_string(u.journal_path())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|e| e["kind"] == "stop" && e["pid"] == cut)
    };
    wait_for("the cut turn's stop event", 10, || stop_event().is_some());
    let stop = stop_event().expect("a stop event for the cut turn");
    assert_eq!(stop["stop"], "restart-unverified");
    assert_eq!(worker(&u, cut)["state"], "ended");
    wait_for("the resumed turn", 10, || {
        fake.calls.load(Ordering::SeqCst) == 1
    });
    assert_eq!(worker(&u, pid)["driven"]["session"], "s-1");

    // Drain while the turn is in flight: it is reported busy, not cut.
    let d = req(&u, json!({"op": "drain", "on": true}));
    assert_eq!(d["draining"], true);
    assert_eq!(d["busy"], json!([pid]));
    assert_eq!(req(&u, json!({"op": "ping"}))["ok"], true);

    // The turn finishes; the worker parks `working` and no new turn starts.
    fake.open();
    wait_for("the drain", 10, || {
        req(&u, json!({"op": "drain"}))["busy"] == json!([])
    });
    std::thread::sleep(Duration::from_millis(1500)); // three sweeps
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    let w = worker(&u, pid);
    assert_eq!(w["state"], "working");
    assert_eq!(w["driven"]["busy"], false);
    assert_eq!(w["driven"]["turns"], 3);
    assert_eq!(req(&u, json!({"op": "table"}))["kernel"]["draining"], true);

    // Undrain (an aborted deploy): the sweep resumes the worker.
    assert_eq!(
        req(&u, json!({"op": "drain", "on": false}))["draining"],
        false
    );
    wait_for("the resumed turn", 10, || {
        fake.calls.load(Ordering::SeqCst) >= 2
    });

    req(&u, json!({"op": "stop"}));
    let _ = fs::remove_dir_all(&dir);
}
