//! Read-only probes of the host for the preview view (`/preview`): what the kernel
//! snapshot does not carry yet, read from real state and never written.
//!
//! - `kernel/state.json` (the kernel's own state file): who raised each call, PID
//!   parents, driven sessions, bound threads and when they were last seen.
//! - `kernel/journal.jsonl` (the kernel's journal, DrvObs folded into uKe): the last
//!   events of each driver, for the Alive lights and their last error.
//! - `statvfs` on the UNVRS home: free disk.
//! - `ps -axo comm=`: which surface apps are open.
//! - the harness transcript of a driven worker (`~/.claude/projects/<cwd>/<session>.jsonl`):
//!   its modification time is when the worker last produced output.
//! - `projects/*/tasks/pid-*/ownership.json` (the kernel's context-ownership checks,
//!   context-ownership.md) with the task's `contract.json` and `result.json`.
//! - `git worktree list` for every git source in `sources.toml`: live workspaces, their
//!   branch, owner (from the path), merged state and size (`du`, measured in the
//!   background, slow).
//!
//! Every probe is cached (see the TTLs) so a page that re-renders every second does not
//! hammer the disk. A probe that fails leaves its field `None`: the view says "unknown".
//!
//! The only files written: the last good Claude usage reading (`kernel/claude-usage.json`,
//! `USAGE_FILE`) and the last worktree sizes (`kernel/ws-sizes.json`, `SIZES_FILE`), so a
//! kernel restart shows them with their age instead of "unknown"/"measuring" while fresh
//! readings are on their way.
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mtime_ms(p: &Path) -> Option<u64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    m.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// Free space where the UNVRS home lives (statvfs).
#[derive(Clone, Debug, PartialEq)]
pub struct Disk {
    pub path: String,
    pub free: u64,
    pub total: u64,
    pub at_ms: u64,
}

/// One PID as the kernel's state file has it (only what the view needs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PidInfo {
    pub pid: usize,
    pub parent: usize,
    pub rank: u64,
    pub state: String,
    pub project: Option<String>,
    pub updated: u64,
    /// Driven CPU: a turn is running now.
    pub busy: bool,
    pub session: Option<String>,
    pub cwd: Option<String>,
    pub harness: String,
    pub model: Option<String>,
}

/// One harness thread the kernel has seen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadInfo {
    pub pid: usize,
    pub app: String,
    pub harness: String,
    pub bound: bool,
    pub last_seen: u64,
    pub turn_open: bool,
}

/// What the view reads from `kernel/state.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KernelState {
    pub path: String,
    pub at_ms: u64,
    /// Call id -> the PID of the seat that raised it.
    pub raised_by: HashMap<String, usize>,
    /// Call id -> deadline (epoch ms), where one was set.
    pub until: HashMap<String, u64>,
    pub pids: HashMap<usize, PidInfo>,
    pub threads: Vec<ThreadInfo>,
}

impl KernelState {
    pub fn parse(v: &Value, path: &str, at_ms: u64) -> Self {
        let mut ks = KernelState {
            path: path.into(),
            at_ms,
            ..Default::default()
        };
        if let Some(holds) = v["holds"].as_object() {
            for (id, h) in holds {
                if let Some(by) = h["by"].as_u64() {
                    ks.raised_by.insert(id.clone(), by as usize);
                }
                if let Some(u) = h["until"].as_u64() {
                    ks.until.insert(id.clone(), u);
                }
            }
        }
        if let Some(pids) = v["pids"].as_object() {
            for p in pids.values() {
                let Some(pid) = p["pid"].as_u64() else {
                    continue;
                };
                let d = &p["driven"];
                ks.pids.insert(
                    pid as usize,
                    PidInfo {
                        pid: pid as usize,
                        parent: p["parent"].as_u64().unwrap_or(0) as usize,
                        rank: p["rank"].as_u64().unwrap_or(0),
                        state: p["state"].as_str().unwrap_or("").into(),
                        project: p["project"].as_str().map(str::to_owned),
                        updated: p["updated"].as_u64().unwrap_or(0),
                        busy: d["busy"].as_bool().unwrap_or(false),
                        session: d["session"].as_str().map(str::to_owned),
                        cwd: d["cwd"].as_str().map(str::to_owned),
                        harness: p["harness"].as_str().unwrap_or("").into(),
                        model: d["model"].as_str().map(str::to_owned),
                    },
                );
            }
        }
        if let Some(threads) = v["threads"].as_object() {
            for t in threads.values() {
                ks.threads.push(ThreadInfo {
                    pid: t["pid"].as_u64().unwrap_or(0) as usize,
                    app: t["app"].as_str().unwrap_or("").into(),
                    harness: t["harness"].as_str().unwrap_or("").into(),
                    bound: t["bound"].as_bool().unwrap_or(false),
                    last_seen: t["last_seen"].as_u64().unwrap_or(0),
                    turn_open: t["turn_open"].as_bool().unwrap_or(false),
                });
            }
        }
        ks
    }
}

/// The journal's tail, newest last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Journal {
    pub path: String,
    pub at_ms: u64,
    pub events: Vec<Value>,
}

/// Which surface apps have a process running (`ps`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Procs {
    pub at_ms: u64,
    pub t3: bool,
    pub codex: bool,
    pub claude_desktop: bool,
}

