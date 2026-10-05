//! The kernel's process table. One writer (the kernel process); the files under
//! `.unvrs/kernel/` are the record and survive the kernel.
use crate::memory::ActiveScope;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mail {
    pub id: u64,
    pub from: String,
    pub text: String,
    pub at: u64,
    #[serde(default)]
    pub read: bool,
}

/// A durable wake for a seat (D45): queued by the kernel for actionable events,
/// delivered at the seat's next prompt (or a detached run) and acknowledged after the
/// turn that handled it ends.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Wake {
    pub id: u64,
    /// done · failed · answered · outcome · blocked · note
    pub kind: String,
    pub text: String,
    pub at: u64,
    #[serde(default)]
    pub from: Option<usize>,
    /// Pointer to the full record (a result package, a hold id).
    #[serde(default)]
    pub reference: Option<String>,
    /// Thread key (or `driven`) that received it, while unacknowledged.
    #[serde(default)]
    pub delivered: Option<String>,
    #[serde(default)]
    pub acked: bool,
}

/// A decision is a held task (D46): open until the captain's exact words close it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hold {
    pub id: String,
    /// decision · proposal
    pub kind: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub until: Option<u64>,
    #[serde(default)]
    pub project: Option<String>,
    /// The seat that asked (receives the answer as a wake).
    #[serde(default)]
    pub by: Option<usize>,
    pub created: u64,
    /// open · answered · approved · moot
    pub status: String,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub closed_at: Option<u64>,
    /// What approval does (proposals): e.g. {"create_project": {...}}.
    #[serde(default)]
    pub action: Option<Value>,
    /// Why a moot close was made (a ref, commit or fact). Evidence closes a question;
    /// it authorizes nothing (D48).
    #[serde(default)]
    pub evidence: Option<String>,
    /// The seat that closed it as moot (None: the captain, or not moot).
    #[serde(default)]
    pub closed_by: Option<usize>,
    /// The call this one replaces (`decide --supersedes <id>`).
    #[serde(default)]
    pub supersedes: Option<String>,
}

/// A captain prompt observed by a bound thread's UserPromptSubmit hook.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CaptainPrompt {
    pub id: u64,
    pub pid: usize,
    pub thread: String,
    pub project: Option<String>,
    pub at: u64,
    pub text: String,
}

/// Away mode (D48): the captain's words and a cap on L1's detached runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Away {
    pub words: String,
    pub cap: u32,
    pub runs: u32,
    pub since: u64,
}

/// Kernel-side record of a driven seat's CPU (headless harness or ACP).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Driven {
    /// headless (claude -p / codex exec), acp (`unvrs seat`) or demo (simulated).
    #[serde(default)]
    pub cpu: String,
    pub session: Option<String>,
    pub os_pid: Option<u32>,
    pub turns: u32,
    /// The model requested for this CPU (the task's --model, else UNVRS_L3_<HARNESS>_MODEL).
    pub model: Option<String>,
    /// Model accepted by the harness (last turn).
    #[serde(default)]
    pub actual_model: Option<String>,
    /// The reasoning effort pinned by the task's --effort (Codex or Claude).
    #[serde(default)]
    pub effort: Option<String>,
    /// The pinned effort once the driver confirmed it applied (last turn).
    #[serde(default)]
    pub actual_effort: Option<String>,
    /// Evidence for the harness-accepted configuration.
    #[serde(default)]
    pub effort_evidence: Option<String>,
    pub cwd: PathBuf,
    pub busy: bool,
    /// Recovery owns the checkpoint/summary until a successor or blocked result is recorded.
    #[serde(default)]
    pub recovering: bool,
    #[serde(default)]
    pub last_tools: Vec<String>,
    /// Last non-heartbeat driver lifecycle event (milliseconds since epoch).
    #[serde(default)]
    pub last_activity: Option<u64>,
    /// Recovery (worker-recovery.md): 0 for the first worker of a lineage, n for the
    /// n-th auto-continuation.
    #[serde(default)]
    pub attempt: u32,
    /// The first PID of this lineage (None = this is the first).
    #[serde(default)]
    pub lineage: Option<usize>,
    /// How this attempt stopped (no-progress, timeout, crash, …), once it has.
    #[serde(default)]
    pub last_stop: Option<String>,
    /// The progress mark of the attempt this one continued from (stall detection).
    #[serde(default)]
    pub progress_mark: Option<String>,
    /// Consecutive attempts that made no progress before this one.
    #[serde(default)]
    pub stalls: u32,
    /// Every attempt of the lineage so far: {pid, attempt, stop, turns, commit, progressed}.
    #[serde(default)]
    pub attempts: Vec<Value>,
    /// The brief and work-tree HEAD after the last turn (per-turn stall detection).
    #[serde(default)]
    pub turn_mark: Option<String>,
    /// Turns in a row, up to the last, that left `turn_mark` unchanged.
    #[serde(default)]
    pub idle_turns: u32,
    /// This lineage's continuation ceiling (None: UNVRS_MAX_CONTINUATIONS, else unbounded).
    #[serde(default)]
    pub max_continuations: Option<u32>,
    /// The continuation preamble the first prompt of this attempt carries.
    #[serde(default)]
    pub preamble: Option<String>,
    /// Who stopped this worker and why (`stop`), while it is ending as cancelled.
    #[serde(default)]
    pub stop_request: Option<String>,
    /// Results the context-ownership check sent back (ownership.rs).
    #[serde(default)]
    pub ownership_bounces: u32,
}

