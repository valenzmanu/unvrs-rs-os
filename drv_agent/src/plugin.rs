//! D61 plugin: one package rendered by the installed binary for Claude Code and Codex,
//! registered through a local marketplace. It holds no logic: explicit-only entry
//! skills (text from `mapp_unvrs`), lifecycle hooks and one MCP server, all naming
//! only the stable binary `<UNVRS_HOME>/bin/unvrs`. Its version carries a hash of its
//! contents, because both harnesses copy a plugin into a cache keyed by version and never
//! re-copy a version they hold: a deploy re-renders it and refreshes both caches.
//!
//! Every harness path comes from the environment (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
//! `UNVRS_HOME`, `UNVRS_LAUNCHAGENTS_DIR`, `UNVRS_LAUNCHD_LABEL`) so tests run on copies.
use crate::protocol::{self, Line};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use uke::signals::CommandTracking;

pub const PLUGIN: &str = mapp_unvrs::PLUGIN;
pub const MARKETPLACE: &str = "unvrs-local";
/// The MCP server name (one tool, `unvrs(command)`).
pub const MCP_SERVER: &str = "unvrs";
pub const DEFAULT_LABEL: &str = "dev.unvrs.kernel";
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn plugin_id() -> String {
    format!("{PLUGIN}@{MARKETPLACE}")
}

// ───────────────────────────── paths ─────────────────────────────

/// Every path install, uninstall and doctor touch, computed once from the environment.
#[derive(Clone, Debug)]
pub struct Homes {
    /// `UNVRS_HOME`, else `~/.unvrs`.
    pub unvrs: PathBuf,
    /// `CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude: PathBuf,
    /// `CODEX_HOME`, else `~/.codex`.
    pub codex: PathBuf,
    /// `UNVRS_LAUNCHAGENTS_DIR`, else `~/Library/LaunchAgents`.
    pub launch_agents: PathBuf,
    /// `UNVRS_LAUNCHD_LABEL`, else `dev.unvrs.kernel`.
    pub label: String,
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Homes {
    pub fn from_env() -> Result<Self> {
        let home = env_path("HOME").context("HOME unset")?;
        Ok(Self {
            unvrs: env_path("UNVRS_HOME").unwrap_or_else(|| home.join(".unvrs")),
            claude: env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            codex: env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            launch_agents: env_path("UNVRS_LAUNCHAGENTS_DIR")
                .unwrap_or_else(|| home.join("Library/LaunchAgents")),
            label: std::env::var("UNVRS_LAUNCHD_LABEL")
                .ok()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| DEFAULT_LABEL.into()),
        })
    }
    /// The stable binary every hook, the MCP server and the LaunchAgent run.
    pub fn bin(&self) -> PathBuf {
        self.unvrs.join("bin/unvrs")
    }
    pub fn plugin_root(&self) -> PathBuf {
        self.unvrs.join("plugin")
    }
    pub fn plist(&self) -> PathBuf {
        self.launch_agents.join(format!("{}.plist", self.label))
    }
    /// Environment for a harness child process: always our homes, never the caller's guess.
    pub fn harness_env(&self, cmd: &mut Command) {
        cmd.env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", &self.codex)
            .env("UNVRS_HOME", &self.unvrs);
    }
    /// User-level harness files the install may touch; each is backed up before any change.
    pub fn touched_files(&self) -> Vec<PathBuf> {
        vec![
            self.claude.join("settings.json"),
            self.claude.join("settings.local.json"),
            self.claude.join("plugins/installed_plugins.json"),
            self.claude.join("plugins/known_marketplaces.json"),
            self.codex.join("config.toml"),
            self.codex.join("hooks.json"),
        ]
    }
    /// Harness caches and data dirs that belong to our plugin only.
    pub fn owned_caches(&self) -> Vec<PathBuf> {
        vec![
            self.claude.join("plugins/cache").join(MARKETPLACE),
            self.claude.join("plugins/marketplaces").join(MARKETPLACE),
            self.claude
                .join("plugins/data")
                .join(format!("{PLUGIN}-{MARKETPLACE}")),
            self.codex.join("plugins/cache").join(MARKETPLACE),
        ]
    }
}

// ───────────────────────────── render ─────────────────────────────

/// Single-quoted for `sh`.
pub fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

pub fn hook_command(home: &Path, event: &str, harness: &str) -> String {
    let exe = sh_quote(&home.join("bin/unvrs"));
    format!(
        "[ -x {exe} ] && UNVRS_HOME={h} {exe} hook {event} --harness {harness} 2>/dev/null; exit 0",
        h = sh_quote(home)
    )
}

/// The rewake watcher (G-L1push): unlike `hook_command` it keeps its exit code, since
/// exit 2 is what makes Claude Code's `asyncRewake` wake the idle model.
pub fn watch_command(home: &Path, harness: &str) -> String {
    let exe = sh_quote(&home.join("bin/unvrs"));
    format!(
        "[ -x {exe} ] || exit 0; UNVRS_HOME={h} exec {exe} hook Watch --harness {harness} --for {}",
        WATCH_SECS - 60,
        h = sh_quote(home)
    )
}

/// How long one watcher may wait for a wake; the next Stop arms a fresh one.
pub const WATCH_SECS: u64 = 86_400;

pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "PreCompact",
    "SessionEnd",
];