impl Procs {
    pub fn parse(ps: &str, at_ms: u64) -> Self {
        let mut p = Procs {
            at_ms,
            ..Default::default()
        };
        for line in ps.lines() {
            if line.contains("/T3 Code") {
                p.t3 = true;
            }
            if line.contains("/ChatGPT.app/") && line.contains("Codex")
                || line.contains("/Codex.app/")
            {
                p.codex = true;
            }
            if line.contains("/Applications/Claude.app/Contents/MacOS/Claude") {
                p.claude_desktop = true;
            }
        }
        p
    }
}

/// One git worktree of a source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub source: String,
    pub path: String,
    pub branch: Option<String>,
    pub head: String,
    /// Owner as the path says it: "PID 25" (a task dir), "Claude Code agent", "main
    /// checkout", or None when the path names nobody.
    pub owner: Option<String>,
    pub owner_pid: Option<usize>,
    /// Merged into the main checkout's branch (`into`), None when unknown.
    pub merged: Option<bool>,
    pub into: Option<String>,
    pub main: bool,
    /// `du -k -d 1` of the worktree (KiB -> bytes), and of its `target/` (rebuildable).
    pub size: Option<u64>,
    pub target: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspaces {
    pub at_ms: u64,
    /// When the sizes were measured (None: not yet).
    pub sized_ms: Option<u64>,
    /// A sizing pass is running: (worktrees measured so far, worktrees in the pass).
    pub measuring: Option<(usize, usize)>,
    pub list: Vec<Workspace>,
}

/// Transcript modification time per driven PID: when it last produced output.
pub type Outputs = HashMap<usize, u64>;

/// Everything the probes found. `None` = the probe could not read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Host {
    pub at_ms: u64,
    pub disk: Option<Disk>,
    pub state: Option<KernelState>,
    pub journal: Option<Journal>,
    pub procs: Option<Procs>,
    pub outputs: Outputs,
    pub workspaces: Option<Workspaces>,
    /// The last good Claude usage reading (its `at_ms` may be old: the page shows the age).
    pub claude_usage: Option<ClaudeUsage>,
    /// Why there is no fresh reading, when the last attempt failed.
    pub usage_error: Option<String>,
    /// A usage reading is on its way right now (the page says "reading…", not "unknown").
    pub usage_reading: bool,
    /// Context-ownership checks, newest first (`ownership`).
    pub ownership: Option<Vec<Value>>,
}

// ---------------------------------------------------------------- the probes

/// How many context-ownership checks the view keeps (newest first).
pub const OWNERSHIP_MAX: usize = 24;

