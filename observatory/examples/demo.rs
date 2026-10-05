//! A rich, moving fake snapshot for looking at the Observatory without a kernel:
//! `cargo run -p observatory --example demo [port]`, then open http://unvrs.localhost:7576
//!
//! Every 20 s a seat move lands (a comet crosses the sky), workers come and go, quota
//! drains, and the calls age.
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

const MOVES: [(&str, &str, &str); 6] = [
    (
        "move",
        "T3 · Claude · \"brand audit\"",
        "Codex app · \"brand audit, cont.\"",
    ),
    ("swap", "L2 acme · claude", "L2 acme · codex"),
    ("handoff", "L1 · Codex app", "L1 · T3 Claude"),
    (
        "move",
        "Codex app · \"kernel 0.8\"",
        "T3 · Claude · \"kernel 0.8\"",
    ),
    (
        "handoff",
        "L2 unvrs-rs · T3 Claude",
        "L2 unvrs-rs · driven (detached)",
    ),
    ("swap", "L1 · claude", "L1 · codex"),
];

const WORK: [(&str, &str, &str, &str, &str); 6] = [
    (
        "acme",
        "Compare our spring palette with the three competitors' launch pages and say which colours clash",
        "report",
        "codex",
        "gpt-5.5-codex",
    ),
    (
        "unvrs-rs",
        "Find every place the kernel still reads cwd to bind a thread, list them with line refs",
        "report",
        "claude",
        "claude-opus-5-5",
    ),
    (
        "acme",
        "Summarise the last 40 customer emails into the five complaints that repeat",
        "report",
        "claude",
        "claude-sonnet-5",
    ),
    (
        "atlas",
        "Read the 2025 vendor contracts folder and extract renewal dates and notice periods",
        "report",
        "codex",
        "gpt-5.5",
    ),
    (
        "unvrs-rs",
        "Measure how long hook round-trips take under load and report p50 and p99",
        "report",
        "codex",
        "gpt-5.5-codex",
    ),
    (
        "acme",
        "Draft three subject lines for the May newsletter in the brand voice",
        "report",
        "claude",
        "claude-haiku-5",
    ),
];

