//! Paid, bounded context-ownership verification in a fresh sandbox (context-ownership.md).
//! Never touches the live kernel: its own home, socket and Observatory port.
//! cargo run -p unvrs --example ownership_e2e -- <fresh-sandbox> <unvrs-binary>
//!
//! Three real workers through the new path: one on Claude Code and one on Codex that
//! must pass the check, and one whose deliverable is written outside UNVRS and must be
//! bounced. Writes `<sandbox>/ownership-e2e.json` and leaves the sandbox kernel stopped.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uke::{
    Universe,
    signals::{CommandTracking, TrackedChild},
};

const PROJECT: &str = "own-e2e";
const THREAD: &str = "ownership-e2e";

struct Daemon {
    child: TrackedChild,
    u: Universe,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = uke::kernel_request(&self.u, &json!({"op":"stop"}), Duration::from_secs(3));
        let until = Instant::now() + Duration::from_secs(15);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn strip(cmd: &mut Command) {
    cmd.env_remove("UNVRS_DRIVEN_PID")
        .env_remove("UNVRS_DAEMON")
        .env_remove("UNVRS_SOCKET")
        .env_remove("UNVRS_TOKEN");
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("CODEX_THREAD")
            || key.starts_with("CODEX_SESSION")
            || key.starts_with("CODEX_SANDBOX")
            || key.starts_with("CLAUDE_CODE_")
            || matches!(key.as_ref(), "CLAUDECODE" | "CODEX_CI")
        {
            cmd.env_remove(key.as_ref());
        }
    }
}

fn start(u: &Universe, binary: &Path, port: u16) -> Result<Daemon> {
    let log = fs::File::create(u.root().join("daemon.log"))?;
    let mut cmd = Command::new(binary);
    cmd.args(["kernel", "run"])
        .env("UNVRS_HOME", u.root())
        .env("UNVRS_OBSERVATORY_PORT", port.to_string())
        .env("UNVRS_NOTIFY", "0")
        .env("UNVRS_QUOTA", "0")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    strip(&mut cmd);
    let mut daemon = Daemon {
        child: cmd.spawn_owned()?,
        u: u.clone(),
    };
    let until = Instant::now() + Duration::from_secs(100);
    loop {
        if let Ok(v) = uke::kernel_request(u, &json!({"op":"ping"}), Duration::from_secs(1)) {
            ensure!(
                v["home"].as_str() == Some(u.root().to_str().unwrap()),
                "wrong daemon home"
            );
            return Ok(daemon);
        }
        ensure!(
            daemon.child.try_wait()?.is_none(),
            "sandbox daemon exited; inspect daemon.log"
        );
        ensure!(Instant::now() < until, "sandbox daemon did not start");
        thread::sleep(Duration::from_millis(100));
    }
}

fn seed(u: &Universe) -> Result<()> {
    u.init()?;
    let dir = u.project_dir(PROJECT);
    fs::create_dir_all(dir.join("tasks"))?;
    fs::write(
        dir.join("project.toml"),
        format!(
            "id = '{PROJECT}'\npurpose = 'Isolated context-ownership verification'\nsources = []\n"
        ),
    )?;
    let mut st = uke::State::load(&u.state_path())?;
    let l1 = st.create(
        0,
        1,
        "attached",
        "codex",
        "sandbox",
        Default::default(),
        None,
    );
    let l2 = st.create(
        l1,
        2,
        "attached",
        "codex",
        "sandbox lead",
        Default::default(),
        None,
    );
    st.pid_mut(l2)?.project = Some(PROJECT.into());
    let key = format!("codex:{THREAD}");
    st.pid_mut(l2)?.thread = Some(key.clone());
    // An isolated fixture binding authenticates the CLI as the lead; no seat runs are paid.
    st.threads.insert(
        key.clone(),
        uke::ThreadRec {
            key,
            harness: "codex".into(),
            session: THREAD.into(),
            pid: Some(l2),
            bound: true,
            os_pid: Some(std::process::id()),
            last_seen: uke::now_ms(),
            ..Default::default()
        },
    );
    st.save(&u.state_path())?;
    Ok(())
}

