//! Drive channel for driven seats in headless mode: `claude -p` and `codex exec`,
//! one turn per process, resumed by harness session id.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uke::signals::CommandTracking;
use uke::{TurnRequest, TurnResult};

/// A turn whose harness is silent this long is hung (UNVRS_TURN_IDLE_SECS overrides).
pub const TURN_IDLE: Duration = Duration::from_secs(1200);
/// A runaway ceiling on one turn however busy it is (UNVRS_TURN_MAX_SECS overrides).
pub const TURN_MAX: Duration = Duration::from_secs(4 * 3600);

/// How long one turn may run: it ends after `idle` without output, or at `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnBudget {
    pub idle: Duration,
    pub max: Duration,
}

impl TurnBudget {
    pub fn from_env() -> Self {
        let secs = |name: &str, default: Duration| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
                .map_or(default, Duration::from_secs)
        };
        Self {
            idle: secs("UNVRS_TURN_IDLE_SECS", TURN_IDLE),
            max: secs("UNVRS_TURN_MAX_SECS", TURN_MAX),
        }
    }
    /// One wall-clock budget (summaries).
    pub fn fixed(d: Duration) -> Self {
        Self { idle: d, max: d }
    }
}

/// A running turn's clock: output pushes the idle deadline, never past the ceiling.
pub(super) struct TurnClock {
    budget: TurnBudget,
    started: Instant,
    last: Instant,
}

impl TurnClock {
    pub(super) fn new(budget: TurnBudget) -> Self {
        let now = Instant::now();
        Self {
            budget,
            started: now,
            last: now,
        }
    }
    pub(super) fn touch(&mut self) {
        self.last = Instant::now();
    }
    pub(super) fn deadline(&self) -> Instant {
        (self.last + self.budget.idle).min(self.started + self.budget.max)
    }
    /// None while the turn may go on, else why it timed out.
    pub(super) fn expired(&self, harness: &str) -> Option<String> {
        let now = Instant::now();
        if now >= self.started + self.budget.max {
            Some(format!(
                "{harness} turn timed out at its {}s ceiling",
                self.budget.max.as_secs()
            ))
        } else if now >= self.last + self.budget.idle {
            Some(format!(
                "{harness} turn timed out after {}s without output",
                self.budget.idle.as_secs()
            ))
        } else {
            None
        }
    }
}

pub(super) fn quota_text(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "usage limit",
        "rate limit",
        "quota",
        "429",
        "credit balance",
        "out of credits",
    ]
    .iter()
    .any(|k| t.contains(k))
}

// Tool inputs can contain opaque credentials. Persist only the tool's public name.
pub(super) fn tool_label(name: &str, _input: &Value) -> String {
    name.chars().take(100).collect()
}

/// Runs one turn and returns the final text, tools used and the harness session.
pub fn run_turn(req: &TurnRequest, started: &dyn Fn(u32)) -> Result<TurnResult> {
    run(req, started, &|_| {}, false, TurnBudget::from_env(), None)
}

pub fn run_turn_observed(
    req: &TurnRequest,
    started: &dyn Fn(u32),
    event: &dyn Fn(&Value),
) -> Result<TurnResult> {
    run(req, started, event, false, TurnBudget::from_env(), None)
}

