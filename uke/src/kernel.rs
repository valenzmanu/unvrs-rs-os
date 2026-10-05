//! The kernel daemon (D60): one process per UNVRS home owns the socket, the process
//! table, seats and their wake queues, holds, memory, context sources and the
//! journal. Hooks, `unvrs ctl`, `unvrs mcp` and the Observatory are clients. Files under
//! the home stay the record. The kernel knows ranks and records; every sentence a
//! model or the captain reads comes from the mapp (`Mapp`).
use crate::signals::CommandTracking;
mod bind;
mod captain;
mod crew;
mod driven;
mod econ;
mod hot;
mod ops;
mod ownership;
pub mod procinfo;
mod recover;
mod seed;
mod snapshot;
mod state;
mod watchdog;

pub use captain::{Captain, parse as parse_captain, words};
pub use hot::HOT_BYTES;
pub use recover::git_checkpoint;
pub use state::{Away, Driven, Hold, Mail, PidRec, State, ThreadRec, Wake, now_ms};

use crate::{BriefFold, FoldJob, FoldOutcome, MemoryIndex};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use state::Journal;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::{
        fs::PermissionsExt,
        io::AsRawFd,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const KERNEL_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit and ref this binary was built from; `unvrs deploy` bakes them in
/// (docs/design/deploy-loop.md). None for a plain `cargo build`.
pub const BUILD_COMMIT: Option<&str> = option_env!("UNVRS_BUILD_COMMIT");
pub const BUILD_REF: Option<&str> = option_env!("UNVRS_BUILD_REF");
pub const MAPP: &str = "unvrs";

static EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// The `unvrs` binary seats and workers call: the home's stable path when present,
/// else the kernel's own binary. Shell-quoted.
pub(crate) fn unvrs_cmd() -> String {
    let exe = EXE.get().cloned().unwrap_or_else(|| PathBuf::from("unvrs"));
    let s = exe.display().to_string();
    if s.contains(' ') || s.contains('\'') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s
    }
}

/// The mapp's words. The kernel asks for every sentence a model or the captain reads;
/// it keeps only mechanics (ranks, seats, holds, wakes, records).
pub trait Mapp: Send + Sync + 'static {
    /// Role and rules at the top of a seat's or worker's hot set.
    fn role(&self, rank: u8, project: Option<&str>, purpose: Option<&str>) -> String;
    /// How this rank calls UNVRS (the MCP tool; the CLI for driven runs).
    fn ops(&self, rank: u8, cli: &str, driven: bool) -> String;
    /// Header for a thread that just took a seat. `how`: bound · moved · swapped · rebound.
    fn seated(&self, rank: u8, project: Option<&str>, how: &str, from: Option<&str>) -> String;
    /// Told once to a thread whose seat moved to another thread.
    fn detached(&self, rank: u8, project: Option<&str>, to: &str) -> String;
    /// The driven L3 protocol and first prompt.
    fn worker_prompt(
        &self,
        pid: usize,
        harness: &str,
        authority: &str,
        package: &str,
        cli: &str,
        handed_from: Option<(usize, String)>,
    ) -> String;
    /// A later L3 turn.
    fn worker_continue(&self, next: &str, mail: &str) -> String;
    /// A detached seat run (C9): the seat handles its wakes on its own.
    fn seat_run(
        &self,
        rank: u8,
        project: Option<&str>,
        hot: &str,
        wakes: &str,
        cli: &str,
    ) -> String;
    /// The captain's digest from kernel facts (D51).
    fn digest(&self, facts: &Value) -> String;
    /// A journal event in outcome words for the captain, or None to leave it out.
    fn event_text(&self, kind: &str, fields: &Value) -> Option<String>;
    /// Help for the entry commands and the seat verbs.
    fn help(&self) -> String;
    /// A result sent back by the context-ownership check (`bounce` of `max`).
    fn ownership_bounce(&self, leaks: &[String], bounce: u32, max: u32) -> String {
        format!(
            "context-ownership check failed ({bounce}/{max}): {}",
            leaks.join("; ")
        )
    }
}

/// A mapp with no words (tests of mechanics only): every text is empty or raw facts.
pub struct SilentMapp;
impl Mapp for SilentMapp {
    fn role(&self, _: u8, _: Option<&str>, _: Option<&str>) -> String {
        String::new()
    }
    fn ops(&self, _: u8, _: &str, _: bool) -> String {
        String::new()
    }
    fn seated(&self, _: u8, _: Option<&str>, how: &str, _: Option<&str>) -> String {
        how.into()
    }
    fn detached(&self, _: u8, _: Option<&str>, to: &str) -> String {
        format!("detached: {to}")
    }
    fn worker_prompt(
        &self,
        _: usize,
        _: &str,
        _: &str,
        package: &str,
        _: &str,
        _: Option<(usize, String)>,
    ) -> String {
        package.into()
    }
    fn worker_continue(&self, next: &str, mail: &str) -> String {
        if mail.is_empty() {
            next.into()
        } else {
            format!("{next}\n{mail}")
        }
    }
    fn seat_run(&self, _: u8, _: Option<&str>, hot: &str, wakes: &str, _: &str) -> String {
        format!("{hot}\n{wakes}")
    }
    fn digest(&self, facts: &Value) -> String {
        facts.to_string()
    }
    fn event_text(&self, _: &str, _: &Value) -> Option<String> {
        None
    }
    fn help(&self) -> String {
        String::new()
    }
}

