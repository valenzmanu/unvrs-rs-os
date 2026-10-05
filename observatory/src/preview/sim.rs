//! The fixtures behind `/preview?sim=problem` (the amber/red states) and `?sim=calm`
//! (what a calm day looks like: nearly empty): made-up snapshots and hosts for screenshots
//! and checks. They are never mixed with real data: in sim mode the page reads nothing
//! from the kernel or the host, and every screen says SIMULATED.
use crate::probe::{
    ClaudeUsage, Disk, Host, Journal, KernelState, Outputs, PidInfo, Procs, ThreadInfo,
    UsageWindow, Workspace, Workspaces,
};
use serde_json::{Value, json};

/// A context-ownership check record as the kernel writes it (ownership.rs).
pub fn ownership_check(now: u64, pid: u64, harness: &str, verdict: &str, leaks: &[&str]) -> Value {
    let bounces = match verdict {
        "bounced" => 1,
        "blocked" => 3,
        _ => 0,
    };
    json!({
        "pid": pid, "project": "unvrs-rs", "at": now - pid * 60_000, "verdict": verdict,
        "pass": verdict == "accepted", "bounces": bounces, "max_bounces": 3,
        "intent": format!("SIMULATED task {pid}"), "harness": harness, "how": if verdict == "accepted" { "done" } else { "" },
        "dir": format!("/sim/.unvrs/projects/unvrs-rs/tasks/pid-{pid}"),
        "notes": if verdict == "accepted" { json!(["p1: decision", "p2: learning"]) } else { json!([]) },
        "check": {
            "pass": verdict == "accepted", "leaks": leaks,
            "brief": format!("/sim/.unvrs/projects/unvrs-rs/tasks/pid-{pid}/contract.json"),
            "record_turns": 4, "record": [format!("/sim/.unvrs/sessions/pid-{pid}/session.json")],
            "deliverables": [{"item": "report.md", "kind": "path", "owned": leaks.is_empty(), "exists": true,
                "resolved": if leaks.is_empty() { format!("/sim/.unvrs/projects/unvrs-rs/tasks/pid-{pid}/report.md") } else { "/sim/Downloads/report.md".into() }}],
            "handoff": {"deliverables": ["report.md"], "decisions": ["kept it small"], "learnings": ["none"]}
        },
        "history": [{"verdict": verdict, "leaks": leaks}]
    })
}