fn read_json(p: &Path) -> Value {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// Every task's context-ownership check (`projects/*/tasks/pid-*/ownership.json`),
/// newest first, each with its contract's intent and harness, its result's `how` and
/// its directory.
pub fn ownership(home: &Path, max: usize) -> Option<Vec<Value>> {
    let mut out = vec![];
    for project in std::fs::read_dir(home.join("projects")).ok()?.flatten() {
        let Ok(tasks) = std::fs::read_dir(project.path().join("tasks")) else {
            continue;
        };
        for task in tasks.flatten() {
            let dir = task.path();
            let mut v = read_json(&dir.join("ownership.json"));
            if !v.is_object() {
                continue;
            }
            let contract = read_json(&dir.join("contract.json"));
            let result = read_json(&dir.join("result.json"));
            v["intent"] = contract["intent"].clone();
            v["harness"] = result["harness"]
                .as_str()
                .or(contract["harness"].as_str())
                .map(Value::from)
                .unwrap_or(Value::Null);
            v["model"] = result["actual_model"].clone();
            v["how"] = result["how"].clone();
            v["dir"] = Value::from(dir.display().to_string());
            out.push(v);
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v["at"].as_u64().unwrap_or(0)));
    out.truncate(max);
    Some(out)
}

pub fn disk(path: &Path) -> Option<Disk> {
    use std::ffi::CString;
    let c = CString::new(path.as_os_str().to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let unit = if st.f_frsize > 0 {
        st.f_frsize
    } else {
        st.f_bsize
    };
    Some(Disk {
        path: path.display().to_string(),
        free: st.f_bavail as u64 * unit,
        total: st.f_blocks as u64 * unit,
        at_ms: now_ms(),
    })
}

pub fn kernel_state(home: &Path) -> Option<KernelState> {
    let p = home.join("kernel/state.json");
    let text = std::fs::read_to_string(&p).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(KernelState::parse(&v, &p.display().to_string(), now_ms()))
}

/// The last `max` events of the journal (reads at most the last 1 MiB).
pub fn journal(home: &Path, max: usize) -> Option<Journal> {
    use std::io::{Read, Seek, SeekFrom};
    let p = home.join("kernel/journal.jsonl");
    let mut f = std::fs::File::open(&p).ok()?;
    let len = f.metadata().ok()?.len();
    const TAIL: u64 = 1 << 20;
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.take(TAIL).read_to_string(&mut buf).ok()?;
    let mut lines: Vec<&str> = buf.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // a partial first line
    }
    let from = lines.len().saturating_sub(max);
    let events = lines[from..]
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    Some(Journal {
        path: p.display().to_string(),
        at_ms: now_ms(),
        events,
    })
}

pub fn procs() -> Option<Procs> {
    let o = Command::new("ps")
        .args(["-axo", "comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| Procs::parse(&String::from_utf8_lossy(&o.stdout), now_ms()))
}

/// Claude Code keeps a session's transcript under a directory named after its cwd with
/// every character that is not a letter, digit or `-` turned into `-`.
pub fn claude_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn safe_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// When a driven claude worker last wrote to its transcript.
pub fn transcript_mtime(claude_home: &Path, p: &PidInfo) -> Option<u64> {
    if p.harness != "claude" {
        return None; // codex transcripts are not located yet: unknown
    }
    let session = p.session.as_deref().filter(|s| safe_id(s))?;
    let cwd = p.cwd.as_deref()?;
    let direct = claude_home
        .join("projects")
        .join(claude_project_dir(cwd))
        .join(format!("{session}.jsonl"));
    mtime_ms(&direct)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// The owner a worktree path names: a task dir (`…/tasks/pid-<n>/…`), a Claude Code
/// agent worktree, or nobody.
pub fn owner_of(path: &str) -> (Option<String>, Option<usize>) {
    if let Some(i) = path.find("/tasks/pid-") {
        let rest = &path[i + "/tasks/pid-".len()..];
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(pid) = n.parse::<usize>() {
            return (Some(format!("PID {pid}")), Some(pid));
        }
    }
    if path.contains("/.claude/worktrees/") {
        return (Some("Claude Code agent".into()), None);
    }
    (None, None)
}

/// Parses `git worktree list --porcelain`.
pub fn parse_worktrees(source: &str, text: &str) -> Vec<Workspace> {
    let mut out: Vec<Workspace> = vec![];
    for block in text.split("\n\n") {
        let mut w = Workspace {
            source: source.into(),
            ..Default::default()
        };
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                w.path = p.into();
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                w.head = h.into();
            } else if let Some(b) = line.strip_prefix("branch ") {
                w.branch = Some(b.trim_start_matches("refs/heads/").into());
            } else if line == "detached" {
                w.branch = None;
            }
        }
        if w.path.is_empty() {
            continue;
        }
        w.main = out.is_empty();
        let (owner, pid) = if w.main {
            (Some("main checkout".into()), None)
        } else {
            owner_of(&w.path)
        };
        w.owner = owner;
        w.owner_pid = pid;
        out.push(w);
    }
    out
}

/// The git sources in `sources.toml` (id, path), one per repository.
pub fn git_sources(home: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(home.join("sources.toml")) else {
        return vec![];
    };
    let Ok(v) = toml::from_str::<toml::Value>(&text) else {
        return vec![];
    };
    let mut out: Vec<(String, PathBuf)> = vec![];
    let mut seen = vec![];
    for s in v
        .get("source")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let (Some(id), Some(uri)) = (
            s.get("id").and_then(|x| x.as_str()),
            s.get("uri").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        let p = PathBuf::from(uri);
        if !p.join(".git").exists() {
            continue;
        }
        let common = git(
            &p,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .unwrap_or_else(|| p.display().to_string());
        if seen.contains(&common) {
            continue;
        }
        seen.push(common);
        out.push((id.into(), p));
    }
    out
}

/// Worktrees of every git source, with merged state (no sizes; see `measure`).
pub fn workspaces(home: &Path) -> Option<Workspaces> {
    let mut list = vec![];
    for (id, repo) in git_sources(home) {
        let Some(text) = git(&repo, &["worktree", "list", "--porcelain"]) else {
            continue;
        };
        let mut ws = parse_worktrees(&id, &text);
        let into = ws.first().and_then(|m| m.branch.clone());
        for w in ws.iter_mut() {
            if w.main || w.head.is_empty() {
                continue;
            }
            w.into = into.clone();
            if let Some(base) = &into {
                let o = Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(["merge-base", "--is-ancestor", &w.head, base])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .ok();
                w.merged = o.and_then(|s| match s.code() {
                    Some(0) => Some(true),
                    Some(1) => Some(false),
                    _ => None,
                });
            }
        }
        list.extend(ws);
    }
    Some(Workspaces {
        at_ms: now_ms(),
        sized_ms: None,
        measuring: None,
        list,
    })
}

/// Reads `du -k -d 1 <p>` output (BSD du refuses `-s` with `-d`): the total of `p` and
/// of `p/target` (0 when there is no target/), in bytes. None when `p` itself has no
/// line.
pub fn parse_du(p: &Path, text: &str) -> Option<(u64, u64)> {
    let (mut total, mut target) = (None, 0);
    for line in text.lines() {
        let Some((k, path)) = line.split_once('\t') else {
            continue;
        };
        let Ok(k) = k.trim().parse::<u64>() else {
            continue;
        };
        let path = Path::new(path);
        if path == p {
            total = Some(k * 1024);
        } else if path == p.join("target") {
            target = k * 1024;
        }
    }
    total.map(|t| (t, target))
}

/// Size of a worktree and of its `target/` (rebuildable), in one `du` walk.
fn du_split(p: &Path) -> Option<(u64, u64)> {
    if !p.exists() {
        return Some((0, 0));
    }
    let o = Command::new("du")
        .args(["-k", "-d", "1"])
        .arg(p)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    // du exits 1 on unreadable entries but still prints its totals
    parse_du(p, &String::from_utf8_lossy(&o.stdout))
}

/// Size of a worktree and of its `target/` (None: du could not read it).
pub type Size = (Option<u64>, Option<u64>);
pub type Sizes = HashMap<String, Size>;

/// How many worktrees are measured at once.
pub const DU_PARALLEL: usize = 4;

/// Sizes of every worktree and its `target/` (slow: runs `du`, `DU_PARALLEL` at a time);
/// `done` gets each one as it lands, so the page can show progress and partial totals.
pub fn measure(list: &[Workspace], done: &(dyn Fn(&str, Size) + Sync)) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..DU_PARALLEL.min(list.len()) {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(w) = list.get(i) else { break };
                    let r = du_split(Path::new(&w.path));
                    done(&w.path, (r.map(|r| r.0), r.map(|r| r.1)));
                }
            });
        }
    });
}

