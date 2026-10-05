//! The kernel daemon, the plugin's hooks, the seat channel (`ctl`, and `mcp`: one MCP
//! tool `unvrs(command)`), and the Observatory link. Wiring only: behaviour lives
//! behind the `uke`, `drv_hdff`, `drv_agent`, `mapp_unvrs` and `observatory` front doors.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    env,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use uke::{BriefFold, FoldJob, TurnRequest, TurnResult, Universe};

struct Drivers {
    summaries: drv_hdff::SubscriptionSummaries,
}
impl uke::Drivers for Drivers {
    fn fold(&self, job: &FoldJob) -> Result<BriefFold> {
        self.summaries.fold(job)
    }
    fn recovery_summary(&self, prompt: &str) -> Result<String> {
        self.summaries.summarize(prompt)
    }
    fn check_summaries(&self) {
        self.summaries.preflight();
    }
    fn diagnostics(&self) -> Vec<Value> {
        self.summaries.events()
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
    fn quota(&self) -> Vec<Value> {
        if env::var("UNVRS_QUOTA").is_ok_and(|v| v == "0") {
            return vec![];
        }
        match drv_agent::codex_quota() {
            Ok(row) => vec![row],
            Err(e) => vec![json!({"harness":"codex","account":"default","error":format!("{e:#}")})],
        }
    }
    fn harness_ready(&self, harness: &str) -> Result<()> {
        drv_agent::harness_ready(harness)
    }
    fn catalogs(&self, home: &Path) -> std::collections::BTreeMap<String, uke::drv_econ::Catalog> {
        drv_agent::catalogs(home)
    }
}

fn exe() -> Result<PathBuf> {
    Ok(env::current_exe()?.canonicalize()?)
}
/// The binary hooks, MCP and the daemon use: `<home>/bin/unvrs` when installed.
fn stable_exe(u: &Universe) -> Result<PathBuf> {
    let stable = u.root().join("bin/unvrs");
    if stable.exists() { Ok(stable) } else { exe() }
}

fn flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 < args.len() {
        let v = args.remove(i + 1);
        args.remove(i);
        Some(v)
    } else {
        args.remove(i);
        None
    }
}

fn observatory_addr() -> Option<std::net::SocketAddr> {
    let port = env::var("UNVRS_OBSERVATORY_PORT").unwrap_or_else(|_| "7576".into());
    if port == "off" || port == "0" {
        return None;
    }
    port.parse::<u16>()
        .ok()
        .map(|p| std::net::SocketAddr::from(([127, 0, 0, 1], p)))
}