impl Driven {
    pub(super) fn remember_tool(&mut self, tool: &str) {
        let name = tool.split(':').next().unwrap_or("tool").to_owned();
        self.last_tools
            .retain(|old| old.split(':').next() != Some(name.as_str()));
        self.last_tools.push(name);
        if self.last_tools.len() > 8 {
            self.last_tools.remove(0);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PidRec {
    pub pid: usize,
    pub parent: usize,
    pub rank: u8,
    /// attached (a captain thread binds it) or driven (the kernel runs its CPU).
    pub kind: String,
    /// Current (or last) harness CPU.
    pub harness: String,
    /// live · idle · working · ended · handed-off
    pub state: String,
    /// Thread key holding this PID (attached seats); the L1 lease is PID 1's holder.
    pub thread: Option<String>,
    pub task: String,
    pub mission: Option<String>,
    pub scope: ActiveScope,
    pub next: String,
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub mailbox: Vec<Mail>,
    #[serde(default)]
    pub quota_low: bool,
    #[serde(default)]
    pub driven: Option<Driven>,
    #[serde(default)]
    pub handed_to: Option<usize>,
    #[serde(default)]
    pub handed_from: Option<usize>,
    #[serde(default)]
    pub result: Option<String>,
    /// Harness sessions this PID has run in (pointers, oldest first).
    #[serde(default)]
    pub sessions: Vec<String>,
    /// Kernel suggestions for the captain (L1 hot set), not yet delivered.
    #[serde(default)]
    pub suggestions: Vec<String>,
    /// Display name (legacy console seats).
    #[serde(default)]
    pub name: String,
    /// Attribution (D44): every PID belongs to a mapp and, below L1, a project.
    #[serde(default = "default_mapp")]
    pub mapp: String,
    #[serde(default)]
    pub project: Option<String>,
    /// Durable wake queue (seats).
    #[serde(default)]
    pub wakes: Vec<Wake>,
    /// L3 task contract (D47).
    #[serde(default)]
    pub contract: Option<Value>,
}
fn default_mapp() -> String {
    "unvrs".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ThreadRec {
    pub key: String,
    pub harness: String,
    pub session: String,
    pub cwd: PathBuf,
    pub os_pid: Option<u32>,
    pub pid: Option<usize>,
    pub bound: bool,
    pub detached: Option<String>,
    /// A notice for the thread's next prompt (e.g. it was detached by a swap).
    pub notice: Option<String>,
    /// Deliver the full hot set on the next prompt (after /l1, /bind, scope change).
    pub rehydrate: bool,
    pub pending_prompt: Option<String>,
    #[serde(default)]
    pub seen_tree: BTreeMap<usize, String>,
    #[serde(default)]
    pub seen_notes: String,
    #[serde(default)]
    pub seen_suggestions: BTreeSet<String>,
    pub attached_at: u64,
    pub last_seen: u64,
    pub transcript: Option<String>,
    /// t3 · codex-app · claude-desktop · claude-cli · codex-cli · headless (from the process tree).
    #[serde(default)]
    pub app: String,
    /// A prompt was submitted and its Stop has not arrived.
    #[serde(default)]
    pub turn_open: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub next_pid: usize,
    pub pids: BTreeMap<usize, PidRec>,
    pub threads: BTreeMap<String, ThreadRec>,
    pub mail_seq: u64,
    pub seq: u64,
    #[serde(default)]
    pub holds: BTreeMap<String, Hold>,
    #[serde(default)]
    pub hold_seq: u64,
    #[serde(default)]
    pub wake_seq: u64,
    #[serde(default)]
    pub away: Option<Away>,
    /// Only bound UserPromptSubmit hooks populate this evidence; model memory cannot.
    #[serde(default)]
    pub captain_prompts: Vec<CaptainPrompt>,
    /// Spawn provenance survives daemon restarts; signals reverify start time and group.
    #[serde(default)]
    pub spawn_ledger: crate::signals::SpawnLedger,
    #[serde(default)]
    pub econ_catalogs: BTreeMap<String, crate::drv_econ::Catalog>,
    #[serde(default)]
    pub econ_refresh_requested: bool,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                next_pid: 1,
                ..Default::default()
            });
        }
        let mut s: Self = serde_json::from_str(&fs::read_to_string(path)?)
            .with_context(|| format!("Kernel state unreadable: {}", path.display()))?;
        if s.next_pid == 0 {
            s.next_pid = s.pids.keys().max().map_or(1, |m| m + 1);
        }
        Ok(s)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = path.with_extension("tmp");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp, path)?;
        Ok(())
    }
    pub fn pid(&self, pid: usize) -> Result<&PidRec> {
        self.pids
            .get(&pid)
            .with_context(|| format!("Unknown PID {pid}"))
    }
    pub fn pid_mut(&mut self, pid: usize) -> Result<&mut PidRec> {
        self.pids
            .get_mut(&pid)
            .with_context(|| format!("Unknown PID {pid}"))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        parent: usize,
        rank: u8,
        kind: &str,
        harness: &str,
        task: &str,
        scope: ActiveScope,
        mission: Option<String>,
    ) -> usize {
        let pid = self.next_pid.max(1);
        self.next_pid = pid + 1;
        let now = now_ms();
        self.pids.insert(
            pid,
            PidRec {
                pid,
                parent,
                rank,
                kind: kind.into(),
                harness: harness.into(),
                state: "idle".into(),
                thread: None,
                task: task.into(),
                mission,
                scope,
                next: String::new(),
                created: now,
                updated: now,
                mailbox: vec![],
                quota_low: false,
                driven: None,
                handed_to: None,
                handed_from: None,
                result: None,
                sessions: vec![],
                suggestions: vec![],
                name: String::new(),
                mapp: default_mapp(),
                project: None,
                wakes: vec![],
                contract: None,
            },
        );
        pid
    }
    pub fn mail(&mut self, to: usize, from: &str, text: &str) -> Result<u64> {
        self.mail_seq += 1;
        let id = self.mail_seq;
        let rec = self.pid_mut(to)?;
        if rec.mailbox.len() >= 256 {
            rec.mailbox.retain(|m| !m.read);
        }
        anyhow::ensure!(rec.mailbox.len() < 256, "Mailbox full (256 messages)");
        rec.mailbox.push(Mail {
            id,
            from: from.into(),
            text: text.into(),
            at: now_ms(),
            read: false,
        });
        Ok(id)
    }
    /// Unread mail moves with the lineage: a handoff or continuation successor reads
    /// what its predecessor never did (S4).
    pub fn carry_mail(&mut self, from: usize, to: usize) -> Result<()> {
        let old = self.pid_mut(from)?;
        let (unread, read): (Vec<Mail>, Vec<Mail>) = std::mem::take(&mut old.mailbox)
            .into_iter()
            .partition(|m| !m.read);
        old.mailbox = read;
        self.pid_mut(to)?.mailbox.extend(unread);
        Ok(())
    }
    /// The L2 seat of a project.
    pub fn seat_of(&self, project: &str) -> Option<usize> {
        self.pids
            .values()
            .find(|p| {
                p.rank == 2
                    && p.kind == "attached"
                    && p.project.as_deref() == Some(project)
                    && !matches!(p.state.as_str(), "ended" | "handed-off")
            })
            .map(|p| p.pid)
    }
    /// Seats: L1 and every project L2.
    pub fn seats(&self) -> Vec<&PidRec> {
        self.pids
            .values()
            .filter(|p| {
                p.kind == "attached"
                    && p.rank <= 2
                    && !matches!(p.state.as_str(), "ended" | "handed-off")
            })
            .collect()
    }
    /// Queues a wake on a seat; returns its id.
    pub fn wake(
        &mut self,
        to: usize,
        kind: &str,
        text: &str,
        from: Option<usize>,
        reference: Option<String>,
    ) -> Result<u64> {
        self.wake_seq += 1;
        let id = self.wake_seq;
        let rec = self.pid_mut(to)?;
        anyhow::ensure!(
            rec.wakes.iter().filter(|w| !w.acked).count() < 256,
            "Wake queue full"
        );
        rec.wakes.push(Wake {
            id,
            kind: kind.into(),
            text: text.into(),
            at: now_ms(),
            from,
            reference,
            delivered: None,
            acked: false,
        });
        // Keep acknowledged history short.
        let acked = rec.wakes.iter().filter(|w| w.acked).count();
        if acked > 64 {
            let mut drop = acked - 64;
            rec.wakes.retain(|w| {
                if w.acked && drop > 0 {
                    drop -= 1;
                    false
                } else {
                    true
                }
            });
        }
        Ok(id)
    }
    /// The captain's L1 seat.
    pub fn l1(&self) -> Option<usize> {
        self.pids
            .values()
            .find(|p| {
                p.rank == 1
                    && p.kind == "attached"
                    && !matches!(p.state.as_str(), "ended" | "handed-off")
            })
            .map(|p| p.pid)
    }
    pub fn children(&self, pid: usize) -> Vec<&PidRec> {
        self.pids.values().filter(|p| p.parent == pid).collect()
    }
}

