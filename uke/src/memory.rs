//! File-owned memory. The index contains references and terms, never the sole copy.
use crate::brief::{Brief, BriefFold, FoldJob, FoldOutcome};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    hash::{Hash, Hasher},
    io::Write,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const UNIVERSE_PIN_BYTES: usize = 4096;
pub const SCOPE_PIN_BYTES: usize = 12288;
pub const TAIL_BYTES: usize = 16384;
pub const KEEP_TAIL_BYTES: usize = 4096;
pub const GENERATED_START: &str =
    "<!-- UNVRS HOT BEGIN: generated; use remember to keep changes -->";
pub const GENERATED_END: &str = "<!-- UNVRS HOT END -->";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryActor {
    Captain,
    Seat(usize),
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveScope {
    pub area: Option<String>,
    pub path: Option<String>,
    pub mission: Option<String>,
}
impl ActiveScope {
    pub fn scopes(&self, pid: usize) -> Vec<String> {
        let mut out = vec!["universe".into(), format!("pid:{pid}")];
        for (key, value) in [
            ("area", &self.area),
            ("path", &self.path),
            ("mission", &self.mission),
        ] {
            if let Some(value) = value {
                out.push(format!("{key}:{value}"));
            }
        }
        out
    }
    fn validate(&self) -> Result<()> {
        for s in self.scopes(1) {
            valid_scope(&s)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub scope: String,
    pub source: String,
    pub pinned: bool,
    pub open: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled: Option<String>,
    pub title: String,
    pub body: String,
    pub path: PathBuf,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PidSession {
    pub summary: String,
    pub tail: Vec<String>,
    pub active_scope: ActiveScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    pub cold_transcript: PathBuf,
    /// R2 brief and its CAS version.
    #[serde(default, skip_serializing_if = "Brief::is_empty")]
    pub brief: Brief,
    #[serde(default)]
    pub brief_version: u64,
    /// Tail entries (from the start) already reflected in the brief.
    #[serde(default)]
    pub brief_mark: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryHit {
    pub kind: String,
    pub scope: String,
    pub source: String,
    pub time: String,
    pub title: String,
    pub snippet: String,
    pub pointer: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexRow {
    id: String,
    pointer: PathBuf,
    scope: String,
    source: String,
    pinned: bool,
    open: bool,
    status: String,
    updated: String,
    hash: u64,
    terms: BTreeSet<String>,
    kind: String,
    title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionJob {
    pub pid: usize,
    pub summary: String,
    pub turns: Vec<String>,
    pub scope: ActiveScope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionFold {
    pub summary: String,
    pub notes: Vec<String>,
}
#[derive(Debug)]
pub struct MemoryIndex {
    root: PathBuf,
    rows: Vec<IndexRow>,
    secrets: Vec<String>,
}
fn hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}
fn terms(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && !matches!(c, ':' | '/' | '_' | '-'))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "Invalid note id"
    );
    Ok(())
}
fn valid_scope(scope: &str) -> Result<()> {
    if scope == "universe" {
        return Ok(());
    }
    let (kind, value) = scope
        .split_once(':')
        .context("Scope must be universe, area:name, path:relative, mission:id or pid:n")?;
    ensure!(
        !value.trim().is_empty() && !value.chars().any(char::is_control),
        "Empty or invalid scope"
    );
    match kind {
        "area" | "mission" => {}
        "pid" => {
            ensure!(
                value.parse::<usize>().is_ok_and(|n| n > 0),
                "Invalid PID scope"
            );
        }
        "path" => ensure!(
            Path::new(value)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "Path scope must be relative without traversal"
        ),
        _ => bail!("Unknown scope {kind}"),
    }
    Ok(())
}
fn atomic(path: &Path, text: &str) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing parent")?)?;
    let temp = path.with_extension("tmp");
    fs::write(&temp, text)?;
    fs::rename(temp, path)?;
    Ok(())
}
fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for e in fs::read_dir(dir)? {
        let e = e?;
        let ty = e.file_type()?;
        if ty.is_file() {
            out.push(e.path());
        } else if ty.is_dir() {
            out.extend(files(&e.path())?);
        }
    }
    out.sort();
    Ok(out)
}
impl MemoryNote {
    fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        let rest = text
            .strip_prefix("---\n")
            .context("Note needs frontmatter")?;
        let (front, body) = rest
            .split_once("\n---\n")
            .context("Unclosed note frontmatter")?;
        let mut n = Self {
            id: path
                .file_stem()
                .context("Note id missing")?
                .to_string_lossy()
                .into(),
            scope: "universe".into(),
            source: "captain".into(),
            pinned: false,
            open: false,
            status: "live".into(),
            settled: None,
            title: String::new(),
            body: String::new(),
            path: path.into(),
        };
        for line in front.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim();
                let val = serde_json::from_str::<String>(v)
                    .unwrap_or_else(|_| v.trim_matches('\'').into());
                match k.trim() {
                    "scope" => n.scope = val,
                    "source" => n.source = val,
                    "pinned" => n.pinned = v.parse()?,
                    "open" => n.open = v.parse()?,
                    "status" => n.status = val,
                    "settled" => n.settled = Some(val),
                    _ => {}
                }
            }
        }
        valid_scope(&n.scope)?;
        ensure!(
            matches!(n.status.as_str(), "live" | "retired"),
            "Invalid note status"
        );
        let (title, body) = body.trim().split_once('\n').unwrap_or((body.trim(), ""));
        n.title = title.trim_start_matches("# ").into();
        n.body = body.trim().into();
        Ok(n)
    }
    fn write(&self) -> Result<()> {
        let settled = self
            .settled
            .as_ref()
            .map(|v| format!("settled: {}\n", json!(v)))
            .unwrap_or_default();
        atomic(
            &self.path,
            &format!(
                "---\nscope: {}\nsource: {}\npinned: {}\nopen: {}\nstatus: {}\n{settled}---\n\n# {}\n\n{}\n",
                json!(self.scope),
                json!(self.source),
                self.pinned,
                self.open,
                self.status,
                self.title,
                self.body
            ),
        )
    }
}
impl MemoryIndex {
    pub fn open(universe: &Path) -> Result<Self> {
        let root = universe.canonicalize()?;
        fs::create_dir_all(root.join("memory/notes"))?;
        let mut secrets: Vec<String> = std::env::vars()
            .filter(|(k, v)| secret_key(k) && v.len() >= 4)
            .map(|(_, v)| v)
            .collect();
        if let Ok(text) = fs::read_to_string(root.join(".env")) {
            secrets.extend(
                crate::dotenv::parse(&text)
                    .into_iter()
                    .filter(|(k, v)| secret_key(k) && v.len() >= 4)
                    .map(|(_, v)| v),
            );
        }
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut m = Self {
            root,
            rows: vec![],
            secrets,
        };
        m.refresh()?;
        Ok(m)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn redact_value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.redact(text),
            Value::Array(values) => values.iter_mut().for_each(|v| self.redact_value(v)),
            Value::Object(values) => values.values_mut().for_each(|v| self.redact_value(v)),
            _ => {}
        }
    }
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for value in &self.secrets {
            out = out.replace(value, "[REDACTED]");
        }
        out.lines()
            .map(|line| {
                if let Some((key, _)) = line.split_once('=').or_else(|| line.split_once(':'))
                    && secret_key(key.trim())
                {
                    return format!("{key}: [REDACTED]");
                }
                line.split_inclusive(char::is_whitespace)
                    .map(|word| {
                        let t = word.trim();
                        if t.starts_with("sk-")
                            || t.starts_with("ghp_")
                            || t.starts_with("github_pat_")
                        {
                            format!("[REDACTED]{}", &word[t.len()..])
                        } else {
                            word.into()
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn notes(&self) -> Result<Vec<MemoryNote>> {
        files(&self.root.join("memory/notes"))?
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .map(|p| MemoryNote::read(&p))
            .collect()
    }
    pub fn note(&self, id: &str) -> Result<MemoryNote> {
        valid_id(id)?;
        MemoryNote::read(&self.root.join(format!("memory/notes/{id}.md")))
    }
    pub fn session(&self, pid: usize) -> Result<PidSession> {
        ensure!(pid > 0, "PID starts at 1");
        let path = self.session_path(pid);
        if path.exists() {
            Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
        } else {
            Ok(PidSession {
                cold_transcript: self
                    .root
                    .join(format!("sessions/pid-{pid}/transcript.jsonl")),
                ..Default::default()
            })
        }
    }
    fn session_path(&self, pid: usize) -> PathBuf {
        self.root.join(format!("sessions/pid-{pid}/session.json"))
    }
    fn save_session(&self, pid: usize, s: &PidSession) -> Result<()> {
        atomic(&self.session_path(pid), &serde_json::to_string_pretty(s)?)
    }
    pub fn set_scope(&mut self, actor: MemoryActor, pid: usize, scope: ActiveScope) -> Result<()> {
        ensure!(
            actor == MemoryActor::Captain,
            "Only the captain may change scope"
        );
        scope.validate()?;
        self.check_pins(&scope.scopes(pid), None)?;
        let mut s = self.session(pid)?;
        s.active_scope = scope;
        self.save_session(pid, &s)
    }
    fn check_pins(&self, scopes: &[String], new: Option<&MemoryNote>) -> Result<()> {
        let mut pins = self
            .notes()?
            .into_iter()
            .filter(|n| n.pinned && n.status == "live" && scopes.contains(&n.scope))
            .collect::<Vec<_>>();
        if let Some(n) = new {
            pins.push(n.clone());
        }
        for (universe, budget) in [(true, UNIVERSE_PIN_BYTES), (false, SCOPE_PIN_BYTES)] {
            let group = pins
                .iter()
                .filter(|n| (n.scope == "universe") == universe)
                .collect::<Vec<_>>();
            let bytes: usize = group.iter().map(|n| n.title.len() + n.body.len()).sum();
            ensure!(
                bytes <= budget,
                "Pin refused: {} pins need {bytes} bytes, budget {budget}; retire: {}. Previous pins kept.",
                if universe { "universe" } else { "active scope" },
                group
                    .iter()
                    .filter(|n| new.is_none_or(|new| new.id != n.id))
                    .map(|n| format!("{} ({})", n.id, n.title))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &mut self,
        actor: MemoryActor,
        pid: usize,
        scope: Option<&str>,
        title: &str,
        body: &str,
        pinned: bool,
        open: bool,
    ) -> Result<MemoryNote> {
        ensure!(
            actor == MemoryActor::Captain || !pinned,
            "Only the captain may pin"
        );
        ensure!(
            !title.trim().is_empty() && !title.contains('\n'),
            "A one-line title is required"
        );
        ensure!(
            title.len() + body.len() <= 32768,
            "Note exceeds 32768 bytes"
        );
        let s = self.session(pid)?;
        let default = s
            .active_scope
            .area
            .as_ref()
            .map(|a| format!("area:{a}"))
            .unwrap_or_else(|| "universe".into());
        let scope = scope.unwrap_or(&default);
        valid_scope(scope)?;
        let id = format!(
            "note-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let n = MemoryNote {
            id: id.clone(),
            scope: scope.into(),
            source: match actor {
                MemoryActor::Captain => "captain".into(),
                MemoryActor::Seat(n) => format!("pid:{n}"),
            },
            pinned,
            open,
            status: "live".into(),
            settled: None,
            title: self.redact(title),
            body: self.redact(body),
            path: self.root.join(format!("memory/notes/{id}.md")),
        };
        if pinned {
            let mut scopes = s.active_scope.scopes(pid);
            if !scopes.contains(&n.scope) {
                if let Some((kind, _)) = n.scope.split_once(':') {
                    scopes.retain(|scope| !scope.starts_with(&format!("{kind}:")));
                }
                scopes.push(n.scope.clone());
            }
            self.check_pins(&scopes, Some(&n))?;
            for path in files(&self.root.join("sessions"))?
                .into_iter()
                .filter(|p| p.file_name().is_some_and(|x| x == "session.json"))
            {
                let other: PidSession = serde_json::from_str(&fs::read_to_string(&path)?)?;
                let other_pid = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_prefix("pid-"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(pid);
                self.check_pins(
                    &other.active_scope.scopes(other_pid),
                    if other.active_scope.scopes(other_pid).contains(&n.scope) {
                        Some(&n)
                    } else {
                        None
                    },
                )?;
            }
        }
        n.write()?;
        self.refresh()?;
        Ok(n)
    }
    pub fn forget(&mut self, actor: MemoryActor, id: &str) -> Result<()> {
        ensure!(actor == MemoryActor::Captain, "Only the captain may forget");
        let mut n = self.note(id)?;
        n.status = "retired".into();
        n.write()?;
        self.refresh()
    }
    pub fn delete_note(&mut self, actor: MemoryActor, id: &str) -> Result<()> {
        ensure!(actor == MemoryActor::Captain, "Only the captain may delete");
        let n = self.note(id)?;
        fs::remove_file(n.path)?;
        self.refresh()
    }
    pub fn open_notes(&self, pid: usize) -> Result<(Vec<MemoryNote>, usize)> {
        let scopes = self.session(pid)?.active_scope.scopes(pid);
        let mut inside = vec![];
        let mut outside = 0;
        for n in self.notes()? {
            if n.open && n.status == "live" && n.settled.is_none() {
                if scopes.contains(&n.scope) {
                    inside.push(n);
                } else {
                    outside += 1;
                }
            }
        }
        Ok((inside, outside))
    }
    pub fn hot_set(&self, pid: usize) -> Result<String> {
        let s = self.session(pid)?;
        let scopes = s.active_scope.scopes(pid);
        self.check_pins(&scopes, None)?;
        let mut value =
            json!({"summary":s.summary,"tail":bounded_tail(&s.tail),"active_scope":s.active_scope});
        for (key, items) in [
            ("open", s.open),
            ("done", s.done),
            ("artifacts", s.artifacts),
        ] {
            if !items.is_empty() {
                value[key] = json!(items);
            }
        }
        value["pins"] = json!(
            self.notes()?
                .into_iter()
                .filter(|n| n.pinned && n.status == "live" && scopes.contains(&n.scope))
                .map(|n| json!({"id":n.id,"scope":n.scope,"title":n.title,"body":n.body}))
                .collect::<Vec<_>>()
        );
        self.redact_value(&mut value);
        Ok(serde_json::to_string_pretty(&value)?)
    }
    pub fn append_turn(&mut self, pid: usize, text: &str) -> Result<()> {
        let mut s = self.session(pid)?;
        let text = self.redact(text);
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let mut end = rest.len().min(KEEP_TAIL_BYTES);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            s.tail.push(rest[..end].into());
            rest = &rest[end..];
        }
        self.save_session(pid, &s)
    }
    pub fn compaction_job(&self, pid: usize) -> Result<Option<CompactionJob>> {
        let s = self.session(pid)?;
        if s.tail.iter().map(String::len).sum::<usize>() <= TAIL_BYTES {
            return Ok(None);
        }
        let mut keep = 0;
        let mut split = s.tail.len();
        while split > 0 && keep + s.tail[split - 1].len() <= KEEP_TAIL_BYTES {
            split -= 1;
            keep += s.tail[split].len();
        }
        Ok(Some(CompactionJob {
            pid,
            summary: self.redact(&s.summary),
            turns: s.tail[..split].iter().map(|t| self.redact(t)).collect(),
            scope: s.active_scope,
        }))
    }
    pub fn apply_compaction(&mut self, job: &CompactionJob, fold: CompactionFold) -> Result<()> {
        ensure!(
            !fold.summary.trim().is_empty() && !fold.notes.is_empty(),
            "Compaction requires summary and durable notes"
        );
        ensure!(
            fold.summary.len() <= 8192,
            "Compaction summary exceeds 8192 bytes"
        );
        let mut s = self.session(job.pid)?;
        ensure!(
            s.tail.starts_with(&job.turns),
            "Session changed during compaction"
        );
        ensure!(
            fold.notes
                .iter()
                .all(|n| !n.trim().is_empty() && n.len() <= 32600),
            "Compaction notes must contain 1–32600 bytes"
        );
        let mut cold = String::new();
        for turn in &job.turns {
            cold.push_str(&serde_json::to_string(&json!({"text":self.redact(turn)}))?);
            cold.push('\n');
        }
        fs::create_dir_all(
            s.cold_transcript
                .parent()
                .context("Cold transcript parent missing")?,
        )?;
        let mut transcript = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&s.cold_transcript)?;
        transcript.write_all(cold.as_bytes())?;
        transcript.sync_all()?;
        for body in fold.notes {
            let title = body
                .lines()
                .next()
                .unwrap_or("Compaction fact")
                .chars()
                .take(100)
                .collect::<String>();
            let mut n = self.remember(
                MemoryActor::Seat(job.pid),
                job.pid,
                Some(
                    &job.scope
                        .area
                        .as_ref()
                        .map(|area| format!("area:{area}"))
                        .unwrap_or_else(|| "universe".into()),
                ),
                &title,
                &body,
                false,
                false,
            )?;
            n.source = format!("compaction:pid:{}", job.pid);
            n.write()?;
        }
        s.summary = self.redact(&fold.summary);
        s.tail.drain(..job.turns.len());
        self.save_session(job.pid, &s)?;
        self.refresh()
    }
    /// A fold job over the tail not yet reflected in the brief. `drain` folds the old
    /// part out of the tail (threshold compaction); otherwise the tail is kept.
    pub fn fold_job(&self, pid: usize, reason: &str, drain: bool) -> Result<Option<FoldJob>> {
        let s = self.session(pid)?;
        let (turns, covered) = if drain {
            if s.tail.iter().map(String::len).sum::<usize>() <= TAIL_BYTES {
                return Ok(None);
            }
            let mut keep = 0;
            let mut split = s.tail.len();
            while split > 0 && keep + s.tail[split - 1].len() <= KEEP_TAIL_BYTES {
                split -= 1;
                keep += s.tail[split].len();
            }
            (s.tail[..split].to_vec(), split)
        } else {
            let mark = s.brief_mark.min(s.tail.len());
            if mark == s.tail.len() {
                return Ok(None);
            }
            (s.tail[mark..].to_vec(), s.tail.len())
        };
        Ok(Some(FoldJob {
            pid,
            version: s.brief_version,
            reason: reason.into(),
            brief: s.brief.clone(),
            turns: turns.iter().map(|t| self.redact(t)).collect(),
            covered,
            drain,
            area: s.active_scope.area.clone(),
        }))
    }
    /// Compare-and-swap brief write. A job read at version N is discarded when the brief
    /// is already past N. A fold that drops an open item is rejected and the previous
    /// brief stays. Overflowing decisions graduate into unpinned notes; the previous
    /// brief goes to the cold file `briefs.jsonl`.
    pub fn apply_fold(&mut self, job: &FoldJob, fold: BriefFold) -> Result<FoldOutcome> {
        let mut s = self.session(job.pid)?;
        if s.brief_version != job.version {
            return Ok(FoldOutcome::Discarded {
                read: job.version,
                current: s.brief_version,
            });
        }
        let mut brief = fold.brief;
        brief.number_open();
        Brief::check_open_items(&s.brief, &brief)?;
        let graduated = brief.bound();
        brief.validate_shape()?;
        let mut value = serde_json::to_value(&brief)?;
        self.redact_value(&mut value);
        let brief: Brief = serde_json::from_value(value)?;
        let dir = self.root.join(format!("sessions/pid-{}", job.pid));
        fs::create_dir_all(&dir)?;
        if !s.brief.is_empty() {
            let mut cold = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("briefs.jsonl"))?;
            writeln!(
                cold,
                "{}",
                json!({"version":s.brief_version,"at":crate::mission::now(),"brief":s.brief})
            )?;
        }
        let area = job
            .area
            .as_ref()
            .map(|a| format!("area:{a}"))
            .unwrap_or_else(|| "universe".into());
        let facts = graduated
            .iter()
            .map(|d| format!("Decision: {d}"))
            .chain(fold.notes.into_iter().filter(|n| !n.trim().is_empty()))
            .collect::<Vec<_>>();
        let count = graduated.len();
        for body in facts {
            let title: String = body
                .lines()
                .next()
                .unwrap_or("Brief fact")
                .chars()
                .take(100)
                .collect();
            let mut n = self.remember(
                MemoryActor::Seat(job.pid),
                job.pid,
                Some(&area),
                &title,
                &body,
                false,
                false,
            )?;
            n.source = format!("compaction:pid:{}", job.pid);
            n.write()?;
        }
        if job.drain {
            ensure!(
                s.tail.len() >= job.covered
                    && s.tail[..job.covered]
                        .iter()
                        .map(|t| self.redact(t))
                        .eq(job.turns.iter().cloned()),
                "Session changed during compaction"
            );
            let mut cold = String::new();
            for turn in &job.turns {
                cold.push_str(&serde_json::to_string(&json!({"text":turn}))?);
                cold.push('\n');
            }
            fs::create_dir_all(
                s.cold_transcript
                    .parent()
                    .context("Cold transcript parent missing")?,
            )?;
            let mut transcript = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&s.cold_transcript)?;
            transcript.write_all(cold.as_bytes())?;
            transcript.sync_all()?;
            s.tail.drain(..job.covered);
            s.brief_mark = 0;
        } else {
            s.brief_mark = job.covered.min(s.tail.len());
        }
        s.summary = brief.text();
        s.brief = brief;
        s.brief_version += 1;
        let version = s.brief_version;
        self.save_session(job.pid, &s)?;
        self.refresh()?;
        Ok(FoldOutcome::Applied {
            version,
            graduated: count,
        })
    }
    /// Kernel-side brief write (driven-seat progress, handoff seeding): same CAS and rules.
    pub fn write_brief(&mut self, pid: usize, reason: &str, brief: Brief) -> Result<FoldOutcome> {
        let s = self.session(pid)?;
        let job = FoldJob {
            pid,
            version: s.brief_version,
            reason: reason.into(),
            brief: s.brief.clone(),
            turns: vec![],
            covered: s.brief_mark.min(s.tail.len()),
            drain: false,
            area: s.active_scope.area.clone(),
        };
        self.apply_fold(
            &job,
            BriefFold {
                brief,
                notes: vec![],
            },
        )
    }
    pub fn admit_note(
        &mut self,
        actor: MemoryActor,
        id: &str,
        outcome: &str,
        done: &str,
    ) -> Result<PathBuf> {
        ensure!(actor == MemoryActor::Captain, "Only the captain may admit");
        let mut n = self.note(id)?;
        ensure!(
            n.open && n.status == "live" && n.settled.is_none(),
            "Note is not open"
        );
        let id = format!("msn_{}", n.id);
        let path = self.root.join(format!("missions/{id}.md"));
        let m = crate::mission::Mission {
            id: id.clone(),
            outcome: outcome.into(),
            done_check: done.into(),
            ..Default::default()
        };
        m.admit_check()?;
        ensure!(!path.exists(), "Mission already exists");
        atomic(
            &path,
            &format!(
                "---\nid: {id}\nstatus: draft\nowner_pid: null\nupdated: {}\n---\n\n# Outcome\n{outcome}\n\n# Done check\n{done}\n\n# Scope\n{}\n\n# Inputs\n- prompt: {}\n- pointers: {}\n\n# Draft\n{}\n\n# Log\n",
                crate::mission::now(),
                n.scope,
                n.title,
                n.path.display(),
                n.body
            ),
        )?;
        let mut registry = crate::mission::MissionRegistry::open(&self.root.join("missions.json"))?;
        registry.write_back = true;
        registry.admit(&path)?;
        n.settled = Some(id);
        n.write()?;
        self.refresh()?;
        Ok(path)
    }
    pub fn session_pointers(
        &mut self,
        pid: usize,
        open: Vec<String>,
        done: Vec<String>,
        artifacts: Vec<String>,
    ) -> Result<()> {
        let mut s = self.session(pid)?;
        s.open = open;
        s.done = done;
        s.artifacts = artifacts;
        self.save_session(pid, &s)
    }
    pub fn save_handoff(&mut self, id: &str, summary: &Value) -> Result<()> {
        valid_id(id)?;
        let path = self.root.join(format!("handoffs/{id}.json"));
        let mut summary = summary.clone();
        self.redact_value(&mut summary);
        atomic(&path, &serde_json::to_string_pretty(&summary)?)?;
        if let Some(pid) = summary["from_pid"].as_u64() {
            let pointers = |key: &str| {
                summary[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(i, _)| format!("{}#/{key}/{i}", path.display()))
                    .collect()
            };
            self.session_pointers(
                pid as usize,
                pointers("open"),
                pointers("done"),
                pointers("artifacts"),
            )?;
        }
        Ok(())
    }
    pub fn refresh(&mut self) -> Result<()> {
        let mut rows = vec![];
        for n in self.notes()? {
            let text = fs::read_to_string(&n.path)?;
            rows.push(row(
                &n.id, &n.path, &n.scope, &n.source, n.pinned, n.open, &n.status, "note", &n.title,
                &text,
            ));
        }
        for (directory, kind) in [
            ("sessions", "session"),
            ("missions", "mission"),
            ("handoffs", "handoff"),
        ] {
            let mut paths = files(&self.root.join(directory))?;
            if kind == "mission" {
                let registry = self.root.join("missions.json");
                if registry.is_file() {
                    let value: Value = serde_json::from_str(&fs::read_to_string(registry)?)?;
                    if let Some(missions) = value["missions"].as_object() {
                        paths.extend(
                            missions
                                .values()
                                .filter_map(|m| m["path"].as_str())
                                .map(|p| self.root.join(p))
                                .filter(|p| p.is_file()),
                        );
                    }
                }
                paths.sort();
                paths.dedup();
            }
            for p in paths {
                let text = fs::read_to_string(&p)?;
                let (kind, scope, body) = if kind == "session" {
                    if p.file_name().is_some_and(|x| x == "session.json") {
                        let s: PidSession = serde_json::from_str(&text)?;
                        (
                            "summary",
                            s.active_scope
                                .area
                                .map(|a| format!("area:{a}"))
                                .unwrap_or_else(|| "universe".into()),
                            s.summary,
                        )
                    } else if p.extension().is_some_and(|x| x == "jsonl") {
                        ("cold", "universe".into(), text)
                    } else {
                        continue;
                    }
                } else {
                    (kind, "universe".into(), text)
                };
                let id = p.file_stem().unwrap_or_default().to_string_lossy();
                let source = if matches!(kind, "summary" | "cold") {
                    p.parent()
                        .and_then(|p| p.file_name())
                        .unwrap_or_default()
                        .to_string_lossy()
                        .replace("pid-", "pid:")
                } else {
                    format!("{kind}:{id}")
                };
                rows.push(row(
                    &id, &p, &scope, &source, false, false, "live", kind, &id, &body,
                ));
            }
        }
        let path = self.root.join("memory/index/lexical.json");
        let text = serde_json::to_string(&rows)?;
        if fs::read_to_string(&path).ok().as_deref() != Some(&text) {
            atomic(&path, &text)?;
        }
        self.rows = rows;
        Ok(())
    }
    pub fn recall(&mut self, query: &str) -> Result<Vec<MemoryHit>> {
        self.refresh()?;
        let query_terms = terms(query);
        let mut hits = vec![];
        for row in &self.rows {
            if !query_terms
                .iter()
                .all(|t| row.terms.iter().any(|term| term.contains(t)))
            {
                continue;
            }
            let text = if row.kind == "summary" {
                serde_json::from_str::<PidSession>(&fs::read_to_string(&row.pointer)?)?.summary
            } else {
                fs::read_to_string(&row.pointer)?
            };
            let line = text
                .lines()
                .find(|l| query_terms.iter().any(|t| l.to_lowercase().contains(t)))
                .unwrap_or(&text);
            hits.push(MemoryHit {
                kind: row.kind.clone(),
                scope: row.scope.clone(),
                source: row.source.clone(),
                time: row.updated.clone(),
                title: row.title.clone(),
                snippet: line.chars().take(240).collect(),
                pointer: row.pointer.clone(),
            });
            if hits.len() == 50 {
                break;
            }
        }
        Ok(hits)
    }
    /// Both cockpit and authenticated seat requests cross this interface.
    pub fn memory_op(&mut self, actor: MemoryActor, pid: usize, request: &Value) -> Result<Value> {
        if let MemoryActor::Seat(caller) = actor {
            ensure!(caller == pid, "Seat identity mismatch");
        }
        let string = |key: &str| request[key].as_str().unwrap_or("");
        match string("op") {
            "remember" => Ok(json!(self.remember(
                actor,
                pid,
                request["scope"].as_str(),
                string("title"),
                string("body"),
                request["pinned"] == true,
                request["open"] == true
            )?)),
            "recall" => Ok(json!(self.recall(string("query"))?)),
            "forget" => {
                self.forget(actor, string("id"))?;
                Ok(json!({"retired":string("id")}))
            }
            "delete-note" => {
                self.delete_note(actor, string("id"))?;
                Ok(json!({"deleted":string("id")}))
            }
            "scope" => {
                self.set_scope(
                    actor,
                    pid,
                    serde_json::from_value(request["scope"].clone())?,
                )?;
                Ok(json!(self.session(pid)?.active_scope))
            }
            "admit-note" => Ok(
                json!({"mission":self.admit_note(actor,string("id"),string("outcome"),string("done"))?}),
            ),
            _ => bail!("Memory operation refused"),
        }
    }
}
fn secret_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    ["SECRET", "PASSWORD", "API_KEY", "TOKEN", "PRIVATE_KEY"]
        .iter()
        .any(|s| k.contains(s))
}
#[allow(clippy::too_many_arguments)]
fn row(
    id: &str,
    path: &Path,
    scope: &str,
    source: &str,
    pinned: bool,
    open: bool,
    status: &str,
    kind: &str,
    title: &str,
    text: &str,
) -> IndexRow {
    let updated = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    IndexRow {
        id: id.into(),
        pointer: path.into(),
        scope: scope.into(),
        source: source.into(),
        pinned,
        open,
        status: status.into(),
        updated: updated.clone(),
        hash: hash(text),
        terms: terms(&format!("{text} {scope} {source} {updated}")),
        kind: kind.into(),
        title: title.into(),
    }
}

fn bounded_tail(turns: &[String]) -> Vec<String> {
    let mut budget = TAIL_BYTES;
    let mut out = vec![];
    for turn in turns.iter().rev() {
        if budget == 0 {
            break;
        }
        let mut start = turn.len().saturating_sub(budget);
        while !turn.is_char_boundary(start) {
            start += 1;
        }
        let text = &turn[start..];
        budget -= text.len();
        out.push(text.into());
    }
    out.reverse();
    out
}