/// A UNVRS home: `~/.unvrs`, or `UNVRS_HOME` (tests use a temp dir).
#[derive(Clone, Debug)]
pub struct Universe {
    root: PathBuf,
}

impl Universe {
    /// The home directory path (`UNVRS_HOME`, else `~/.unvrs`), without creating it.
    pub fn home_path() -> Result<PathBuf> {
        Ok(match std::env::var_os("UNVRS_HOME") {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => PathBuf::from(std::env::var_os("HOME").context("HOME unset")?).join(".unvrs"),
        })
    }
    /// `UNVRS_HOME`, else `~/.unvrs` (created when missing).
    pub fn home() -> Result<Self> {
        Self::at(&Self::home_path()?)
    }
    /// Opens (creating) a home directory. `$HOME` itself stays refused.
    pub fn at(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)
            .with_context(|| format!("UNVRS home {} cannot be created", path.display()))?;
        let root = path.canonicalize()?;
        if let Some(home) = std::env::var_os("HOME")
            && Path::new(&home).canonicalize().ok().as_deref() == Some(root.as_path())
        {
            bail!("$HOME itself cannot be the UNVRS home; use ~/.unvrs");
        }
        Ok(Self { root })
    }
    pub fn init(&self) -> Result<()> {
        fs::create_dir_all(self.kernel_dir())?;
        let _ = fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700));
        let marker = self.root.join("home.json");
        if !marker.exists() {
            fs::write(
                &marker,
                serde_json::to_vec_pretty(
                    &json!({"home": self.root, "created": crate::mission::now(), "version": KERNEL_VERSION}),
                )?,
            )?;
        }
        Ok(())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn kernel_dir(&self) -> PathBuf {
        self.root.join("kernel")
    }
    pub fn journal_path(&self) -> PathBuf {
        self.kernel_dir().join("journal.jsonl")
    }
    pub fn state_path(&self) -> PathBuf {
        self.kernel_dir().join("state.json")
    }
    /// One marker file per thread the kernel knows (bound, or owed a notice). A hook
    /// for a thread without a marker exits at once: unbound threads cost nothing.
    pub fn thread_marker(&self, harness: &str, session: &str) -> PathBuf {
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
        self.kernel_dir().join("threads").join(clean)
    }
    pub fn project_dir(&self, id: &str) -> PathBuf {
        self.root.join("projects").join(id)
    }
    /// `kernel/kernel.sock`, or a private temp dir when that path is too long.
    pub fn socket_path(&self) -> PathBuf {
        let local = self.kernel_dir().join("kernel.sock");
        if local.as_os_str().len() < 100 {
            return local;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&self.root, &mut h);
        let uid = unsafe { libc::geteuid() };
        PathBuf::from(format!(
            "/tmp/unvrs-{uid}-{:016x}/kernel.sock",
            std::hash::Hasher::finish(&h)
        ))
    }
}

// ───────────────────────── client ─────────────────────────

/// Sends one request line and reads one response line.
pub fn request(u: &Universe, req: &Value, timeout: Duration) -> Result<Value> {
    let socket = u.socket_path();
    let mut conn = UnixStream::connect(&socket)
        .with_context(|| format!("No kernel running for {}", u.root().display()))?;
    conn.set_read_timeout(Some(timeout))?;
    conn.set_write_timeout(Some(timeout))?;
    crate::write_json(&mut conn, req)?;
    let mut line = String::new();
    BufReader::new(conn)
        .take(8 * 1024 * 1024)
        .read_line(&mut line)?;
    ensure!(!line.is_empty(), "Kernel closed the connection");
    let v: Value = serde_json::from_str(&line)?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        bail!("{e}");
    }
    Ok(v)
}

pub fn running(u: &Universe) -> bool {
    UnixStream::connect(u.socket_path()).is_ok()
}

/// Starts a detached kernel process (`<exe> kernel run`) unless one is running, and
/// waits for its socket. The LaunchAgent normally keeps it up; this covers the gaps.
pub fn ensure_running(u: &Universe, exe: &Path, wait: Duration) -> Result<()> {
    if running(u) {
        return Ok(());
    }
    u.init()?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(u.kernel_dir().join("kernel.log"))?;
    let mut cmd = Command::new(exe);
    cmd.args(["kernel", "run"])
        .env("UNVRS_HOME", u.root())
        .current_dir(u.root())
        .env_remove("UNVRS_DRIVEN_PID")
        .env_remove("UNVRS_DAEMON")
        .stdin(Stdio::null());
    // The kernel outlives the thread whose hook started it: none of that thread's
    // harness identity may leak into the daemon or its driven runs.
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("CLAUDE_CODE_")
            || k.starts_with("CODEX_THREAD")
            || k.starts_with("CODEX_SESSION")
            || k.starts_with("CODEX_SANDBOX")
            || k == "CLAUDECODE"
            || k == "CODEX_CI"
        {
            cmd.env_remove(k.as_ref());
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            // Separate group (set by spawn_owned), and no inherited descriptors: a harness waits for EOF on the
            // hook's pipes, so the kernel must not keep them open.

            for fd in 3..1024 {
                libc::close(fd);
            }
            Ok(())
        });
    }
    cmd.spawn_owned().context("Kernel launch failed")?.detach();
    let until = Instant::now() + wait;
    while Instant::now() < until {
        if running(u) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    bail!("Kernel did not start within {:?}", wait)
}