/// K9 journal: `.unvrs/journal.jsonl`, one event per line.
pub struct Journal {
    path: PathBuf,
}
impl Journal {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn append(&self, seq: u64, kind: &str, fields: Value) -> Result<()> {
        let mut event = serde_json::Map::new();
        if let Value::Object(map) = fields {
            event.extend(map);
        }
        event.insert("seq".into(), seq.into());
        event.insert("at".into(), now_ms().into());
        event.insert("kind".into(), kind.into());
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        writeln!(file, "{}", Value::Object(event))?;
        Ok(())
    }
    pub fn tail(&self, n: usize) -> Vec<Value> {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(n)..]
            .iter()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    pub fn counters(&self) -> Result<(u64, u64, usize)> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0, 0)),
            Err(e) => return Err(e.into()),
        };
        let mut counters = (0, 0, 0);
        for line in BufReader::new(file).split(b'\n') {
            if let Ok(event) = serde_json::from_slice::<Value>(&line?) {
                counters.0 = counters.0.max(event["seq"].as_u64().unwrap_or(0));
                counters.1 = counters.1.max(event["wake"].as_u64().unwrap_or(0));
                if let Some(pid) = event["pid"].as_u64().and_then(|n| usize::try_from(n).ok()) {
                    counters.2 = counters.2.max(pid);
                }
            }
        }
        Ok(counters)
    }
}