pub fn kernel(mut args: Vec<String>) -> Result<()> {
    let sub = if args.is_empty() {
        "status".to_owned()
    } else {
        args.remove(0)
    };
    let u = Universe::home()?;
    match sub.as_str() {
        "run" => {
            let start: Option<uke::ObservatoryStart> = observatory_addr().map(|addr| {
                Box::new(
                    move |snap: uke::SnapshotFn, settings: uke::SettingsFn, token: String| {
                        std::thread::spawn(move || {
                            if let Err(e) =
                                observatory::serve_with_settings(addr, snap, settings, token)
                            {
                                eprintln!("observatory: {e:#} (the kernel keeps running)");
                            }
                        });
                    },
                ) as uke::ObservatoryStart
            });
            uke::serve_kernel(
                u,
                Arc::new(Drivers {
                    summaries: Default::default(),
                }),
                Arc::new(mapp_unvrs::UnvrsMapp),
                start,
            )
        }
        "start" => {
            uke::ensure_running(&u, &stable_exe(&u)?, Duration::from_secs(8))?;
            println!(
                "kernel: running\nhome: {}\nsocket: {}",
                u.root().display(),
                u.socket_path().display()
            );
            Ok(())
        }
        "status" => {
            if !uke::kernel_running(&u) {
                println!(
                    "kernel: not running\nhome: {}\nhelp: the LaunchAgent keeps it up after `unvrs install`; hooks and `unvrs kernel start` start it on demand",
                    u.root().display()
                );
                return Ok(());
            }
            let v = uke::kernel_request(&u, &json!({"op": "status"}), Duration::from_secs(5))?;
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&v["snapshot"])?);
            } else {
                print!("{}", v["text"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        "snapshot" => {
            let v = uke::kernel_request(&u, &json!({"op": "snapshot"}), Duration::from_secs(5))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        "stop" => {
            if uke::kernel_running(&u) {
                uke::kernel_request(&u, &json!({"op": "stop"}), Duration::from_secs(5))?;
                println!("kernel: stopped");
            } else {
                println!("kernel: not running");
            }
            Ok(())
        }
        "hot" => {
            let pid = flag(&mut args, "--pid").context("usage: unvrs kernel hot --pid <n>")?;
            let v = uke::kernel_request(
                &u,
                &json!({"op": "hot", "pid": pid.parse::<u64>()?}),
                Duration::from_secs(5),
            )?;
            print!("{}", v["text"].as_str().unwrap_or(""));
            Ok(())
        }
        _ => bail!("usage: unvrs kernel status|start|stop|run|hot|snapshot"),
    }
}

/// Plugin hook. Always exits 0; prints nothing unless the kernel answered. A thread the
/// kernel does not know costs one file check: no kernel call, no output (D61).
pub fn hook(args: Vec<String>) -> ! {
    if args.first().is_some_and(|a| a == "Watch") {
        let code = std::panic::catch_unwind(|| watch(args[1..].to_vec())).unwrap_or(0);
        std::process::exit(code)
    }
    let result = std::panic::catch_unwind(|| hook_inner(args));
    if let Ok(Err(e)) = &result
        && let Ok(home) = Universe::home_path()
    {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.join("kernel/hook-errors.log"))
            .and_then(|mut f| writeln!(f, "{} {e:#}", uke::now_ms()));
    }
    std::process::exit(0)
}

fn hook_inner(mut args: Vec<String>) -> Result<()> {
    std::panic::set_hook(Box::new(|_| {}));
    if env::var_os("UNVRS_DRIVEN_PID").is_some() {
        return Ok(()); // driven runs are driven by the kernel, not by their hooks
    }
    let event = if args.is_empty() {
        String::new()
    } else {
        args.remove(0)
    };
    let harness = flag(&mut args, "--harness").unwrap_or_default();
    let input = read_stdin();
    let mut payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let session = payload["session_id"].as_str().unwrap_or("").to_owned();
    let home = Universe::home_path()?;
    let command = event == "UserPromptSubmit"
        && uke::parse_captain(payload["prompt"].as_str().unwrap_or("")).is_some();
    if !command {
        // Fast path: only threads the kernel knows have a marker.
        if !marked(&home, &harness, &session) {
            return Ok(());
        }
    }
    let u = Universe::home()?;
    let wait = match event.as_str() {
        "SessionStart" | "UserPromptSubmit" => 10,
        "SessionEnd" => 2,
        _ => 6,
    };
    uke::ensure_running(&u, &stable_exe(&u)?, Duration::from_secs(wait.min(8)))?;
    if let Some(m) = payload.get_mut("last_assistant_message")
        && let Some(s) = m.as_str()
        && s.len() > 200_000
    {
        *m = json!(s.chars().take(200_000).collect::<String>());
    }
    let reply = uke::kernel_request(
        &u,
        &json!({"op": "hook", "event": event, "harness": harness, "payload": payload}),
        Duration::from_secs(wait),
    )?;
    let context = reply["context"].as_str().filter(|s| !s.is_empty());
    let system = reply["system"].as_str().filter(|s| !s.is_empty());
    let block = reply["block"].as_str().filter(|s| !s.is_empty());
    let stop = reply["stop"].as_str().filter(|s| !s.is_empty());
    let out = match event.as_str() {
        "UserPromptSubmit" if block.is_some() => json!({"decision": "block", "reason": block}),
        "Stop" if stop.is_some() => json!({"decision": "block", "reason": stop}),
        "SessionStart" | "UserPromptSubmit" if context.is_some() || system.is_some() => {
            let mut v = json!({});
            if let Some(c) = context {
                v["hookSpecificOutput"] = json!({"hookEventName": event, "additionalContext": c});
            }
            if let Some(s) = system {
                v["systemMessage"] = json!(s);
            }
            v
        }
        _ => return Ok(()),
    };
    println!("{out}");
    Ok(())
}

fn read_stdin() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin()
            .take(8 * 1024 * 1024)
            .read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap_or_default()
}

