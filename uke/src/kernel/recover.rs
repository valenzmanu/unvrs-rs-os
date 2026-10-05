//! Worker recovery (docs/design/worker-recovery.md): a budget running out is a
//! checkpoint, never an ending. Every stop of a driven L3 is classified; a retryable
//! stop is checkpointed (brief, git work tree, REPORT.md) and continued in place, under
//! the same PID with a fresh session, on the same harness with the same pins; the lead
//! is woken only when the lineage is truly blocked, with the reason and every attempt.
use super::{
    Inner, Kernel, hot,
    state::{clip, now_ms},
};
use crate::signals::CommandTracking;
use crate::{Brief, FoldOutcome};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// Consecutive attempts without progress that end a lineage.
pub const MAX_STALLS: u32 = 2;
/// Consecutive turns without progress that stop an attempt (UNVRS_STALL_TURNS overrides).
pub const DEFAULT_STALL_TURNS: u32 = 3;
/// Budget of one checkpoint git command (UNVRS_CHECKPOINT_SECS overrides).
pub const DEFAULT_CHECKPOINT_SECS: u64 = 600;
/// Git commands one checkpoint runs at most (the watchdog allows this many budgets).
pub const CHECKPOINT_COMMANDS: u64 = 7;

fn env_num<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// An optional ceiling on continuations: unset (or `none`/`unlimited`) is unbounded,
/// so a lineage runs until it finishes, stalls or is stopped.
pub fn parse_ceiling(raw: Option<&str>) -> Option<u32> {
    raw.map(str::trim)
        .filter(|v| !v.is_empty() && !matches!(*v, "none" | "unlimited"))
        .and_then(|v| v.parse().ok())
}

/// UNVRS_MAX_CONTINUATIONS, the kernel-wide ceiling (None = unbounded).
pub fn max_continuations() -> Option<u32> {
    parse_ceiling(std::env::var("UNVRS_MAX_CONTINUATIONS").ok().as_deref())
}

/// Turns in a row with an unchanged brief and work-tree HEAD before an attempt stops.
pub fn stall_turns() -> u32 {
    env_num("UNVRS_STALL_TURNS")
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_STALL_TURNS)
}

/// The budget of each git command of a checkpoint.
pub fn checkpoint_budget() -> Duration {
    Duration::from_secs(
        env_num("UNVRS_CHECKPOINT_SECS")
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_CHECKPOINT_SECS),
    )
}

/// Why a driven worker stopped without a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// This many turns in a row changed neither the brief nor the work tree's HEAD.
    NoProgress(u32),
    /// The driver's turn timeout (no output for too long, or the runaway ceiling).
    Timeout,
    /// The harness process died or its stream broke (message).
    Crash(String),
    /// The harness cannot run here (not installed); a retry on it cannot help.
    Harness(String),
    CleanupUnconfirmed(String),
    ModelMismatch(String),
    EffortMismatch(String),
    /// Its owner stopped it (`stop`); never continued.
    Cancelled(String),
}

impl Stop {
    pub fn label(&self) -> &'static str {
        match self {
            Stop::NoProgress(_) => "no-progress",
            Stop::Timeout => "timeout",
            Stop::Crash(_) => "crash",
            Stop::Harness(_) => "harness",
            Stop::CleanupUnconfirmed(_) => "cleanup-unconfirmed",
            Stop::ModelMismatch(_) => "model-mismatch",
            Stop::EffortMismatch(_) => "effort-mismatch",
            Stop::Cancelled(_) => "cancelled",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Stop::NoProgress(_) | Stop::Timeout | Stop::Crash(_))
    }
    pub fn describe(&self) -> String {
        match self {
            Stop::NoProgress(n) => {
                format!("{n} turns in a row changed neither its brief nor its work tree's HEAD")
            }
            Stop::Timeout => "its turn timed out".into(),
            Stop::Crash(m) => format!("its harness crashed: {}", clip(m, 300)),
            Stop::Harness(m) => format!("its harness cannot run here: {}", clip(m, 300)),
            Stop::CleanupUnconfirmed(m)
            | Stop::ModelMismatch(m)
            | Stop::EffortMismatch(m)
            | Stop::Cancelled(m) => clip(m, 500),
        }
    }
}

/// A driver error, classified, including pins rejected before a turn returns.
// ponytail: Drivers returns text errors; use typed stops when a new driver needs other wording.
pub fn classify_error(err: &str) -> Stop {
    let e = err.to_lowercase();
    if e.contains("cleanup unconfirmed") {
        Stop::CleanupUnconfirmed(err.into())
    } else if e.contains("model mismatch") || e.contains("model not confirmed") {
        Stop::ModelMismatch(err.into())
    } else if e.contains("effort not confirmed")
        || e.contains("effort not applied")
        || e.contains("effort not verified")
        || (e.contains("effort") && e.contains("is pinned"))
    {
        Stop::EffortMismatch(err.into())
    } else if e.contains("turn timed out") {
        Stop::Timeout
    } else if e.contains("not installed")
        || e.contains("not on path")
        || e.contains("native execution cleanup capability not confirmed")
        || e.contains("native delegation restriction")
        || e.contains("working directory")
        || e.contains("codex thread/start rejected")
        || e.contains("codex thread/resume rejected")
    {
        Stop::Harness(err.into())
    } else {
        Stop::Crash(err.into())
    }
}

/// A git checkpoint of a worker's work tree.
#[derive(Clone, Debug, Serialize, Default)]
pub struct Checkpoint {
    pub path: PathBuf,
    pub branch: String,
    pub commit: String,
    /// True when this call created the commit (else the tree was already clean).
    pub committed: bool,
}
use serde::Serialize;

/// Build output never goes into a checkpoint (any `target/` directory, at any depth).
const CHECKPOINT_PATHS: [&str; 4] = [
    "--",
    ".",
    ":(exclude,glob)**/target/**",
    ":(exclude,glob)**/target",
];
struct CheckpointChild(crate::signals::TrackedChild);
fn captured(mut stream: impl Read) -> std::io::Result<String> {
    let mut output = String::new();
    let mut buf = [0; 4096];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(output);
        }
        if output.len() < 65536 {
            output.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }
}
#[cfg(test)]
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_until(dir, args, Instant::now() + checkpoint_budget())
}
fn git_until(dir: &Path, args: &[&str], deadline: Instant) -> Result<String> {
    ensure!(Instant::now() < deadline, "git checkpoint timed out");
    let mut child = CheckpointChild(
        Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "core.fsmonitor=false",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn_owned()
            .context("git checkpoint launch")?,
    );
    let stdout = child.0.stdout.take().context("git stdout")?;
    let stderr = child.0.stderr.take().context("git stderr")?;
    let stdout = thread::spawn(move || captured(stdout));
    let stderr = thread::spawn(move || captured(stderr));
    let status = loop {
        if let Some(status) = crate::signals::try_wait_group(&mut child.0)? {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "git checkpoint timed out during {}",
            args.first().copied().unwrap_or("operation")
        );
        thread::sleep(Duration::from_millis(10));
    };
    let out = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("git stdout reader failed"))??;
    let err = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("git stderr reader failed"))??;
    ensure!(
        status.success(),
        "git {} failed: {}",
        args.first().copied().unwrap_or("operation"),
        err.trim()
    );
    Ok(out.trim().to_owned())
}

/// The worker's repository (`<cwd>/wt`, else `<cwd>`): only a directory that is itself
/// a repository root, and never the universe root.
fn work_tree(cwd: &Path, universe: &Path, budget: Duration) -> Result<Option<PathBuf>> {
    for dir in [cwd.join("wt"), cwd.to_path_buf()] {
        let Ok(canon) = dir.canonicalize() else {
            continue;
        };
        if universe
            .canonicalize()
            .is_ok_and(|u| u == canon || u.starts_with(&canon))
        {
            continue;
        }
        // Only a missing repository is optional. A timed-out git is a failed
        // checkpoint and must block continuation while retaining the tree.
        if !canon.join(".git").exists() {
            continue;
        }
        let top = git_until(
            &canon,
            &["rev-parse", "--show-toplevel"],
            Instant::now() + budget,
        )?;
        if Path::new(&top).canonicalize().ok() == Some(canon.clone()) {
            return Ok(Some(canon));
        }
    }
    Ok(None)
}

/// The work tree's HEAD, for per-turn progress (None: no repository, no commit yet,
/// or git failed; the brief alone then decides).
pub fn work_head(cwd: &Path, universe: &Path) -> Option<String> {
    let budget = Duration::from_secs(30);
    let dir = work_tree(cwd, universe, budget).ok()??;
    git_until(&dir, &["rev-parse", "HEAD"], Instant::now() + budget).ok()
}