// ───────────────────────── server ─────────────────────────

/// Work the kernel delegates to drivers (DrvHdff folds, DrvAgent CPUs).
pub trait Drivers: Send + Sync + 'static {
    /// The compaction worker: returns a new brief for the job.
    fn fold(&self, job: &FoldJob) -> Result<BriefFold>;
    /// Runs one headless turn. `started` receives the CPU's OS pid.
    fn turn(&self, req: &TurnRequest, started: &dyn Fn(u32)) -> Result<TurnResult>;
    /// Fresh subscription worker; same route list as seat folds.
    fn recovery_summary(&self, _prompt: &str) -> Result<String> {
        anyhow::bail!("recovery summary driver unavailable")
    }
    fn check_summaries(&self) {}
    fn diagnostics(&self) -> Vec<Value> {
        vec![]
    }
    /// Quota per harness account, where it can be read without scraping.
    fn turn_observed(
        &self,
        req: &TurnRequest,
        started: &dyn Fn(u32),
        _event: &dyn Fn(&Value),
    ) -> Result<TurnResult> {
        self.turn(req, started)
    }
    fn quota(&self) -> Vec<Value> {
        vec![]
    }
    /// Harness-owned catalog/auth/quota snapshot. Empty means no evidence.
    fn catalogs(&self, _home: &Path) -> BTreeMap<String, crate::drv_econ::Catalog> {
        BTreeMap::new()
    }
    /// Whether a driven turn on this harness can start here. Dispatch refuses when it
    /// cannot; the kernel never substitutes another harness.
    fn harness_ready(&self, _harness: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub harness: String,
    pub cwd: PathBuf,
    pub session: Option<String>,
    pub prompt: String,
    pub env: Vec<(String, String)>,
    pub model: Option<String>,
    /// A pinned reasoning effort (low|medium|high|xhigh|max); None = harness default.
    pub effort: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct TurnResult {
    pub session: Option<String>,
    pub text: String,
    pub tools: Vec<String>,
    /// The harness reported a quota or rate limit (maps to the low-quota signal).
    pub quota: bool,
    pub error: Option<String>,
    /// A rate-limit report seen during the turn (Claude `rate_limit_event`).
    pub rate: Option<Value>,
    /// Model accepted by the harness (Codex thread response; Claude init/message).
    /// This is configuration evidence, not independent internal model telemetry.
    pub model: Option<String>,
    /// The pinned effort once the driver confirmed it applied (None when not pinned).
    pub effort: Option<String>,
    /// Harness configuration evidence; internal reasoning telemetry is unavailable.
    pub effort_evidence: Option<String>,
}

/// Reasoning levels a task may pin; each harness must confirm acceptance.
pub const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The Observatory's view of the kernel: one JSON snapshot per call.
pub type SnapshotFn = Arc<dyn Fn() -> Value + Send + Sync>;
/// Private server bridge; credential is supplied only after the HTTP boundary checks.
pub type SettingsFn = Arc<dyn Fn(Value, &str) -> Result<Value> + Send + Sync>;
/// Starts the Observatory beside the kernel (its own thread and runtime).
pub type ObservatoryStart = Box<dyn FnOnce(SnapshotFn, SettingsFn, String) + Send>;

pub(crate) struct Inner {
    pub st: State,
    dirty: bool,
    watchdog: watchdog::Watchdog,
    pub mem: MemoryIndex,
    pub folding: BTreeSet<usize>,
    /// PIDs with a driver thread (L3 workers and detached seat runs).
    pub running: BTreeSet<usize>,
    /// Last journal events (Observatory, digest).
    pub recent: VecDeque<Value>,
    /// Quota per account, refreshed in the background.
    pub quota: BTreeMap<String, Value>,
    /// Failed detached seat runs: no new run for the seat before `until` (I9).
    pub seat_backoff: BTreeMap<usize, SeatBackoff>,
    /// Thread key → OS pid of its rewake watcher (`unvrs hook Watch`); the newest wins.
    pub watchers: BTreeMap<String, u32>,
}

/// Consecutive failed detached runs of a seat and the earliest next run (ms).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SeatBackoff {
    pub fails: u32,
    pub until: u64,
}

pub(crate) struct Kernel {
    pub u: Universe,
    pub inner: Mutex<Inner>,
    pub drivers: Arc<dyn Drivers>,
    pub mapp: Arc<dyn Mapp>,
    pub journal: Journal,
    pub stop: AtomicBool,
    /// Deploy drain: no new worker turn or seat run starts; parked workers stay
    /// `working` and the next kernel's sweep resumes them. In memory only.
    pub draining: AtomicBool,
    pub started: u64,
    /// Last request (ms); without a LaunchAgent a quiet kernel exits.
    pub last_active: std::sync::atomic::AtomicU64,
}

const RECENT: usize = 400;
const QUOTA_INTERVAL_SECS: u64 = 600;
const MEMORY_INTERVAL_SECS: u64 = 3600;
const MAINTENANCE_INTERVAL_MS: u64 = 500;
const PERIODIC_GRACE_MS: u64 = 120_000;

