//! Paid, bounded DrvEcon verification in a fresh sandbox. Never restarts the live kernel.
//! cargo run -p unvrs --example drvecon_e2e -- <sandbox> <unvrs-binary> --live
//! --check exercises hook admission and held receipts without paid inference.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use uke::{
    TurnRequest, TurnResult, Universe,
    signals::{CommandTracking, TrackedChild},
};

// Catalog masking exists only in this verification adapter, never the production CLI.
struct MissingModels(String);
impl uke::Drivers for MissingModels {
    fn catalogs(&self, home: &Path) -> BTreeMap<String, uke::drv_econ::Catalog> {
        let mut catalogs = drv_agent::catalogs(home);
        for (h, c) in &mut catalogs {
            if self.0 == "hold" {
                c.models.clear();
            } else if (self.0 == "full-fallback" && h == "claude")
                || (self.0 != "full-fallback" && h == "codex")
            {
                let family = if self.0 == "quick-fallback" {
                    "luna"
                } else if self.0 == "full-fallback" {
                    "sonnet"
                } else {
                    "sol"
                };
                c.models.retain(|m| !m.id.contains(family));
            }
        }
        catalogs
    }
    fn harness_ready(&self, h: &str) -> Result<()> {
        drv_agent::harness_ready(h)
    }
    fn fold(&self, _: &uke::FoldJob) -> Result<uke::BriefFold> {
        anyhow::bail!("smoke sends no progress to fold")
    }
    fn turn(&self, req: &TurnRequest, started: &dyn Fn(u32)) -> Result<TurnResult> {
        drv_agent::run_turn(req, started)
    }
    fn turn_observed(
        &self,
        req: &TurnRequest,
        started: &dyn Fn(u32),
        event: &dyn Fn(&Value),
    ) -> Result<TurnResult> {
        drv_agent::run_turn_observed(req, started, event)
    }
}