/// The fixture as of `now` (epoch ms): ages stay the same whenever it is rendered.
pub fn problem(now: u64) -> (Value, Host) {
    let snap = json!({
        "at": now - 1000,
        "kernel": {"version": "0.8.0-sim", "os_pid": 4242, "uptime_s": 3 * 3600 + 720, "home": "/sim/.unvrs"},
        "calls": [
            {"id": "s3", "kind": "decision", "question": "SIMULATED: the fixture worker needs write access to the sim repo. Allow it for this task only?",
             "options": ["allow once", "report only"], "age_s": 240, "project": "sim-project"},
            {"id": "s2", "kind": "decision", "question": "SIMULATED: pick the palette for the fixture landing page.",
             "options": ["warm", "cool"], "age_s": 3600, "project": "sim-project"},
            {"id": "s1", "kind": "decision", "question": "SIMULATED: an older call that has waited two days.",
             "options": ["yes", "no"], "age_s": 2 * 86_400, "project": null},
            {"id": "s4", "kind": "proposal", "question": "SIMULATED: create project \"fixture\".",
             "options": ["approve", "no"], "age_s": 600, "project": null},
        ],
        "seats": [
            {"rank": 1, "pid": 1, "project": null, "state": "live", "wakes": 0,
             "occupied_by": {"app": "t3", "harness": "claude", "thread_id": "sim-l1", "title": "SIMULATED L1", "link": null}},
            {"rank": 2, "pid": 2, "project": "sim-project", "state": "running", "wakes": 1, "occupied_by": null},
            {"rank": 2, "pid": 3, "project": "sim-a", "state": "free", "wakes": 0, "occupied_by": null},
            {"rank": 2, "pid": 4, "project": "sim-b", "state": "free", "wakes": 0, "occupied_by": null},
            {"rank": 2, "pid": 5, "project": "sim-c", "state": "free", "wakes": 0, "occupied_by": null},
        ],
        "workers": [
            {"pid": 31, "project": "sim-project", "intent": "SIMULATED worker producing output", "shape": "report",
             "harness": "claude", "model": "claude-opus-5-5", "effort": "high", "elapsed_s": 1260, "state": "working"},
            {"pid": 32, "project": "sim-project", "intent": "SIMULATED worker with no output for 14 minutes", "shape": "report",
             "harness": "claude", "model": null, "elapsed_s": 2400, "state": "working"},
        ],
        "quota": [
            {"account": "sim-claude", "harness": "claude", "remaining_pct": null, "resets_at": null, "source": "SIMULATED fixture"},
            {"account": "sim-codex", "harness": "codex", "remaining_pct": null, "resets_at": null, "source": "SIMULATED fixture"},
        ],
    });
    let mut ks = KernelState {
        path: "SIMULATED".into(),
        at_ms: now,
        ..Default::default()
    };
    ks.raised_by.insert("s3".into(), 2);
    ks.raised_by.insert("s2".into(), 2);
    ks.raised_by.insert("s4".into(), 1);
    for (pid, parent, rank, state, busy) in [
        (1, 0, 1, "live", false),
        (2, 1, 2, "running", true),
        (3, 1, 2, "idle", false),
        (4, 1, 2, "idle", false),
        (5, 1, 2, "idle", false),
        (31, 2, 3, "working", true),
        (32, 2, 3, "working", true),
    ] {
        ks.pids.insert(
            pid,
            PidInfo {
                pid,
                parent,
                rank,
                state: state.into(),
                busy,
                updated: now - 1_800_000,
                harness: "claude".into(),
                ..Default::default()
            },
        );
    }
    ks.threads.push(ThreadInfo {
        pid: 1,
        app: "t3".into(),
        harness: "claude".into(),
        bound: true,
        last_seen: now - 6000,
        turn_open: false,
    });
    let mut outputs = Outputs::new();
    outputs.insert(31, now - 4000);
    outputs.insert(32, now - 14 * 60_000);
    let events: Vec<Value> = vec![
        json!({"at": now - 900_000, "kind": "turn", "pid": 31, "error": null}),
        json!({"at": now - 420_000, "kind": "turn", "pid": 32, "error": "SIMULATED: claude exited 1: rate limit reached for this account"}),
        json!({"at": now - 300_000, "kind": "turn", "pid": 32, "error": "SIMULATED: claude exited 1: rate limit reached for this account"}),
        json!({"at": now - 180_000, "kind": "turn", "pid": 31, "error": "SIMULATED: claude exited 1: rate limit reached for this account"}),
        json!({"at": now - 120_000, "kind": "fold", "pid": 1, "result": "rejected", "error": "SIMULATED: Brief worker failed; previous brief kept"}),
        json!({"at": now - 60_000, "kind": "captain", "op": "l1", "ok": true}),
        json!({"at": now - 2000, "kind": "wake", "pid": 1}),
    ];
    let ws = Workspaces {
        at_ms: now - 30_000,
        sized_ms: Some(now - 240_000),
        measuring: None,
        list: vec![
            Workspace {
                source: "sim".into(),
                path: "/sim/repo".into(),
                branch: Some("main".into()),
                head: "0000000".into(),
                owner: Some("main checkout".into()),
                main: true,
                size: Some(9_800_000_000),
                target: Some(8_700_000_000),
                ..Default::default()
            },
            Workspace {
                source: "sim".into(),
                path: "/sim/tasks/pid-31/wt".into(),
                branch: Some("feat/sim-fixture".into()),
                head: "1111111".into(),
                owner: Some("PID 31".into()),
                owner_pid: Some(31),
                merged: Some(false),
                into: Some("main".into()),
                size: Some(2_100_000_000),
                target: Some(1_900_000_000),
                ..Default::default()
            },
            Workspace {
                source: "sim".into(),
                path: "/sim/tasks/pid-9/wt".into(),
                branch: Some("fix/sim-old".into()),
                head: "2222222".into(),
                owner: Some("PID 9".into()),
                owner_pid: Some(9),
                merged: Some(true),
                into: Some("main".into()),
                size: Some(3_300_000_000),
                target: Some(3_100_000_000),
                ..Default::default()
            },
        ],
    };
    let host = Host {
        at_ms: now,
        disk: Some(Disk {
            path: "/sim".into(),
            free: 6_400_000_000,
            total: 460_000_000_000,
            at_ms: now - 3000,
        }),
        state: Some(ks),
        journal: Some(Journal {
            path: "SIMULATED".into(),
            at_ms: now,
            events,
        }),
        procs: Some(Procs {
            at_ms: now - 5000,
            t3: true,
            codex: false,
            claude_desktop: true,
        }),
        outputs,
        workspaces: Some(ws),
        claude_usage: Some(ClaudeUsage {
            at_ms: now - 90_000,
            plan: "max".into(),
            five_hour: Some(UsageWindow {
                used_pct: 93.0,
                resets_at: Some(now + 2 * 3_600_000),
            }),
            seven_day: Some(UsageWindow {
                used_pct: 41.0,
                resets_at: Some(now + 4 * 86_400_000),
            }),
            source: "SIMULATED fixture".into(),
        }),
        usage_error: None,
        usage_reading: false,
        ownership: Some(vec![
            ownership_check(
                now,
                31,
                "claude",
                "bounced",
                &["deliverable outside UNVRS: ~/Downloads/report.md"],
            ),
            ownership_check(now, 32, "codex", "accepted", &[]),
        ]),
    };
    (snap, host)
}