pub fn pid_line(p: &PidRec) -> String {
    let mut line = format!(
        "PID {} · L{} · {} · {} · {}",
        p.pid,
        p.rank,
        p.harness,
        p.state,
        scope_label(&p.scope)
    );
    if let Some(m) = &p.mission {
        line.push_str(&format!(" · mission {m}"));
    }
    if let Some(d) = &p.driven {
        // a finished seat run is a record of that run, not a pin still waiting (C8)
        let last = if d.cpu == "seat-run" && !d.busy {
            "last seat run: "
        } else {
            ""
        };
        line.push_str(&format!(" · {last}{}", model_label(d)));
        if let Some(e) = effort_label(d) {
            line.push_str(&format!(" · {e}"));
        }
    }
    let next = if p.next.is_empty() { &p.task } else { &p.next };
    if !next.is_empty() {
        line.push_str(&format!(" · next: {}", clip(next, 140)));
    }
    line
}
/// "model X (accepted X)" · "model X (not yet accepted)" · "model harness default (accepted Y)".
pub fn model_label(d: &Driven) -> String {
    let asked = d.model.as_deref().unwrap_or("harness default");
    match &d.actual_model {
        Some(a) => format!("model {asked} (accepted {a})"),
        None => format!("model {asked} (not yet accepted)"),
    }
}
/// "effort high (accepted)" · "effort high (not yet accepted)"; None when not pinned.
pub fn effort_label(d: &Driven) -> Option<String> {
    let asked = d.effort.as_deref()?;
    Some(match d.actual_effort.as_deref() {
        Some(a) if a == asked => format!("effort {asked} (accepted)"),
        Some(a) => format!("effort {asked} (accepted {a})"),
        None => format!("effort {asked} (not yet accepted)"),
    })
}
pub fn scope_label(s: &ActiveScope) -> String {
    let mut parts = vec![];
    if let Some(a) = &s.area {
        parts.push(format!("area:{a}"));
    }
    if let Some(p) = &s.path {
        parts.push(format!("path:{p}"));
    }
    if let Some(m) = &s.mission {
        parts.push(format!("mission:{m}"));
    }
    if parts.is_empty() {
        "universe".into()
    } else {
        parts.join(" ")
    }
}
pub fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
pub fn json_pid(p: &PidRec) -> Value {
    json!({
        "pid": p.pid, "parent": p.parent, "rank": p.rank, "kind": p.kind,
        "harness": p.harness, "state": p.state, "thread": p.thread,
        "task": p.task, "mission": p.mission, "scope": scope_label(&p.scope),
        "next": p.next, "quota_low": p.quota_low, "handed_to": p.handed_to,
        "handed_from": p.handed_from, "result": p.result, "updated": p.updated,
        "unread": p.mailbox.iter().filter(|m| !m.read).count(),
        "lease": if p.rank == 1 && p.kind == "attached" && p.thread.is_some() { Some("L1") } else { None },
        "name": p.name, "mapp": p.mapp, "project": p.project,
        "wakes": p.wakes.iter().filter(|w| !w.acked).count(),
        "driven": p.driven.as_ref().map(|d| json!({"cpu": d.cpu, "session": d.session, "os_pid": d.os_pid, "turns": d.turns, "busy": d.busy,
            "model": d.model, "actual_model": d.actual_model, "effort": d.effort, "actual_effort": d.actual_effort})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_append_recovers_after_torn_line() {
        let path = std::env::temp_dir().join(format!(
            "unvrs-journal-torn-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::write(&path, b"{\"seq\":100").unwrap();
        let journal = Journal::new(path.clone());
        journal
            .append(101, "wake", json!({"pid": 7, "wake": 9}))
            .unwrap();
        assert_eq!(journal.counters().unwrap(), (101, 9, 7));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn model_label_shows_requested_and_accepted() {
        let mut d = Driven {
            model: Some("claude-opus-5-5".into()),
            ..Default::default()
        };
        assert_eq!(model_label(&d), "model claude-opus-5-5 (not yet accepted)");
        d.actual_model = Some("claude-opus-5-5".into());
        assert_eq!(
            model_label(&d),
            "model claude-opus-5-5 (accepted claude-opus-5-5)"
        );
        d.model = None;
        assert_eq!(
            model_label(&d),
            "model harness default (accepted claude-opus-5-5)"
        );
    }

    #[test]
    fn json_pid_driven_carries_model_and_effort() {
        let mut st = State::default();
        let pid = st.create(0, 3, "driven", "claude", "work", Default::default(), None);
        let mut p = st.pid(pid).unwrap().clone();
        p.driven = Some(Driven {
            model: Some("claude-opus-5-5".into()),
            actual_model: Some("claude-opus-5-5".into()),
            effort: Some("high".into()),
            ..Default::default()
        });
        let d = &json_pid(&p)["driven"];
        assert_eq!(d["model"], "claude-opus-5-5");
        assert_eq!(d["actual_model"], "claude-opus-5-5");
        assert_eq!(d["effort"], "high");
        assert_eq!(d["actual_effort"], Value::Null);
    }

    #[test]
    fn effort_label_only_when_pinned() {
        let mut d = Driven::default();
        assert_eq!(effort_label(&d), None);
        d.effort = Some("high".into());
        assert_eq!(effort_label(&d).unwrap(), "effort high (not yet accepted)");
        d.actual_effort = Some("high".into());
        assert_eq!(effort_label(&d).unwrap(), "effort high (accepted)");
    }
}