struct Daemon {
    child: TrackedChild,
    u: Universe,
}
impl Daemon {
    fn stop(&mut self) -> Result<()> {
        let _ = uke::kernel_request(&self.u, &json!({"op":"stop"}), Duration::from_secs(3));
        let until = Instant::now() + Duration::from_secs(15);
        while self.child.try_wait()?.is_none() {
            ensure!(Instant::now() < until, "sandbox daemon did not stop");
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.stop();
    } // TrackedChild guards cleanup if stop fails.
}
fn start(
    u: &Universe,
    binary: &Path,
    mode: &str,
    port: u16,
    summary_model: &str,
) -> Result<Daemon> {
    let log = fs::File::create(u.root().join(format!("{mode}-daemon.log")))?;
    let mut cmd = if mode == "native" {
        Command::new(binary)
    } else {
        Command::new(std::env::current_exe()?)
    };
    if mode == "native" {
        cmd.args(["kernel", "run"]);
    } else {
        cmd.args(["--serve", mode]);
    }
    cmd.env("UNVRS_HOME", u.root())
        .env("UNVRS_OBSERVATORY_PORT", port.to_string())
        .env("UNVRS_SUMMARY_ROUTES", format!("codex={summary_model}"))
        .env("UNVRS_NOTIFY", "0")
        .env("UNVRS_QUOTA", "0")
        .env("UNVRS_TURN_IDLE_SECS", "90")
        .env("UNVRS_TURN_MAX_SECS", "150")
        .env_remove("UNVRS_DRIVEN_PID")
        .env_remove("UNVRS_DAEMON")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
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
            "sandbox daemon exited; inspect its log"
        );
        ensure!(
            Instant::now() < until,
            "sandbox daemon did not become ready"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
fn cli_output(u: &Universe, binary: &Path, args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new(binary)
        .args(args)
        .env("UNVRS_HOME", u.root())
        .env("CODEX_THREAD_ID", "drvecon-e2e")
        .output_owned()?)
}
fn cli(u: &Universe, binary: &Path, args: &[&str]) -> Result<String> {
    let out = cli_output(u, binary, args)?;
    ensure!(
        out.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn seed(u: &Universe) -> Result<()> {
    u.init()?;
    let dir = u.project_dir("econ-e2e");
    fs::create_dir_all(dir.join("tasks"))?;
    fs::write(
        dir.join("project.toml"),
        "id = 'econ-e2e'\npurpose = 'Isolated DrvEcon verification'\nsources = []\n",
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
    st.pid_mut(l2)?.project = Some("econ-e2e".into());
    let key = "codex:drvecon-e2e".to_owned();
    st.pid_mut(l2)?.thread = Some(key.clone());
    // An isolated fixture binding authenticates the real client ancestor and suppresses paid seat runs.
    st.threads.insert(
        key.clone(),
        uke::ThreadRec {
            key,
            harness: "codex".into(),
            session: "drvecon-e2e".into(),
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
fn captain_go(u: &Universe) -> Result<()> {
    uke::kernel_request(
        u,
        &json!({
            "op":"hook", "event":"UserPromptSubmit", "harness":"codex",
            "payload":{"session_id":"drvecon-e2e", "cwd":u.root(), "prompt":"Ok, go"}
        }),
        Duration::from_secs(5),
    )?;
    Ok(())
}
fn refuse_no_go(u: &Universe, binary: &Path) -> Result<()> {
    let before = uke::kernel_request(u, &json!({"op":"table"}), Duration::from_secs(5))?;
    let out = cli_output(
        u,
        binary,
        &[
            "ctl",
            "task",
            "--intent",
            "unapproved sentinel",
            "--spec",
            "finish",
            "--done-when",
            "Sentinel returned",
        ],
    )?;
    ensure!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr)
                .contains("No captain go found for this work: propose it and ask the captain."),
        "missing go was not refused"
    );
    let after = uke::kernel_request(u, &json!({"op":"table"}), Duration::from_secs(5))?;
    ensure!(
        before["pids"].as_array().map(Vec::len) == after["pids"].as_array().map(Vec::len),
        "refusal created a worker"
    );
    write_json(
        &u.root().join("no-go-verified.json"),
        &json!({"refusal":String::from_utf8_lossy(&out.stderr), "workers_created":0}),
    )
}
fn task(
    u: &Universe,
    binary: &Path,
    label: &str,
    flags: &[&str],
    expect_hold: bool,
) -> Result<usize> {
    let spec = format!(
        "Reply exactly: UNVRS-RESULT: econ-e2e-{label}. Use no tools, sources, files, delegation or commands. Emit no progress; finish in one short reply."
    );
    let done_when = format!("Returned econ-e2e-{label} with verified model and effort");
    let mut args = vec![
        "ctl",
        "task",
        "--intent",
        "Return the requested verification sentinel",
        "--spec",
        &spec,
        "--shape",
        "report",
        "--go",
        "Ok, go",
        "--done-when",
        &done_when,
    ];
    args.extend_from_slice(flags);
    let response = cli(u, binary, &args)?;
    fs::write(u.root().join(format!("{label}-dispatch.txt")), &response)?;
    let pid: usize = response
        .split("L3 PID ")
        .nth(1)
        .context("dispatch PID missing")?
        .split_whitespace()
        .next()
        .context("PID missing")?
        .trim_end_matches(';')
        .parse()?;
    ensure!(
        response.contains("Task held") == expect_hold,
        "unexpected admission: {response}"
    );
    println!(
        "{label}: PID {pid} {}",
        if expect_hold { "held" } else { "dispatched" }
    );
    Ok(pid)
}
fn receipt(u: &Universe, pid: usize) -> Result<Value> {
    let contract: Value = serde_json::from_slice(&fs::read(
        u.project_dir("econ-e2e")
            .join(format!("tasks/pid-{pid}/contract.json")),
    )?)?;
    ensure!(
        contract["go_quote"] == "Ok, go"
            && contract["go_source"]["kind"] == "captain_prompt"
            && contract["go_source"]["thread"] == "codex:drvecon-e2e",
        "hook admission receipt missing"
    );
    for key in ["go_quote", "go_source", "done_when"] {
        ensure!(
            contract["econ"][key] == contract[key],
            "econ admission field differs: {key}"
        );
    }
    ensure!(
        contract["done_when"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && !s.contains(['\r', '\n'])),
        "done-when missing"
    );
    Ok(contract)
}
fn result(u: &Universe, pid: usize, label: &str) -> Result<Value> {
    let path = u
        .project_dir("econ-e2e")
        .join(format!("tasks/pid-{pid}/result.json"));
    let until = Instant::now() + Duration::from_secs(180);
    loop {
        if let Ok(bytes) = fs::read(&path) {
            let value: Value = serde_json::from_slice(&bytes)?;
            ensure!(
                value["how"] == "done",
                "task did not finish: {}",
                value["how"]
            );
            ensure!(
                value["result"]
                    .as_str()
                    .is_some_and(|s| s.contains(&format!("econ-e2e-{label}"))),
                "sentinel missing"
            );
            ensure!(
                value["contract"] == receipt(u, pid)?,
                "result contract differs from disk"
            );
            let chosen = &value["contract"]["econ"]["chosen"];
            ensure!(
                value["actual_model"] == chosen["model"]
                    && value["actual_effort"] == chosen["effort"],
                "accepted axes differ from receipt"
            );
            ensure!(
                value["tools"].as_array().is_some_and(Vec::is_empty),
                "smoke task unexpectedly used tools"
            );
            ensure!(
                value["effort_evidence"].is_string(),
                "effort acceptance evidence missing"
            );
            write_json(&u.root().join(format!("{label}-verified.json")), &value)?;
            println!(
                "{label}: accepted {} effort {}",
                value["actual_model"], value["actual_effort"]
            );
            return Ok(value);
        }
        let table = uke::kernel_request(u, &json!({"op":"table"}), Duration::from_secs(5))?;
        if let Some(p) = table["pids"]
            .as_array()
            .and_then(|ps| ps.iter().find(|p| p["pid"] == pid))
        {
            ensure!(
                !matches!(p["state"].as_str(), Some("blocked" | "ended" | "held")),
                "task stopped without successful result: {}",
                p["state"]
            );
        }
        ensure!(Instant::now() < until, "task PID {pid} timed out");
        thread::sleep(Duration::from_millis(200));
    }
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--serve") {
        let mode = args.get(1).context("mask mode required")?.clone();
        ensure!(
            matches!(
                mode.as_str(),
                "fallback" | "quick-fallback" | "full-fallback" | "hold"
            ),
            "invalid mask mode"
        );
        let port: u16 = std::env::var("UNVRS_OBSERVATORY_PORT")?.parse()?;
        let observer: uke::ObservatoryStart = Box::new(move |snapshot, settings, token| {
            thread::spawn(move || {
                let _ = observatory::serve_with_settings(
                    ([127, 0, 0, 1], port).into(),
                    snapshot,
                    settings,
                    token,
                );
            });
        });
        return uke::serve_kernel(
            Universe::home()?,
            Arc::new(MissingModels(mode)),
            Arc::new(mapp_unvrs::UnvrsMapp),
            Some(observer),
        );
    }
    ensure!(
        args.len() == 3 && matches!(args[2].as_str(), "--live" | "--check"),
        "usage: drvecon_e2e <fresh-sandbox> <unvrs-binary> --live|--check"
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
    if args[2] == "--check" {
        let mut daemon = start(&u, &binary, "hold", port, "unused-check")?;
        refuse_no_go(&u, &binary)?;
        captain_go(&u)?;
        let pid = task(
            &u,
            &binary,
            "check",
            &["--kind", "research", "--thoroughness", "low"],
            true,
        )?;
        let contract = receipt(&u, pid)?;
        ensure!(
            contract["econ"]["role"] == "research_quick" && contract["econ"]["chosen"].is_null(),
            "held quick route missing"
        );
        let explain: Value =
            serde_json::from_str(&cli(&u, &binary, &["econ", "explain", &pid.to_string()])?)?;
        ensure!(explain == contract["econ"], "explain receipt differs");
        let snapshot = uke::kernel_request(&u, &json!({"op":"snapshot"}), Duration::from_secs(5))?;
        let worker = snapshot["workers"]
            .as_array()
            .and_then(|ws| ws.iter().find(|w| w["pid"] == pid))
            .context("held worker missing")?;
        ensure!(
            worker["go_quote"] == contract["go_quote"]
                && worker["done_when"] == contract["done_when"],
            "Crew admission fields differ"
        );
        let journal = fs::read_to_string(u.journal_path())?;
        for line in journal.lines() {
            let event: Value = serde_json::from_str(line)?;
            ensure!(
                !matches!(event["kind"].as_str(), Some("turn" | "seat.run")),
                "unpaid check started inference"
            );
        }
        write_json(
            &u.root().join("check-verified.json"),
            &json!({"contract":contract,"snapshot":snapshot,"paid_turns":0}),
        )?;
        daemon.stop()?;
        println!(
            "Verified no-go refusal, hook admission, econ explain and Crew snapshot; no paid inference"
        );
        return Ok(());
    }
    let catalogs = drv_agent::catalogs(u.root());
    let config = uke::drv_econ::Config::load(u.root())?;
    let worker = uke::drv_econ::route(
        &config,
        &uke::drv_econ::TaskFacts {
            kind: Some("validate".into()),
            thoroughness: Some("low".into()),
            ..Default::default()
        },
        &Default::default(),
        &catalogs,
        2,
        uke::now_ms(),
    )?;
    let codex = worker.chosen.context("live worker route unavailable")?;
    ensure!(
        codex.harness == "codex",
        "live default worker did not route to Codex"
    );
    write_json(
        &u.root().join("preflight-catalogs.json"),
        &serde_json::to_value(&catalogs)?,
    )?;
    let mut daemon = start(&u, &binary, "native", port, &codex.model)?;
    refuse_no_go(&u, &binary)?;
    captain_go(&u)?;
    let show: Value = serde_json::from_str(&cli(&u, &binary, &["econ", "show"])?)?;
    write_json(&u.root().join("native-show.json"), &show)?;
    let dry: Value = serde_json::from_str(&cli(
        &u,
        &binary,
        &[
            "econ",
            "route",
            "--dry-run",
            "--kind",
            "validate",
            "--thoroughness",
            "low",
        ],
    )?)?;
    ensure!(
        dry["chosen"]["harness"] == "codex" && dry["reasoning"] == "light",
        "CLI dry-run diverged"
    );
    let pid = task(
        &u,
        &binary,
        "light",
        &["--kind", "validate", "--thoroughness", "low"],
        false,
    )?;
    let light = result(&u, pid, "light")?;
    ensure!(
        light["contract"]["econ"]["role"] == "worker"
            && light["actual_effort"] == "low"
            && light["contract"]["econ"]["chosen"]["harness"] == "codex",
        "worker/light mismatch"
    );
    let explain: Value =
        serde_json::from_str(&cli(&u, &binary, &["econ", "explain", &pid.to_string()])?)?;
    ensure!(
        explain == light["contract"]["econ"],
        "explain receipt differs"
    );
    let pid = task(
        &u,
        &binary,
        "judge",
        &["--kind", "decide", "--judgment", "high"],
        false,
    )?;
    let judge = result(&u, pid, "judge")?;
    ensure!(
        judge["contract"]["econ"]["role"] == "judge"
            && judge["contract"]["econ"]["reasoning"] == "deep"
            && judge["contract"]["econ"]["chosen"]["harness"] == "claude",
        "judge/deep mismatch"
    );
    let pid = task(
        &u,
        &binary,
        "override",
        &[
            "--on",
            "codex",
            "--model",
            &codex.model,
            "--effort",
            "medium",
            "--judgment",
            "high",
        ],
        false,
    )?;
    let pinned = result(&u, pid, "override")?;
    ensure!(
        pinned["contract"]["econ"]["override_by"] == 2 && pinned["actual_effort"] == "medium",
        "override provenance mismatch"
    );
    for (label, thoroughness, role, harness, selector, effort, fallback) in [
        (
            "quick",
            "low",
            "research_quick",
            "codex",
            "newest luna",
            "medium",
            false,
        ),
        (
            "full",
            "high",
            "research",
            "claude",
            "newest sonnet",
            "high",
            false,
        ),
    ] {
        let pid = task(
            &u,
            &binary,
            label,
            &["--kind", "research", "--thoroughness", thoroughness],
            false,
        )?;
        let verified = result(&u, pid, label)?;
        let econ = &verified["contract"]["econ"];
        ensure!(
            econ["role"] == role
                && econ["chosen"]["harness"] == harness
                && econ["chosen"]["selector"] == selector
                && econ["chosen"]["effort"] == effort
                && econ["chosen"]["captain_fallback"] == fallback,
            "{label} research route mismatch"
        );
    }
    daemon.stop()?;
    let mut daemon = start(&u, &binary, "fallback", port, &codex.model)?;
    let pid = task(
        &u,
        &binary,
        "fallback",
        &["--kind", "validate", "--thoroughness", "low"],
        false,
    )?;
    let fallback = result(&u, pid, "fallback")?;
    ensure!(
        fallback["contract"]["econ"]["chosen"]["harness"] == "claude"
            && fallback["contract"]["econ"]["skipped"][0]["reason"]
                .as_str()
                .is_some_and(|s| s.contains("catalog")),
        "missing model did not fall back"
    );
    daemon.stop()?;
    for (mode, thoroughness, role, harness, selector, effort, fallback) in [
        (
            "quick-fallback",
            "low",
            "research_quick",
            "claude",
            "newest sonnet",
            "low",
            false,
        ),
        (
            "full-fallback",
            "high",
            "research",
            "codex",
            "newest sol",
            "low",
            true,
        ),
    ] {
        let mut daemon = start(&u, &binary, mode, port, &codex.model)?;
        let pid = task(
            &u,
            &binary,
            mode,
            &["--kind", "research", "--thoroughness", thoroughness],
            false,
        )?;
        let verified = result(&u, pid, mode)?;
        let econ = &verified["contract"]["econ"];
        ensure!(
            econ["role"] == role
                && econ["chosen"]["harness"] == harness
                && econ["chosen"]["selector"] == selector
                && econ["chosen"]["effort"] == effort
                && econ["chosen"]["captain_fallback"] == fallback
                && econ["skipped"].as_array().is_some_and(|s| s.len() == 1),
            "{mode} research route mismatch"
        );
        ensure!(
            econ["reason"]
                .as_str()
                .is_some_and(|s| s.contains("captain fallback") == fallback),
            "{mode} fallback marker mismatch"
        );
        daemon.stop()?;
    }
    let mut daemon = start(&u, &binary, "hold", port, &codex.model)?;
    let pid = task(&u, &binary, "hold", &["--kind", "validate"], true)?;
    let contract = receipt(&u, pid)?;
    ensure!(
        contract["econ"]["chosen"].is_null() && contract["econ"]["reasoning"] == "deep",
        "hold weakened class"
    );
    thread::sleep(Duration::from_secs(2));
    let snapshot = uke::kernel_request(&u, &json!({"op":"snapshot"}), Duration::from_secs(5))?;
    ensure!(
        snapshot["calls"].as_array().context("calls missing")?.len() == 1,
        "hold must raise one call"
    );
    let journal = fs::read_to_string(u.journal_path())?;
    let events: Vec<Value> = journal
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        !events
            .iter()
            .any(|e| e["kind"] == "turn" && e["pid"] == pid),
        "held task started a turn"
    );
    ensure!(
        !u.project_dir("econ-e2e")
            .join(format!("tasks/pid-{pid}/result.json"))
            .exists(),
        "hold fabricated a result"
    );
    write_json(
        &u.root().join("hold-verified.json"),
        &json!({"contract":contract,"snapshot":snapshot,"turns":0}),
    )?;
    daemon.stop()?;
    println!(
        "Verified go admission, worker/light, judge/deep, explicit override, quick/full research and their fallbacks, and hold; sandbox {} port {port}",
        u.root().display()
    );
    Ok(())
}