fn hooks(home: &Path, harness: &str) -> Value {
    let mut map = serde_json::Map::new();
    for event in HOOK_EVENTS {
        let timeout = match *event {
            "SessionStart" => 20,
            "UserPromptSubmit" => 15,
            "SessionEnd" if harness == "codex" => 3,
            "SessionEnd" => 5,
            _ => 10,
        };
        let mut h = json!({"type": "command", "command": hook_command(home, event, harness), "timeout": timeout});
        if harness == "codex" && matches!(*event, "SessionStart" | "UserPromptSubmit") {
            h["additionalContextLimit"] = json!(4000);
        }
        let mut list = vec![h];
        // Claude Code wakes an idle thread when an `asyncRewake` hook exits 2: armed at
        // SessionStart and at every Stop, it waits in the background for the seat's wakes.
        if harness == "claude" && matches!(*event, "SessionStart" | "Stop") {
            list.push(
                json!({"type": "command", "command": watch_command(home, harness),
                "timeout": WATCH_SECS, "asyncRewake": true,
                "rewakeMessage": "UNVRS woke this idle thread with its queued wakes:",
                "rewakeSummary": "UNVRS wake"}),
            );
        }
        map.insert((*event).into(), json!([{"matcher": "", "hooks": list}]));
    }
    json!({"hooks": map})
}

/// The one MCP server, in the `.mcp.json` shape both harnesses read (Claude through
/// `.claude-plugin/plugin.json`, Codex through `.codex-plugin/plugin.json`).
/// `default_tools_approval_mode = "approve"` is Codex's per-server key (its
/// `PluginMcpServerConfig`): without it, a thread with `approval_policy = "never"` and a
/// `workspace-write` sandbox fails every call with "MCP tool call requires approval, but
/// approval policy is never". Verified in the plugin's own `.mcp.json` (Codex 0.155.1),
/// so config.toml needs no entry; Claude Code 2.1.283 ignores the key.
pub fn mcp_json(home: &Path) -> Value {
    json!({"mcpServers": {MCP_SERVER: {
        "command": home.join("bin/unvrs"),
        "args": ["mcp"],
        "env": {"UNVRS_HOME": home},
        "default_tools_approval_mode": "approve"
    }}})
}

fn write(path: &Path, text: &str) -> Result<()> {
    fs::create_dir_all(path.parent().context("parent")?)?;
    fs::write(path, text)?;
    Ok(())
}
fn pretty(v: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)? + "\n")
}

const DESCRIPTION: &str =
    "UNVRS: your crew (L1 chief of staff, project L2s) in any thread, by explicit command only.";

fn render_into(home: &Path, root: &Path) -> Result<()> {
    let p = root.join(PLUGIN);
    write(&p.join(".mcp.json"), &pretty(&mcp_json(home))?)?;
    write(
        &p.join("hooks/claude.json"),
        &pretty(&hooks(home, "claude"))?,
    )?;
    write(&p.join("hooks/codex.json"), &pretty(&hooks(home, "codex"))?)?;
    // The code of conduct: model-invocable, so seats and workers load it on their own.
    let conduct = p.join("skills").join(mapp_unvrs::CONDUCT_NAME);
    write(
        &conduct.join("SKILL.md"),
        &mapp_unvrs::conduct_claude_skill(),
    )?;
    write(
        &conduct.join("agents/openai.yaml"),
        &mapp_unvrs::conduct_codex_yaml(),
    )?;
    for s in mapp_unvrs::ENTRY {
        let dir = p.join("skills").join(s.name);
        write(&dir.join("SKILL.md"), &mapp_unvrs::claude_skill(s))?;
        write(
            &dir.join("agents/openai.yaml"),
            &mapp_unvrs::codex_skill_yaml(s),
        )?;
    }
    // The manifests last: their version names the contents written above.
    let version = content_version(&p);
    write(
        &root.join(".claude-plugin/marketplace.json"),
        &pretty(&json!({
            "name": MARKETPLACE, "owner": {"name": "UNVRS"},
            "plugins": [{"name": PLUGIN, "source": "./unvrs", "description": DESCRIPTION, "version": version}]
        }))?,
    )?;
    write(
        &root.join(".agents/plugins/marketplace.json"),
        &pretty(&json!({
            "name": MARKETPLACE, "interface": {"displayName": "UNVRS (local)"},
            "plugins": [{"name": PLUGIN, "source": {"source": "local", "path": "./unvrs"},
                "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"}, "category": "Productivity"}]
        }))?,
    )?;
    write(
        &p.join(".claude-plugin/plugin.json"),
        &pretty(&json!({
            "name": PLUGIN, "version": version, "description": DESCRIPTION,
            "author": {"name": "UNVRS"},
            "skills": "./skills/",
            "hooks": "./hooks/claude.json",
            "mcpServers": "./.mcp.json"
        }))?,
    )?;
    write(
        &p.join(".codex-plugin/plugin.json"),
        &pretty(&json!({
            "name": PLUGIN, "version": version, "description": DESCRIPTION,
            "skills": "./skills/", "hooks": "./hooks/codex.json", "mcpServers": "./.mcp.json",
            "interface": {"displayName": "UNVRS", "shortDescription": "Your crew: L1 and project L2s",
                "capabilities": ["Lifecycle hooks", "Skills", "MCP"], "category": "Productivity"}
        }))?,
    )?;
    Ok(())
}

/// `<crate version>+g<12 hex of the plugin's files>`: semver build metadata, so a
/// changed skill, hook or MCP entry is a new version to both harnesses.
fn content_version(plugin: &Path) -> String {
    let mut data = vec![];
    for (rel, bytes) in tree(plugin) {
        data.extend_from_slice(rel.to_string_lossy().as_bytes());
        data.push(0);
        data.extend_from_slice(&bytes);
        data.push(0);
    }
    format!("{VERSION}+g{}", &sha256_hex(&data)[..12])
}

fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
                out.push((rel, fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// Renders `<home>/plugin/` (marketplace + plugin) and returns (root, changed).
/// Rendering is atomic per tree: a fresh copy is built beside it and swapped in only
/// when it differs, so a second run is a no-op.
pub fn render_plugin_checked(home: &Path) -> Result<(PathBuf, bool)> {
    let root = home.join("plugin");
    let tmp = home.join(".plugin.render");
    let _ = fs::remove_dir_all(&tmp);
    render_into(home, &tmp)?;
    if root.exists() && tree(&root) == tree(&tmp) {
        fs::remove_dir_all(&tmp)?;
        return Ok((root, false));
    }
    let old = home.join(".plugin.old");
    let _ = fs::remove_dir_all(&old);
    if root.exists() {
        fs::rename(&root, &old)?;
    }
    fs::rename(&tmp, &root)?;
    let _ = fs::remove_dir_all(&old);
    Ok((root, true))
}

pub fn render_plugin(home: &Path) -> Result<PathBuf> {
    Ok(render_plugin_checked(home)?.0)
}

// ───────────────────────────── processes ─────────────────────────────

pub fn which(cmd: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

fn run(h: &Homes, cmd: &str, args: &[&str]) -> Result<String> {
    let mut c = Command::new(cmd);
    c.args(args)
        .current_dir(&h.unvrs)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    h.harness_env(&mut c);
    let out = c
        .output_owned()
        .with_context(|| format!("{cmd} not found"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    ensure!(
        out.status.success(),
        "{cmd} {} failed: {}",
        args.join(" "),
        text.trim()
    );
    Ok(text)
}

/// A line-reading child with a deadline per read.
struct Lines {
    child: uke::signals::TrackedChild,
    rx: mpsc::Receiver<std::io::Result<String>>,
}
impl Lines {
    fn spawn(mut cmd: Command) -> Result<Self> {
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_owned()
            .context("spawn")?;
        let out = child.stdout.take().context("stdout")?;
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(out);
            loop {
                match protocol::read_line(&mut reader) {
                    Ok(None) => break,
                    Ok(Some(Line::Dropped(_))) => continue,
                    Ok(Some(Line::Json(line))) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        Ok(Self { child, rx })
    }
    fn next_json(&self, until: Instant, pred: impl Fn(&Value) -> bool) -> Result<Value> {
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(line) => {
                    let line = line.context("protocol stdout read failed")?;
                    if let Ok(v) = serde_json::from_str::<Value>(&line)
                        && pred(&v)
                    {
                        return Ok(v);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("process exited"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        bail!("timed out")
    }
}
/// One JSON-RPC stdio session: `initialize` (+ `initialized`), then each call in order.
/// Returns each call's full response (`result` or `error`).
pub fn jsonrpc_session(
    mut cmd: Command,
    init_params: Value,
    calls: &[(&str, Value)],
    timeout: Duration,
) -> Result<Vec<Value>> {
    cmd.stdin(Stdio::piped());
    let mut lines = Lines::spawn(cmd)?;
    let mut input = lines.child.stdin.take().context("stdin")?;
    let until = Instant::now() + timeout;
    let mut send = |v: Value| -> Result<()> {
        writeln!(input, "{v}")?;
        input.flush()?;
        Ok(())
    };
    send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": init_params}))?;
    lines.next_json(until, |v| v["id"] == 0)?;
    send(json!({"jsonrpc": "2.0", "method": "initialized"}))?;
    let mut out = vec![];
    for (i, (method, params)) in calls.iter().enumerate() {
        let id = i as u64 + 1;
        send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        out.push(
            lines
                .next_json(until, |v| v["id"] == id)
                .with_context(|| format!("{method}: no answer"))?,
        );
    }
    Ok(out)
}

/// Calls Codex's own `codex app-server` (the core the Codex app and T3 use).
pub fn codex_app_server(h: &Homes, calls: &[(&str, Value)]) -> Result<Vec<Value>> {
    let mut cmd = Command::new("codex");
    cmd.arg("app-server").current_dir(&h.unvrs);
    h.harness_env(&mut cmd);
    jsonrpc_session(
        cmd,
        json!({"clientInfo": {"name": "unvrs", "version": VERSION}}),
        calls,
        Duration::from_secs(40),
    )
}

/// The first `system/init` line of a headless Claude run; the process is killed right
/// after, before any model turn.
pub fn claude_init(h: &Homes, prompt: &str) -> Result<Value> {
    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--output-format", "stream-json", "--verbose", prompt])
        .current_dir(&h.unvrs)
        .stdin(Stdio::null());
    h.harness_env(&mut cmd);
    let lines = Lines::spawn(cmd)?;
    lines.next_json(Instant::now() + Duration::from_secs(60), |v| {
        v["type"] == "system" && v["subtype"] == "init"
    })
}

/// Claude's SDK control plane. No user message or inference request is sent.
pub fn claude_control(mut cmd: Command, requests: &[Value]) -> Result<Vec<Value>> {
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
    ])
    .stdin(Stdio::piped());
    let mut lines = Lines::spawn(cmd)?;
    let mut input = lines.child.stdin.take().context("Claude control stdin")?;
    let until = Instant::now() + Duration::from_secs(40);
    let mut out = vec![];
    for (i, request) in std::iter::once(json!({"subtype":"initialize"}))
        .chain(requests.iter().cloned())
        .enumerate()
    {
        let id = format!("unvrs-catalog-{i}");
        writeln!(
            input,
            "{}",
            json!({"type":"control_request", "request_id":id, "request":request})
        )?;
        input.flush()?;
        out.push(lines.next_json(until, |v| {
            v["type"] == "control_response" && v["response"]["request_id"] == id
        })?);
    }
    drop(input);
    Ok(out)
}

// ───────────────────────────── backup ─────────────────────────────

/// SHA-256 (FIPS 180-4), hex; small enough to keep the workspace free of a hash crate.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut hs: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = hs;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in hs.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    hs.iter().map(|x| format!("{x:08x}")).collect()
}

pub fn sha_file(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|b| sha256_hex(&b))
}

/// Copies every touched file into `dir` and writes `dir/manifest.json`
/// (`[{path, existed, sha256, copy}]`). Called before any harness change.
pub fn backup(h: &Homes, dir: &Path) -> Result<Value> {
    fs::create_dir_all(dir)?;
    let mut files = vec![];
    for (i, f) in h.touched_files().iter().enumerate() {
        let existed = f.is_file();
        let copy = dir.join(format!(
            "{i}-{}",
            f.file_name().unwrap_or_default().to_string_lossy()
        ));
        if existed {
            fs::copy(f, &copy).with_context(|| format!("back up {}", f.display()))?;
        }
        files.push(json!({"path": f, "existed": existed, "sha256": sha_file(f), "copy": if existed { json!(copy) } else { Value::Null }}));
    }
    let manifest = json!({"files": files, "home": h.unvrs});
    fs::write(dir.join("manifest.json"), pretty(&manifest)?)?;
    Ok(manifest)
}

// ───────────────────────────── registration ─────────────────────────────

fn read_json(p: &Path) -> Value {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

pub fn claude_installed(h: &Homes) -> bool {
    read_json(&h.claude.join("plugins/installed_plugins.json"))["plugins"]
        .get(plugin_id())
        .is_some()
}
pub fn claude_marketplace_known(h: &Homes) -> bool {
    read_json(&h.claude.join("plugins/known_marketplaces.json"))
        .get(MARKETPLACE)
        .is_some()
}
/// `enabledPlugins["unvrs@unvrs-local"]` in Claude's user settings.
pub fn claude_enabled(h: &Homes) -> bool {
    read_json(&h.claude.join("settings.json"))["enabledPlugins"][plugin_id()] == true
}

fn toml_doc(p: &Path) -> toml::Table {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .unwrap_or_default()
}
pub fn codex_marketplace_known(h: &Homes) -> bool {
    toml_doc(&h.codex.join("config.toml"))
        .get("marketplaces")
        .and_then(|m| m.get(MARKETPLACE))
        .is_some()
}
pub fn codex_installed(h: &Homes) -> bool {
    toml_doc(&h.codex.join("config.toml"))
        .get("plugins")
        .and_then(|m| m.get(plugin_id()))
        .is_some()
}
pub fn codex_enabled(h: &Homes) -> bool {
    toml_doc(&h.codex.join("config.toml"))
        .get("plugins")
        .and_then(|m| m.get(plugin_id()))
        .and_then(|p| p.get("enabled"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// The version in the rendered plugin under `root` (`<home>/plugin`).
pub fn rendered_version(root: &Path) -> Option<String> {
    read_json(&root.join(PLUGIN).join(".claude-plugin/plugin.json"))["version"]
        .as_str()
        .map(str::to_owned)
}

/// The rendered plugin's files, without the harness's own markers (Claude's `.in_use`).
fn plugin_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = tree(dir);
    files.retain(|(rel, _)| !rel.starts_with(".in_use"));
    files
}

/// `(version, installPath)` of Claude's user-scope install, from installed_plugins.json.
pub fn claude_cached(h: &Homes) -> Option<(String, PathBuf)> {
    let v = read_json(&h.claude.join("plugins/installed_plugins.json"));
    let entries = v["plugins"][plugin_id()].as_array()?.clone();
    let e = entries
        .iter()
        .find(|e| e["scope"] == "user")
        .or(entries.first())?;
    Some((
        e["version"].as_str()?.to_owned(),
        PathBuf::from(e["installPath"].as_str()?),
    ))
}

/// Codex's cache dir for `version` (it keeps one, named by the manifest's version).
pub fn codex_cache_dir(h: &Homes, version: &str) -> PathBuf {
    h.codex
        .join("plugins/cache")
        .join(MARKETPLACE)
        .join(PLUGIN)
        .join(version)
}

/// `Ok(version)` when Claude loads exactly the plugin rendered under `root`; else why not.
pub fn claude_fresh(h: &Homes, root: &Path) -> Result<String> {
    let want = rendered_version(root).context("no rendered plugin version")?;
    let (have, dir) = claude_cached(h).context("claude has no install record")?;
    ensure!(have == want, "claude caches {have}, rendered is {want}");
    ensure!(
        plugin_files(&dir) == plugin_files(&root.join(PLUGIN)),
        "claude cache {} differs from the rendered plugin",
        dir.display()
    );
    Ok(want)
}

/// `Ok(version)` when Codex's cache holds exactly the plugin rendered under `root`.
pub fn codex_fresh(h: &Homes, root: &Path) -> Result<String> {
    let want = rendered_version(root).context("no rendered plugin version")?;
    let dir = codex_cache_dir(h, &want);
    ensure!(dir.is_dir(), "codex has no cache for {want}");
    ensure!(
        plugin_files(&dir) == plugin_files(&root.join(PLUGIN)),
        "codex cache {} differs from the rendered plugin",
        dir.display()
    );
    Ok(want)
}

/// Brings Claude's installed copy to the rendered plugin: `plugin update` follows the
/// marketplace version (up or down); a stale cache of the same version is reinstalled.
pub fn refresh_claude(h: &Homes, root: &Path) -> Result<String> {
    if let Ok(v) = claude_fresh(h, root) {
        return Ok(format!("claude: holds {v}"));
    }
    let id = plugin_id();
    run(h, "claude", &["plugin", "update", &id, "--scope", "user"])?;
    if let Ok(v) = claude_fresh(h, root) {
        return Ok(format!("claude: updated to {v}"));
    }
    run(
        h,
        "claude",
        &["plugin", "uninstall", &id, "--scope", "user"],
    )?;
    run(h, "claude", &["plugin", "install", &id, "--scope", "user"])?;
    if !claude_enabled(h) {
        run(h, "claude", &["plugin", "enable", &id, "--scope", "user"])?;
    }
    let v = claude_fresh(h, root).context("after reinstall")?;
    Ok(format!("claude: reinstalled {v}"))
}

/// Brings Codex's cache to the rendered plugin (`plugin add` re-copies it).
pub fn refresh_codex(h: &Homes, root: &Path) -> Result<String> {
    if let Ok(v) = codex_fresh(h, root) {
        return Ok(format!("codex: holds {v}"));
    }
    run(h, "codex", &["plugin", "add", &plugin_id()])?;
    let v = codex_fresh(h, root).context("after plugin add")?;
    Ok(format!("codex: refreshed to {v}"))
}

/// After a re-render (deploy, rollback, install): each harness that has our plugin
/// installed is brought to the rendered copy. A harness not on PATH or without our
/// plugin is skipped; registering it is `unvrs install`'s job.
pub fn refresh_hosts(h: &Homes, root: &Path) -> Result<Vec<String>> {
    let mut out = vec![];
    if which("claude").is_none() {
        out.push("claude: not on PATH (skipped)".into());
    } else if !claude_installed(h) {
        out.push(format!("claude: {} not installed (skipped)", plugin_id()));
    } else {
        out.push(refresh_claude(h, root)?);
    }
    if which("codex").is_none() {
        out.push("codex: not on PATH (skipped)".into());
    } else if !codex_installed(h) {
        out.push(format!("codex: {} not installed (skipped)", plugin_id()));
    } else {
        out.push(refresh_codex(h, root)?);
    }
    Ok(out)
}

/// Registers marketplace + plugin in Claude Code at user scope, and brings an existing
/// install to the rendered copy (Claude caches by version; it does not read `root` live).
pub fn register_claude(h: &Homes, root: &Path) -> Result<String> {
    let dir = root.display().to_string();
    let id = plugin_id();
    let mut did = vec![];
    if !claude_marketplace_known(h) {
        run(
            h,
            "claude",
            &["plugin", "marketplace", "add", &dir, "--scope", "user"],
        )?;
        did.push("marketplace added".to_owned());
    }
    if !claude_installed(h) {
        run(h, "claude", &["plugin", "install", &id, "--scope", "user"])?;
        did.push("plugin installed".to_owned());
    } else if claude_fresh(h, root).is_err() {
        did.push(refresh_claude(h, root)?.replace("claude: ", "plugin "));
    }
    if !claude_enabled(h) {
        run(h, "claude", &["plugin", "enable", &id, "--scope", "user"])?;
        did.push("plugin enabled".to_owned());
    }
    Ok(if did.is_empty() {
        format!("claude: already registered ({id}, user scope)")
    } else {
        format!("claude: {} ({id}, user scope)", did.join(", "))
    })
}

pub fn register_codex(h: &Homes, root: &Path) -> Result<String> {
    let dir = root.display().to_string();
    let id = plugin_id();
    let mut did = vec![];
    if !codex_marketplace_known(h) {
        run(h, "codex", &["plugin", "marketplace", "add", &dir])?;
        did.push("marketplace added".to_owned());
    }
    if !codex_installed(h) {
        run(h, "codex", &["plugin", "add", &id])?;
        did.push("plugin installed".to_owned());
    } else if codex_fresh(h, root).is_err() {
        did.push(refresh_codex(h, root)?.replace("codex: ", "plugin "));
    }
    Ok(if did.is_empty() {
        format!("codex: already registered ({id})")
    } else {
        format!("codex: {} ({id})", did.join(", "))
    })
}

/// Removes plugin, marketplace and our caches from both harnesses. Never fails:
/// each step reports what it did.
pub fn unregister(h: &Homes) -> Vec<String> {
    let id = plugin_id();
    let mut out = vec![];
    if which("claude").is_some() {
        let a = claude_installed(h)
            && run(
                h,
                "claude",
                &["plugin", "uninstall", &id, "--scope", "user"],
            )
            .is_ok();
        let b = claude_marketplace_known(h)
            && run(
                h,
                "claude",
                &["plugin", "marketplace", "remove", MARKETPLACE],
            )
            .is_ok();
        out.push(match (a, b) {
            (false, false) => "claude: already removed".to_owned(),
            _ => "claude: plugin and marketplace removed".to_owned(),
        });
    }
    if which("codex").is_some() {
        let a = codex_installed(h) && run(h, "codex", &["plugin", "remove", &id]).is_ok();
        let b = codex_marketplace_known(h)
            && run(
                h,
                "codex",
                &["plugin", "marketplace", "remove", MARKETPLACE],
            )
            .is_ok();
        out.push(match (a, b) {
            (false, false) => "codex: already removed".to_owned(),
            _ => "codex: plugin and marketplace removed".to_owned(),
        });
    }
    for d in h.owned_caches() {
        if d.exists() && fs::remove_dir_all(&d).is_ok() {
            out.push(format!("cache removed: {}", d.display()));
        }
    }
    out
}

// ───────────────────────────── restore ─────────────────────────────

/// Harness-maintained timestamps (a marketplace's `lastUpdated`, refreshed when Claude
/// Code starts) are not configuration; they may differ from the backup.
fn strip_timestamps(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("lastUpdated");
            m.values_mut().for_each(strip_timestamps);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_timestamps),
        _ => {}
    }
}

/// JSON files: drop our keys (and containers only we created), ignore timestamps.
fn json_without_ours(cur: &str, old: Option<&Value>) -> Option<Value> {
    let mut cur: Value = serde_json::from_str(cur).ok()?;
    let id = plugin_id();
    for (container, key) in [
        ("enabledPlugins", id.as_str()),
        ("extraKnownMarketplaces", MARKETPLACE),
        ("plugins", id.as_str()),
    ] {
        if let Some(map) = cur.get_mut(container).and_then(Value::as_object_mut) {
            map.remove(key);
            if map.is_empty() && old.is_none_or(|o| o.get(container).is_none()) {
                cur.as_object_mut().map(|o| o.remove(container));
            }
        }
    }
    if let Some(o) = cur.as_object_mut() {
        o.remove(MARKETPLACE);
    }
    strip_timestamps(&mut cur);
    Some(cur)
}

/// Removes TOML tables that are ours from Codex's config text, keeping every other
/// byte: `[marketplaces.unvrs-local]`, `[plugins."unvrs@unvrs-local"…]`,
/// `[hooks.state."unvrs@unvrs-local:…"]` and `[projects."<path under UNVRS_HOME>"]`.
pub fn codex_config_without_ours(text: &str, home: &Path) -> String {
    let id = plugin_id();
    let home_s = home.display().to_string();
    let ours = |header: &str| -> bool {
        let h = header.trim();
        h == format!("[marketplaces.{MARKETPLACE}]")
            || h == format!("[marketplaces.\"{MARKETPLACE}\"]")
            || h.starts_with(&format!("[plugins.\"{id}\"]"))
            || h.starts_with(&format!("[plugins.\"{id}\"."))
            || h.starts_with(&format!("[hooks.state.\"{id}:"))
            || (h.starts_with("[projects.\"")
                && h.trim_start_matches("[projects.\"")
                    .trim_end_matches("\"]")
                    .starts_with(&home_s))
    };
    // A removed table takes its header, body and trailing blank lines; the separator
    // before it stays (it now separates the previous table from the next one).
    let mut out = String::new();
    let mut skip = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with('[') {
            skip = ours(t.trim_end());
        }
        if !skip {
            out.push_str(line);
        }
    }
    // Tables Codex appended at the end leave the separator it added before them.
    if skip {
        while out.ends_with("\n\n") {
            out.pop();
        }
    }
    out
}

/// Restores every file of a backup manifest. Returns (lines, all byte-identical).
pub fn restore(h: &Homes, dir: &Path) -> Result<(Vec<String>, bool)> {
    let manifest = read_json(&dir.join("manifest.json"));
    ensure!(
        manifest["files"].is_array(),
        "no backup manifest in {}",
        dir.display()
    );
    let mut lines = vec![];
    let mut all = true;
    for f in manifest["files"].as_array().into_iter().flatten() {
        let path = PathBuf::from(f["path"].as_str().unwrap_or_default());
        let pre = f["sha256"].as_str().map(str::to_owned);
        let existed = f["existed"] == true;
        let copy = f["copy"].as_str().map(PathBuf::from);
        let now = sha_file(&path);
        let ok = if now == pre {
            true
        } else if !existed {
            // Created by the install: gone if nothing but ours is in it.
            let text = fs::read_to_string(&path).unwrap_or_default();
            let empty = if path.extension().is_some_and(|e| e == "toml") {
                codex_config_without_ours(&text, &h.unvrs).trim().is_empty()
            } else {
                json_without_ours(&text, None).is_none_or(|v| {
                    v.as_object().is_none_or(|o| {
                        o.iter().all(|(k, x)| {
                            k == "version" || x.as_object().is_some_and(|m| m.is_empty())
                        })
                    })
                })
            };
            if empty {
                fs::remove_file(&path)?;
                true
            } else {
                false
            }
        } else {
            let copy = copy.context("backup copy missing")?;
            let old_bytes = fs::read(&copy)?;
            let old_text = String::from_utf8_lossy(&old_bytes).into_owned();
            let cur = fs::read_to_string(&path).unwrap_or_default();
            let same = if path.extension().is_some_and(|e| e == "toml") {
                codex_config_without_ours(&cur, &h.unvrs)
                    == codex_config_without_ours(&old_text, &h.unvrs)
            } else {
                let old: Option<Value> = serde_json::from_str(&old_text).ok();
                let mut oldv = old.clone();
                if let Some(o) = oldv.as_mut() {
                    strip_timestamps(o);
                }
                json_without_ours(&cur, old.as_ref()).is_some()
                    && json_without_ours(&cur, old.as_ref()) == oldv
            };
            if same {
                fs::write(&path, &old_bytes)?;
                sha_file(&path) == pre
            } else {
                // Keep the captain's changes; take out only what is ours.
                if path.extension().is_some_and(|e| e == "toml") {
                    let cleaned = codex_config_without_ours(&cur, &h.unvrs);
                    if cleaned != cur {
                        fs::write(&path, cleaned)?;
                    }
                }
                false
            }
        };
        all &= ok;
        lines.push(format!(
            "{} {}",
            if ok {
                "restored byte-identical:"
            } else {
                "CHANGED since install (left as is):"
            },
            path.display()
        ));
    }
    Ok((lines, all))
}

// ───────────────────────────── 0.7 compatibility ─────────────────────────────

/// 0.7 entry points kept only so `cmd_kernel::plugin` still links; the 0.8 install is
/// `unvrs install` (unvrs/src/cmd_install.rs). The integrator deletes these with
/// `cmd_kernel::plugin`.
#[derive(Debug, Default)]
pub struct PluginReport {
    pub lines: Vec<String>,
}
pub fn install_plugin(_universe: &Path, _exe: &Path) -> Result<PluginReport> {
    bail!("replaced in 0.8: run `unvrs install`")
}
pub fn uninstall_plugin(_universe: &Path) -> Result<PluginReport> {
    bail!("replaced in 0.8: run `unvrs uninstall`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hook_command_quotes_and_fails_open() {
        let c = hook_command(Path::new("/tmp/it's here"), "Stop", "codex");
        assert_eq!(
            c,
            "[ -x '/tmp/it'\\''s here/bin/unvrs' ] && UNVRS_HOME='/tmp/it'\\''s here' '/tmp/it'\\''s here/bin/unvrs' hook Stop --harness codex 2>/dev/null; exit 0"
        );
    }

    #[test]
    fn claude_arms_a_rewake_watcher_at_session_start_and_stop() {
        let home = Path::new("/tmp/u");
        let claude = hooks(home, "claude");
        for event in ["SessionStart", "Stop"] {
            let list = claude["hooks"][event][0]["hooks"].as_array().unwrap();
            let w = list.iter().find(|h| h["asyncRewake"] == true).unwrap();
            assert_eq!(w["command"], watch_command(home, "claude"));
            assert!(w["timeout"].as_u64().unwrap() >= 3600, "{w}");
            let cmd = w["command"].as_str().unwrap();
            assert!(cmd.contains("hook Watch --harness claude"), "{cmd}");
            assert!(
                !cmd.ends_with("exit 0"),
                "exit 2 must reach Claude Code: {cmd}"
            );
        }
        for event in ["UserPromptSubmit", "PreCompact", "SessionEnd"] {
            assert_eq!(
                claude["hooks"][event][0]["hooks"].as_array().unwrap().len(),
                1
            );
        }
        let codex = hooks(home, "codex");
        assert!(
            !codex.to_string().contains("asyncRewake"),
            "Codex has no asyncRewake"
        );
    }

    #[test]
    fn render_is_idempotent_and_has_entry_skills_plus_conduct() {
        let home = std::env::temp_dir().join(format!("unvrs-render-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let (root, changed) = render_plugin_checked(&home).unwrap();
        assert!(changed);
        let skills: Vec<_> = fs::read_dir(root.join("unvrs/skills"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(skills.len(), mapp_unvrs::ENTRY.len() + 1);
        assert!(skills.iter().any(|s| s == mapp_unvrs::CONDUCT_NAME));
        let conduct = root.join("unvrs/skills/conduct");
        let skill = fs::read_to_string(conduct.join("SKILL.md")).unwrap();
        assert!(skill.contains("HANDOFF") && !skill.contains("disable-model-invocation"));
        assert!(
            fs::read_to_string(conduct.join("agents/openai.yaml"))
                .unwrap()
                .contains("allow_implicit_invocation: true")
        );
        assert!(!render_plugin_checked(&home).unwrap().1);
        let _ = fs::remove_dir_all(&home);
    }

    fn copy_tree(from: &Path, to: &Path) {
        for (rel, bytes) in tree(from) {
            write(&to.join(rel), &String::from_utf8(bytes).unwrap()).unwrap();
        }
    }

    #[test]
    fn version_names_the_contents_and_fresh_compares_each_host_cache() {
        // PID 199: both hosts cached 0.8.0 and never re-copied it, so a deploy that added
        // the conduct skill went unseen by Claude Code and Codex.
        let root = std::env::temp_dir().join(format!("unvrs-fresh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let h = Homes {
            unvrs: root.join("u"),
            claude: root.join("c"),
            codex: root.join("x"),
            launch_agents: root.join("l"),
            label: "dev.unvrs.test.unit".into(),
        };
        let (plugin, _) = render_plugin_checked(&h.unvrs).unwrap();
        let v = rendered_version(&plugin).unwrap();
        let (base, hash) = v.split_once("+g").unwrap();
        assert_eq!(base, VERSION);
        assert_eq!(hash.len(), 12);
        let market = read_json(&plugin.join(".claude-plugin/marketplace.json"));
        assert_eq!(market["plugins"][0]["version"], json!(v));
        assert_eq!(
            read_json(&plugin.join("unvrs/.codex-plugin/plugin.json"))["version"],
            json!(v)
        );
        // Another home renders other hook paths: another version.
        let other = render_plugin(&root.join("u2")).unwrap();
        assert_ne!(rendered_version(&other).unwrap(), v);

        // Neither host has a copy yet.
        assert!(claude_fresh(&h, &plugin).is_err());
        assert!(codex_fresh(&h, &plugin).is_err());
        // The hosts as the failed deploy left them: an old copy without conduct.
        let old = root.join("c/plugins/cache/unvrs-local/unvrs/0.8.0");
        copy_tree(&plugin.join(PLUGIN), &old);
        fs::remove_dir_all(old.join("skills/conduct")).unwrap();
        let record = |version: &str, dir: &Path| {
            write(
                &h.claude.join("plugins/installed_plugins.json"),
                &json!({"version": 2, "plugins": {plugin_id(): [
                    {"scope": "user", "installPath": dir, "version": version}]}})
                .to_string(),
            )
            .unwrap();
        };
        record("0.8.0", &old);
        let e = claude_fresh(&h, &plugin).unwrap_err().to_string();
        assert!(
            e.contains(&format!("claude caches 0.8.0, rendered is {v}")),
            "{e}"
        );
        // Same version, stale contents (what a same-version re-render looked like).
        record(&v, &old);
        assert!(
            claude_fresh(&h, &plugin)
                .unwrap_err()
                .to_string()
                .contains("differs")
        );
        // Claude's copy of this version, plus its own .in_use marker: fresh.
        let cur = root.join("c/plugins/cache/unvrs-local/unvrs/new");
        copy_tree(&plugin.join(PLUGIN), &cur);
        write(&cur.join(".in_use/123"), "").unwrap();
        record(&v, &cur);
        assert_eq!(claude_fresh(&h, &plugin).unwrap(), v);
        // Codex: its cache dir named by the version must hold the same files.
        copy_tree(&plugin.join(PLUGIN), &codex_cache_dir(&h, &v));
        assert_eq!(codex_fresh(&h, &plugin).unwrap(), v);
        fs::remove_dir_all(codex_cache_dir(&h, &v).join("skills/conduct")).unwrap();
        assert!(
            codex_fresh(&h, &plugin)
                .unwrap_err()
                .to_string()
                .contains("differs")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_is_byte_identical_or_reports_captain_changes() {
        let root = std::env::temp_dir().join(format!("unvrs-restore-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let h = Homes {
            unvrs: root.join("u"),
            claude: root.join("c"),
            codex: root.join("x"),
            launch_agents: root.join("l"),
            label: "dev.unvrs.test.unit".into(),
        };
        fs::create_dir_all(&h.codex).unwrap();
        fs::create_dir_all(h.claude.join("plugins")).unwrap();
        let cfg = h.codex.join("config.toml");
        let settings = h.claude.join("settings.json");
        let known = h.claude.join("plugins/known_marketplaces.json");
        let orig = "model = \"m\"\n\n[plugins.\"a@b\"]\nenabled = true\n";
        let orig_settings = "{\n  \"enabledPlugins\": {\n    \"a@b\": true\n  }\n}\n";
        let orig_known = "{\n  \"x\": {\n    \"lastUpdated\": \"1\"\n  }\n}\n";
        fs::write(&cfg, orig).unwrap();
        fs::write(&settings, orig_settings).unwrap();
        fs::write(&known, orig_known).unwrap();
        let dir = root.join("backup");
        backup(&h, &dir).unwrap();
        // Ours only (plus Codex's own project trust for a path under UNVRS_HOME and a
        // harness-refreshed timestamp): byte-identical again.
        fs::write(&cfg, format!("{orig}\n[plugins.\"unvrs@unvrs-local\"]\nenabled = true\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n", h.unvrs.join("w").display())).unwrap();
        fs::write(&settings, "{\"enabledPlugins\": {\"a@b\": true, \"unvrs@unvrs-local\": true}, \"extraKnownMarketplaces\": {\"unvrs-local\": {}}}").unwrap();
        fs::write(
            &known,
            "{\"x\": {\"lastUpdated\": \"2\"}, \"unvrs-local\": {}}",
        )
        .unwrap();
        let (lines, all) = restore(&h, &dir).unwrap();
        assert!(all, "{lines:?}");
        assert_eq!(fs::read_to_string(&cfg).unwrap(), orig);
        assert_eq!(fs::read_to_string(&settings).unwrap(), orig_settings);
        assert_eq!(fs::read_to_string(&known).unwrap(), orig_known);
        assert!(!h.claude.join("settings.local.json").exists());
        // The captain changed config.toml meanwhile: reported, their change kept, ours gone.
        fs::write(&cfg, "model = \"other\"\n\n[plugins.\"a@b\"]\nenabled = true\n\n[plugins.\"unvrs@unvrs-local\"]\nenabled = true\n").unwrap();
        let (lines, all) = restore(&h, &dir).unwrap();
        assert!(!all);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("CHANGED since install") && l.ends_with("config.toml"))
        );
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            "model = \"other\"\n\n[plugins.\"a@b\"]\nenabled = true\n"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_config_cleanup_keeps_every_other_byte() {
        let home = Path::new("/u/.unvrs");
        let before = "model = \"x\"\n\n[projects.\"/a\"]\ntrust_level = \"trusted\"\n\n[plugins.\"github@openai-curated\"]\nenabled = true\n";
        let after = format!(
            "{before}\n[marketplaces.unvrs-local]\nsource = \"/u/.unvrs/plugin\"\n\n[plugins.\"unvrs@unvrs-local\"]\nenabled = true\n\n[projects.\"/u/.unvrs\"]\ntrust_level = \"trusted\"\n"
        );
        assert_eq!(codex_config_without_ours(&after, home), before);
        // Inserted between tables, as `codex plugin add` does.
        let before = "[plugins.\"a@b\"]\nenabled = true\n\n[features]\nx = true\n";
        let after = "[plugins.\"a@b\"]\nenabled = true\n\n[plugins.\"unvrs@unvrs-local\"]\nenabled = true\n\n[features]\nx = true\n";
        assert_eq!(codex_config_without_ours(after, home), before);
    }
}