/// Commits uncommitted work in the worker's tree (see `work_tree`) on its current
/// branch, leaving build output (`target/`) out. A worker's cwd inside a bigger
/// repository is left alone. Each git command gets `checkpoint_budget()`.
pub fn git_checkpoint(
    cwd: &Path,
    universe: &Path,
    pid: usize,
    why: &str,
) -> Result<Option<Checkpoint>> {
    git_checkpoint_with(cwd, universe, pid, why, checkpoint_budget())
}
fn git_checkpoint_with(
    cwd: &Path,
    universe: &Path,
    pid: usize,
    why: &str,
    budget: Duration,
) -> Result<Option<Checkpoint>> {
    let git = |dir: &Path, args: &[&str]| git_until(dir, args, Instant::now() + budget);
    let paths = |args: &[&'static str]| [args, &CHECKPOINT_PATHS[..]].concat();
    if let Some(canon) = work_tree(cwd, universe, budget)? {
        let branch = git(&canon, &["symbolic-ref", "--short", "HEAD"])
            .context("checkpoint requires a branch")?;
        let dirty = !git(&canon, &paths(&["status", "--porcelain"]))?.is_empty();
        if dirty {
            let msg = format!("unvrs checkpoint: PID {pid} stopped ({why})");
            git(&canon, &paths(&["add", "-A"]))?;
            git(
                &canon,
                &[
                    "-c",
                    "user.name=unvrs",
                    "-c",
                    "user.email=unvrs@localhost",
                    "commit",
                    "-q",
                    "--no-verify",
                    "-m",
                    &msg,
                ],
            )?;
            ensure!(
                git(&canon, &paths(&["status", "--porcelain"]))?.is_empty(),
                "checkpoint left uncommitted changes in {}",
                canon.display()
            );
        }
        let commit = git(&canon, &["rev-parse", "--short", "HEAD"])?;
        return Ok(Some(Checkpoint {
            path: canon,
            branch,
            commit,
            committed: dirty,
        }));
    }
    Ok(None)
}

/// What "progress" means between attempts: the brief moved or the tree got a commit.
pub fn progress_mark(brief: &Brief, commit: Option<&str>) -> String {
    json!([brief.now, brief.next, brief.done, brief.open, commit]).to_string()
}

/// Bytes of a dead worker's tail a summarizer is shown (recent turns first kept).
pub const SUMMARY_TAIL_BYTES: usize = 12000;

/// The one-turn prompt for the recovery summarizer.
pub fn summary_prompt(
    pid: usize,
    stop: &Stop,
    brief: &Brief,
    tail: &[String],
    report: Option<&str>,
) -> String {
    let mut shown = vec![];
    let mut used = 0;
    for t in tail.iter().rev() {
        let t = clip(t, 4000);
        if used + t.len() > SUMMARY_TAIL_BYTES {
            break;
        }
        used += t.len();
        shown.push(t);
    }
    shown.reverse();
    format!(
        "You are the UNVRS recovery summarizer. Worker PID {pid} stopped ({}) before it reported progress. Do NOT do its task and change NO files. From its goal, transcript tail and REPORT.md below, say where the work stands.

CURRENT BRIEF (preserve its item ids):
{}

TRANSCRIPT TAIL (oldest first):
{}

REPORT.md:
{}

Reply with ONLY this block, ids kept, every open item of the goal still listed as open or done:
UNVRS-PROGRESS
now: <the last thing the worker was doing>
next: <the next concrete step>
open:
- [o1] <item>
done:
- [oN] <item, with its result>
END-PROGRESS",
        stop.describe(),
        serde_json::to_string(brief).unwrap_or_default(),
        if shown.is_empty() { "(empty)".to_owned() } else { shown.join("\n---\n") },
        report.map(|r| clip(r, 4000)).unwrap_or_else(|| "(none)".into())
    )
}

/// The lines every driven worker gets about checkpoint discipline.
pub const CHECKPOINT_RULE: &str = "Commit a checkpoint after every meaningful step (git, in your work tree) and keep REPORT.md's \"next step\" current, so a fresh worker can continue if you are cut off.";

#[allow(clippy::too_many_arguments)]
pub fn continuation_preamble(
    attempt: u32,
    max: Option<u32>,
    from: usize,
    stop: &Stop,
    turns: u32,
    checkpoint: Option<&Checkpoint>,
    report: Option<&Path>,
    next: &str,
) -> String {
    let mut s = format!(
        "CONTINUATION attempt {attempt}{}. PID {from} stopped: {} after {turns} turn(s). Resume; do not restart. Everything under `done` stays done; work from `now` and `next`.",
        max.map(|m| format!(" of {m}")).unwrap_or_default(),
        stop.describe()
    );
    if let Some(c) = checkpoint {
        s.push_str(&format!(
            "\nWork tree: {} on branch {} at commit {}{}.",
            c.path.display(),
            c.branch,
            c.commit,
            if c.committed {
                " (the kernel committed the uncommitted work as a checkpoint)"
            } else {
                ""
            }
        ));
    }
    if let Some(r) = report {
        s.push_str(&format!("\nREPORT.md so far: {}", r.display()));
    }
    if !next.trim().is_empty() {
        s.push_str(&format!("\nNext step: {next}"));
    }
    s.push('\n');
    s.push_str(CHECKPOINT_RULE);
    s
}

/// The checkpoint record of one stop.
pub struct Saved {
    pub checkpoint: Option<Checkpoint>,
    pub report: Option<PathBuf>,
    pub brief_version: u64,
    pub checkpoint_error: Option<String>,
    /// The brief never advanced and there is a tail: a recovery summary is wanted.
    pub needs_summary: bool,
}

impl Kernel {
    /// Recover the missing brief through the driver's shared subscription routes.
    /// The driver bounds the call and returns route failures; no kernel lock is held.
    pub(crate) fn recovery_summary(self: &Arc<Self>, pid: usize, stop: &Stop) -> bool {
        let _owner = crate::signals::owner_scope(self.u.root(), pid);
        let (session, cwd) = {
            let inner = self.lock();
            let session = inner.mem.session(pid).unwrap_or_default();
            let cwd = inner
                .st
                .pid(pid)
                .ok()
                .and_then(|r| r.driven.as_ref().map(|d| d.cwd.clone()))
                .unwrap_or_else(|| self.u.root().to_path_buf());
            (session, cwd)
        };
        let report = [cwd.join("REPORT.md"), cwd.join("wt/REPORT.md")]
            .into_iter()
            .find_map(|p| std::fs::read_to_string(p).ok());
        let prompt = summary_prompt(pid, stop, &session.brief, &session.tail, report.as_deref());
        let prompt = self.lock().mem.redact(&prompt);
        let result = self.drivers.recovery_summary(&prompt);
        let mut inner = self.lock();
        let result = result.and_then(|text| {
            let (now, next, open, done) = super::driven::parse_progress(&text)
                .context("recovery summary returned no progress block")?;
            let mut brief = session.brief.clone();
            brief.now = now;
            brief.next = if next.is_empty() {
                vec![]
            } else {
                vec![next.clone()]
            };
            brief.open = open;
            brief.done = done;
            let job = crate::FoldJob {
                pid,
                version: session.brief_version,
                reason: "recovery summary".into(),
                brief: session.brief,
                turns: vec![],
                covered: session.tail.len(),
                drain: false,
                area: session.active_scope.area,
            };
            let fold = crate::BriefFold {
                brief,
                ..Default::default()
            };
            match inner.mem.apply_fold(&job, fold)? {
                FoldOutcome::Applied { version, .. } => {
                    if let Ok(r) = inner.st.pid_mut(pid) {
                        r.next = next;
                    }
                    Ok(version)
                }
                FoldOutcome::Discarded { .. } => anyhow::bail!("brief advanced while summary ran"),
            }
        });
        self.event(&mut inner, "fold", match &result {
            Ok(version) => json!({"pid": pid, "reason": "recovery summary", "result": "applied", "version": version}),
            Err(e) => json!({"pid": pid, "reason": "recovery summary", "result": "failed", "error": format!("{e:#}")}),
        });
        result.is_ok()
    }

    /// Journals a stop (also for the non-retryable ones, so the Observatory sees all).
    pub(crate) fn stop_event(&self, inner: &mut Inner, pid: usize, stop: &str, retryable: bool) {
        let attempt = inner
            .st
            .pid(pid)
            .ok()
            .and_then(|r| r.driven.as_ref().map(|d| d.attempt))
            .unwrap_or(0);
        if let Ok(r) = inner.st.pid_mut(pid)
            && let Some(d) = r.driven.as_mut()
        {
            d.last_stop = Some(stop.into());
        }
        self.event(
            inner,
            "stop",
            json!({"pid": pid, "stop": stop, "retryable": retryable, "attempt": attempt}),
        );
    }

    /// Saves checkpoint/report pointers after git ran outside the kernel lock.
    pub(crate) fn checkpoint_locked(
        &self,
        inner: &mut Inner,
        pid: usize,
        stop: &Stop,
        result: Result<Option<Checkpoint>>,
    ) -> Saved {
        let cwd = inner
            .st
            .pid(pid)
            .ok()
            .and_then(|r| r.driven.as_ref().map(|d| d.cwd.clone()))
            .unwrap_or_else(|| self.u.root().to_path_buf());
        let (checkpoint, checkpoint_error) = match result {
            Ok(c) => (c, None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let report: Option<PathBuf> = [cwd.join("REPORT.md"), cwd.join("wt/REPORT.md")]
            .into_iter()
            .find(|p: &PathBuf| p.exists());
        let session = inner.mem.session(pid).unwrap_or_default();
        let mut brief = session.brief.clone();
        let needs_summary = (brief.now.is_empty() || brief.now == "not started")
            && (!session.tail.is_empty() || report.is_some());
        let mut pointers = vec![];
        if let Some(c) = &checkpoint {
            pointers.push(format!(
                "checkpoint {} on {} in {}",
                c.commit,
                c.branch,
                c.path.display()
            ));
        }
        if let Some(r) = &report {
            pointers.push(format!("REPORT.md: {}", r.display()));
        }
        let mut version = session.brief_version;
        let before = brief.artifacts.len();
        // Continuations are unbounded: the brief names the latest checkpoint only
        // (every attempt's commit stays in `attempts`).
        if checkpoint.is_some() {
            brief
                .artifacts
                .retain(|a| !a.starts_with("checkpoint ") || pointers.contains(a));
        }
        let new: Vec<String> = pointers
            .iter()
            .filter(|p| !brief.artifacts.contains(p))
            .cloned()
            .collect();
        if !new.is_empty() || brief.artifacts.len() != before {
            brief.artifacts.extend(new);
            if let Ok(FoldOutcome::Applied { version: v, .. }) =
                inner.mem.write_brief(pid, "checkpoint", brief)
            {
                version = v;
            }
        }
        self.event(
            inner,
            "checkpoint",
            json!({"pid": pid, "stop": stop.label(), "brief_version": version,
                "commit": checkpoint.as_ref().map(|c| c.commit.clone()),
                "committed": checkpoint.as_ref().map(|c| c.committed),
                "tree": checkpoint.as_ref().map(|c| c.path.display().to_string()),
                "report": report.as_ref().map(|r| r.display().to_string()),
                "needs_summary": needs_summary, "error": checkpoint_error}),
        );
        Saved {
            checkpoint,
            report,
            brief_version: version,
            checkpoint_error,
            needs_summary,
        }
    }

    /// Step 2: continue with a fresh session under the same PID, or escalate when the
    /// lineage stalled twice, reached an optional ceiling, or the stop is not
    /// retryable. True when the caller must restart the worker loop.
    pub(crate) fn continue_or_escalate(
        self: &Arc<Self>,
        inner: &mut Inner,
        pid: usize,
        stop: &Stop,
        saved: &Saved,
    ) -> bool {
        let Ok(rec) = inner.st.pid(pid).cloned() else {
            return false;
        };
        let d = rec.driven.clone().unwrap_or_default();
        let brief = inner.mem.session(pid).map(|s| s.brief).unwrap_or_default();
        let mark = progress_mark(&brief, saved.checkpoint.as_ref().map(|c| c.commit.as_str()));
        let progressed = d.progress_mark.as_deref() != Some(mark.as_str());
        let stalls = if progressed { 0 } else { d.stalls + 1 };
        let mut attempts = d.attempts.clone();
        attempts.push(
            json!({"pid": pid, "attempt": d.attempt, "stop": stop.label(),
            "turns": d.turns, "commit": saved.checkpoint.as_ref().map(|c| c.commit.clone()),
            "progressed": progressed, "brief_version": saved.brief_version}),
        );
        if let Ok(r) = inner.st.pid_mut(pid)
            && let Some(dd) = r.driven.as_mut()
        {
            dd.attempts = attempts.clone();
            dd.stalls = stalls;
        }
        let max = d.max_continuations.or_else(max_continuations);
        let tried = attempts
            .iter()
            .map(|a| {
                format!(
                    "attempt {} PID {} {} after {} turn(s){}{}",
                    a["attempt"],
                    a["pid"],
                    a["stop"].as_str().unwrap_or("?"),
                    a["turns"],
                    a["commit"]
                        .as_str()
                        .map(|c| format!(", checkpoint {c}"))
                        .unwrap_or_default(),
                    if a["progressed"] == true {
                        ", progressed"
                    } else {
                        ", no progress"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let blocked = if let Some(error) = &saved.checkpoint_error {
            Some(format!("checkpoint failed; work tree retained: {error}"))
        } else if !stop.retryable() {
            Some(format!("not retryable: {}", stop.describe()))
        } else if stalls >= MAX_STALLS {
            Some(format!(
                "{MAX_STALLS} consecutive attempts made no progress (no brief change, no new commit)"
            ))
        } else {
            max.filter(|m| d.attempt >= *m)
                .map(|max| format!("out of continuations ({max}; UNVRS_MAX_CONTINUATIONS)"))
        };
        if let Some(reason) = blocked {
            let next = brief.next.join(" / ");
            let last = saved
                .checkpoint
                .as_ref()
                .map(|c| {
                    format!(
                        " Last checkpoint: {} on {} in {}.",
                        c.commit,
                        c.branch,
                        c.path.display()
                    )
                })
                .unwrap_or_default();
            let text = format!(
                "blocked: {reason}. Tried: {tried}.{last}{}",
                if next.is_empty() {
                    String::new()
                } else {
                    format!(" Brief's next step: {}", clip(&next, 300))
                }
            );
            self.event(
                inner,
                "blocked",
                json!({"pid": pid, "lineage": d.lineage.unwrap_or(pid), "reason": reason, "tried": attempts}),
            );
            let how = if matches!(stop, Stop::ModelMismatch(_) | Stop::EffortMismatch(_)) {
                stop.label()
            } else {
                "blocked"
            };
            self.finish(inner, pid, &text, how);
            return false;
        }
        match self.continue_locked(inner, pid, stop, saved, mark, stalls, progressed, max) {
            Ok(()) => true,
            Err(e) => {
                self.finish(
                    inner,
                    pid,
                    &format!(
                        "blocked: could not continue after {}: {e:#}. Tried: {tried}.",
                        stop.label()
                    ),
                    "blocked",
                );
                false
            }
        }
    }

    /// A continuation in place: the same PID, harness, contract, pins, brief and
    /// transcript, with a fresh harness session whose first prompt is the full package
    /// behind a continuation preamble. handoffs/ keeps an audit record of each attempt.
    #[allow(clippy::too_many_arguments)]
    fn continue_locked(
        self: &Arc<Self>,
        inner: &mut Inner,
        pid: usize,
        stop: &Stop,
        saved: &Saved,
        mark: String,
        stalls: u32,
        progressed: bool,
        max: Option<u32>,
    ) -> Result<()> {
        let rec = inner.st.pid(pid)?.clone();
        super::driven::approved_contract(rec.contract.as_ref())?;
        let d = rec.driven.clone().unwrap_or_default();
        let brief = inner.mem.session(pid)?.brief;
        let package = hot::hot_text(self, inner, pid)?;
        let attempt = d.attempt + 1;
        let work = format!("pid{pid}-attempt{attempt}-{}", now_ms());
        let path = self.u.root().join(format!("handoffs/{work}.json"));
        std::fs::create_dir_all(path.parent().context("handoff dir")?)?;
        let mut value = json!({
            "work_id": work, "from_pid": pid, "to_pid": pid, "rank": 3,
            "from_harness": rec.harness, "to_harness": rec.harness,
            "reason": format!("continuation after {}", stop.label()), "attempt": attempt,
            "created": now_ms(), "brief": brief, "package": package,
            "checkpoint": saved.checkpoint,
        });
        value["package"] = json!(inner.mem.redact(value["package"].as_str().unwrap_or("")));
        std::fs::write(&path, serde_json::to_vec_pretty(&value)?)?;
        let preamble = continuation_preamble(
            attempt,
            max,
            pid,
            stop,
            d.turns,
            saved.checkpoint.as_ref(),
            saved.report.as_deref(),
            &rec.next,
        );
        {
            let dd = inner
                .st
                .pid_mut(pid)?
                .driven
                .as_mut()
                .context("not a driven seat")?;
            dd.attempt = attempt;
            dd.lineage = Some(d.lineage.unwrap_or(pid));
            dd.progress_mark = Some(mark);
            dd.stalls = stalls;
            dd.preamble = Some(preamble);
            dd.session = None;
            dd.turns = 0;
            dd.idle_turns = 0;
            dd.os_pid = None;
        }
        self.event(
            inner,
            "continue",
            json!({"pid": pid, "attempt": attempt, "stop": stop.label(),
                "progressed": progressed, "stalls": stalls, "harness": rec.harness,
                "model": d.model, "effort": d.effort, "package": path,
                "commit": saved.checkpoint.as_ref().map(|c| c.commit.clone())}),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::Driven;
    use super::*;
    use crate::{BriefFold, Drivers, FoldJob, SilentMapp, TurnRequest, TurnResult, Universe};
    use anyhow::bail;
    use serde_json::Value;
    use std::{
        collections::VecDeque,
        fs,
        sync::Mutex,
        time::{Duration, Instant},
    };

    /// A complete handoff with nothing to deliver (passes the context-ownership check).
    pub(crate) const HANDOFF_NONE: &str =
        "HANDOFF\ndeliverables:\n- none\ndecisions:\n- none\nlearnings:\n- none\nEND-HANDOFF";

    /// One scripted turn of the fake harness.
    #[derive(Clone)]
    enum Step {
        /// A normal turn that reports progress (now, next).
        Progress(&'static str, &'static str),
        /// A turn with text but no progress block (a worker that forgot).
        Text(&'static str),
        /// The final turn: UNVRS-RESULT.
        Done(&'static str),
        /// The driver's wall-clock timeout.
        Timeout,
        /// A harness crash.
        Crash,
        /// The harness reports it ran another model.
        Model(&'static str),
        Error(&'static str),
        GatedError(&'static str, Arc<std::sync::Barrier>, bool),
        ReportedError(&'static str),
        /// Waits at the gate, then runs the step (the test acts mid-turn).
        Gated(Arc<std::sync::Barrier>, Box<Step>),
    }

    /// Runs a script of turns in order and records every request it saw.
    struct Scripted {
        steps: Mutex<VecDeque<Step>>,
        seen: Mutex<Vec<TurnRequest>>,
    }
    impl Scripted {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                steps: Mutex::new(VecDeque::new()),
                seen: Mutex::new(vec![]),
            })
        }
        fn script(&self, steps: &[Step]) {
            *self.steps.lock().unwrap() = steps.iter().cloned().collect();
        }
        fn seen(&self) -> Vec<TurnRequest> {
            self.seen.lock().unwrap().clone()
        }
    }
    impl Drivers for Scripted {
        fn recovery_summary(&self, prompt: &str) -> Result<String> {
            self.turn(
                &TurnRequest {
                    harness: "summary".into(),
                    cwd: PathBuf::new(),
                    session: None,
                    prompt: prompt.into(),
                    env: vec![],
                    model: None,
                    effort: None,
                },
                &|_| {},
            )
            .map(|out| out.text)
        }
        fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
            bail!("no fold worker")
        }
        fn turn(&self, req: &TurnRequest, started: &dyn Fn(u32)) -> Result<TurnResult> {
            self.seen.lock().unwrap().push(req.clone());
            started(1);
            // The recovery summarizer is answered by its own script line if present,
            // else it fails like a route that is not installed.
            let step = match self.steps.lock().unwrap().pop_front() {
                Some(Step::Gated(gate, step)) => {
                    gate.wait();
                    Some(*step)
                }
                step => step,
            };
            let progress = |now: &str, next: &str| {
                format!(
                    "did a step\nUNVRS-PROGRESS\nnow: {now}\nnext: {next}\nopen:\n- [o1] the task\ndone:\n- [o2] step\nEND-PROGRESS"
                )
            };
            let ok = |text: String| TurnResult {
                session: Some("s".into()),
                text,
                model: req.model.clone(),
                effort: req.effort.clone(),
                ..Default::default()
            };
            match step {
                Some(Step::Progress(now, next)) => Ok(ok(progress(now, next))),
                Some(Step::Text(t)) if t.contains("UNVRS-RESULT") && !t.contains("HANDOFF") => {
                    Ok(ok(format!("{HANDOFF_NONE}\n{t}")))
                }
                Some(Step::Text(t)) => Ok(ok(t.into())),
                Some(Step::Done(r)) => Ok(ok(format!(
                    "{}\n{HANDOFF_NONE}\nUNVRS-RESULT: {r}",
                    progress("finishing", "")
                ))),
                Some(Step::Timeout) => bail!("{} turn timed out", req.harness),
                Some(Step::Crash) => bail!("{} exited with signal 9: boom", req.harness),
                Some(Step::Error(e)) => bail!("{e}"),
                Some(Step::GatedError(e, gate, reported)) => {
                    gate.wait();
                    if reported {
                        Ok(TurnResult {
                            error: Some(e.into()),
                            quota: true,
                            ..ok(progress("unsafe progress", "must not apply"))
                        })
                    } else {
                        bail!("{e}")
                    }
                }
                Some(Step::ReportedError(e)) => Ok(TurnResult {
                    error: Some(e.into()),
                    ..ok(String::new())
                }),
                Some(Step::Model(m)) => Ok(TurnResult {
                    model: Some(m.into()),
                    ..ok(progress("x", "y"))
                }),
                Some(Step::Gated(..)) => bail!("a gated step cannot be nested"),
                None => bail!("{} is not installed or not on PATH", req.harness),
            }
        }
    }

    struct Rig {
        k: Arc<Kernel>,
        drv: Arc<Scripted>,
        root: PathBuf,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn rig(tag: &str) -> Rig {
        let root = std::env::temp_dir().join(format!(
            "unvrs-recover-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        let u = Universe::at(&root).unwrap();
        u.init().unwrap();
        let drv = Scripted::new();
        let k = Kernel::open(u, drv.clone(), Arc::new(SilentMapp), now_ms()).unwrap();
        {
            let mut inner = k.lock();
            let l1 = inner
                .st
                .create(0, 1, "attached", "claude", "", Default::default(), None);
            let l2 = inner
                .st
                .create(l1, 2, "attached", "claude", "", Default::default(), None);
            inner.st.pid_mut(l2).unwrap().project = Some("proj".into());
            inner.st.pid_mut(l2).unwrap().state = "idle".into();
        }
        Rig { k, drv, root }
    }

    /// Spawns a worker under the L2 (PID 2) with a task dir like `task()` gives it.
    fn spawn(r: &Rig, model: Option<&str>) -> usize {
        let mut inner = r.k.lock();
        let pid = inner.st.create(
            2,
            3,
            "driven",
            "claude",
            "the task",
            Default::default(),
            None,
        );
        inner
            .mem
            .write_brief(
                pid,
                "spawn",
                Brief {
                    goal: "the task".into(),
                    now: "not started".into(),
                    next: vec!["start the task".into()],
                    open: vec!["[o1] the task".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let rec = inner.st.pid_mut(pid).unwrap();
        rec.state = "working".into();
        rec.project = Some("proj".into());
        rec.next = "start the task".into();
        rec.driven = Some(Driven {
            cpu: "headless".into(),
            model: model.map(str::to_owned),
            ..Default::default()
        });
        let dir = r.k.u.project_dir("proj").join(format!("tasks/pid-{pid}"));
        fs::create_dir_all(&dir).unwrap();
        let rec = inner.st.pid_mut(pid).unwrap();
        rec.contract = Some(
            json!({"intent": "the task", "spec": "do it", "shape": "report",
            "go_quote":"Ok, go", "done_when":"result delivered", "authority":"implement", "sources":[],
            "go_source":{"kind":"captain_prompt", "prompt_id":1, "thread":"claude:fixture",
                "at":now_ms() - super::super::driven::GO_WINDOW_MS - 1000},
            "model": model}),
        );
        fs::write(
            dir.join("contract.json"),
            serde_json::to_vec(rec.contract.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        rec.driven.as_mut().unwrap().cwd = dir;
        pid
    }

    /// Waits until no worker of the lineage is running; returns the last PID.
    fn settle(r: &Rig, first: usize) -> usize {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            {
                let inner = r.k.lock();
                let mut pid = first;
                while let Some(n) = inner.st.pid(pid).unwrap().handed_to {
                    pid = n;
                }
                let rec = inner.st.pid(pid).unwrap();
                if rec.state == "ended" && inner.running.is_empty() {
                    return pid;
                }
            }
            assert!(
                Instant::now() < until,
                "lineage from PID {first} did not settle"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn events(r: &Rig, kind: &str) -> Vec<Value> {
        r.k.journal
            .tail(2000)
            .into_iter()
            .filter(|e| e["kind"] == kind || e["event"] == kind)
            .collect()
    }

    fn result_json(r: &Rig, pid: usize) -> Value {
        let p =
            r.k.u
                .project_dir("proj")
                .join(format!("tasks/pid-{pid}/result.json"));
        serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        super::git(dir, args).unwrap()
    }

    #[test]
    fn stops_are_classified() {
        assert_eq!(classify_error("claude turn timed out"), Stop::Timeout);
        assert!(matches!(
            classify_error("codex exited with 1: x"),
            Stop::Crash(_)
        ));
        assert!(matches!(
            classify_error("claude is not installed or not on PATH"),
            Stop::Harness(_)
        ));
        for refusal in [
            "model not confirmed: missing response",
            "effort not confirmed: missing response",
            "native delegation restriction not confirmed",
            "working directory mismatch",
            "codex thread/start rejected: unavailable model",
            "codex thread/resume rejected: invalid session",
        ] {
            assert!(!classify_error(refusal).retryable(), "{refusal}");
        }
        assert!(Stop::NoProgress(3).retryable() && Stop::Timeout.retryable());
        assert!(!Stop::Harness("x".into()).retryable());
    }

    #[test]
    fn unconfirmed_cleanup_blocks_without_checkpoint_summary_or_successor() {
        for (quota_low, reported) in [(false, false), (true, false), (false, true), (true, true)] {
            let r = rig("cleanup-unconfirmed");
            let pid = spawn(&r, None);
            let cwd =
                r.k.lock()
                    .st
                    .pid(pid)
                    .unwrap()
                    .driven
                    .as_ref()
                    .unwrap()
                    .cwd
                    .clone();
            git(&cwd, &["init", "-q", "-b", "work"]);
            fs::write(cwd.join("preserved.txt"), "unfinished work").unwrap();
            let brief = r.k.lock().mem.session(pid).unwrap().brief;
            let gate = Arc::new(std::sync::Barrier::new(2));
            r.drv.script(&[Step::GatedError(
                "cleanup unconfirmed: owned terminal unavailable",
                gate.clone(),
                reported,
            )]);
            let k = r.k.clone();
            let worker = thread::spawn(move || k.worker_loop(pid));
            let until = Instant::now() + Duration::from_secs(2);
            while r.drv.seen().is_empty() {
                assert!(Instant::now() < until);
                thread::yield_now();
            }
            r.k.lock().st.pid_mut(pid).unwrap().quota_low = quota_low;
            gate.wait();
            worker.join().unwrap();
            assert_eq!(settle(&r, pid), pid);
            let inner = r.k.lock();
            let rec = inner.st.pid(pid).unwrap();
            assert!(
                rec.result
                    .as_ref()
                    .unwrap()
                    .contains("cleanup is unconfirmed")
            );
            assert!(rec.handed_to.is_none());
            assert_eq!(
                serde_json::to_value(inner.mem.session(pid).unwrap().brief).unwrap(),
                serde_json::to_value(brief).unwrap()
            );
            assert!(
                inner
                    .st
                    .pid(2)
                    .unwrap()
                    .wakes
                    .iter()
                    .any(|w| w.text.contains("blocked"))
            );
            drop(inner);
            assert!(events(&r, "checkpoint").is_empty());
            assert!(events(&r, "continue").is_empty());
            assert_eq!(r.drv.seen().len(), 1);
            assert_eq!(
                fs::read_to_string(cwd.join("preserved.txt")).unwrap(),
                "unfinished work"
            );
            assert!(git(&cwd, &["status", "--porcelain"]).contains("preserved.txt"));
        }
    }

    /// Context ownership (context-ownership.md §4): a result without a full handoff,
    /// or with a deliverable outside UNVRS, goes back to the worker with the reasons;
    /// the fixed reply is accepted and its decisions and learnings become notes.
    #[test]
    fn ownership_bounces_a_leaky_result_then_accepts_and_files_notes() {
        let r = rig("ownership-bounce");
        r.drv.script(&[
            Step::Text(
                "HANDOFF\ndeliverables:\n- /definitely/outside/report.md\ndecisions:\nEND-HANDOFF\nUNVRS-RESULT: first try",
            ),
            Step::Text(
                "HANDOFF\ndeliverables:\n- report.md\ndecisions:\n- kept reports in the task directory\nlearnings:\n- the gate reads HANDOFF\nEND-HANDOFF\nUNVRS-RESULT: fixed",
            ),
        ]);
        let pid = spawn(&r, None);
        let dir = r.k.u.project_dir("proj").join(format!("tasks/pid-{pid}"));
        fs::write(dir.join("report.md"), "the report").unwrap();
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(last, pid);
        let res = result_json(&r, pid);
        assert_eq!(res["how"], "done", "{res}");
        assert_eq!(res["result"], "fixed", "{res}");
        let own = &res["ownership"];
        assert_eq!(own["verdict"], "accepted", "{own}");
        assert_eq!(own["bounces"], 1, "{own}");
        assert_eq!(own["history"][0]["verdict"], "bounced", "{own}");
        let first: Vec<String> =
            serde_json::from_value(own["history"][0]["leaks"].clone()).unwrap();
        assert!(
            first
                .iter()
                .any(|l| l.contains("deliverable outside UNVRS: /definitely/outside/report.md")),
            "{first:?}"
        );
        assert!(
            first.iter().any(|l| l.contains("no `learnings:` section")),
            "{first:?}"
        );
        assert_eq!(own["notes"].as_array().unwrap().len(), 2, "{own}");
        let seen = r.drv.seen();
        assert!(
            seen[1]
                .prompt
                .contains("context-ownership check failed (1/3)")
                && seen[1].prompt.contains("deliverable outside UNVRS"),
            "{}",
            seen[1].prompt
        );
        let notes = crate::MemStore::project(r.k.u.root(), "proj")
            .notes()
            .unwrap();
        let src = format!("pid:{pid}");
        assert!(notes.iter().any(|n| n.source == src
            && n.text.contains("decision (PID")
            && n.text.contains("kept reports in the task directory")));
        assert!(
            notes
                .iter()
                .any(|n| n.source == src && n.text.contains("learning (PID"))
        );
    }

    /// A worker that never fixes its handoff ends blocked after MAX_BOUNCES, with the leaks.
    #[test]
    fn ownership_blocks_after_max_bounces() {
        let r = rig("ownership-block");
        let n = super::super::ownership::MAX_BOUNCES as usize + 1;
        r.drv.script(&vec![
            Step::Text(
                "HANDOFF\nEND-HANDOFF\nUNVRS-RESULT: never fixed"
            );
            n
        ]);
        let pid = spawn(&r, None);
        r.k.start_worker(pid);
        let res = result_json(&r, settle(&r, pid));
        assert_eq!(res["how"], "blocked", "{res}");
        assert!(
            res["result"]
                .as_str()
                .unwrap()
                .contains("context-ownership check failed 4 times"),
            "{res}"
        );
        assert_eq!(res["ownership"]["verdict"], "blocked", "{res}");
        assert_eq!(r.drv.seen().len(), n);
    }

    /// S7 (C10): a Done with a FIELD-NOTES block files its notes in result.json and the
    /// lead's wake; the block is not part of the result.
    #[test]
    fn s7_done_with_notes_files_field_notes() {
        let r = rig("s7-notes");
        r.drv.script(&[Step::Text(
            "did it\nUNVRS-PROGRESS\nnow: finishing\nnext: \nopen:\ndone:\n- [o1] the task\nEND-PROGRESS\nFIELD-NOTES\n- bug: finish() dropped notes (driven.rs)\n- friction: slow build\nEND-NOTES\nUNVRS-RESULT: all done",
        )]);
        let pid = spawn(&r, None);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        let res = result_json(&r, last);
        assert_eq!(res["how"], "done", "{res}");
        assert_eq!(res["result"], "all done", "{res}");
        assert_eq!(
            res["field_notes"],
            json!([
                "bug: finish() dropped notes (driven.rs)",
                "friction: slow build"
            ]),
            "{res}"
        );
        let inner = r.k.lock();
        let wake = inner.st.pid(2).unwrap().wakes.last().unwrap().text.clone();
        assert!(wake.contains("Field notes: bug: finish()"), "{wake}");
    }

    /// S7 (C10): notes written after UNVRS-RESULT are cut from the result; a Done with no
    /// block records why instead of an empty list.
    #[test]
    fn s7_missing_or_trailing_notes_are_never_empty() {
        let r = rig("s7-none");
        r.drv.script(&[Step::Done("plain")]);
        let pid = spawn(&r, None);
        r.k.start_worker(pid);
        let res = result_json(&r, settle(&r, pid));
        assert_eq!(
            res["field_notes"],
            json!(["none: the final reply had no FIELD-NOTES block"]),
            "{res}"
        );
        assert_eq!(
            super::super::driven::parse_field_notes("UNVRS-RESULT: x\nFIELD-NOTES\n- none\n"),
            Some(vec![])
        );
        let r = rig("s7-trailing");
        r.drv.script(&[Step::Text(
            "UNVRS-RESULT: the report\nFIELD-NOTES\n- fact: a\nEND-NOTES",
        )]);
        let pid = spawn(&r, None);
        r.k.start_worker(pid);
        let res = result_json(&r, settle(&r, pid));
        assert_eq!(res["result"], "the report", "{res}");
        assert_eq!(res["field_notes"], json!(["fact: a"]), "{res}");
    }

    #[test]
    fn inline_result_marker_in_field_note_preserves_saved_result_and_note() {
        let r = rig("inline-result-note");
        r.drv.script(&[Step::Text(
            "UNVRS-RESULT: intended result\nFIELD-NOTES\n- fact: inline UNVRS-RESULT: is prose\n- fact: the whole note survives\nEND-NOTES",
        )]);
        let pid = spawn(&r, None);
        r.k.start_worker(pid);
        let res = result_json(&r, settle(&r, pid));
        assert_eq!(res["result"], "intended result", "{res}");
        assert_eq!(
            res["field_notes"],
            json!([
                "fact: inline UNVRS-RESULT: is prose",
                "fact: the whole note survives"
            ]),
            "{res}"
        );
    }

    /// A colon-less `UNVRS-RESULT` after FIELD-NOTES finishes on the turn that
    /// would otherwise trigger the no-progress stop.
    #[test]
    fn colon_less_result_before_stall_finishes_with_notes() {
        use super::super::driven::{parse_field_notes, parse_result};
        assert_eq!(
            parse_result("x\nUNVRS-RESULT: a\nEND-RESULT").as_deref(),
            Some("a")
        );
        assert_eq!(parse_result("**UNVRS-RESULT**\nb\n").as_deref(), Some("b"));
        assert_eq!(parse_result("add FIELD-NOTES and then UNVRS-RESULT"), None);
        assert_eq!(
            parse_result("UNVRS-RESULT: fixed FIELD-NOTES parsing\nFIELD-NOTES\n- x").as_deref(),
            Some("fixed FIELD-NOTES parsing")
        );
        assert_eq!(
            parse_field_notes(
                "FIELD-NOTES\n- fact: a\nEND-FIELD-NOTES\nUNVRS-RESULT\n- not a note"
            ),
            Some(vec!["fact: a".into()])
        );
        let r = rig("colon-less");
        let mut steps: Vec<Step> = (0..stall_turns())
            .map(|_| Step::Progress("working", "keep going"))
            .collect();
        steps.push(Step::Text(
            "done\nUNVRS-PROGRESS\nnow: finishing\nnext: \nopen:\ndone:\n- [o1] the task\nEND-PROGRESS\nFIELD-NOTES\n- fact: a\nEND-FIELD-NOTES\n\nUNVRS-RESULT\nstatus: done\n- commit abc\nEND-RESULT",
        ));
        r.drv.script(&steps);
        let pid = spawn(&r, None);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(last, pid, "no continuation");
        assert_eq!(r.drv.seen().len(), stall_turns() as usize + 1);
        let res = result_json(&r, pid);
        assert_eq!(res["how"], "done", "{res}");
        assert_eq!(res["result"], "status: done\n- commit abc", "{res}");
        assert_eq!(res["field_notes"], json!(["fact: a"]), "{res}");
        assert!(events(&r, "stop").is_empty());
        assert!(events(&r, "continue").is_empty());
    }

    #[test]
    fn no_progress_is_checkpointed_and_continued_in_place_to_a_result() {
        let r = rig("stalled-turns");
        let pid = spawn(&r, Some("claude-fable-5-1"));
        {
            let mut inner = r.k.lock();
            let rec = inner.st.pid_mut(pid).unwrap();
            rec.driven.as_mut().unwrap().effort = Some("high".into());
            rec.contract.as_mut().unwrap()["effort"] = json!("high");
        }
        // A dirty work tree the worker left behind, with build output beside it.
        let wt =
            r.k.lock()
                .st
                .pid(pid)
                .unwrap()
                .driven
                .as_ref()
                .unwrap()
                .cwd
                .join("wt");
        fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q", "-b", "work"]);
        fs::write(wt.join("a.txt"), "v1").unwrap();
        git(&wt, &["add", "-A"]);
        git(
            &wt,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "start",
            ],
        );
        fs::write(wt.join("a.txt"), "v2 uncommitted").unwrap();
        fs::create_dir_all(wt.join("target/debug")).unwrap();
        fs::write(wt.join("target/debug/big.bin"), "build output").unwrap();
        fs::write(
            wt.parent().unwrap().join("REPORT.md"),
            "# wip\nnext step: finish",
        )
        .unwrap();
        // The same progress block every turn: the first sets the mark, the next
        // stall_turns() leave brief and HEAD unchanged.
        let stalled = stall_turns() as usize + 1;
        let mut steps = vec![Step::Progress("s1", "keep going"); stalled];
        steps.push(Step::Done("all good"));
        let approved = r.k.lock().st.pid(pid).unwrap().contract.clone();
        r.drv.script(&steps);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(last, pid, "the continuation keeps the PID");
        let inner = r.k.lock();
        assert_eq!(inner.st.pids.len(), 3, "no successor PID");
        let rec = inner.st.pid(pid).unwrap().clone();
        assert_eq!(
            rec.contract, approved,
            "recovery inherits the original, now stale go"
        );
        assert_eq!(rec.result.as_deref(), Some("all good"));
        assert!(rec.handed_to.is_none() && rec.handed_from.is_none());
        let d = rec.driven.as_ref().unwrap();
        assert_eq!((d.attempt, d.lineage), (1, Some(pid)));
        assert_eq!(
            d.model.as_deref(),
            Some("claude-fable-5-1"),
            "the pin stays"
        );
        assert_eq!(d.effort.as_deref(), Some("high"));
        drop(inner);
        // The checkpoint commit exists, without the build output.
        let log = git(&wt, &["log", "--oneline", "-1"]);
        assert!(
            log.contains(&format!(
                "unvrs checkpoint: PID {pid} stopped (no-progress)"
            )),
            "{log}"
        );
        assert_eq!(git(&wt, &["status", "--porcelain"]), "?? target/");
        assert_eq!(git(&wt, &["ls-files", "target"]), "");
        let sha = git(&wt, &["rev-parse", "--short", "HEAD"]);
        // The continuation's first prompt is a fresh session with the preamble.
        let seen = r.drv.seen();
        assert_eq!(seen.len(), stalled + 1);
        assert_eq!(seen[stalled - 1].session.as_deref(), Some("s"));
        let first = &seen[stalled];
        assert_eq!(first.session, None);
        let first_prompt = &first.prompt;
        assert!(
            first_prompt.contains(&format!("CONTINUATION attempt 1. PID {pid} stopped")),
            "{first_prompt}"
        );
        assert!(
            first_prompt.contains("changed neither its brief"),
            "{first_prompt}"
        );
        assert!(
            first_prompt.contains("Resume; do not restart"),
            "{first_prompt}"
        );
        assert!(
            first_prompt.contains(&format!("at commit {sha}")),
            "{first_prompt}"
        );
        assert!(first_prompt.contains("REPORT.md so far"), "{first_prompt}");
        assert!(
            first_prompt.contains("Next step: keep going"),
            "{first_prompt}"
        );
        assert!(first_prompt.contains(CHECKPOINT_RULE));
        assert_eq!(first.model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(first.effort.as_deref(), Some("high"));
        // Journal, audit record and result package.
        let stop = &events(&r, "stop")[0];
        assert_eq!(stop["stop"], "no-progress");
        let cp = &events(&r, "checkpoint")[0];
        assert_eq!(cp["committed"], true);
        assert_eq!(cp["commit"], sha);
        let c = &events(&r, "continue")[0];
        assert_eq!(
            (c["pid"].as_u64(), c["attempt"].as_u64()),
            (Some(pid as u64), Some(1))
        );
        let audit: Vec<_> = fs::read_dir(r.k.u.root().join("handoffs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            audit
                .iter()
                .any(|f| f.starts_with(&format!("pid{pid}-attempt1-"))),
            "{audit:?}"
        );
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "done");
        assert_eq!(pkg["attempt"], 1);
        assert_eq!(pkg["lineage"], pid);
        assert_eq!(pkg["attempts"][0]["stop"], "no-progress");
        assert_eq!(pkg["attempts"][0]["turns"], stalled);
        assert_eq!(pkg["attempts"][0]["commit"], sha);
        assert!(
            pkg["brief"]["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a.as_str().unwrap().contains(&sha))
        );
        // The lead was woken once, for the result only.
        let wakes = r.k.lock().st.pid(2).unwrap().wakes.clone();
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].kind, "done");
    }

    #[test]
    fn a_progressing_worker_has_no_turn_budget() {
        let r = rig("long");
        let pid = spawn(&r, None);
        let mut steps: Vec<Step> = (0..25)
            .map(|i| Step::Progress(Box::leak(format!("step {i}").into_boxed_str()), "more"))
            .collect();
        steps.push(Step::Done("long job done"));
        r.drv.script(&steps);
        r.k.start_worker(pid);
        assert_eq!(settle(&r, pid), pid);
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "done");
        assert_eq!(pkg["attempt"], 0);
        assert_eq!(r.drv.seen().len(), 26);
        assert!(events(&r, "stop").is_empty());
    }

    #[test]
    fn timeout_and_crash_are_continued_and_a_missing_brief_is_summarized() {
        let r = rig("timeout");
        let pid = spawn(&r, None);
        // Turn 1 works but reports no progress block; turn 2 times out. The brief never
        // advanced, so the summarizer route is asked (script line 3) and its block
        // becomes the brief. The continuation crashes once more, then finishes.
        r.drv.script(&[
            Step::Text("looked around, forgot the block"),
            Step::Timeout,
            Step::Progress("halfway through the study", "write the second half"),
            Step::Crash,
            Step::Done("finished"),
        ]);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        let seen = r.drv.seen();
        assert_eq!(seen.len(), 5);
        let summary = &seen[2];
        assert!(
            summary.prompt.contains("recovery summarizer"),
            "{}",
            summary.prompt
        );
        assert!(summary.prompt.contains("forgot the block"));
        assert_eq!(summary.harness, "summary");
        assert!(
            seen[3].prompt.contains("Next step: write the second half"),
            "{}",
            seen[3].prompt
        );
        assert!(seen[3].prompt.contains("its turn timed out"));
        assert!(seen[4].prompt.contains("CONTINUATION attempt 2. PID"));
        assert_eq!(last, pid, "continuations keep the PID");
        assert_eq!(
            seen[4].session, None,
            "each continuation starts a fresh session"
        );
        assert!(seen[4].prompt.contains("harness crashed"));
        let folds = events(&r, "fold");
        assert!(
            folds
                .iter()
                .any(|f| f["reason"] == "recovery summary" && f["result"] == "applied"),
            "{folds:?}"
        );
        let pkg = result_json(&r, last);
        assert_eq!(pkg["how"], "done");
        assert_eq!(pkg["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(pkg["attempts"][0]["stop"], "timeout");
        assert_eq!(pkg["attempts"][1]["stop"], "crash");
        assert_eq!(pkg["attempts"][1]["progressed"], false);
        let stops = events(&r, "stop");
        assert_eq!(stops.len(), 2);
    }

    #[test]
    fn two_attempts_without_progress_block_the_lineage() {
        let r = rig("stall");
        let pid = spawn(&r, None);
        r.drv.script(&[
            Step::Timeout,
            Step::Timeout,
            Step::Timeout,
            Step::Done("never"),
        ]);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        let pkg = result_json(&r, last);
        assert_eq!(pkg["how"], "blocked");
        let text = pkg["result"].as_str().unwrap();
        assert!(
            text.starts_with("blocked: 2 consecutive attempts made no progress"),
            "{text}"
        );
        assert!(
            text.contains(&format!("attempt 0 PID {pid} timeout")),
            "{text}"
        );
        assert!(text.contains("attempt 2 PID"), "{text}");
        assert_eq!(pkg["attempts"].as_array().unwrap().len(), 3);
        assert_eq!(r.drv.seen().len(), 3, "the fourth script line never ran");
        let b = &events(&r, "blocked")[0];
        assert_eq!(b["lineage"], pid);
        let wakes = r.k.lock().st.pid(2).unwrap().wakes.clone();
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].kind, "blocked");
        assert!(wakes[0].text.contains("blocked"), "{}", wakes[0].text);
    }

    #[test]
    fn continuations_are_unbounded_unless_a_ceiling_is_set() {
        // Every attempt progresses (a new `now`), and times out.
        let script = |finish: Step| {
            let mut steps = vec![];
            for now in ["a", "b", "c", "d", "e", "f"] {
                steps.push(Step::Progress(now, "n"));
                steps.push(Step::Timeout);
            }
            steps.push(finish);
            steps
        };
        let r = rig("unbounded");
        let pid = spawn(&r, None);
        r.drv.script(&script(Step::Done("finally")));
        r.k.start_worker(pid);
        assert_eq!(settle(&r, pid), pid);
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "done", "{}", pkg["result"]);
        assert_eq!(pkg["attempt"], 6);
        assert_eq!(events(&r, "continue").len(), 6);
        assert!(
            r.drv.seen()[12]
                .prompt
                .contains("CONTINUATION attempt 6. PID")
        );

        let r = rig("ceiling");
        let pid = spawn(&r, None);
        r.k.lock()
            .st
            .pid_mut(pid)
            .unwrap()
            .driven
            .as_mut()
            .unwrap()
            .max_continuations = Some(3);
        r.drv.script(&script(Step::Done("never")));
        r.k.start_worker(pid);
        assert_eq!(settle(&r, pid), pid);
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "blocked");
        let text = pkg["result"].as_str().unwrap();
        assert!(
            text.starts_with("blocked: out of continuations (3"),
            "{text}"
        );
        assert!(text.contains("Brief's next step: n"), "{text}");
        assert_eq!(pkg["attempts"].as_array().unwrap().len(), 4);
        assert!(
            pkg["attempts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["progressed"] == true)
        );
        assert_eq!(pkg["attempt"], 3);
        assert_eq!(events(&r, "continue").len(), 3);
        assert!(
            r.drv.seen()[2]
                .prompt
                .contains("CONTINUATION attempt 1 of 3.")
        );
    }

    #[test]
    fn ceiling_and_budgets_parse() {
        assert_eq!(parse_ceiling(None), None);
        assert_eq!(parse_ceiling(Some("")), None);
        assert_eq!(parse_ceiling(Some("unlimited")), None);
        assert_eq!(parse_ceiling(Some("none")), None);
        assert_eq!(parse_ceiling(Some(" 5 ")), Some(5));
        assert_eq!(parse_ceiling(Some("0")), Some(0));
        assert_eq!(parse_ceiling(Some("lots")), None);
    }

    #[test]
    fn a_pin_mismatch_is_not_retried() {
        let r = rig("pin");
        let pid = spawn(&r, Some("claude-fable-5-1"));
        r.drv
            .script(&[Step::Model("claude-sonnet-5"), Step::Done("never")]);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(last, pid, "no continuation");
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "model-mismatch");
        assert!(pkg["result"].as_str().unwrap().contains("not retryable"));
        assert_eq!(r.drv.seen().len(), 1);
        let s = &events(&r, "stop")[0];
        assert_eq!(
            (s["stop"].as_str(), s["retryable"].as_bool()),
            (Some("model-mismatch"), Some(false))
        );
        assert!(events(&r, "continue").is_empty());
    }

    #[test]
    fn a_missing_harness_is_blocked_at_once_with_the_reason() {
        let r = rig("harness");
        let pid = spawn(&r, None);
        r.drv.script(&[]); // the fake answers "not installed"
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(last, pid);
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "blocked");
        assert!(
            pkg["result"]
                .as_str()
                .unwrap()
                .contains("not retryable: its harness cannot run here")
        );
    }

    #[test]
    fn failed_summary_keeps_transcript_available_to_the_continuation() {
        let r = rig("summary-failure");
        let pid = spawn(&r, None);
        r.drv.script(&[
            Step::Text("preserve this unfinished investigation"),
            Step::Timeout,
            Step::Error("all summary routes unavailable"),
            Step::Done("finished"),
        ]);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(result_json(&r, last)["how"], "done");
        let seen = r.drv.seen();
        assert!(
            seen[3]
                .prompt
                .contains("preserve this unfinished investigation")
        );
        assert!(
            events(&r, "fold")
                .iter()
                .any(|e| e["reason"] == "recovery summary" && e["result"] == "failed")
        );
    }

    #[test]
    fn driver_pin_errors_are_not_retried() {
        for (error, how) in [
            (
                "model mismatch: requested fable, the harness ran other",
                "model-mismatch",
            ),
            ("effort not confirmed: requested high", "effort-mismatch"),
            ("effort not applied: requested high", "effort-mismatch"),
            (
                "effort high is pinned, but codex cannot verify it",
                "effort-mismatch",
            ),
        ] {
            let r = rig(how);
            let pid = spawn(&r, Some("claude-fable-5-1"));
            r.drv.script(&[Step::Error(error), Step::Done("never")]);
            r.k.start_worker(pid);
            assert_eq!(settle(&r, pid), pid);
            assert_eq!(result_json(&r, pid)["how"], how);
            assert_eq!(r.drv.seen().len(), 1);
            assert!(events(&r, "continue").is_empty());
        }
    }

    #[test]
    fn reported_harness_error_recovers_without_spending_the_turn_budget() {
        let r = rig("reported-error");
        let pid = spawn(&r, None);
        r.drv.script(&[
            Step::Progress("started", "finish"),
            Step::ReportedError("broken stream"),
            Step::Done("finished"),
        ]);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(result_json(&r, last)["how"], "done");
        assert_eq!(events(&r, "stop")[0]["stop"], "crash");
        assert_eq!(r.drv.seen().len(), 3);
    }

    #[test]
    fn changed_items_count_as_progress_even_when_lengths_stay_equal() {
        let mut brief = Brief {
            done: vec!["old".into()],
            open: vec!["old".into()],
            ..Default::default()
        };
        let before = progress_mark(&brief, Some("abc"));
        brief.done[0] = "new".into();
        assert_ne!(before, progress_mark(&brief, Some("abc")));
        let before = progress_mark(&brief, Some("abc"));
        brief.open[0] = "new".into();
        assert_ne!(before, progress_mark(&brief, Some("abc")));
        assert_ne!(
            progress_mark(&brief, Some("abc")),
            progress_mark(&brief, Some("def"))
        );
    }

    #[test]
    fn failed_git_checkpoint_keeps_work_and_blocks_continuation() {
        let r = rig("checkpoint-failure");
        let pid = spawn(&r, None);
        let wt =
            r.k.lock()
                .st
                .pid(pid)
                .unwrap()
                .driven
                .as_ref()
                .unwrap()
                .cwd
                .join("wt");
        fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q", "-b", "work"]);
        fs::write(wt.join("work.txt"), "valuable work").unwrap();
        fs::write(wt.join(".git/index.lock"), "held").unwrap();
        r.drv.script(&[Step::Timeout, Step::Done("never")]);
        r.k.start_worker(pid);
        assert_eq!(settle(&r, pid), pid);
        let pkg = result_json(&r, pid);
        assert_eq!(pkg["how"], "blocked");
        assert!(
            pkg["result"]
                .as_str()
                .unwrap()
                .contains("checkpoint failed")
        );
        assert_eq!(
            fs::read_to_string(wt.join("work.txt")).unwrap(),
            "valuable work"
        );
        assert_eq!(r.drv.seen().len(), 1);
        assert!(events(&r, "continue").is_empty());
    }

    #[test]
    fn unapproved_workers_cannot_start_handoff_or_continue() {
        let r = rig("no-go");
        let pid = spawn(&r, None);
        {
            let mut inner = r.k.lock();
            inner.st.pid_mut(pid).unwrap().contract.as_mut().unwrap()["go_source"] =
                json!({"kind":"model_memory"});
            let before = inner.st.pids.len();
            assert_eq!(
                r.k.handoff_locked(&mut inner, pid, Some("codex"), "manual")
                    .unwrap_err()
                    .to_string(),
                super::super::driven::NO_GO
            );
            assert_eq!(inner.st.pids.len(), before);
            let saved = Saved {
                checkpoint: None,
                report: None,
                brief_version: 0,
                checkpoint_error: None,
                needs_summary: false,
            };
            assert_eq!(
                r.k.continue_locked(
                    &mut inner,
                    pid,
                    &Stop::Timeout,
                    &saved,
                    String::new(),
                    0,
                    false,
                    None
                )
                .unwrap_err()
                .to_string(),
                super::super::driven::NO_GO
            );
        }
        r.k.start_worker(pid);
        assert_eq!(settle(&r, pid), pid);
        assert!(
            r.drv.seen.lock().unwrap().is_empty(),
            "no harness turn may start"
        );
        assert_eq!(result_json(&r, pid)["result"], super::super::driven::NO_GO);
        assert!(events(&r, "continue").is_empty());
        assert!(events(&r, "handoff").is_empty());
    }

    #[test]
    fn active_handoff_refuses_without_successor_or_signal() {
        let r = rig("active-handoff");
        let pid = spawn(&r, None);
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .spawn_owned()
            .unwrap();
        {
            let mut inner = r.k.lock();
            let d = inner.st.pid_mut(pid).unwrap().driven.as_mut().unwrap();
            d.busy = true;
            d.os_pid = Some(child.id());
            inner.running.insert(pid);
            let err =
                r.k.handoff_locked(&mut inner, pid, Some("claude"), "manual")
                    .unwrap_err();
            assert!(err.to_string().contains("active driver"));
            assert_eq!(inner.st.pids.len(), 3);
            assert_eq!(inner.st.pid(pid).unwrap().state, "working");
            assert!(inner.st.pid(pid).unwrap().handed_to.is_none());
        }
        thread::sleep(Duration::from_millis(30));
        assert!(
            child.try_wait().unwrap().is_none(),
            "refused handoff signalled live writer"
        );
        assert!(r.drv.seen().is_empty());
        assert!(events(&r, "handoff").is_empty());
        child.stdin.take();
        child.wait().unwrap();
    }

    #[test]
    fn quota_handoff_preserves_recovery_history_and_effort_harness() {
        let r = rig("quota-history");
        let pid = spawn(&r, None);
        {
            let mut inner = r.k.lock();
            let rec = inner.st.pid_mut(pid).unwrap();
            rec.harness = "codex".into();
            rec.contract.as_mut().unwrap()["effort"] = json!("high");
            let d = rec.driven.as_mut().unwrap();
            d.effort = Some("high".into());
            d.attempt = 2;
            d.lineage = Some(42);
            d.stalls = 1;
            d.progress_mark = Some("mark".into());
            d.attempts = vec![json!({"pid":42,"stop":"timeout"})];
            d.max_continuations = Some(3);
            let err =
                r.k.handoff_locked(&mut inner, pid, None, "low quota")
                    .unwrap_err();
            assert!(err.to_string().contains("change its harness"));
            assert_eq!(inner.st.pids.len(), 3);
        }
        let approved = r.k.lock().st.pid(pid).unwrap().contract.clone();
        let child = r.k.lock().st.create(
            pid,
            3,
            "driven",
            "codex",
            "bounded child",
            Default::default(),
            None,
        );
        r.drv.script(&[Step::Done("same-harness resume")]);
        let next = {
            let mut inner = r.k.lock();
            r.k.handoff_locked(&mut inner, pid, Some("codex"), "manual resume")
                .unwrap()
        };
        assert_eq!(settle(&r, pid), next);
        let inner = r.k.lock();
        let rec = inner.st.pid(next).unwrap();
        assert_eq!(
            rec.contract, approved,
            "handoff inherits the original go and done-when"
        );
        assert_eq!(
            inner.st.pid(child).unwrap().parent,
            next,
            "children follow their parent handoff"
        );
        let d = rec.driven.as_ref().unwrap();
        assert_eq!(rec.harness, "codex");
        assert_eq!(d.effort.as_deref(), Some("high"));
        assert_eq!(d.attempt, 3);
        assert_eq!(d.lineage, Some(42));
        assert_eq!(d.stalls, 1);
        assert_eq!(d.progress_mark.as_deref(), Some("mark"));
        assert_eq!(d.attempts[0]["pid"], 42);
        assert_eq!(d.max_continuations, Some(3), "the ceiling travels");
        drop(inner);
        let mut inner = r.k.lock();
        assert!(
            r.k.handoff_locked(&mut inner, next, Some("codex"), "resume")
                .is_err()
        );
    }

    #[test]
    fn checkpoint_filter_wait_does_not_hold_kernel_lock_or_allow_overlap() {
        let r = rig("filter-lock");
        let pid = spawn(&r, None);
        let wt =
            r.k.lock()
                .st
                .pid(pid)
                .unwrap()
                .driven
                .as_ref()
                .unwrap()
                .cwd
                .join("wt");
        fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q", "-b", "work"]);
        fs::write(wt.join(".gitattributes"), "*.txt filter=hang\n").unwrap();
        fs::write(wt.join("work.txt"), "valuable work").unwrap();
        let marker = r.root.join("checkpoint-started");
        let release = r.root.join("checkpoint-release");
        // The filter holds the checkpoint open until the test releases it, so the
        // checks below never race the filter. On a panic the rig removes root (and
        // the marker), which also ends the loop; CHECKPOINT_TIMEOUT reaps it otherwise.
        git(
            &wt,
            &[
                "config",
                "filter.hang.clean",
                &format!(
                    "touch {m}; while [ ! -e {r} ] && [ -e {m} ]; do sleep 0.01; done; cat",
                    m = marker.display(),
                    r = release.display()
                ),
            ],
        );
        r.drv.script(&[Step::Timeout, Step::Done("continued")]);
        r.k.start_worker(pid);
        let until = Instant::now() + Duration::from_secs(20);
        while !marker.exists() {
            assert!(Instant::now() < until, "checkpoint did not start");
            thread::sleep(Duration::from_millis(10));
        }
        {
            let mut inner = r.k.inner.try_lock().expect("git filter held kernel lock");
            assert!(
                inner
                    .st
                    .pid(pid)
                    .unwrap()
                    .driven
                    .as_ref()
                    .unwrap()
                    .recovering
            );
            assert!(
                r.k.handoff_locked(&mut inner, pid, Some("claude"), "manual")
                    .unwrap_err()
                    .to_string()
                    .contains("checkpointing")
            );
            assert_eq!(inner.st.pids.len(), 3);
        }
        fs::write(&release, "").unwrap();
        assert_eq!(settle(&r, pid), pid, "continued in place");
        assert_eq!(result_json(&r, pid)["how"], "done");
    }

    #[test]
    fn restart_blocks_interrupted_workers_without_spawning_or_killing_old_writer() {
        for (busy, recovering) in [(true, false), (false, true)] {
            let r = rig(if busy {
                "restart-busy"
            } else {
                "restart-checkpoint"
            });
            let pid = spawn(&r, Some("gpt-6.1-sol"));
            let mut child = Command::new("cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .process_group(0)
                .spawn_owned()
                .unwrap();
            {
                let mut inner = r.k.lock();
                let d = inner.st.pid_mut(pid).unwrap().driven.as_mut().unwrap();
                d.busy = busy;
                d.recovering = recovering;
                d.os_pid = Some(child.id());
                d.attempt = 2;
                d.lineage = Some(42);
                d.attempts = vec![json!({"pid":42,"stop":"timeout"})];
                r.k.save(&mut inner);
            }
            let reopened = Kernel::open(
                Universe::at(&r.root).unwrap(),
                r.drv.clone(),
                Arc::new(SilentMapp),
                now_ms(),
            )
            .unwrap();
            {
                let inner = reopened.lock();
                let rec = inner.st.pid(pid).unwrap();
                let d = rec.driven.as_ref().unwrap();
                assert_eq!(rec.state, "ended");
                assert!(
                    rec.result
                        .as_deref()
                        .unwrap()
                        .contains("Old writer ownership is unverified")
                );
                assert_eq!(d.os_pid, Some(child.id()));
                assert_eq!(d.attempt, 2);
                assert_eq!(d.lineage, Some(42));
                assert_eq!(d.attempts.len(), 1);
                assert_eq!(inner.st.pid(2).unwrap().wakes.len(), 1);
            }
            reopened.start_worker(pid);
            thread::sleep(Duration::from_millis(30));
            assert!(
                r.drv.seen().is_empty(),
                "restart launched a duplicate writer"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "restart killed an unverifiable process"
            );
            reopened.shutdown_workers();
            thread::sleep(Duration::from_millis(30));
            assert!(
                child.try_wait().unwrap().is_none(),
                "shutdown killed an unverified historical process group"
            );
            let saved = super::super::State::load(&reopened.u.state_path()).unwrap();
            assert_eq!(saved.pid(pid).unwrap().state, "ended");
            let again = Kernel::open(
                Universe::at(&r.root).unwrap(),
                r.drv.clone(),
                Arc::new(SilentMapp),
                now_ms(),
            )
            .unwrap();
            assert_eq!(
                again.lock().st.pid(2).unwrap().wakes.len(),
                1,
                "restart duplicated alarm"
            );
            child.stdin.take();
            child.wait().unwrap();
        }
    }

    #[test]
    fn failed_state_save_reconciles_ids_on_restart() {
        let r = rig("state-reconcile");
        let first = spawn(&r, None);
        {
            let mut inner = r.k.lock();
            r.k.save(&mut inner);
        }
        let tmp = r.k.u.state_path().with_extension("tmp");
        fs::create_dir(&tmp).unwrap();
        let lost = spawn(&r, None);
        assert!(lost > first);
        {
            let mut inner = r.k.lock();
            r.k.wake_seat(&mut inner, 1, "test", "lost wake", None, None)
                .unwrap();
            r.k.save(&mut inner);
        }
        let before = r.k.journal.tail(1000);
        assert!(before.iter().any(|e| e["kind"] == "state.save"));
        let max_seq = before
            .iter()
            .filter_map(|e| e["seq"].as_u64())
            .max()
            .unwrap();
        let max_wake = before
            .iter()
            .filter_map(|e| e["wake"].as_u64())
            .max()
            .unwrap();
        assert!(
            super::super::State::load(&r.k.u.state_path())
                .unwrap()
                .pid(lost)
                .is_err()
        );

        let reopened = Kernel::open(
            Universe::at(&r.root).unwrap(),
            r.drv.clone(),
            Arc::new(SilentMapp),
            now_ms(),
        )
        .unwrap();
        let after = reopened.journal.tail(1000);
        assert!(
            after[before.len()..]
                .iter()
                .all(|e| e["seq"].as_u64().unwrap() > max_seq)
        );
        assert!(
            after[before.len()..]
                .iter()
                .any(|e| e["kind"] == "state.recovered")
        );
        let mut inner = reopened.lock();
        let next = inner
            .st
            .create(1, 2, "attached", "claude", "next", Default::default(), None);
        assert!(next > lost);
        let wake = reopened
            .wake_seat(&mut inner, 1, "test", "new wake", None, None)
            .unwrap();
        assert!(wake > max_wake);
    }

    #[test]
    fn maintenance_retries_dirty_state_without_another_mutation() {
        let r = rig("state-retry");
        let mut inner = r.k.lock();
        r.k.save(&mut inner);
        let tmp = r.k.u.state_path().with_extension("tmp");
        fs::create_dir(&tmp).unwrap();
        let lost = inner
            .st
            .create(1, 2, "attached", "claude", "lost", Default::default(), None);
        r.k.save(&mut inner);
        assert!(inner.dirty);
        drop(inner);
        assert!(
            super::super::State::load(&r.k.u.state_path())
                .unwrap()
                .pid(lost)
                .is_err()
        );
        fs::remove_dir(&tmp).unwrap();
        r.k.sweep();
        assert!(!r.k.lock().dirty);
        assert!(
            super::super::State::load(&r.k.u.state_path())
                .unwrap()
                .pid(lost)
                .is_ok()
        );
    }

    #[test]
    fn the_universe_root_is_never_checkpointed() {
        let root = std::env::temp_dir().join(format!("unvrs-nocp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        fs::write(root.join("x"), "dirty").unwrap();
        assert!(git_checkpoint(&root, &root, 1, "t").unwrap().is_none());
        // A cwd inside a bigger repository is left alone too.
        let sub = root.join("tasks/pid-1");
        fs::create_dir_all(&sub).unwrap();
        assert!(
            git_checkpoint(&sub, &root.join("elsewhere"), 1, "t")
                .unwrap()
                .is_none()
        );
        assert_eq!(git(&root, &["status", "--porcelain"]).lines().count(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The lead's message, sent as the given caller while the worker runs.
    fn steer(r: &Rig, pid: usize, text: &str) {
        let mut inner = r.k.lock();
        let lead = super::super::ops::Caller::Seat(2, "claude:s2".into());
        r.k.mail_worker(&mut inner, &lead, 2, pid, text).unwrap();
    }

    fn wait_turns(r: &Rig, n: usize) {
        let until = Instant::now() + Duration::from_secs(10);
        while r.drv.seen().len() < n {
            assert!(Instant::now() < until, "turn {n} never started");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// S4 (C2): a message sent during a turn reaches the worker in its next prompt,
    /// once, and its delivery is journaled.
    #[test]
    fn lead_mail_reaches_the_next_turn() {
        let r = rig("s4-mail");
        let pid = spawn(&r, None);
        let gate = Arc::new(std::sync::Barrier::new(2));
        r.drv.script(&[
            Step::Gated(
                gate.clone(),
                Box::new(Step::Progress("step one", "step two")),
            ),
            Step::Progress("step two", "finish"),
            Step::Done("finished"),
        ]);
        r.k.start_worker(pid);
        wait_turns(&r, 1);
        steer(&r, pid, "steer: stop at step 2");
        gate.wait();
        settle(&r, pid);
        let seen = r.drv.seen();
        let has = |i: usize| {
            seen[i]
                .prompt
                .contains("- from PID 2: steer: stop at step 2")
        };
        assert!(!has(0), "turn 1 was built before the message");
        assert!(has(1), "turn 2 prompt: {}", seen[1].prompt);
        assert!(!has(2), "delivered twice: {}", seen[2].prompt);
        let inner = r.k.lock();
        assert!(inner.st.pid(pid).unwrap().mailbox.iter().all(|m| m.read));
        drop(inner);
        let delivered = events(&r, "mail.delivered");
        assert!(
            delivered
                .iter()
                .any(|e| e["pid"] == pid && e["ids"] == json!([1])),
            "{delivered:?}"
        );
    }

    /// S4: mail queued before the first turn is in the first prompt, not marked read
    /// unseen.
    #[test]
    fn lead_mail_before_the_first_turn() {
        let r = rig("s4-first");
        let pid = spawn(&r, None);
        steer(&r, pid, "read the design first");
        r.drv.script(&[Step::Done("finished")]);
        r.k.start_worker(pid);
        settle(&r, pid);
        let seen = r.drv.seen();
        assert!(
            seen[0]
                .prompt
                .contains("- from PID 2: read the design first"),
            "{}",
            seen[0].prompt
        );
    }

    /// S4: unread mail survives an in-place continuation and is delivered once.
    #[test]
    fn lead_mail_survives_in_place_continuation() {
        let r = rig("s4-carry");
        let pid = spawn(&r, None);
        let gate = Arc::new(std::sync::Barrier::new(2));
        r.drv.script(&[
            Step::Progress("step one", "step two"),
            Step::Gated(gate.clone(), Box::new(Step::Crash)),
            Step::Done("finished"),
        ]);
        r.k.start_worker(pid);
        wait_turns(&r, 2);
        steer(&r, pid, "keep the old API");
        gate.wait();
        let last = settle(&r, pid);
        assert_eq!(last, pid, "continuation keeps the PID");
        let seen = r.drv.seen();
        assert!(
            seen[2].prompt.contains("- from PID 2: keep the old API"),
            "{}",
            seen[2].prompt
        );
        let inner = r.k.lock();
        let mail = &inner.st.pid(pid).unwrap().mailbox;
        assert_eq!(mail.len(), 1);
        assert!(mail[0].read);
    }

    #[test]
    fn mail_survives_pre_prompt_timeout() {
        let r = rig("mail-pre-prompt-timeout");
        let pid = spawn(&r, Some("gpt-6.1-sol"));
        {
            let mut inner = r.k.lock();
            let rec = inner.st.pid_mut(pid).unwrap();
            rec.harness = "codex".into();
            rec.driven.as_mut().unwrap().effort = Some("high".into());
        }
        steer(&r, pid, "critical steering mail");
        r.drv.script(&[
            Step::Timeout,
            Step::Progress("continued", "finish"),
            Step::Done("finished"),
        ]);
        r.k.start_worker(pid);
        let last = settle(&r, pid);
        assert_eq!(last, pid, "the timeout continues under the same PID");
        let seen = r.drv.seen();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].prompt.contains("critical steering mail"));
        assert_eq!(seen[1].session, None, "continuation starts a fresh session");
        assert!(seen[1].prompt.contains("CONTINUATION attempt"));
        assert!(
            seen[1].prompt.contains("critical steering mail"),
            "continuation prompt omitted steering mail after a pre-response timeout"
        );
        assert!(!seen[2].prompt.contains("critical steering mail"));
        let inner = r.k.lock();
        let mail = &inner.st.pid(pid).unwrap().mailbox;
        assert_eq!(mail.len(), 1);
        assert!(mail[0].read);
        drop(inner);
        let delivered = events(&r, "mail.delivered");
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(delivered[0]["pid"], last);
        assert_eq!(delivered[0]["ids"], json!([1]));
    }
}