impl Kernel {
    /// Only this daemon's live worker loops own process groups. Persisted PIDs
    /// from a previous daemon are evidence and must never be signalled here.
    fn shutdown_workers(&self) {
        let mut inner = self.lock();
        for p in inner.st.pids.values() {
            if inner.running.contains(&p.pid)
                && let Some(d) = p.driven.as_ref()
                && d.busy
                && let Some(os) = d.os_pid
            {
                let _ = crate::signals::signal_group(i64::from(os), libc::SIGTERM);
            }
        }
        self.save(&mut inner);
    }

    /// The kernel over a universe, in-process (serve() adds the socket, the sweeper
    /// and the quota thread; tests drive it directly).
    pub(crate) fn open(
        u: Universe,
        drivers: Arc<dyn Drivers>,
        mapp: Arc<dyn Mapp>,
        started: u64,
    ) -> Result<Arc<Self>> {
        crate::signals::set_home(u.root());
        let mut st = State::load(&u.state_path())?;
        crate::drv_econ::Config::load(u.root())?;
        let ledger = crate::signals::LedgerStore::new(u.root());
        ledger.initialize(&st.spawn_ledger)?;
        st.spawn_ledger = ledger.snapshot()?;
        let mem = MemoryIndex::open(u.root())?;
        let journal = Journal::new(u.journal_path());
        let before = (st.seq, st.wake_seq, st.next_pid);
        let (seq, wake, mut pid) = journal.counters()?;
        let projects = u.root().join("projects");
        if projects.is_dir() {
            for project in fs::read_dir(projects)? {
                let tasks = project?.path().join("tasks");
                if tasks.is_dir() {
                    for task in fs::read_dir(tasks)? {
                        if let Some(n) = task?
                            .file_name()
                            .to_str()
                            .and_then(|s| s.strip_prefix("pid-"))
                            .and_then(|s| s.parse::<usize>().ok())
                        {
                            pid = pid.max(n);
                        }
                    }
                }
            }
        }
        st.seq = st.seq.max(seq);
        st.wake_seq = st.wake_seq.max(wake);
        st.next_pid = st.next_pid.max(pid.saturating_add(1));
        let after = (st.seq, st.wake_seq, st.next_pid);
        let recent: VecDeque<Value> = journal.tail(RECENT).into();
        let k = Arc::new(Kernel {
            journal,
            u,
            inner: Mutex::new(Inner {
                st,
                dirty: false,
                watchdog: Default::default(),
                mem,
                folding: BTreeSet::new(),
                running: BTreeSet::new(),
                watchers: BTreeMap::new(),
                recent,
                quota: BTreeMap::new(),
                seat_backoff: BTreeMap::new(),
            }),
            drivers,
            mapp,
            stop: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            started,
            last_active: std::sync::atomic::AtomicU64::new(started),
        });
        if before != after {
            let mut inner = k.lock();
            k.event(
                &mut inner,
                "state.recovered",
                json!({"from": before, "to": after}),
            );
            k.save(&mut inner);
        }
        k.reconcile_restart();
        k.refresh_apps();
        Ok(k)
    }

    /// A saved busy/recovering flag cannot prove ownership of an OS PID after a
    /// daemon restart. Preserve the work and ask the lead to verify writer absence.
    /// Seat wakes in flight to a run of the old kernel go back to the queue (S3).
    fn reconcile_restart(&self) {
        let mut inner = self.lock();
        self.requeue_seat_wakes(&mut inner);
        let interrupted: Vec<usize> = inner
            .st
            .pids
            .values()
            .filter(|p| {
                p.kind == "driven"
                    && p.rank == 3
                    && matches!(p.state.as_str(), "working" | "idle")
                    && p.driven
                        .as_ref()
                        .is_some_and(|d| d.cpu == "headless" && (d.busy || d.recovering))
            })
            .map(|p| p.pid)
            .collect();
        let settled = self.settle_seat_runs(&mut inner);
        for pid in &interrupted {
            self.stop_event(&mut inner, *pid, "restart-unverified", false);
            self.finish(&mut inner, *pid,
                "blocked: kernel restarted during a turn or checkpoint/summary. Files, brief and recovery history are retained. Old writer ownership is unverified; no process was killed and no successor was started. Next: verify that the prior process group is gone, then dispatch a continuation from the saved brief and REPORT.md.",
                "blocked");
        }
        if settled || !interrupted.is_empty() {
            self.save(&mut inner);
        }
    }