/// A thread the kernel knows has a marker file.
fn marked(home: &std::path::Path, harness: &str, session: &str) -> bool {
    let clean: String = format!("{harness}-{session}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    !session.is_empty() && home.join("kernel/threads").join(clean).exists()
}

/// `unvrs hook Watch`: Claude Code's `asyncRewake` hook (G-L1push). Waits in the
/// background for the seat's wakes while its thread is idle, then prints them and exits
/// 2, which wakes the model. Exits 0 (no wake) for driven runs, unknown threads, a
/// newer watcher, or when its time is up. In `claude -p` without streaming input Claude
/// Code runs this hook in the foreground; those runs are driven, so it returns at once.
fn watch(mut args: Vec<String>) -> i32 {
    std::panic::set_hook(Box::new(|_| {}));
    if env::var_os("UNVRS_DRIVEN_PID").is_some() {
        return 0;
    }
    let harness = flag(&mut args, "--harness").unwrap_or_default();
    let secs: u64 = flag(&mut args, "--for")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    let payload: Value = serde_json::from_str(&read_stdin()).unwrap_or(Value::Null);
    let session = payload["session_id"].as_str().unwrap_or("").to_owned();
    let (Ok(home), Ok(u)) = (Universe::home_path(), Universe::home()) else {
        return 0;
    };
    if !marked(&home, &harness, &session) {
        return 0;
    }
    let until = std::time::Instant::now() + Duration::from_secs(secs);
    let mut arm = true;
    while std::time::Instant::now() < until {
        let reply = uke::kernel_request(
            &u,
            &json!({"op": "watch", "harness": harness, "session": session,
                "watcher": std::process::id(), "arm": arm}),
            Duration::from_secs(5),
        );
        if let Ok(v) = reply {
            arm = false;
            if v["done"] == true {
                return 0;
            }
            if let Some(text) = v["wake"].as_str() {
                eprintln!("{text}");
                return 2;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    0
}

/// The harness thread a model's shell call comes from (the harness sets these).
fn env_hint() -> Value {
    if let Ok(t) = env::var("CODEX_THREAD_ID") {
        return json!({"harness": "codex", "session": t});
    }
    if let Ok(s) = env::var("CLAUDE_CODE_SESSION_ID") {
        return json!({"harness": "claude", "session": s});
    }
    Value::Null
}

fn call(argv: Vec<String>, hint: Value, via: &str) -> Result<String> {
    let u = Universe::home()?;
    uke::ensure_running(&u, &stable_exe(&u)?, Duration::from_secs(8))?;
    let v = uke::kernel_request(
        &u,
        &json!({"op": "ctl", "argv": argv, "hint": hint, "via": via}),
        Duration::from_secs(60),
    )?;
    Ok(v["text"].as_str().unwrap_or("").to_owned())
}

/// `unvrs ctl <command>`: seat operations for driven runs and scripts; the captain's
/// terminal may also run the captain-only ones.
pub fn ctl(args: &[String]) -> Result<()> {
    let text = call(args.to_vec(), env_hint(), "ctl").map_err(|e| anyhow::anyhow!("{e:#}"))?;
    println!("{}", text.trim_end());
    Ok(())
}

/// Typed settings ops preserve the caller's harness hint on the Unix socket.
pub fn settings(args: &[String]) -> Result<()> {
    let mut req = uke::settings::command(args)?;
    req["hint"] = env_hint();
    req["via"] = json!("cli");
    let u = Universe::home()?;
    uke::ensure_running(&u, &stable_exe(&u)?, Duration::from_secs(8))?;
    let value = uke::kernel_request(&u, &req, Duration::from_secs(60))?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// `unvrs mcp`: a stdio MCP server with one tool, `unvrs(command)`, mirroring
/// `unvrs ctl`. It refuses in threads that are not UNVRS seats.
pub fn mcp() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue; // notifications
        };
        let method = msg["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "unvrs", "version": env!("CARGO_PKG_VERSION")},
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": [{
                "name": "unvrs",
                "description": mapp_unvrs::TOOL_DESCRIPTION,
                "inputSchema": {"type": "object", "properties": {"command": {"type": "string", "description": "An UNVRS seat command, e.g. `help`, `ctx search \"brand voice\"`"}}, "required": ["command"]},
            }]}),
            "tools/call" => {
                let p = &msg["params"];
                let command = p["arguments"]["command"].as_str().unwrap_or("").to_owned();
                let meta = &p["_meta"];
                let thread = meta["threadId"]
                    .as_str()
                    .or_else(|| meta["x-codex-turn-metadata"]["thread_id"].as_str());
                let hint = match thread {
                    Some(t) => json!({"harness": "codex", "session": t}),
                    None => env_hint(),
                };
                match uke::shell_words(&command).and_then(|argv| call(argv, hint, "mcp")) {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                    Err(e) => {
                        json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})
                    }
                }
            }
            _ => {
                let err = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Unknown method {method}")}});
                writeln!(out, "{err}")?;
                out.flush()?;
                continue;
            }
        };
        writeln!(
            out,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )?;
        out.flush()?;
    }
    Ok(())
}