// Shared tracked child handles every return, group cleanup and reap.
pub(super) struct Child(pub(super) uke::signals::TrackedChild);
impl std::ops::Deref for Child {
    type Target = uke::signals::TrackedChild;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn subscription_env(cmd: &mut Command) {
    for key in [
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "OPENAI_FEDERATION_RULE_ID",
        "OPENAI_IDENTITY_TOKEN_FILE",
        "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
        "OPENAI_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDECODE",
    ] {
        cmd.env_remove(key);
    }
}

fn request_scope(req: &TurnRequest) -> Option<uke::signals::OwnerScope> {
    req.env
        .iter()
        .find(|(k, _)| k == "UNVRS_HOME")
        .map(|(_, home)| {
            let owner = req
                .env
                .iter()
                .find(|(k, _)| k == "UNVRS_DRIVEN_PID")
                .and_then(|(_, p)| p.parse().ok())
                .unwrap_or(0);
            uke::signals::owner_scope(Path::new(home), owner)
        })
}

fn codex_summary_auth(req: &TurnRequest) -> Result<()> {
    let mut cmd = Command::new(std::env::var("UNVRS_CODEX_BIN").unwrap_or_else(|_| "codex".into()));
    cmd.args([
        "login",
        "status",
        "-c",
        "cli_auth_credentials_store=\"file\"",
    ])
    .current_dir(&req.cwd)
    .envs(req.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .process_group(0);
    subscription_env(&mut cmd);
    let mut child = Child(
        cmd.spawn_owned()
            .context("Codex auth-mode status unavailable")?,
    );
    let stdout = child.stdout.take().context("Codex status stdout")?;
    let stderr = child.stderr.take().context("Codex status stderr")?;
    let stdout = thread::spawn(move || {
        let mut s = String::new();
        stdout.take(65536).read_to_string(&mut s).map(|_| ())
    });
    let stderr = thread::spawn(move || {
        let mut s = String::new();
        stderr.take(65536).read_to_string(&mut s).map(|_| s)
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "Codex auth-mode status timed out"
        );
        thread::sleep(Duration::from_millis(25));
    };
    stdout
        .join()
        .map_err(|_| anyhow::anyhow!("Codex status reader failed"))??;
    let mode = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("Codex status reader failed"))??;
    // Other status modes can print key fragments. Never include captured text in
    // an error; this gate only accepts the exact non-secret ChatGPT status line.
    ensure!(
        status.success() && mode.trim() == "Logged in using ChatGPT",
        "Codex summary requires an existing ChatGPT auth mode; login status did not confirm it (no login changes made)"
    );
    Ok(())
}

pub fn run_summary(req: &TurnRequest) -> Result<String> {
    run_summary_with_schema(req, None)
}

/// Codex supports a final-response JSON schema; other harnesses retain parser validation.
pub fn run_summary_with_schema(req: &TurnRequest, schema: Option<&Path>) -> Result<String> {
    let _owner = request_scope(req);
    ensure!(
        req.session.is_none(),
        "A subscription summary needs a fresh worker"
    );
    if req.harness == "codex" {
        codex_summary_auth(req)?;
    }
    if req.harness == "claude" {
        let mut cmd =
            Command::new(std::env::var("UNVRS_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()));
        cmd.args(["auth", "status", "--json"])
            .envs(req.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        subscription_env(&mut cmd);
        let mut child = Child(
            cmd.spawn_owned()
                .context("Claude subscription auth check unavailable")?,
        );
        let stdout = child.stdout.take().context("auth stdout")?;
        let reader = thread::spawn(move || {
            let mut s = String::new();
            stdout.take(65536).read_to_string(&mut s).map(|_| s)
        });
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(
                    status.success(),
                    "Claude subscription auth check failed: {status}"
                );
                break;
            }
            ensure!(
                Instant::now() < until,
                "Claude subscription auth check timed out"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let auth: Value = serde_json::from_str(
            &reader
                .join()
                .map_err(|_| anyhow::anyhow!("auth reader failed"))??,
        )?;
        ensure!(
            auth["loggedIn"] == true
                && auth["authMethod"] == "claude.ai"
                && auth["subscriptionType"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
            "Claude summary requires a claude.ai subscription login"
        );
    }
    let out = run(
        req,
        &|_| {},
        &|_| {},
        true,
        TurnBudget::fixed(Duration::from_secs(60)),
        schema,
    )?;
    if let Some(error) = out.error {
        bail!("{error}");
    }
    ensure!(!out.quota, "Summary subscription quota unavailable");
    ensure!(!out.text.trim().is_empty(), "Summary returned no text");
    ensure!(out.tools.is_empty(), "Summary unexpectedly used tools");
    Ok(out.text)
}

fn run(
    req: &TurnRequest,
    started: &dyn Fn(u32),
    event: &dyn Fn(&Value),
    summary: bool,
    budget: TurnBudget,
    schema: Option<&Path>,
) -> Result<TurnResult> {
    let _owner = request_scope(req);
    if req.harness == "codex" && !summary {
        return crate::codex::run(req, started, event, budget);
    }
    let mut cmd = match req.harness.as_str() {
        "claude" => {
            let mut c =
                Command::new(std::env::var("UNVRS_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()));
            c.args(["-p", "--output-format", "stream-json", "--verbose"]);
            if summary {
                c.args([
                    "--permission-mode",
                    "dontAsk",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--mcp-config",
                    "{\"mcpServers\":{}}",
                    "--safe-mode",
                    "--no-session-persistence",
                ]);
            } else {
                c.args([
                    "--include-partial-messages",
                    "--permission-mode",
                    "bypassPermissions",
                    "--disallowed-tools",
                    "Agent,Task",
                ]);
            }
            if let Some(m) = &req.model {
                c.args(["--model", m]);
            }
            if let Some(e) = &req.effort {
                c.args(["--effort", e]);
            }
            if let Some(s) = &req.session {
                c.args(["--resume", s]);
            }
            c
        }
        "codex" => {
            // `codex exec --json` reports neither the model's reasoning effort nor a
            // config echo, so a pinned effort could never be verified: refuse it.
            if let Some(e) = &req.effort {
                bail!(
                    "effort {e} is pinned, but codex does not report the reasoning effort it ran, so it cannot be verified; refused before starting codex (pin effort on claude, or drop --effort)"
                );
            }
            let mut c =
                Command::new(std::env::var("UNVRS_CODEX_BIN").unwrap_or_else(|_| "codex".into()));
            c.args(["exec", "--json", "--skip-git-repo-check"]);
            if let Some(schema) = schema {
                c.arg("--output-schema").arg(schema);
            }
            if summary {
                c.args([
                    "--sandbox",
                    "read-only",
                    "--ephemeral",
                    "--ignore-user-config",
                    "--ignore-rules",
                    "-c",
                    "model_provider=\"openai\"",
                    "-c",
                    "cli_auth_credentials_store=\"file\"",
                    "-c",
                    "features.shell_tool=false",
                    "-c",
                    "features.hooks=false",
                    "-c",
                    "features.apps=false",
                    "-c",
                    "features.skip_host_skill_discovery=true",
                    "-c",
                    "suppress_unstable_features_warning=true",
                    "-c",
                    "agents.enabled=false",
                    "-c",
                    "features.multi_agent=false",
                    "-c",
                    "features.multi_agent_v2=false",
                    "-c",
                    "web_search=\"disabled\"",
                    "-c",
                    "project_doc_max_bytes=0",
                ]);
            } else {
                c.arg("--dangerously-bypass-approvals-and-sandbox");
            }
            if let Some(m) = &req.model {
                c.args(["-m", m]);
            }
            match &req.session {
                Some(s) => {
                    c.args(["resume", s, "-"]);
                }
                None => {
                    c.arg("-C").arg(&req.cwd).arg("-");
                }
            }
            c
        }
        other => bail!("No headless driver for {other}"),
    };
    cmd.current_dir(&req.cwd)
        .envs(req.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env_remove("UNVRS_SOCKET")
        .env_remove("UNVRS_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(e) = &req.effort {
        // CLAUDE_CODE_EFFORT_LEVEL overrides --effort inside claude: set it to the pin
        // so an inherited value can never downgrade the flag.
        cmd.env("CLAUDE_CODE_EFFORT_LEVEL", e);
    }
    if summary {
        subscription_env(&mut cmd);
    }
    let mut child = Child(
        cmd.spawn_owned()
            .with_context(|| format!("{} is not installed or not on PATH", req.harness))?,
    );
    started(child.id());
    let mut out = TurnResult::default();
    // A pinned effort is checked on the live process before the prompt is written,
    // so a CPU that did not get it never sees the task.
    if let Some(e) = &req.effort {
        match check_launch_effort(child.id(), e) {
            Ok(_) => {
                out.effort_evidence = Some(format!(
                    "launch --effort {e} verified; env CLAUDE_CODE_EFFORT_LEVEL={e}"
                ))
            }
            Err(err) => {
                let _ = child.signal_group(libc::SIGKILL);
                let _ = child.wait();
                return Err(err);
            }
        }
    }
    let mut stdin = child.stdin.take().context("stdin")?;
    let prompt = req.prompt.clone();
    let input_writer = thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    let stderr = child.stderr.take().context("stderr")?;
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let mut reader = BufReader::new(stderr);
        let mut buf = [0; 4096];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if s.len() < 64 * 1024 {
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        Ok::<_, std::io::Error>(s)
    });
    let stdout = child.stdout.take().context("stdout")?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<String>>(1);
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut texts: Vec<String> = vec![];
    let mut clock = TurnClock::new(budget);
    loop {
        if let Some(why) = clock.expired(&req.harness) {
            bail!(why);
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                clock.touch();
                let line = line.context("Driver stdout read failed")?;
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                event(&Value::Null);
                if req.harness == "claude" {
                    // The model claude runs: `system/init` first, then every assistant
                    // message. Checked before that message's tool calls are even read.
                    let ran = match v["type"].as_str() {
                        Some("system") => v["model"].as_str(),
                        Some("assistant") => v["message"]["model"].as_str(),
                        _ => None,
                    }
                    .filter(|m| *m != "<synthetic>");
                    if let Some(ran) = ran {
                        out.model = Some(ran.into());
                        if let Err(e) = check_model(req.model.as_deref(), ran) {
                            let _ = child.signal_group(libc::SIGKILL);
                            let _ = child.wait();
                            return Err(e);
                        }
                    }
                    if let Some(asked) = &req.effort
                        && out.effort.is_none()
                    {
                        match check_init_effort(asked, &v) {
                            Ok(Some(evidence)) => {
                                out.effort = Some(asked.clone());
                                let launch = out.effort_evidence.take().unwrap_or_default();
                                out.effort_evidence = Some(format!("{launch}; {evidence}"));
                            }
                            Ok(None) => {}
                            Err(e) => {
                                let _ = child.signal_group(libc::SIGKILL);
                                let _ = child.wait();
                                return Err(e);
                            }
                        }
                    }
                    if !summary && v["type"] == "system" && v["subtype"] == "init" {
                        let tools = v["tools"].as_array().context("native delegation restriction not confirmed: Claude init tool list missing")?;
                        ensure!(
                            !tools
                                .iter()
                                .any(|t| matches!(t.as_str(), Some("Agent" | "Task"))),
                            "native delegation restriction not applied: Claude init includes Agent or Task; stopped before tools"
                        );
                    }
                    match v["type"].as_str().unwrap_or("") {
                        "stream_event" if v["event"]["type"] == "content_block_delta" => {
                            // Tool-input and thinking deltas can contain secrets; record liveness only.
                            event(&serde_json::json!({"kind":"delta", "session":out.session}));
                        }
                        "system" => {
                            if let Some(s) = v["session_id"].as_str() {
                                out.session = Some(s.into());
                            }
                            if v["subtype"] == "init" {
                                event(
                                    &serde_json::json!({"kind": "accepted", "session": out.session, "model": out.model,
                                    "effort": out.effort, "effort_evidence": out.effort_evidence, "native_delegation": false}),
                                );
                            }
                        }
                        "rate_limit_event" => {
                            out.rate = Some(claude_rate(&v));
                        }
                        "assistant" => {
                            for c in v["message"]["content"].as_array().into_iter().flatten() {
                                match c["type"].as_str() {
                                    Some("text") => {
                                        let text = c["text"].as_str().unwrap_or("");
                                        event(
                                            &serde_json::json!({"kind": "progress", "session": out.session,
                                            "text": text.chars().take(400).collect::<String>()}),
                                        );
                                        texts.push(text.into())
                                    }
                                    Some("tool_use") => {
                                        let name = c["name"].as_str().unwrap_or("tool");
                                        ensure!(
                                            !matches!(name, "Agent" | "Task"),
                                            "native delegation restriction violated by Claude"
                                        );
                                        event(
                                            &serde_json::json!({"kind": "tool.started", "session": out.session, "tool": name}),
                                        );
                                        out.tools.push(tool_label(name, &c["input"]));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "user" => {
                            for c in v["message"]["content"].as_array().into_iter().flatten() {
                                if c["type"] == "tool_result" {
                                    event(
                                        &serde_json::json!({"kind": "tool.completed", "session": out.session, "error": c["is_error"]}),
                                    );
                                }
                            }
                        }
                        "result" => {
                            if let Some(s) = v["session_id"].as_str() {
                                out.session = Some(s.into());
                            }
                            if let Some(r) = v["result"].as_str() {
                                out.text = r.into();
                            }
                            if v["is_error"] == true {
                                let msg = format!("{} {}", v["result"], v["api_error_status"]);
                                out.quota = v["api_error_status"] == 429 || quota_text(&msg);
                                out.error = Some(msg);
                            }
                            event(
                                &serde_json::json!({"kind": "turn.completed", "session": out.session, "error": out.error}),
                            );
                        }
                        _ => {}
                    }
                } else {
                    if summary
                        && matches!(
                            v["type"].as_str(),
                            Some("item.started" | "item.updated" | "item.completed")
                        )
                    {
                        let kind = v["item"]["type"].as_str().unwrap_or("unknown");
                        ensure!(
                            matches!(kind, "agent_message" | "reasoning"),
                            "Summary rejected unexpected item {kind}: {}",
                            v["item"]["message"]
                                .as_str()
                                .unwrap_or("tools are disabled")
                        );
                    }
                    match v["type"].as_str().unwrap_or("") {
                        "thread.started" => {
                            out.session = v["thread_id"].as_str().map(str::to_owned)
                        }
                        "item.completed" => {
                            let item = &v["item"];
                            match item["type"].as_str() {
                                Some("agent_message") => {
                                    // A summary is the final response, not progress concatenated
                                    // with a JSON document. All items still pass the no-tools gate.
                                    texts.clear();
                                    texts.push(item["text"].as_str().unwrap_or("").into())
                                }
                                Some("command_execution") => out.tools.push(format!(
                                    "shell: {}",
                                    item["command"]
                                        .as_str()
                                        .unwrap_or("")
                                        .chars()
                                        .take(200)
                                        .collect::<String>()
                                )),
                                Some("file_change") => out.tools.push("edit: file change".into()),
                                _ => {}
                            }
                        }
                        "turn.failed" | "error" => {
                            let msg = v["error"]["message"]
                                .as_str()
                                .or_else(|| v["message"].as_str())
                                .unwrap_or("codex error")
                                .to_owned();
                            out.quota |= quota_text(&msg);
                            out.error = Some(msg);
                        }
                        _ => {}
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(why) = clock.expired(&req.harness) {
                    let _ = child.signal_group(libc::SIGTERM);
                    bail!(why);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        ensure!(
            Instant::now() < clock.deadline(),
            "{} turn timed out after stdout closed",
            req.harness
        );
        thread::sleep(Duration::from_millis(25));
    };
    let stderr = err_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader failed"))?
        .context("Driver stderr read failed")?;
    if out.text.is_empty() {
        out.text = texts.join("\n\n");
    }
    if !status.success() {
        let msg = format!(
            "{} exited with {status}: {}",
            req.harness,
            [out.error.as_deref().unwrap_or(""), stderr.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        );
        out.quota |= quota_text(&msg);
        if out.quota {
            out.error = Some(msg);
            return Ok(out);
        }
        bail!(msg);
    }
    input_writer
        .join()
        .map_err(|_| anyhow::anyhow!("Driver stdin writer failed"))?
        .context("Driver prompt write failed")?;
    Ok(out)
}

/// A requested model must be the model the harness ran, compared exactly: aliases
/// (`opus`) and variants (`claude-opus-5-5[1m]`) are different requests, so pin the id
/// the harness reports. Nothing requested → nothing to enforce (the harness default).
pub(super) fn check_model(requested: Option<&str>, ran: &str) -> Result<()> {
    match requested {
        Some(asked) if asked != ran => bail!(
            "model mismatch: requested {asked}, the harness reported {ran}; stopped before any tool ran (no substitution)"
        ),
        _ => Ok(()),
    }
}

/// The live CPU's argv must carry `--effort <asked>` (read from the OS, not from what
/// the driver meant to pass). Returns the argv as evidence.
fn check_launch_effort(os_pid: u32, asked: &str) -> Result<String> {
    let argv = live_argv(os_pid).with_context(|| format!(
        "effort not confirmed: requested {asked}, but live harness arguments could not be read; stopped before the task was sent (no silent downgrade)"
    ))?;
    if argv_has_effort(&argv, asked) {
        Ok(argv)
    } else {
        bail!(
            "effort not confirmed: requested {asked}, but the started process does not carry --effort {asked}; stopped before the task was sent (no silent downgrade)"
        )
    }
}

fn live_argv(os_pid: u32) -> Result<String> {
    let pid = i32::try_from(os_pid)?;
    ensure!(pid > 1, "invalid harness PID");
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline"))?;
        Ok(String::from_utf8_lossy(&bytes).replace('\0', " "))
    }
    #[cfg(target_os = "macos")]
    {
        // Read the same kernel argument buffer as ps, without spawning setuid ps.
        let limit = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
        ensure!(limit > 0, "invalid process argument limit");
        let mut bytes = vec![0u8; usize::try_from(limit)?];
        let mut size = bytes.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as u32,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error().into());
        }
        macos_argv(&bytes[..size])
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_argv(bytes: &[u8]) -> Result<String> {
    let header = bytes.get(..4).context("missing argument count")?;
    let argc = i32::from_ne_bytes(header.try_into()?);
    ensure!(argc > 0, "invalid argument count");
    let bytes = &bytes[4..];
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .context("missing executable terminator")?;
    let bytes = &bytes[end..];
    let start = bytes
        .iter()
        .position(|b| *b != 0)
        .context("missing arguments")?;
    let mut fields = bytes[start..].split_inclusive(|b| *b == 0);
    let mut args = Vec::new();
    for _ in 0..argc {
        let field = fields.next().context("missing argument")?;
        ensure!(field.last() == Some(&0), "unterminated argument");
        args.push(String::from_utf8_lossy(&field[..field.len() - 1]).into_owned());
    }
    Ok(args.join(" "))
}

fn argv_has_effort(argv: &str, asked: &str) -> bool {
    let words: Vec<&str> = argv.split_whitespace().collect();
    words
        .windows(2)
        .any(|w| w[0] == "--effort" && w[1] == asked)
        || words.contains(&format!("--effort={asked}").as_str())
}

/// Claude's `system/init` says whether the session runs with a per-turn effort
/// (`per_turn_effort_active`; false when the model ignores effort, e.g. haiku). A pinned
/// effort needs it true there, and needs that init before any work is reported.
/// Ok(None): not the event that decides.
fn check_init_effort(asked: &str, v: &Value) -> Result<Option<String>> {
    match (v["type"].as_str(), v["subtype"].as_str()) {
        (Some("system"), Some("init")) => match v["per_turn_effort_active"].as_bool() {
            Some(true) => Ok(Some(format!(
                "init per_turn_effort_active=true (model {})",
                v["model"].as_str().unwrap_or("?")
            ))),
            other => bail!(
                "effort not applied: requested {asked}, but claude's init reports per_turn_effort_active={} for model {}; stopped before any tool ran (no silent downgrade)",
                other.map_or("missing".into(), |b| b.to_string()),
                v["model"].as_str().unwrap_or("?")
            ),
        },
        (Some("assistant" | "result"), _) => bail!(
            "effort not confirmed: requested {asked}, but claude did work before its init event confirmed the effort; stopped (no silent downgrade)"
        ),
        _ => Ok(None),
    }
}

/// Claude's `rate_limit_event` as a quota row (unknown fields stay null, never 0).
fn claude_rate(v: &Value) -> Value {
    let info = &v["rate_limit_info"];
    let used = info["utilization"]
        .as_f64()
        .map(|u| if u <= 1.0 { u * 100.0 } else { u });
    let resets = info["resetsAt"]
        .as_u64()
        .map(|r| if r < 10_000_000_000 { r * 1000 } else { r });
    serde_json::json!({
        "account": "default", "harness": "claude",
        "remaining_pct": used.map(|u| (100.0 - u).max(0.0)),
        "resets_at": resets,
        "status": info["status"], "window": info["rateLimitType"],
        "source": "claude rate_limit_event (last driven turn)",
    })
}

/// Codex quota from its own app server (`account/rateLimits/read`), no model call.
pub fn codex_quota() -> Result<Value> {
    let bin = std::env::var("UNVRS_CODEX_BIN").unwrap_or_else(|_| "codex".into());
    let mut child = Child(
        Command::new(bin)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn_owned()
            .context("Codex quota helper unavailable")?,
    );
    let mut input = child.stdin.take().context("quota stdin")?;
    let output = child.stdout.take().context("quota stdout")?;
    let stderr = child.stderr.take().context("quota stderr")?;
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.take(65536).read_to_string(&mut s);
        s
    });
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let wait = |id: u64| -> Result<Value> {
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line)
                        && v["id"] == id
                    {
                        ensure!(v.get("error").is_none(), "Codex quota RPC: {}", v["error"]);
                        return Ok(v);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("Codex quota helper closed stdout")
                }
                Err(_) => {}
            }
        }
        bail!("Codex quota RPC {id} timed out after 15 seconds")
    };
    let mut send = |v: Value| writeln!(input, "{v}").and_then(|_| input.flush());
    let result = (|| -> Result<Value> {
        send(
            serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"unvrs","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}}),
        )?;
        wait(1)?;
        send(serde_json::json!({"method":"initialized"}))?;
        send(serde_json::json!({"id":2,"method":"account/rateLimits/read","params":{}}))?;
        let r = wait(2)?;
        codex_quota_row(&r["result"], uke::now_ms())
    })();
    drop(child);
    let stderr = err_reader
        .join()
        .unwrap_or_else(|_| "quota stderr reader failed".into());
    result.map_err(|e| anyhow::anyhow!("{e:#}\n{}", stderr.trim()))
}

/// Public quota fields only; timestamps are milliseconds and absent readings stay null.
pub(super) fn codex_quota_row(result: &Value, observed_at: u64) -> Result<Value> {
    let limits = &result["rateLimits"];
    ensure!(
        limits.is_object(),
        "Codex quota response missing rateLimits"
    );
    let millis = |v: &Value| v.as_u64().and_then(|t| t.checked_mul(1000));
    let window = |w: &Value| -> Value {
        if !w.is_object() {
            return Value::Null;
        }
        let used = w["usedPercent"].as_f64().filter(|u| *u >= 0.0);
        serde_json::json!({"used_pct":used,"remaining_pct":used.map(|u|(100.0-u).max(0.0)),
            "resets_at":millis(&w["resetsAt"]),"window_mins":w["windowDurationMins"].as_u64()})
    };
    let primary = window(&limits["primary"]);
    let secondary = window(&limits["secondary"]);
    let by_duration = |mins: u64| {
        [&primary, &secondary]
            .into_iter()
            .find(|w| w["window_mins"].as_u64() == Some(mins))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let summary = &result["rateLimitResetCredits"];
    let reset_credits = summary.is_object().then(|| {
        let credits = summary["credits"].as_array().map(|rows| {
            rows.iter().map(|r| {
            serde_json::json!({"id":r["id"],"status":r["status"],"reset_type":r["resetType"],
                "granted_at":millis(&r["grantedAt"]),"expires_at":millis(&r["expiresAt"]),
                "title":r["title"],"description":r["description"]})
        }).collect::<Vec<_>>()
        });
        serde_json::json!({"available_count":summary["availableCount"].as_u64(),"credits":credits})
    });
    Ok(serde_json::json!({"account":"default","harness":"codex",
        "remaining_pct":primary["remaining_pct"],"resets_at":primary["resets_at"],
        "window_mins":primary["window_mins"],"plan":limits["planType"],
        "weekly":by_duration(10080),"five_hour":by_duration(300),
        "reset_credits":reset_credits,"ordinary_usage_allowed":result["ordinaryUsageAllowed"].as_bool(),
        "observed_at":observed_at,"source":"codex app-server account/rateLimits/read"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_labels_never_contain_arguments() {
        assert_eq!(
            tool_label(
                "shell",
                &serde_json::json!({"command": "curl -H 'Authorization: Bearer opaque-secret' example.com", "description": "opaque-secret"})
            ),
            "shell"
        );
    }

    #[test]
    fn output_extends_a_turn_up_to_its_ceiling_only() {
        let budget = TurnBudget {
            idle: Duration::from_secs(60),
            max: Duration::from_secs(600),
        };
        let now = Instant::now();
        let at = |started: u64, last: u64| TurnClock {
            budget,
            started: now - Duration::from_secs(started),
            last: now - Duration::from_secs(last),
        };
        // Busy almost to the ceiling, output a second ago: running, deadline capped.
        let busy = at(590, 1);
        assert_eq!(busy.expired("x"), None);
        assert_eq!(busy.deadline(), busy.started + budget.max);
        // Silent past the idle budget: hung.
        let hung = at(120, 61).expired("x").unwrap();
        assert!(hung.contains("without output"), "{hung}");
        // Chatty past the ceiling: runaway.
        let runaway = at(601, 0).expired("x").unwrap();
        assert!(runaway.contains("ceiling"), "{runaway}");
    }

    #[test]
    fn launch_argv_must_carry_the_pinned_effort() {
        let argv = "claude -p --output-format stream-json --effort high --resume s1";
        assert!(argv_has_effort(argv, "high"));
        assert!(argv_has_effort("claude -p --effort=xhigh", "xhigh"));
        // A different or missing level is a mismatch, not a match.
        assert!(!argv_has_effort(argv, "max"));
        assert!(!argv_has_effort("claude -p --effort", "high"));
        assert!(!argv_has_effort("claude -p", "high"));
        // OS argument-read refusal must use the same fail-closed pin classification.
        let error = check_launch_effort(u32::MAX, "high").unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("effort not confirmed: requested high")
        );
        assert!(error.to_string().contains("no silent downgrade"));
        assert!(
            error.chain().count() > 1,
            "preserve the argument-read cause"
        );
    }

    #[test]
    fn kernel_arguments_exclude_environment_and_reject_truncation() {
        let buffer = |argc: i32, tail: &[u8]| [argc.to_ne_bytes().as_slice(), tail].concat();
        assert_eq!(
            macos_argv(&buffer(
                3,
                b"/claude\0\0claude\0--effort\0high\0SECRET=value\0"
            ))
            .unwrap(),
            "claude --effort high"
        );
        let argv = macos_argv(&buffer(1, b"/claude\0claude\0ENV=--effort high\0")).unwrap();
        assert!(!argv_has_effort(&argv, "high"));
        for bytes in [
            vec![],
            buffer(-1, b""),
            buffer(2, b"/claude\0claude\0high"),
            buffer(3, b"/claude\0claude\0"),
        ] {
            assert!(macos_argv(&bytes).is_err());
        }
    }

    #[test]
    fn init_decides_whether_the_effort_applied() {
        let init = |v: Value| json!({"type": "system", "subtype": "init", "model": "m", "per_turn_effort_active": v});
        assert!(
            check_init_effort("high", &init(json!(true)))
                .unwrap()
                .is_some()
        );
        assert!(check_init_effort("high", &init(json!(false))).is_err());
        assert!(check_init_effort("high", &init(Value::Null)).is_err());
        assert!(
            check_init_effort(
                "high",
                &json!({"type": "system", "subtype": "hook_started"})
            )
            .unwrap()
            .is_none()
        );
        assert!(check_init_effort("high", &json!({"type": "assistant", "message": {}})).is_err());
    }
}