    /// A seat that is not running holds no wake as "driven", and none delivered to a
    /// thread other than its own (strays from before detach requeued them). Such wakes
    /// go back to the queue; a seat run cut off by the restart is no longer busy.
    fn requeue_seat_wakes(&self, inner: &mut Inner) {
        let seats: Vec<usize> = inner
            .st
            .seats()
            .iter()
            .map(|s| s.pid)
            .filter(|p| !inner.running.contains(p))
            .collect();
        let mut changed = false;
        for pid in seats {
            let Ok(rec) = inner.st.pid_mut(pid) else {
                continue;
            };
            let thread = rec.thread.clone();
            let mut requeued = vec![];
            for w in rec.wakes.iter_mut().filter(|w| !w.acked) {
                if w.delivered.is_some() && w.delivered != thread {
                    w.delivered = None;
                    requeued.push(w.id);
                }
            }
            if let Some(d) = rec.driven.as_mut()
                && d.cpu == "seat-run"
                && d.busy
            {
                d.busy = false;
                d.os_pid = None;
                if rec.state == "running" {
                    rec.state = if thread.is_some() { "live" } else { "idle" }.into();
                }
                changed = true;
            }
            if !requeued.is_empty() {
                changed = true;
                self.event(
                    inner,
                    "wake.requeue",
                    json!({"pid": pid, "wakes": requeued, "reason": "kernel restart"}),
                );
            }
        }
        if changed {
            self.save(inner);
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        crate::signals::set_home(self.u.root());
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Journals an event with attribution (D44): mapp always, project when a PID has one.
    pub fn event(&self, inner: &mut Inner, kind: &str, fields: Value) {
        inner.st.seq += 1;
        let mut fields = fields;
        if let Value::Object(m) = &mut fields {
            m.entry("mapp").or_insert(json!(MAPP));
            if !m.contains_key("project") {
                let pid = m
                    .get("pid")
                    .or_else(|| m.get("from_pid"))
                    .and_then(Value::as_u64);
                let project = pid
                    .and_then(|p| inner.st.pids.get(&(p as usize)))
                    .and_then(|p| p.project.clone());
                m.insert("project".into(), json!(project));
            }
        }
        inner.mem.redact_value(&mut fields);
        if let Err(e) = self.journal.append(inner.st.seq, kind, fields.clone()) {
            eprintln!("kernel journal append failed: {e:#}");
            if kind != "watchdog" {
                self.observe_job(
                    inner,
                    "journal",
                    &json!({"job":"journal","error":format!("{e:#}")}),
                );
            }
        } else {
            inner.watchdog.finish("journal", None, false);
        }
        self.observe_job(inner, kind, &fields);
        let mut e = fields;
        e["seq"] = json!(inner.st.seq);
        e["at"] = json!(now_ms());
        e["kind"] = json!(kind);
        inner.recent.push_back(e);
        while inner.recent.len() > RECENT {
            inner.recent.pop_front();
        }
    }
    /// Saves the state and keeps one marker file per known thread.
    pub fn save(&self, inner: &mut Inner) {
        match crate::signals::LedgerStore::new(self.u.root()).snapshot() {
            Ok(ledger) => inner.st.spawn_ledger = ledger,
            Err(e) => {
                inner.dirty = true;
                self.event(
                    inner,
                    "state.save",
                    json!({"job":"spawn-ledger","error":e.to_string()}),
                );
                return;
            }
        }
        if let Err(e) = inner.st.save(&self.u.state_path()) {
            inner.dirty = true;
            eprintln!("kernel state save failed: {e:#}");
            self.event(
                inner,
                "state.save",
                json!({"job":"state.save","error":format!("{e:#}")}),
            );
        } else {
            inner.dirty = false;
            inner.watchdog.finish("state.save", None, false);
        }
        let dir = self.u.kernel_dir().join("threads");
        let _ = fs::create_dir_all(&dir);
        let want: BTreeSet<PathBuf> = inner
            .st
            .threads
            .values()
            .map(|t| self.u.thread_marker(&t.harness, &t.session))
            .collect();
        for p in &want {
            if !p.exists() {
                let _ = fs::write(p, b"");
            }
        }
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if !want.contains(&e.path()) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
    /// Runs a fold job on a worker thread; applies it with compare-and-swap.
    pub fn schedule_fold(self: &Arc<Self>, pid: usize, reason: &str, drain: bool) {
        let job = {
            let mut inner = self.lock();
            if inner.folding.contains(&pid) {
                return;
            }
            match inner.mem.fold_job(pid, reason, drain) {
                Ok(Some(job)) => {
                    inner.folding.insert(pid);
                    job
                }
                Ok(None) => return,
                Err(e) => {
                    self.event(
                        &mut inner,
                        "fold",
                        json!({"pid":pid,"error":format!("{e:#}")}),
                    );
                    return;
                }
            }
        };
        self.job_begin(&format!("fold:{pid}"), 150_000);
        let k = Arc::clone(self);
        thread::spawn(move || {
            let _owner = crate::signals::owner_scope(k.u.root(), pid);
            let mut job = job;
            let mut outcome = k
                .drivers
                .fold(&job)
                .and_then(|f| k.lock().mem.apply_fold(&job, f));
            // One retry with the rejection reason; the previous brief stays meanwhile.
            if let Err(e) = &outcome
                && e.to_string().contains("open items vanished")
            {
                let mut inner = k.lock();
                k.event(
                    &mut inner,
                    "fold",
                    json!({"pid": pid, "reason": job.reason, "result": "rejected", "read": job.version, "error": format!("{e:#}")}),
                );
                drop(inner);
                job.reason = format!("retry: {e:#}");
                outcome = k
                    .drivers
                    .fold(&job)
                    .and_then(|f| k.lock().mem.apply_fold(&job, f));
            }
            k.job_finish(
                &format!("fold:{pid}"),
                outcome.as_ref().err().map(|e| format!("{e:#}")),
                false,
            );
            let mut inner = k.lock();
            inner.folding.remove(&pid);
            let fields = match &outcome {
                Ok(FoldOutcome::Applied { version, graduated }) => {
                    json!({"pid": pid, "reason": job.reason, "result": "applied", "version": version, "read": job.version, "graduated": graduated, "turns": job.turns.len()})
                }
                Ok(FoldOutcome::Discarded { read, current }) => {
                    json!({"pid": pid, "reason": job.reason, "result": "discarded", "read": read, "current": current})
                }
                Err(e) => {
                    json!({"pid": pid, "reason": job.reason, "result": "rejected", "read": job.version, "error": format!("{e:#}")})
                }
            };
            k.event(&mut inner, "fold", fields);
        });
    }

    fn status_text(&self, inner: &Inner) -> String {
        let mut out = format!(
            "kernel: running · pid {} · version {}{}{}\nhome: {}\nsocket: {}\n",
            std::process::id(),
            KERNEL_VERSION,
            BUILD_COMMIT
                .map(|c| format!(" · commit {c}"))
                .unwrap_or_default(),
            if self.draining.load(Ordering::SeqCst) {
                " · draining"
            } else {
                ""
            },
            self.u.root().display(),
            self.u.socket_path().display()
        );
        out.push_str(&captain::tree_text(&inner.st));
        out
    }

    /// The raw process table (tests, `kernel status --json`).
    fn table(&self, inner: &Inner) -> Value {
        json!({
            "kernel": {"pid": std::process::id(), "version": KERNEL_VERSION, "commit": BUILD_COMMIT, "ref": BUILD_REF,
                "draining": self.draining.load(Ordering::SeqCst), "started": self.started, "socket": self.u.socket_path(), "now": now_ms()},
            "home": self.u.root(),
            "pids": inner.st.pids.values().map(state::json_pid).collect::<Vec<_>>(),
            "threads": inner.st.threads.values().map(|t| json!({
                "key": t.key, "harness": t.harness, "session": t.session, "cwd": t.cwd, "app": t.app,
                "pid": t.pid, "bound": t.bound, "detached": t.detached, "os_pid": t.os_pid, "notice": t.notice.is_some(),
                "live": t.bound && t.os_pid.is_none_or(procinfo::alive), "last_seen": t.last_seen,
            })).collect::<Vec<_>>(),
            "holds": inner.st.holds.values().collect::<Vec<_>>(),
            "away": inner.st.away,
        })
    }

    fn handle(self: &Arc<Self>, req: Value, peer: Option<u32>) -> Result<Value> {
        let op = req["op"].as_str().unwrap_or("");
        match op {
            "ping" => Ok(
                json!({"ok": true, "version": KERNEL_VERSION, "commit": BUILD_COMMIT, "ref": BUILD_REF, "home": self.u.root()}),
            ),
            "drain" => {
                if let Some(on) = req["on"].as_bool()
                    && self.draining.swap(on, Ordering::SeqCst) != on
                {
                    let mut inner = self.lock();
                    self.event(
                        &mut inner,
                        "kernel.drain",
                        json!({"on": on, "os_pid": std::process::id()}),
                    );
                }
                let inner = self.lock();
                Ok(json!({
                    "draining": self.draining.load(Ordering::SeqCst),
                    "busy": inner.running.iter().collect::<Vec<_>>(),
                    "folding": inner.folding.len(),
                }))
            }
            "status" => {
                let inner = self.lock();
                Ok(json!({"text": self.status_text(&inner), "snapshot": self.table(&inner)}))
            }
            "table" => Ok(self.table(&self.lock())),
            "snapshot" => Ok(self.snapshot()),
            "settings.get" | "settings.set" | "settings.history" | "settings.revert" => {
                self.settings_request(&req, peer)
            }
            "hot" => {
                let pid = req["pid"].as_u64().context("pid required")? as usize;
                let inner = self.lock();
                let text = hot::hot_text(self, &inner, pid)?;
                Ok(json!({"pid": pid, "text": text, "bytes": text.len()}))
            }
            "hook" => {
                let harness_pid = peer.and_then(procinfo::harness_process);
                let reply = self.hook(
                    req["event"].as_str().unwrap_or(""),
                    req["harness"].as_str().unwrap_or(""),
                    &req["payload"],
                    harness_pid,
                )?;
                Ok(
                    json!({"context": reply.context, "system": reply.system, "block": reply.block, "stop": reply.stop}),
                )
            }
            "watch" => {
                let key = format!(
                    "{}:{}",
                    req["harness"].as_str().unwrap_or(""),
                    req["session"].as_str().unwrap_or("")
                );
                let watcher = req["watcher"].as_u64().context("watcher required")? as u32;
                let mut inner = self.lock();
                Ok(self.watch(&mut inner, &key, watcher, req["arm"] == true))
            }
            "ctl" => self.ctl(&req, peer),
            "stop" => {
                self.stop.store(true, Ordering::SeqCst);
                let mut inner = self.lock();
                self.event(
                    &mut inner,
                    "kernel.stop",
                    json!({"os_pid": std::process::id()}),
                );
                Ok(json!({"stopped": true}))
            }
            other => bail!("Unknown kernel op {other:?}"),
        }
    }

    /// Without a LaunchAgent (tests, foreground runs) a kernel with nothing live and no
    /// request for `UNVRS_KERNEL_IDLE_SECS` (default 1800) exits; the next hook or
    /// call starts it again. Under launchd it stays up (D60).
    fn idle_too_long(&self) -> bool {
        if std::env::var("UNVRS_DAEMON").is_ok_and(|v| v == "launchd") {
            return false;
        }
        let limit = std::env::var("UNVRS_KERNEL_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1800);
        if now_ms().saturating_sub(self.last_active.load(Ordering::SeqCst)) < limit * 1000 {
            return false;
        }
        let inner = self.lock();
        inner.running.is_empty()
            && inner.folding.is_empty()
            && !inner
                .st
                .threads
                .values()
                .any(|t| t.bound && t.os_pid.is_none_or(procinfo::alive))
            && !inner.st.pids.values().any(|p| p.state == "working")
    }

    fn collect_signal_refusals(&self) {
        let store = crate::signals::LedgerStore::new(self.u.root());
        match store.refusals() {
            Ok(refusals) => {
                for (path, refusal) in refusals {
                    let mut inner = self.lock();
                    self.event(
                        &mut inner,
                        "signal.refused",
                        json!({
                            "os_pid":refusal.os_pid,"signal":refusal.signal,
                            "pid":refusal.owning_pid,"reason":refusal.reason
                        }),
                    );
                    // The journal watchdog retains the marker on any failed append.
                    if !inner.watchdog.active("journal") {
                        let _ = fs::remove_file(path);
                    }
                }
            }
            Err(e) => {
                let mut inner = self.lock();
                self.event(
                    &mut inner,
                    "state.save",
                    json!({"job":"signal-refusals","error":e.to_string()}),
                );
            }
        }
    }
    fn sweep(self: &Arc<Self>) {
        self.collect_signal_refusals();
        let mut folds = vec![];
        let mut restart = vec![];
        {
            let mut inner = self.lock();
            let dead: Vec<String> = inner
                .st
                .threads
                .values()
                .filter(|t| t.bound && t.os_pid.is_some_and(|p| !procinfo::alive(p)))
                .map(|t| t.key.clone())
                .collect();
            for key in dead {
                if let Some(pid) = self.detach(&mut inner, &key, "harness exited", None) {
                    folds.push(pid);
                }
            }
            for p in inner.st.pids.values() {
                if p.kind == "driven"
                    && p.state == "working"
                    && !inner.running.contains(&p.pid)
                    && p.driven
                        .as_ref()
                        .is_some_and(|d| d.cpu == "headless" && !d.busy && !d.recovering)
                {
                    restart.push(p.pid);
                }
            }
            if !folds.is_empty() || inner.dirty {
                self.save(&mut inner);
            }
        }
        for pid in folds {
            self.schedule_fold(pid, "detach", false);
        }
        if self.draining.load(Ordering::SeqCst) {
            return;
        }
        for pid in restart {
            self.start_worker(pid);
        }
        self.run_detached_seats();
    }
}

fn serve_conn(k: Arc<Kernel>, conn: UnixStream) {
    k.last_active.store(now_ms(), Ordering::SeqCst);
    let peer = procinfo::peer_pid(&conn);
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut writer) = conn.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    if (&mut reader)
        .take(4 * 1024 * 1024)
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let reply = match serde_json::from_str::<Value>(&line) {
        Ok(req) => k
            .handle(req, peer)
            .unwrap_or_else(|e| json!({"error": format!("{e:#}")})),
        Err(e) => json!({"error": format!("Invalid request: {e}")}),
    };
    let _ = crate::write_json(&mut writer, &reply);
}

/// Runs the kernel in the foreground until `stop`. Exits quietly when another kernel
/// already holds this home's lock. `observatory` starts the Observatory on its own
/// thread with a live snapshot function.
pub fn serve(
    u: Universe,
    drivers: Arc<dyn Drivers>,
    mapp: Arc<dyn Mapp>,
    observatory: Option<ObservatoryStart>,
) -> Result<()> {
    u.init()?;
    let stable = u.root().join("bin/unvrs");
    let exe = if stable.exists() {
        Some(stable)
    } else {
        std::env::current_exe().and_then(|e| e.canonicalize()).ok()
    };
    if let Some(exe) = exe {
        let _ = EXE.set(exe);
    }
    let dir = u.kernel_dir();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("kernel.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        eprintln!("kernel: another kernel owns {}", u.root().display());
        return Ok(());
    }
    let socket = u.socket_path();
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let started = now_ms();
    fs::write(
        dir.join("kernel.json"),
        serde_json::to_vec_pretty(
            &json!({"os_pid": std::process::id(), "socket": socket, "version": KERNEL_VERSION, "commit": BUILD_COMMIT, "ref": BUILD_REF, "started": started, "home": u.root()}),
        )?,
    )?;
    let k = Kernel::open(u, drivers, mapp, started)?;
    // Admission must see a startup read, not a catalog from the prior daemon.
    k.refresh_econ();
    {
        let mut inner = k.lock();
        k.event(
            &mut inner,
            "kernel.start",
            json!({"os_pid": std::process::id(), "home": k.u.root(), "version": KERNEL_VERSION, "commit": BUILD_COMMIT}),
        );
        k.save(&mut inner);
    }
    if let Some(start) = observatory {
        match k.observatory_callbacks() {
            Ok((snapshot, settings, token)) => start(snapshot, settings, token),
            Err(error) => {
                eprintln!("observatory settings credential: {error:#} (reads remain available)");
                let snap = Arc::clone(&k);
                let reads = Arc::clone(&k);
                start(
                    Arc::new(move || snap.snapshot()),
                    Arc::new(move |req, _| {
                        ensure!(
                            !matches!(req["op"].as_str(), Some("settings.set" | "settings.revert")),
                            "Refused: local settings authorization is unavailable"
                        );
                        reads.settings_request(&req, None)
                    }),
                    String::new(),
                );
            }
        }
    }
    let summaries = Arc::clone(&k);
    summaries.job_begin("summary:startup", 150_000);
    thread::spawn(move || {
        let _owner = crate::signals::owner_scope(summaries.u.root(), 0);
        summaries.drivers.check_summaries();
        summaries.job_finish("summary:startup", None, false);
        summaries.sweep_watchdog();
    });
    k.job_periodic("quota", QUOTA_INTERVAL_SECS * 1000, PERIODIC_GRACE_MS);
    let econ = Arc::clone(&k);
    thread::spawn(move || {
        let _owner = crate::signals::owner_scope(econ.u.root(), 0);
        let mut next = Instant::now() + Duration::from_secs(econ.econ_refresh_seconds());
        loop {
            if econ.stop.load(Ordering::SeqCst) {
                return;
            }
            if Instant::now() >= next || econ.lock().st.econ_refresh_requested {
                econ.refresh_econ();
                next = Instant::now() + Duration::from_secs(econ.econ_refresh_seconds());
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
    k.job_periodic(
        "memory.sweep",
        MEMORY_INTERVAL_SECS * 1000,
        PERIODIC_GRACE_MS,
    );
    k.job_periodic("maintenance", MAINTENANCE_INTERVAL_MS, PERIODIC_GRACE_MS);
    let quota = Arc::clone(&k);
    thread::spawn(move || {
        let _owner = crate::signals::owner_scope(quota.u.root(), 0);
        loop {
            quota.job_begin("quota", 60_000);
            let rows = quota.drivers.quota();
            let errors: Vec<_> = rows.iter().filter_map(|r| r["error"].as_str()).collect();
            quota.job_finish(
                "quota",
                (!errors.is_empty()).then(|| errors.join("\n")),
                false,
            );
            {
                let mut inner = quota.lock();
                for r in rows {
                    let key = format!(
                        "{}:{}",
                        r["harness"].as_str().unwrap_or(""),
                        r["account"].as_str().unwrap_or("")
                    );
                    inner.quota.insert(key, r);
                }
            }
            let next = Instant::now() + Duration::from_secs(QUOTA_INTERVAL_SECS);
            while Instant::now() < next {
                thread::sleep(
                    next.saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(1)),
                );
                if quota.stop.load(Ordering::SeqCst) {
                    return;
                }
            }
        }
    });
    let memory = Arc::clone(&k);
    thread::spawn(move || {
        loop {
            memory.sweep_memory();
            let next = Instant::now() + Duration::from_secs(MEMORY_INTERVAL_SECS);
            while Instant::now() < next {
                thread::sleep(
                    next.saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(1)),
                );
                if memory.stop.load(Ordering::SeqCst) {
                    return;
                }
            }
        }
    });
    let sweeper = Arc::clone(&k);
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(MAINTENANCE_INTERVAL_MS));
            if sweeper.stop.load(Ordering::SeqCst) {
                break;
            }
            // A home that was removed (a throwaway test dir) takes its kernel with it.
            if !sweeper.u.kernel_dir().is_dir() {
                sweeper.stop.store(true, Ordering::SeqCst);
                break;
            }
            sweeper.job_begin("maintenance", PERIODIC_GRACE_MS);
            sweeper.sweep();
            let idle = sweeper.idle_too_long();
            // A 500 ms heartbeat must not flood the journal with successful ticks.
            sweeper.lock().watchdog.finish("maintenance", None, false);
            if idle {
                let mut inner = sweeper.lock();
                sweeper.event(
                    &mut inner,
                    "kernel.stop",
                    json!({"os_pid": std::process::id(), "reason": "idle"}),
                );
                drop(inner);
                sweeper.stop.store(true, Ordering::SeqCst);
                break;
            }
        }
    });
    listener.set_nonblocking(true)?;
    let mut watchdog_due = Instant::now();
    loop {
        if k.stop.load(Ordering::SeqCst) {
            break;
        }
        // Independent of the periodic scheduling threads: their disappearance must alarm.
        if Instant::now() >= watchdog_due {
            k.sweep_watchdog();
            watchdog_due = Instant::now() + Duration::from_millis(500);
        }
        match listener.accept() {
            Ok((conn, _)) => {
                let _ = conn.set_nonblocking(false);
                let k = Arc::clone(&k);
                thread::spawn(move || serve_conn(k, conn));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(15));
            }
            Err(e) => return Err(e.into()),
        }
    }
    k.shutdown_workers();
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_file(k.u.kernel_dir().join("kernel.json"));
    Ok(())
}