/// `unvrs observe`: the Observatory link (the kernel serves it).
pub fn observe(args: Vec<String>) -> Result<()> {
    let port = env::var("UNVRS_OBSERVATORY_PORT").unwrap_or_else(|_| "7576".into());
    if args.iter().any(|a| a == "--short-url") {
        return short_url(args.iter().any(|a| a == "off"));
    }
    let u = Universe::home()?;
    let running = uke::kernel_running(&u);
    println!(
        "observatory: http://unvrs.localhost:{port}\nkernel: {}\nloopback only; settings writes require page authorization\nSafari and other browsers without *.localhost: unvrs observe --short-url",
        if running {
            "running"
        } else {
            "not running (unvrs kernel start)"
        }
    );
    Ok(())
}

const PF_ANCHOR: &str = "/etc/pf.anchors/dev.unvrs";
const HOSTS_MARK: &str = "# unvrs observatory";

/// Opt-in `http://unvrs.localhost` for every browser: a hosts entry and a loopback-only
/// pf redirect 80 → 7576 (one sudo; `unvrs uninstall` or `--short-url off` undoes it).
fn short_url(off: bool) -> Result<()> {
    let script = if off {
        format!(
            "sed -i '' '/{HOSTS_MARK}/d' /etc/hosts; rm -f {PF_ANCHOR}; pfctl -a com.apple/unvrs -F all 2>/dev/null; true"
        )
    } else {
        format!(
            "grep -q '{HOSTS_MARK}' /etc/hosts || echo '127.0.0.1 unvrs.localhost {HOSTS_MARK}' >> /etc/hosts; \
echo 'rdr pass on lo0 inet proto tcp from any to 127.0.0.1 port 80 -> 127.0.0.1 port 7576' > {PF_ANCHOR}; \
pfctl -a com.apple/unvrs -f {PF_ANCHOR} && pfctl -E"
        )
    };
    println!(
        "UNVRS needs sudo once to {}:\n  sudo sh -c \"{script}\"",
        if off {
            "remove the short URL"
        } else {
            "add http://unvrs.localhost (hosts entry + loopback 80 → 7576)"
        }
    );
    let status = std::process::Command::new("sudo")
        .args(["sh", "-c", &script])
        .status()?;
    anyhow::ensure!(status.success(), "sudo step failed");
    println!("done");
    Ok(())
}

/// Resolves `path` for the ctl `sources add` convenience.
pub fn sources(args: &[String]) -> Result<()> {
    let mut argv = vec!["sources".to_owned()];
    argv.extend(args.iter().cloned());
    if let Some(i) = argv.iter().position(|a| a == "add")
        && let Some(uri) = argv.get(i + 3).cloned()
        && Path::new(&uri).exists()
    {
        argv[i + 3] = Path::new(&uri).canonicalize()?.display().to_string();
    }
    ctl(&argv)
}

/// `unvrs seed import <dir>`: the captain's context seed (captain's terminal only; the
/// kernel refuses a model). The folder is resolved here, read by the kernel.
pub fn seed(args: &[String]) -> Result<()> {
    let [sub, dir] = args else {
        bail!("usage: unvrs seed import <dir>");
    };
    anyhow::ensure!(sub == "import", "usage: unvrs seed import <dir>");
    let dir = Path::new(dir)
        .canonicalize()
        .with_context(|| format!("No seed folder at {dir}"))?;
    ctl(&[
        "seed".to_owned(),
        "import".to_owned(),
        dir.display().to_string(),
    ])
}