/// Where the last worktree sizes are saved, under the UNVRS home.
pub const SIZES_FILE: &str = "kernel/ws-sizes.json";

/// The saved sizes under `home` and when they were measured.
pub fn load_sizes(home: &Path) -> Option<(u64, Sizes)> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(SIZES_FILE)).ok()?).ok()?;
    let at = v["at_ms"].as_u64().filter(|t| *t > 0)?;
    let sizes = v["sizes"]
        .as_object()?
        .iter()
        .map(|(k, s)| (k.clone(), (s[0].as_u64(), s[1].as_u64())))
        .collect();
    Some((at, sizes))
}

/// Saves the sizes under `home` (atomically; only where the kernel dir exists).
pub fn save_sizes(home: &Path, at_ms: u64, sizes: &Sizes) -> std::io::Result<()> {
    let path = home.join(SIZES_FILE);
    let tmp = path.with_extension("json.tmp");
    let v = serde_json::json!({"at_ms": at_ms, "sizes": sizes});
    std::fs::write(&tmp, v.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// One Claude rate-limit window as Anthropic reports it: how much of it is used (0..100)
/// and when it resets (epoch ms).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageWindow {
    pub used_pct: f64,
    pub resets_at: Option<u64>,
}

/// Claude subscription usage, read from Anthropic's OAuth usage endpoint with the Claude
/// Code login on this machine: the same numbers Claude Code's own `/usage` screen shows.
/// `at_ms` is when the reading was taken; the page shows its age.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeUsage {
    pub at_ms: u64,
    /// "max", "pro", "team"… as the login records it; "" when it does not say.
    pub plan: String,
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    pub source: String,
}

pub const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CLAUDE_USAGE_SOURCE: &str = "api.anthropic.com/api/oauth/usage (Claude Code login)";

/// The Claude Code OAuth login: (access token, plan). macOS keeps it in the Keychain item
/// "Claude Code-credentials"; other hosts in `~/.claude/.credentials.json`. Never logged.
pub fn claude_login(claude_home: &Path, now: u64) -> Result<(String, String), String> {
    let from_keychain = || -> Option<String> {
        let o = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let raw = from_keychain()
        .or_else(|| std::fs::read_to_string(claude_home.join(".credentials.json")).ok())
        .ok_or("no Claude Code login on this machine (Keychain / ~/.claude/.credentials.json)")?;
    let v: Value =
        serde_json::from_str(&raw).map_err(|_| "the Claude Code login is not readable")?;
    let o = &v["claudeAiOauth"];
    let token = o["accessToken"]
        .as_str()
        .ok_or("the Claude Code login has no access token")?;
    if let Some(exp) = o["expiresAt"].as_u64()
        && exp <= now
    {
        return Err(
            "the Claude Code login has expired; open Claude Code once to refresh it".into(),
        );
    }
    Ok((
        token.to_owned(),
        o["subscriptionType"].as_str().unwrap_or("").to_owned(),
    ))
}

/// Epoch ms of an RFC 3339 time such as `2026-09-29T21:40:00.196022+00:00` or `…Z`.
pub fn rfc3339_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (time, off) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0i64)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (t, o) = rest.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let mut hm = o[1..].split(':').map(|x| x.parse::<i64>().ok());
        let (oh, om) = (hm.next()??, hm.next().flatten().unwrap_or(0));
        (t, sign * (oh * 60 + om))
    } else {
        (rest, 0)
    };
    let mut t = time.split(':');
    let (hh, mm) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let sec_f = t.next().unwrap_or("0").parse::<f64>().ok()?;
    // days from civil (Howard Hinnant)
    let (yy, mo) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mo + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days as f64 * 86_400.0 + (hh * 3600 + mm * 60 - off * 60) as f64 + sec_f;
    (secs > 0.0).then_some((secs * 1000.0) as u64)
}

/// Reads Anthropic's usage JSON (what `CLAUDE_USAGE_URL` returns) into a `ClaudeUsage`.
pub fn parse_claude_usage(v: &Value, plan: &str, at_ms: u64) -> Option<ClaudeUsage> {
    let window = |w: &Value| -> Option<UsageWindow> {
        Some(UsageWindow {
            used_pct: w["utilization"].as_f64()?.clamp(0.0, 100.0),
            resets_at: w["resets_at"].as_str().and_then(rfc3339_ms),
        })
    };
    let five_hour = window(&v["five_hour"]);
    let seven_day = window(&v["seven_day"]);
    (five_hour.is_some() || seven_day.is_some()).then(|| ClaudeUsage {
        at_ms,
        plan: plan.into(),
        five_hour,
        seven_day,
        source: CLAUDE_USAGE_SOURCE.into(),
    })
}