/// A calm day: nothing needs the captain, every built driver answered, one worker
/// producing output, the rest idle.
pub fn calm(now: u64) -> (Value, Host) {
    let snap = json!({
        "at": now - 1000,
        "kernel": {"version": "0.8.0-sim", "os_pid": 4242, "uptime_s": 26 * 3600, "home": "/sim/.unvrs"},
        "calls": [],
        "seats": [
            {"rank": 1, "pid": 1, "project": null, "state": "live", "wakes": 0,
             "occupied_by": {"app": "t3", "harness": "claude", "thread_id": "sim-l1", "title": "SIMULATED L1", "link": null}},
            {"rank": 2, "pid": 2, "project": "sim-project", "state": "free", "wakes": 0, "occupied_by": null},
            {"rank": 2, "pid": 3, "project": "sim-a", "state": "free", "wakes": 0, "occupied_by": null},
            {"rank": 2, "pid": 4, "project": "sim-b", "state": "free", "wakes": 0, "occupied_by": null},
            {"rank": 2, "pid": 5, "project": "sim-c", "state": "free", "wakes": 0, "occupied_by": null},
            {"rank": 2, "pid": 6, "project": "sim-d", "state": "free", "wakes": 0, "occupied_by": null},
        ],
        "workers": [
            {"pid": 41, "project": "sim-project", "intent": "SIMULATED worker producing output", "shape": "report",
             "harness": "claude", "model": "claude-opus-5-5", "effort": "high", "elapsed_s": 540, "state": "working"},
        ],
        "quota": [
            {"account": "sim-claude", "harness": "claude", "remaining_pct": 64.0, "resets_at": now + 3 * 3_600_000, "source": "SIMULATED fixture"},
            {"account": "sim-codex", "harness": "codex", "remaining_pct": 71.0, "resets_at": now + 4 * 86_400_000, "source": "SIMULATED fixture"},
        ],
    });
    let mut ks = KernelState {
        path: "SIMULATED".into(),
        at_ms: now,
        ..Default::default()
    };
    for (pid, parent, rank, state, busy) in [
        (1, 0, 1, "live", false),
        (2, 1, 2, "idle", false),
        (3, 1, 2, "idle", false),
        (4, 1, 2, "idle", false),
        (5, 1, 2, "idle", false),
        (6, 1, 2, "idle", false),
        (41, 2, 3, "working", true),
    ] {
        ks.pids.insert(
            pid,
            PidInfo {
                pid,
                parent,
                rank,
                state: state.into(),
                busy,
                updated: now - 3_600_000,
                harness: "claude".into(),
                ..Default::default()
            },
        );
    }
    ks.threads.push(ThreadInfo {
        pid: 1,
        app: "t3".into(),
        harness: "claude".into(),
        bound: true,
        last_seen: now - 40_000,
        turn_open: false,
    });
    let mut outputs = Outputs::new();
    outputs.insert(41, now - 3000);
    let events: Vec<Value> = vec![
        json!({"at": now - 1_500_000, "kind": "captain", "op": "l1", "ok": true}),
        json!({"at": now - 900_000, "kind": "fold", "pid": 1, "result": "applied"}),
        json!({"at": now - 60_000, "kind": "turn", "pid": 41, "error": null}),
        json!({"at": now - 5000, "kind": "wake", "pid": 2}),
    ];
    let ws = Workspaces {
        at_ms: now - 30_000,
        sized_ms: Some(now - 300_000),
        measuring: None,
        list: vec![
            Workspace {
                source: "sim".into(),
                path: "/sim/repo".into(),
                branch: Some("main".into()),
                head: "0000000".into(),
                owner: Some("main checkout".into()),
                main: true,
                size: Some(1_200_000_000),
                target: Some(900_000_000),
                ..Default::default()
            },
            Workspace {
                source: "sim".into(),
                path: "/sim/tasks/pid-41/wt".into(),
                branch: Some("feat/sim-calm".into()),
                head: "3333333".into(),
                owner: Some("PID 41".into()),
                owner_pid: Some(41),
                merged: Some(false),
                into: Some("main".into()),
                size: Some(640_000_000),
                target: Some(600_000_000),
                ..Default::default()
            },
        ],
    };
    let host = Host {
        at_ms: now,
        disk: Some(Disk {
            path: "/sim".into(),
            free: 142_000_000_000,
            total: 460_000_000_000,
            at_ms: now - 4000,
        }),
        state: Some(ks),
        journal: Some(Journal {
            path: "SIMULATED".into(),
            at_ms: now,
            events,
        }),
        procs: Some(Procs {
            at_ms: now - 5000,
            t3: true,
            codex: true,
            claude_desktop: false,
        }),
        outputs,
        workspaces: Some(ws),
        claude_usage: Some(ClaudeUsage {
            at_ms: now - 60_000,
            plan: "max".into(),
            five_hour: Some(UsageWindow {
                used_pct: 31.0,
                resets_at: Some(now + 3 * 3_600_000),
            }),
            seven_day: Some(UsageWindow {
                used_pct: 12.0,
                resets_at: Some(now + 5 * 86_400_000),
            }),
            source: "SIMULATED fixture".into(),
        }),
        usage_error: None,
        usage_reading: false,
        ownership: Some(vec![ownership_check(now, 41, "claude", "accepted", &[])]),
    };
    (snap, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::model::{Inputs, Model, NeedKind, Tone};

    #[test]
    fn the_problem_fixture_shows_every_fault_state() {
        let now = 1_790_000_000_000;
        let (snap, host) = problem(now);
        let m = Model::build(Inputs {
            snap: &snap,
            host: &host,
            now_ms: now,
            sim: true,
        });
        assert!(m.sim);
        assert_eq!(m.status_tone, Tone::Bad);
        assert!(m.status.starts_with("Disk 6.4 GB free"), "{}", m.status);
        assert_eq!(m.needs[0].id, "disk");
        assert!(m.needs[0].blocking);
        assert_eq!(m.needs[1].kind, NeedKind::Fault, "the Agent driver is down");
        assert!(m.needs.len() > 3, "enough for +N more");
        assert_eq!(m.drivers[0].tone, Tone::Bad);
        assert_eq!(m.drivers[1].tone, Tone::Warn);
        let w = &m.crew.active[0].workers;
        assert!(w[0].pulse && w[0].model == "claude-opus-5-5" && w[0].effort == "high");
        assert_eq!(w[1].tone, Tone::Warn);
        assert_eq!(
            (m.fuel.quota[0].key.as_str(), m.fuel.quota[0].tone),
            ("quota:claude:5h", Tone::Bad)
        );
        assert_eq!(
            m.fuel.quota[2].value, "unknown",
            "the codex row has no reading"
        );
        assert_eq!(m.fuel.ws_rows.len(), 1);
    }

    #[test]
    fn the_calm_fixture_is_nearly_empty() {
        let now = 1_790_000_000_000;
        let (snap, host) = calm(now);
        let m = Model::build(Inputs {
            snap: &snap,
            host: &host,
            now_ms: now,
            sim: true,
        });
        assert_eq!((m.status_tone, m.status.as_str()), (Tone::Ok, "All quiet."));
        assert!(m.needs.is_empty());
        assert!(
            m.drivers
                .iter()
                .chain(m.surfaces.iter())
                .all(|l| !l.tone.is_fault())
        );
        assert_eq!(m.crew.idle.len(), 4, "idle leads collapse");
        assert!(m.crew.active[0].workers[0].pulse);
    }
}