fn snapshot(t0: u64, started: Instant) -> Value {
    let e = started.elapsed().as_secs();
    let at = now_ms();
    let nmoves = 5 + e / 20;
    let moves: Vec<Value> = (0..nmoves)
        .map(|i| {
            let (kind, from, to) = MOVES[i as usize % MOVES.len()];
            let when = if i < 5 { t0 - (5 - i) * 610_000 } else { t0 + (i - 4) * 20_000 };
            json!({"at": when, "kind": kind, "from": from, "to": to, "pid": 1000 + i * 7, "bytes": 1800 + (i * 7919) % 14000})
        })
        .collect();
    let workers: Vec<Value> = (0..4)
        .map(|k| {
            let slot = (e / 45 + k) as usize % WORK.len();
            let (project, intent, shape, harness, model) = WORK[slot];
            let elapsed = (e % 45) + k * 170 + 12;
            let state = if k == 3 && e % 45 > 30 {
                "blocked"
            } else {
                "working"
            };
            json!({"pid": 2100 + slot * 3, "project": project, "intent": intent, "shape": shape,
                   "harness": harness, "model": model, "elapsed_s": elapsed, "state": state})
        })
        .take(if e % 90 < 60 { 4 } else { 3 })
        .collect();
    let mut recent = vec![
        json!({"at": t0 - 2_400_000, "kind": "decided", "text": "You chose \"keep the old logo\" for acme"}),
        json!({"at": t0 - 1_900_000, "kind": "delivered", "text": "Competitor pricing report reached L2 acme"}),
        json!({"at": t0 - 1_200_000, "kind": "failed", "text": "L3 PID 2088 stopped: codex quota exhausted, requeued on claude"}),
        json!({"at": t0 - 600_000, "kind": "remembered", "text": "L1 kept: \"Fridays are for reviews, no launches\""}),
        json!({"at": t0 - 180_000, "kind": "woke", "text": "L2 unvrs-rs ran detached to handle 2 wakes"}),
    ];
    for i in 0..e / 15 {
        let texts = [
            (
                "delivered",
                "Report \"hook latency p50/p99\" reached L2 unvrs-rs",
            ),
            ("moved", "L1 moved to a Codex app thread; codeword survived"),
            (
                "delivered",
                "Email complaints summary reached L2 acme",
            ),
            ("proposed", "L1 proposed project \"atlas\" (held for you)"),
        ];
        let (kind, text) = texts[i as usize % texts.len()];
        recent.push(json!({"at": t0 + (i + 1) * 15_000, "kind": kind, "text": text}));
    }
    let drain = (e as f64 / 30.0).min(40.0);
    json!({
        "at": at,
        "kernel": {"version": "0.8.0-eval.1", "os_pid": 48213, "uptime_s": 11_520 + e, "home": "/Users/captain/.unvrs"},
        "calls": [
            {"id": "d-0412", "kind": "decision", "question": "Ship the acme spring palette today, or hold it for Monday's brand review?",
             "options": ["ship today", "hold for Monday"], "age_s": 1840 + e, "project": "acme"},
            {"id": "p-0007", "kind": "proposal", "question": "New project \"atlas\": one map of every vendor contract, renewal date and notice period.",
             "options": ["approve", "decline"], "age_s": 312 + e, "project": null},
            {"id": "d-0419", "kind": "decision", "question": "L3 needs repo write access to fix the hook race. Allow for this task only?",
             "options": ["allow once", "report only"], "age_s": 45 + e, "project": "unvrs-rs"},
        ],
        "seats": [
            {"rank": 1, "project": null, "pid": 1001, "state": "live", "wakes": 0,
             "occupied_by": {"app": "Codex app", "harness": "codex", "thread_id": "019a2f4e-7c1b-7d20-9f3e-5b8c2a61d4f0",
                             "title": "Morning bridge: what needs me today", "link": "codex://threads/019a2f4e-7c1b-7d20-9f3e-5b8c2a61d4f0"}},
            {"rank": 2, "project": "unvrs-rs", "pid": 1044, "state": "working", "wakes": 1,
             "occupied_by": {"app": "T3 Code", "harness": "claude", "thread_id": "t3-8841", "title": "Kernel 0.8: seats and wakes", "link": null}},
            {"rank": 2, "project": "acme", "pid": 1057, "state": "idle", "wakes": 2, "occupied_by": null},
            {"rank": 2, "project": "atlas", "pid": null, "state": "vacant", "wakes": 0, "occupied_by": null},
        ],
        "workers": workers,
        "quota": [
            {"account": "captain@claude max", "harness": "claude", "remaining_pct": 64.0 - drain, "resets_at": at + 7_380_000, "source": "claude status"},
            {"account": "captain@chatgpt pro", "harness": "codex", "remaining_pct": null, "resets_at": null, "source": "app-server (no data yet)"},
            {"account": "team@codex business", "harness": "codex", "remaining_pct": 31.0 - drain / 2.0, "resets_at": at + 190_000_000, "source": "app-server rate limits"},
            {"account": "api@anthropic", "harness": "claude", "remaining_pct": 12.0, "resets_at": at + 2_700_000, "source": "console"},
        ],
        "moves": moves,
        "recent": recent,
        "context_reads": [
            {"at": at - 4_000, "pid": 2103, "source": "acme", "ref": "ctx://acme/brand/palette.md@a41c9e2", "bytes": 3812},
            {"at": at - 38_000, "pid": 2106, "source": "unvrs-rs", "ref": "ctx://unvrs-rs/uke/src/kernel/seats.rs#L120-L188@6c1b5e0", "bytes": 5120},
            {"at": at - 95_000, "pid": 1057, "source": "memory", "ref": "ctx://memory/acme/pinned@v31", "bytes": 1460},
            {"at": at - 240_000, "pid": 2103, "source": "acme", "ref": "ctx://acme/customers/emails/2026-09?limit=40@7f01d33", "bytes": 48_211},
            {"at": at - 610_000, "pid": 1001, "source": "memory", "ref": "ctx://memory/captain/standing@v88", "bytes": 2210},
        ],
        "projects": [
            {"id": "unvrs-rs", "purpose": "The UNVRS kernel and apps: one universe across every harness.", "sources": ["git:~/github/unvrs-rs", "memory"], "seat_pid": 1044},
            {"id": "acme", "purpose": "Flower subscription brand: site, newsletter, customer care.", "sources": ["path:~/acme", "git:acme-site", "memory"], "seat_pid": 1057},
            {"id": "atlas", "purpose": "Proposed: vendor contracts and renewals in one map.", "sources": ["path:~/Documents/contracts"], "seat_pid": null},
        ],
    })
}

fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(7576);
    let t0 = now_ms();
    let started = Instant::now();
    println!(
        "observatory demo: http://unvrs.localhost:{port}  (read-only fake snapshot; Ctrl+C to stop)"
    );
    observatory::serve(
        ([127, 0, 0, 1], port).into(),
        Arc::new(move || snapshot(t0, started)),
    )
}