/// One reading of Claude subscription usage (blocking, ~1 s; runs `curl`, the token goes
/// to curl through its stdin config and never onto a command line). `Err` says why it is
/// unknown, in words the page can show.
pub fn claude_usage(claude_home: &Path, now: u64) -> Result<ClaudeUsage, String> {
    let (token, plan) = claude_login(claude_home, now)?;
    let mut child = Command::new("curl")
        .args(["-sS", "-m", "12", "-K", "-", "-w", "\n%{http_code}"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "curl is not available".to_string())?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or("curl stdin")?;
        let cfg = format!(
            "url = \"{CLAUDE_USAGE_URL}\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\nuser-agent = \"unvrs-observatory\"\n"
        );
        stdin.write_all(cfg.as_bytes()).map_err(|_| "curl stdin")?;
    }
    let out = child
        .wait_with_output()
        .map_err(|_| "curl did not finish")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text
        .trim_end()
        .rsplit_once('\n')
        .ok_or("no answer from api.anthropic.com")?;
    match code.trim() {
        "200" => {}
        "401" | "403" => return Err("api.anthropic.com refused the Claude Code login (HTTP 401): open Claude Code once to refresh it".into()),
        "000" | "" => return Err("api.anthropic.com is not reachable (offline?)".into()),
        c => return Err(format!("api.anthropic.com answered HTTP {c}")),
    }
    let v: Value = serde_json::from_str(body).map_err(|_| "api.anthropic.com sent no JSON")?;
    parse_claude_usage(&v, &plan, now)
        .ok_or_else(|| "api.anthropic.com sent no usage windows".into())
}

/// Where the last good usage reading is saved, under the UNVRS home.
pub const USAGE_FILE: &str = "kernel/claude-usage.json";

pub fn usage_to_json(u: &ClaudeUsage) -> Value {
    let w = |w: &Option<UsageWindow>| match w {
        Some(w) => serde_json::json!({"used_pct": w.used_pct, "resets_at": w.resets_at}),
        None => Value::Null,
    };
    serde_json::json!({
        "at_ms": u.at_ms,
        "plan": u.plan,
        "source": u.source,
        "five_hour": w(&u.five_hour),
        "seven_day": w(&u.seven_day),
    })
}

/// Reads what `usage_to_json` wrote; None when it is not a usable reading.
pub fn usage_from_json(v: &Value) -> Option<ClaudeUsage> {
    let w = |w: &Value| {
        Some(UsageWindow {
            used_pct: w["used_pct"].as_f64()?,
            resets_at: w["resets_at"].as_u64(),
        })
    };
    let (five_hour, seven_day) = (w(&v["five_hour"]), w(&v["seven_day"]));
    (five_hour.is_some() || seven_day.is_some())
        .then(|| ClaudeUsage {
            at_ms: v["at_ms"].as_u64().unwrap_or(0),
            plan: v["plan"].as_str().unwrap_or("").to_owned(),
            five_hour,
            seven_day,
            source: v["source"]
                .as_str()
                .unwrap_or(CLAUDE_USAGE_SOURCE)
                .to_owned(),
        })
        .filter(|u| u.at_ms > 0)
}

/// The saved reading under `home`, if any.
pub fn load_usage(home: &Path) -> Option<ClaudeUsage> {
    let text = std::fs::read_to_string(home.join(USAGE_FILE)).ok()?;
    usage_from_json(&serde_json::from_str(&text).ok()?)
}

/// Saves a good reading under `home` (atomically; only where the kernel dir exists).
pub fn save_usage(home: &Path, u: &ClaudeUsage) -> std::io::Result<()> {
    let path = home.join(USAGE_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, usage_to_json(u).to_string())?;
    std::fs::rename(&tmp, &path)
}

/// How often the Claude usage is re-read (a network call).
pub const USAGE_TTL: u64 = 3 * 60_000;
/// A usage reading older than this is shown as stale ("unknown" tone).
pub const USAGE_STALE_MS: u64 = 30 * 60_000;

// ---------------------------------------------------------------- cached

struct Cached<T> {
    at: u64,
    val: Option<T>,
}
impl<T: Clone> Cached<T> {
    fn get(&mut self, ttl_ms: u64, now: u64, f: impl FnOnce() -> Option<T>) -> Option<T> {
        if self.at == 0 || now.saturating_sub(self.at) >= ttl_ms {
            self.val = f();
            self.at = now;
        }
        self.val.clone()
    }
}
impl<T> Default for Cached<T> {
    fn default() -> Self {
        Cached { at: 0, val: None }
    }
}

#[derive(Default)]
struct Caches {
    disk: Cached<Disk>,
    state: Cached<KernelState>,
    journal: Cached<Journal>,
    procs: Cached<Procs>,
    ws: Cached<Workspaces>,
    ownership: Cached<Vec<Value>>,
    outputs: HashMap<usize, (u64, Option<u64>)>,
    sizes: Sizes,
    sized_ms: Option<u64>,
    measuring: bool,
    /// Progress of the running pass: (measured, in the pass).
    measure_progress: (usize, usize),
    /// When the last pass started (a failed pass is retried after `SIZE_RETRY`).
    size_started: Option<u64>,
    usage: Option<ClaudeUsage>,
    usage_error: Option<String>,
    usage_tried: u64,
    usage_refreshing: bool,
    /// The saved readings have been loaded (once, on the first read).
    loaded: bool,
}

/// The probes with their caches. Clone-cheap; share one per server.
#[derive(Clone, Default)]
pub struct Probes {
    inner: Arc<Mutex<Caches>>,
}