fn cli(u: &Universe, binary: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .env("UNVRS_HOME", u.root())
        .env("CODEX_THREAD_ID", THREAD);
    strip(&mut cmd);
    cmd.env("CODEX_THREAD_ID", THREAD);
    let out = cmd.output_owned()?;
    ensure!(
        out.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn task(u: &Universe, binary: &Path, harness: &str, spec: &str) -> Result<usize> {
    let response = cli(
        u,
        binary,
        &[
            "ctl",
            "task",
            "--intent",
            "Context-ownership end-to-end check",
            "--spec",
            spec,
            "--shape",
            "report",
            "--go",
            "Ok, go",
            "--done-when",
            "The handoff passes the context-ownership check",
            "--harness",
            harness,
            "--kind",
            "validate",
            "--judgment",
            "low",
            "--thoroughness",
            "low",
        ],
    )?;
    fs::write(u.root().join(format!("{harness}-dispatch.txt")), &response)?;
    response
        .split("L3 PID ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .context("dispatch PID missing")?
        .trim_end_matches(';')
        .parse()
        .with_context(|| format!("dispatch: {response}"))
}

fn finished(u: &Universe, pid: usize, budget: Duration) -> Result<Value> {
    let path = u
        .project_dir(PROJECT)
        .join(format!("tasks/pid-{pid}/result.json"));
    let until = Instant::now() + budget;
    loop {
        if let Ok(b) = fs::read(&path) {
            return Ok(serde_json::from_slice(&b)?);
        }
        ensure!(Instant::now() < until, "PID {pid} did not finish in time");
        thread::sleep(Duration::from_millis(500));
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: ownership_e2e <fresh-sandbox> <unvrs-binary>"
    );
    let sandbox = PathBuf::from(&args[0]);
    ensure!(!sandbox.exists(), "sandbox must be fresh");
    fs::create_dir_all(&sandbox)?;
    let sandbox = sandbox.canonicalize()?;
    let u = Universe::at(&sandbox)?;
    let _owner = uke::signals::owner_scope(u.root(), 0);
    let binary = PathBuf::from(&args[1]).canonicalize()?;
    seed(&u)?;
    fs::create_dir_all(u.root().join("bin"))?;
    std::os::unix::fs::symlink(&binary, u.root().join("bin/unvrs"))?;
    let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    fs::write(u.root().join("port.txt"), port.to_string())?;
    let _daemon = start(&u, &binary, port)?;
    uke::kernel_request(
        &u,
        &json!({"op":"hook", "event":"UserPromptSubmit", "harness":"codex",
            "payload":{"session_id": THREAD, "cwd": u.root(), "prompt":"Ok, go"}}),
        Duration::from_secs(5),
    )?;
    let leak = std::env::temp_dir().join(format!("unvrs-ownership-leak-{}.md", std::process::id()));
    let _ = fs::remove_file(&leak);
    let pass = |h: &str| {
        format!(
            "Write a file named e2e-{h}.md in your working directory (your UNVRS task directory) containing one sentence on why UNVRS keeps the copy of record of job context. Then finish in this same reply: the HANDOFF block listing that file under deliverables, one decision, one learning, FIELD-NOTES, and UNVRS-RESULT last. No other tools, sources or delegation."
        )
    };
    let claude = task(&u, &binary, "claude", &pass("claude"))?;
    let codex = task(&u, &binary, "codex", &pass("codex"))?;
    let leaky = task(
        &u,
        &binary,
        "claude",
        &format!(
            "Write a one-sentence note to {} (outside your task directory, on purpose) and list exactly that absolute path as your only deliverable in the HANDOFF block; finish in this same reply with HANDOFF, FIELD-NOTES and UNVRS-RESULT last. If UNVRS sends the result back, follow its instructions.",
            leak.display()
        ),
    )?;
    println!("dispatched: claude PID {claude}, codex PID {codex}, leak PID {leaky}");
    let mut report = json!({"sandbox": u.root(), "port": port, "binary": binary});
    for (label, pid) in [("claude", claude), ("codex", codex), ("leak", leaky)] {
        let res = finished(&u, pid, Duration::from_secs(600))?;
        let own = &res["ownership"];
        println!(
            "{label}: PID {pid} how={} verdict={} bounces={} history={} model={}",
            res["how"], own["verdict"], own["bounces"], own["history"], res["actual_model"]
        );
        report[label] = json!({"pid": pid, "how": res["how"], "harness": res["harness"],
            "model": res["actual_model"], "ownership": own, "result": res["result"]});
    }
    fs::write(
        u.root().join("ownership-e2e.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    for label in ["claude", "codex"] {
        ensure!(
            report[label]["ownership"]["verdict"] == "accepted" && report[label]["how"] == "done",
            "{label} did not pass: {}",
            report[label]
        );
    }
    let history = report["leak"]["ownership"]["history"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure!(
        history.first().is_some_and(|h| h["verdict"] == "bounced"
            && h["leaks"].to_string().contains("deliverable outside UNVRS")),
        "the leak was not bounced: {}",
        report["leak"]
    );
    let _ = fs::remove_file(&leak);
    println!(
        "verified: claude and codex passed; the leak was bounced ({} checks)",
        history.len()
    );
    Ok(())
}