pub const DISK_TTL: u64 = 10_000;
pub const STATE_TTL: u64 = 2_000;
pub const JOURNAL_TTL: u64 = 2_000;
pub const PROCS_TTL: u64 = 10_000;
pub const WS_TTL: u64 = 60_000;
pub const OUTPUT_TTL: u64 = 2_000;
pub const OWNERSHIP_TTL: u64 = 5_000;
/// Worktree sizes are re-measured this often (du is slow).
pub const SIZE_TTL: u64 = 15 * 60_000;
/// A pass that did not finish, or new worktrees without a size, are retried this often.
pub const SIZE_RETRY: u64 = 60_000;

/// Ends a sizing pass however it ends (done, error or panic): `measuring` is always reset,
/// so the page can never say "measuring" forever.
struct PassGuard {
    inner: Arc<Mutex<Caches>>,
    home: PathBuf,
    full: bool,
    finished: bool,
}
impl Drop for PassGuard {
    fn drop(&mut self) {
        let mut c = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        c.measuring = false;
        if self.finished && self.full {
            let at = now_ms();
            c.sized_ms = Some(at);
            let _ = save_sizes(&self.home, at, &c.sizes);
        }
    }
}

impl Probes {
    /// Reads the host around the UNVRS home `home` (blocking; call off the async runtime).
    pub fn read(&self, home: &Path) -> Host {
        let now = now_ms();
        let claude_home = std::env::var("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                Path::new(&std::env::var("HOME").unwrap_or_default()).join(".claude")
            });
        let mut c = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let disk = c.disk.get(DISK_TTL, now, || disk(home));
        let state = c.state.get(STATE_TTL, now, || kernel_state(home));
        let journal = c.journal.get(JOURNAL_TTL, now, || journal(home, 600));
        let procs = c.procs.get(PROCS_TTL, now, procs);
        let mut ws = c.ws.get(WS_TTL, now, || workspaces(home));
        let ownership = c
            .ownership
            .get(OWNERSHIP_TTL, now, || ownership(home, OWNERSHIP_MAX));
        // after a restart the saved readings stand in (with their age) until fresh ones land
        if !c.loaded {
            c.loaded = true;
            if c.usage.is_none() {
                c.usage = load_usage(home);
            }
            if c.sized_ms.is_none()
                && let Some((at, sizes)) = load_sizes(home)
            {
                c.sized_ms = Some(at);
                c.sizes = sizes;
            }
        }
        // the usage reading is a network call: it runs on its own thread, never on the
        // render path; the page shows the last good reading (with its age) meanwhile
        if !c.usage_refreshing
            && (c.usage_tried == 0 || now.saturating_sub(c.usage_tried) >= USAGE_TTL)
        {
            c.usage_tried = now;
            c.usage_refreshing = true;
            let (inner, claude_home) = (self.inner.clone(), claude_home.clone());
            let home = home.to_path_buf();
            std::thread::spawn(move || {
                let r = claude_usage(&claude_home, now_ms());
                if let Ok(u) = &r {
                    let _ = save_usage(&home, u);
                }
                let mut c = inner.lock().unwrap_or_else(|e| e.into_inner());
                match r {
                    Ok(u) => {
                        c.usage = Some(u);
                        c.usage_error = None;
                    }
                    Err(e) => c.usage_error = Some(e),
                }
                c.usage_refreshing = false;
            });
        }
        let mut outputs = Outputs::new();
        if let Some(st) = &state {
            for p in st.pids.values().filter(|p| p.state == "working") {
                let hit = c.outputs.get(&p.pid).copied();
                let t = match hit {
                    Some((at, t)) if now.saturating_sub(at) < OUTPUT_TTL => t,
                    _ => {
                        let t = transcript_mtime(&claude_home, p);
                        c.outputs.insert(p.pid, (now, t));
                        t
                    }
                };
                if let Some(t) = t {
                    outputs.insert(p.pid, t);
                }
            }
        }
        // sizes: measured in the background, DU_PARALLEL at a time; the page shows the
        // last sizes (saved across restarts) and the progress of the running pass
        if let Some(w) = &ws
            && !c.measuring
        {
            let retry = c
                .size_started
                .is_none_or(|t| now.saturating_sub(t) >= SIZE_RETRY);
            let full = match c.sized_ms {
                Some(t) => now.saturating_sub(t) >= SIZE_TTL,
                None => retry,
            };
            let list: Vec<Workspace> = if full {
                w.list.clone()
            } else if retry {
                // new worktrees since the last pass: size just those
                w.list
                    .iter()
                    .filter(|x| !c.sizes.contains_key(&x.path))
                    .cloned()
                    .collect()
            } else {
                vec![]
            };
            if !list.is_empty() {
                c.measuring = true;
                c.size_started = Some(now);
                c.measure_progress = (0, list.len());
                if full {
                    // forget worktrees that are gone; the others keep their last size
                    let keep: std::collections::HashSet<&str> =
                        list.iter().map(|x| x.path.as_str()).collect();
                    c.sizes.retain(|k, _| keep.contains(k.as_str()));
                }
                let me = self.inner.clone();
                let home = home.to_path_buf();
                let spawned = std::thread::Builder::new()
                    .name("observatory-du".into())
                    .spawn(move || {
                        let mut guard = PassGuard {
                            inner: me.clone(),
                            home,
                            full,
                            finished: false,
                        };
                        measure(&list, &|path, sz| {
                            let mut c = me.lock().unwrap_or_else(|e| e.into_inner());
                            c.sizes.insert(path.to_owned(), sz);
                            c.measure_progress.0 += 1;
                        });
                        guard.finished = true;
                    });
                if spawned.is_err() {
                    c.measuring = false;
                }
            }
        }
        if let Some(w) = ws.as_mut() {
            w.sized_ms = c.sized_ms;
            w.measuring = c.measuring.then_some(c.measure_progress);
            for x in w.list.iter_mut() {
                if let Some((s, t)) = c.sizes.get(&x.path) {
                    x.size = *s;
                    x.target = *t;
                }
            }
        }
        Host {
            at_ms: now,
            disk,
            state,
            journal,
            procs,
            outputs,
            workspaces: ws,
            claude_usage: c.usage.clone(),
            usage_error: c.usage_error.clone(),
            usage_reading: c.usage_refreshing,
            ownership,
        }
    }
}

/// How long to wait before giving up on an upstream kernel (the preview kernel's fetch).
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_millis(1500);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ownership_checks_are_read_newest_first_with_their_task() {
        let home =
            std::env::temp_dir().join(format!("obs-own-{}-{}", std::process::id(), now_ms()));
        for (pid, at) in [(7, 100), (8, 200)] {
            let dir = home.join(format!("projects/p/tasks/pid-{pid}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("ownership.json"),
                format!("{{\"pid\":{pid},\"at\":{at},\"verdict\":\"accepted\"}}"),
            )
            .unwrap();
            std::fs::write(
                dir.join("contract.json"),
                "{\"intent\":\"do it\",\"harness\":\"codex\"}",
            )
            .unwrap();
        }
        std::fs::create_dir_all(home.join("projects/p/tasks/pid-9")).unwrap();
        let v = ownership(&home, 10).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["pid"], 8);
        assert_eq!(v[0]["intent"], "do it");
        assert_eq!(v[0]["harness"], "codex");
        assert_eq!(ownership(&home, 1).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn claude_project_dirs_match_claude_codes_naming() {
        assert_eq!(
            claude_project_dir("/Users/m/.unvrs/projects/unvrs-rs/tasks/pid-25"),
            "-Users-m--unvrs-projects-unvrs-rs-tasks-pid-25"
        );
    }

    #[test]
    fn worktree_owners_come_from_the_path() {
        assert_eq!(
            owner_of("/Users/m/.unvrs/projects/unvrs-rs/tasks/pid-24/wt"),
            (Some("PID 24".into()), Some(24))
        );
        assert_eq!(
            owner_of("/Users/m/github/unvrs-rs/.claude/worktrees/agent-a05"),
            (Some("Claude Code agent".into()), None)
        );
        assert_eq!(owner_of("/Users/m/github/unvrs-bridge-deck"), (None, None));
    }

    #[test]
    fn porcelain_worktrees_parse_with_the_main_checkout_first() {
        let text = "worktree /r\nHEAD aaa\nbranch refs/heads/bsla/iterate\n\nworktree /x/tasks/pid-9/wt\nHEAD bbb\nbranch refs/heads/fix/t3\n\nworktree /y\nHEAD ccc\ndetached\n";
        let ws = parse_worktrees("unvrs-rs", text);
        assert_eq!(ws.len(), 3);
        assert!(ws[0].main && ws[0].owner.as_deref() == Some("main checkout"));
        assert_eq!(ws[0].branch.as_deref(), Some("bsla/iterate"));
        assert_eq!(ws[1].owner_pid, Some(9));
        assert_eq!(ws[1].branch.as_deref(), Some("fix/t3"));
        assert_eq!(ws[2].branch, None);
        assert_eq!(ws[2].owner, None);
    }

    #[test]
    fn surface_processes_are_recognised() {
        let ps = "/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code (Nightly)\n/Applications/ChatGPT.app/Contents/Frameworks/Codex Framework.framework/Versions/1/Helpers/Codex (Service).app/Contents/MacOS/Codex (Service)\n/usr/bin/login\n";
        let p = Procs::parse(ps, 1);
        assert!(p.t3 && p.codex && !p.claude_desktop);
        let p = Procs::parse("/Applications/Claude.app/Contents/MacOS/Claude\n", 1);
        assert!(p.claude_desktop && !p.t3);
    }

    #[test]
    fn kernel_state_reads_raisers_threads_and_driven_sessions() {
        let v = json!({
            "holds": {"d1": {"by": 2, "until": null}, "d2": {"by": null, "until": 99}},
            "pids": {"25": {"pid": 25, "parent": 2, "rank": 3, "state": "working", "project": "unvrs-rs",
                "harness": "claude", "updated": 7,
                "driven": {"busy": true, "session": "s-1", "cwd": "/t", "model": null}}},
            "threads": {"claude:x": {"pid": 1, "app": "t3", "harness": "claude", "bound": true, "last_seen": 5, "turn_open": true}}
        });
        let ks = KernelState::parse(&v, "state.json", 1);
        assert_eq!(ks.raised_by.get("d1"), Some(&2));
        assert_eq!(ks.raised_by.get("d2"), None);
        assert_eq!(ks.until.get("d2"), Some(&99));
        let p = &ks.pids[&25];
        assert!(p.busy && p.parent == 2 && p.session.as_deref() == Some("s-1"));
        assert_eq!(p.model, None);
        assert!(ks.threads[0].turn_open && ks.threads[0].app == "t3");
    }

    #[test]
    fn disk_of_the_temp_dir_is_readable() {
        let d = disk(&std::env::temp_dir()).expect("statvfs");
        assert!(d.total > 0 && d.free <= d.total);
    }

    #[test]
    fn rfc3339_times_become_epoch_ms() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(
            rfc3339_ms("2026-09-29T21:40:00.196022+00:00"),
            Some(1_790_718_000_196)
        );
        assert_eq!(
            rfc3339_ms("2026-09-29T16:40:00-05:00"),
            Some(1_790_718_000_000)
        );
        assert_eq!(rfc3339_ms("nonsense"), None);
    }

    #[test]
    fn du_depth_one_gives_the_total_and_target_in_one_walk() {
        let p = Path::new("/w/a");
        let out = "8\t/w/a/src\n4096\t/w/a/target\n5000\t/w/a\n";
        assert_eq!(parse_du(p, out), Some((5000 * 1024, 4096 * 1024)));
        assert_eq!(
            parse_du(p, "12\t/w/a\n"),
            Some((12 * 1024, 0)),
            "no target/"
        );
        assert_eq!(parse_du(p, "12\t/w/b\n"), None);
        // a real tree, measured with the real du, 4 at a time
        let root = std::env::temp_dir().join(format!("obs-du-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("target")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("target/blob"), vec![7u8; 256 * 1024]).unwrap();
        let list: Vec<Workspace> = [&a, &b, &root.join("gone")]
            .iter()
            .map(|p| Workspace {
                path: p.to_string_lossy().into(),
                ..Default::default()
            })
            .collect();
        let got = Mutex::new(Sizes::new());
        measure(&list, &|p, sz| {
            got.lock().unwrap().insert(p.to_owned(), sz);
        });
        let got = got.into_inner().unwrap();
        assert_eq!(got.len(), 3);
        let (sa, ta) = got[&list[0].path];
        assert!(
            ta.unwrap() >= 256 * 1024 && sa.unwrap() >= ta.unwrap(),
            "{sa:?} {ta:?}"
        );
        assert_eq!(got[&list[1].path].1, Some(0));
        assert_eq!(
            got[&list[2].path],
            (Some(0), Some(0)),
            "a gone worktree is empty"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sizes_survive_a_restart_and_a_panicking_pass_never_stays_measuring() {
        let home = std::env::temp_dir().join(format!("obs-sizes-{}", std::process::id()));
        std::fs::create_dir_all(home.join("kernel")).unwrap();
        let mut sizes = Sizes::new();
        sizes.insert("/w/a".into(), (Some(5 << 30), Some(4 << 30)));
        sizes.insert("/w/b".into(), (None, None));
        assert_eq!(load_sizes(&home), None);
        save_sizes(&home, 42, &sizes).unwrap();
        assert_eq!(load_sizes(&home), Some((42, sizes)));
        let _ = std::fs::remove_dir_all(&home);
        // a pass that panics: the guard still resets `measuring` and claims no pass
        let inner = Arc::new(Mutex::new(Caches {
            measuring: true,
            ..Default::default()
        }));
        let me = inner.clone();
        let r = std::thread::spawn(move || {
            let _guard = PassGuard {
                inner: me,
                home: PathBuf::from("/nonexistent"),
                full: true,
                finished: false,
            };
            panic!("du blew up");
        })
        .join();
        assert!(r.is_err());
        let c = inner.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!c.measuring);
        assert_eq!(c.sized_ms, None);
    }

    #[test]
    fn a_saved_usage_reading_survives_a_restart_and_junk_is_ignored() {
        let home = std::env::temp_dir().join(format!("obs-usage-{}", std::process::id()));
        std::fs::create_dir_all(home.join("kernel")).unwrap();
        let u = ClaudeUsage {
            at_ms: 1_000,
            plan: "max".into(),
            five_hour: Some(UsageWindow {
                used_pct: 5.0,
                resets_at: Some(9_000),
            }),
            seven_day: None,
            source: CLAUDE_USAGE_SOURCE.into(),
        };
        assert_eq!(load_usage(&home), None);
        save_usage(&home, &u).unwrap();
        assert_eq!(load_usage(&home), Some(u));
        std::fs::write(home.join(USAGE_FILE), "{\"at_ms\": 5}").unwrap();
        assert_eq!(load_usage(&home), None, "no windows: not a reading");
        std::fs::write(home.join(USAGE_FILE), "not json").unwrap();
        assert_eq!(load_usage(&home), None);
        // no kernel dir: nothing is written, nothing breaks
        let bare = home.join("bare");
        assert!(save_usage(&bare, &load_usage(&home).unwrap_or_default()).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_usage_json_reads_both_windows_and_never_invents_one() {
        let v = json!({"five_hour": {"utilization": 35.0, "resets_at": "2026-09-29T21:40:00+00:00"},
                       "seven_day": {"utilization": 3.0, "resets_at": null}});
        let u = parse_claude_usage(&v, "max", 5).unwrap();
        assert_eq!(u.five_hour.as_ref().unwrap().used_pct, 35.0);
        assert_eq!(
            u.five_hour.as_ref().unwrap().resets_at,
            Some(1_790_718_000_000)
        );
        assert_eq!(u.seven_day.as_ref().unwrap().used_pct, 3.0);
        assert!(u.seven_day.as_ref().unwrap().resets_at.is_none());
        assert_eq!((u.at_ms, u.plan.as_str()), (5, "max"));
        assert!(
            parse_claude_usage(&json!({"five_hour": null, "seven_day": null}), "", 1).is_none()
        );
        assert!(parse_claude_usage(&json!({}), "", 1).is_none());
    }
}
