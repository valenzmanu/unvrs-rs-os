//! Cockpit Boot: the same admit → nest → install → ready | refused path as `unvrs boot`, run off
//! the UI thread for one seat and reported back as a COMMS card.
//!
//! Nothing here picks or installs by itself: `uke::mission`, `uke::pool`, `uke::boot` and
//! `drv_agent::install` do the work, and every decision lands in the same Obs journal
//! (`.unvrs/boot-journal.jsonl`) the CLI writes.
use anyhow::{Context, Result, bail, ensure};
use drv_agent as install;
use drv_agent::InstallReport;
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uke as boot;
use uke as pool;
use uke::{Backend, BootConfig, BootRecord, JevHttp, Mission, MissionRegistry, Seat, Status};

const REGISTRY: &str = ".unvrs/missions.json";
const JOURNAL: &str = ".unvrs/boot-journal.jsonl";
const MISSION_DIRS: [&str; 2] = [".unvrs/missions", "behaviour-eval/fixtures/missions"];
const POOL_FILES: [&str; 2] = [
    ".unvrs/skill-pools.toml",
    "behaviour-eval/fixtures/skill-pools.toml",
];
/// One Boot at a time: the registry and the install manifest are plain files.
static LOCK: Mutex<()> = Mutex::new(());

/// `$UNVRS_MISSIONS`, else `.unvrs/missions`, else the eval fixtures.
pub fn missions_dir() -> Option<PathBuf> {
    env::var_os("UNVRS_MISSIONS")
        .map(PathBuf::from)
        .into_iter()
        .chain(MISSION_DIRS.iter().map(PathBuf::from))
        .find(|dir| dir.is_dir())
}
/// `$UNVRS_POOLS`, else `.unvrs/skill-pools.toml`, else the eval fixture.
fn pools_file() -> Result<PathBuf> {
    env::var_os("UNVRS_POOLS")
        .map(PathBuf::from)
        .into_iter()
        .chain(POOL_FILES.iter().map(PathBuf::from))
        .find(|file| file.is_file())
        .context("No skill pools file; write .unvrs/skill-pools.toml or set UNVRS_POOLS")
}

/// One row of the cockpit mission list.
#[derive(Clone, Debug)]
pub struct MissionRow {
    pub path: PathBuf,
    pub id: String,
    pub outcome: String,
    /// Registry status when admitted this flight root, else the file's own status.
    pub status: String,
    /// Why admit would refuse this file, if it would.
    pub problem: Option<String>,
}

fn row(path: &Path, registry: Option<&MissionRegistry>) -> MissionRow {
    match Mission::load(path) {
        Ok(mission) => MissionRow {
            path: path.to_path_buf(),
            status: registry
                .and_then(|r| r.get(&mission.id))
                .map_or(mission.status, |entry| entry.status)
                .to_string(),
            problem: mission.admit_check().err().map(|e| format!("{e:#}")),
            outcome: mission
                .outcome
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            id: mission.id,
        },
        Err(e) => MissionRow {
            path: path.to_path_buf(),
            id: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            outcome: String::new(),
            status: "unreadable".into(),
            problem: Some(format!("{e:#}")),
        },
    }
}

/// One mission file as the cockpit shows it, with its registry status.
pub fn describe(path: &Path) -> MissionRow {
    row(
        path,
        MissionRegistry::open(Path::new(REGISTRY)).ok().as_ref(),
    )
}

/// Every mission file the captain can pick: the missions dir, its `bad/` corner, then anything
/// else the registry knows.
pub fn list() -> Vec<MissionRow> {
    let registry = MissionRegistry::open(Path::new(REGISTRY)).ok();
    let mut files = vec![];
    if let Some(dir) = missions_dir() {
        for dir in [dir.clone(), dir.join("bad")] {
            let mut found: Vec<PathBuf> = fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
                .collect();
            found.sort();
            files.extend(found);
        }
    }
    if let Some(registry) = &registry {
        for (_, entry) in registry.list() {
            if !files.contains(&entry.path) && entry.path.is_file() {
                files.push(entry.path.clone());
            }
        }
    }
    files
        .iter()
        .map(|path| row(path, registry.as_ref()))
        .collect()
}

/// `coding-tdd`, `bad/bad-no-done-check.md`, a mission id, or any path to a mission file.
pub fn resolve(name: &str) -> Result<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_file() {
        return Ok(direct);
    }
    if let Some(dir) = missions_dir() {
        for candidate in [
            dir.join(name),
            dir.join(format!("{name}.md")),
            dir.join("bad").join(name),
            dir.join("bad").join(format!("{name}.md")),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(found) = list().into_iter().find(|row| row.id == name) {
        return Ok(found.path);
    }
    bail!("No mission file {name} · /missions lists what can be admitted")
}

/// Admit floor plus registry write. A refusal records nothing and comes back as the error.
pub fn admit(path: &Path) -> Result<MissionRow> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut registry = MissionRegistry::open(Path::new(REGISTRY))?;
    if let Err(e) = registry.admit(path) {
        let reason = format!("{e:#}");
        let _ = log(
            "boot.refused",
            json!({"mission": path, "stage": "admit", "reason": reason, "source": "cockpit"}),
        );
        bail!("{reason}");
    }
    let admitted = row(path, Some(&registry));
    log(
        "mission.admitted",
        json!({"mission": admitted.id, "path": path, "source": "cockpit"}),
    )?;
    Ok(admitted)
}

fn log(kind: &str, fields: Value) -> Result<()> {
    uke::append_journal(Path::new(JOURNAL), kind, &fields).context("Obs journal write failed")
}

/// One seat to nest. `mission` is the admitted mission file, if the cockpit has one selected;
/// `goal` is the free text the seat was launched on (may be empty when a mission carries it).
#[derive(Clone, Debug)]
pub struct Job {
    pub pid: usize,
    pub seat: Seat,
    pub profile: String,
    pub picker: Backend,
    pub mission: Option<PathBuf>,
    pub goal: String,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub pid: usize,
    pub mission: String,
    pub profile: String,
    pub seat: Seat,
    pub record: Option<BootRecord>,
    pub install: Option<InstallReport>,
    /// `(stage, reason)` when Boot refused; `None` means ready.
    pub refused: Option<(&'static str, String)>,
}

impl Outcome {
    pub fn ready(&self) -> bool {
        self.refused.is_none()
    }
    /// Prompt text that points the seat at its installed skills; empty for an empty nest.
    pub fn activation(&self) -> Option<String> {
        self.install
            .as_ref()
            .filter(|report| !report.installed.is_empty())
            .map(|report| report.activation.clone())
    }
    /// The COMMS card (`comms::conversation_lines` folds it to one line; Ctrl+O unfolds).
    pub fn card(&self) -> String {
        let number = |n: Option<f64>| n.map_or("n/a".into(), |n| format!("{n:.2}"));
        let mut card = format!(
            "BOOT / {} / PID {} L{} / {} / {}\n",
            self.mission,
            self.pid,
            self.seat.rank,
            self.profile,
            if self.ready() { "READY" } else { "REFUSED" }
        );
        if let Some(record) = &self.record {
            card.push_str(&format!(
                "PICKER: {}{}\nNOUL: {}\nCONFIDENCE: {}\nCHANNEL: {}\n",
                record.backend_used.as_str(),
                record
                    .fallback
                    .as_ref()
                    .map(|why| format!(
                        " (fallback from {}: {})",
                        record.backend_requested.as_str(),
                        one_line(why)
                    ))
                    .unwrap_or_default(),
                number(record.noul),
                number(record.confidence),
                match (record.captain_channel, self.seat.rank) {
                    (true, 1) => "captain channel (L1)".into(),
                    (true, rank) => format!("captain channel (L{rank} focused)"),
                    (false, 2) => "no captain channel (L2 unfocused)".into(),
                    (false, rank) => format!("no captain channel (L{rank})"),
                }
            ));
            if !record.stripped.is_empty() {
                card.push_str(&format!(
                    "STRIPPED: {} interactive withheld · {}\n",
                    record.stripped.len(),
                    record.stripped.join(", ")
                ));
            }
            card.push_str(&format!(
                "NEST: {}\n",
                if record.selected.is_empty() {
                    "(empty · no skill needed)".into()
                } else {
                    record.selected.join(", ")
                }
            ));
        }
        if let Some(report) = &self.install {
            let count = report.installed.len();
            card.push_str(&format!(
                "INSTALL: {} {}\n",
                report.profile,
                match (&self.refused, count) {
                    (Some(("install", reason)), _) => format!("failed · {}", one_line(reason)),
                    (_, 0) => "installed nothing (empty nest)".into(),
                    (_, 1) => "installed 1 skill".into(),
                    (_, n) => format!("installed {n} skills"),
                }
            ));
            if count > 0 {
                let paths: Vec<String> = report
                    .installed
                    .iter()
                    .map(|skill| skill.path.join(install::ENTRY).display().to_string())
                    .collect();
                card.push_str(&format!("PATHS: {}\n", paths.join(", ")));
            }
        }
        if let Some((stage, reason)) = &self.refused {
            card.push_str(&format!("REFUSED: {stage} · {}\n", one_line(reason)));
        }
        if let Some(note) = self
            .record
            .as_ref()
            .map(|r| &r.note)
            .filter(|n| !n.is_empty())
        {
            card.push_str(&format!("NOTE: {}\n", one_line(note)));
        }
        card
    }
    /// Flight-log line; also what the PTY smoke greps.
    pub fn event(&self) -> String {
        let picked = self.record.as_ref().map_or(String::new(), |r| {
            format!(
                " / {} / nest [{}] / stripped {}",
                r.backend_used.as_str(),
                r.selected.join(", "),
                r.stripped.len()
            )
        });
        match &self.refused {
            None => format!(
                "NEST / PID {} / {}{picked} / {} installed / ready",
                self.pid, self.mission, self.profile
            ),
            Some((stage, reason)) => format!(
                "NEST / PID {} / {}{picked} / REFUSED at {stage}: {}",
                self.pid,
                self.mission,
                one_line(reason)
            ),
        }
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Harness dirs under `$HOME` are user-global (`~/.claude/skills`); a flight there must not
/// install. The CLI contract is project-local, so the cockpit refuses instead.
fn workspace() -> Result<PathBuf> {
    let here = env::current_dir()?;
    let home = env::var_os("HOME").map(PathBuf::from);
    ensure!(
        home.as_deref().and_then(|h| h.canonicalize().ok()) != here.canonicalize().ok(),
        "flight workspace is $HOME, where harness skill dirs are user-global · run `make run` (flies in the repo) or start unvrs inside a project directory"
    );
    Ok(PathBuf::from("."))
}

/// Blocking (Jev is an HTTP round trip): call from a worker thread.
pub fn run(job: &Job) -> Outcome {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut outcome = Outcome {
        pid: job.pid,
        mission: "adhoc".into(),
        profile: job.profile.clone(),
        seat: job.seat,
        record: None,
        install: None,
        refused: None,
    };
    if let Err((stage, e)) = nest(job, &mut outcome) {
        let reason = format!("{e:#}");
        let _ = log(
            "boot.refused",
            json!({"mission": outcome.mission, "pid": job.pid, "stage": stage, "reason": reason, "source": "cockpit"}),
        );
        if outcome.mission != "adhoc"
            && let Ok(mut registry) = MissionRegistry::open(Path::new(REGISTRY))
        {
            let _ = registry.set_status(&outcome.mission, Status::Refused, Some(job.pid));
        }
        outcome.refused = Some((stage, reason));
    }
    outcome
}

fn nest(job: &Job, outcome: &mut Outcome) -> Result<(), (&'static str, anyhow::Error)> {
    // 1 · admit (a bare goal has no file to admit: it nests as `adhoc`)
    let mut registry = MissionRegistry::open(Path::new(REGISTRY)).map_err(|e| ("admit", e))?;
    let (brief, hints) = match &job.mission {
        Some(path) => {
            let mission = Mission::load(path)
                .and_then(|mission| {
                    registry.admit(path)?;
                    Ok(mission)
                })
                .map_err(|e| ("admit", e))?;
            outcome.mission = mission.id.clone();
            log(
                "mission.admitted",
                json!({"mission": mission.id, "path": path, "pid": job.pid, "source": "cockpit"}),
            )
            .map_err(|e| ("admit", e))?;
            let brief = if job.goal.trim().is_empty() {
                mission.brief()
            } else {
                format!("{}\ngoal: {}", mission.brief(), job.goal.trim())
            };
            (brief, mission.skills)
        }
        None => (format!("goal: {}", job.goal.trim()), vec![]),
    };

    // 2 · nest plan
    let pick = (|| -> Result<_> {
        let mut config = BootConfig::load(Path::new(".unvrs/boot.toml"))?;
        config.backend = job.picker;
        let catalog = pool::catalog(&pools_file()?, &pool::default_cache_dir()?)?;
        let jev = (config.backend == Backend::Jev)
            .then(|| JevHttp::discover(&env::current_dir().unwrap_or_default()))
            .flatten();
        boot::nest(&brief, &hints, &catalog, job.seat, &config, jev)
    })();
    let (plan, record) = pick.map_err(|e| ("pick", e))?;
    let mut row = serde_json::to_value(&record).map_err(|e| ("pick", e.into()))?;
    row["mission"] = json!(outcome.mission);
    row["pid"] = json!(job.pid);
    row["source"] = json!("cockpit");
    log("boot.pick", row).map_err(|e| ("pick", e))?;
    outcome.record = Some(record);

    // 3 · install into this seat's harness, then ready
    let workspace = workspace().map_err(|e| ("install", e))?;
    let memory_install = install::install_memory_skills(&job.profile, &workspace);
    let report = install::install(&job.profile, &plan.skills, &workspace);
    let error = report
        .error
        .clone()
        .or(memory_install.err().map(|e| format!("{e:#}")))
        .or(install::verify(&report).err().map(|e| format!("{e:#}")));
    let mut row = serde_json::to_value(&report).map_err(|e| ("install", e.into()))?;
    row["mission"] = json!(outcome.mission);
    row["pid"] = json!(job.pid);
    row["ok"] = json!(report.ok && error.is_none());
    row["error"] = json!(error);
    row["source"] = json!("cockpit");
    outcome.install = Some(report);
    log("agent.install", row).map_err(|e| ("install", e))?;
    if let Some(error) = error {
        return Err(("install", anyhow::anyhow!(error)));
    }
    if outcome.mission != "adhoc" {
        registry
            .set_status(&outcome.mission, Status::Active, Some(job.pid))
            .map_err(|e| ("install", e))?;
    }
    let selected = outcome.record.as_ref().map(|r| r.selected.clone());
    log(
        "boot.ready",
        json!({"mission": outcome.mission, "pid": job.pid, "stage": "install", "profiles": [job.profile], "selected": selected, "source": "cockpit"}),
    )
    .map_err(|e| ("install", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(refused: Option<(&'static str, String)>) -> Outcome {
        Outcome {
            pid: 2,
            mission: "msn_x".into(),
            profile: "pi".into(),
            seat: Seat {
                rank: 3,
                intf_focused: true,
            },
            record: Some(BootRecord {
                seat: Seat {
                    rank: 3,
                    intf_focused: true,
                },
                captain_channel: false,
                backend_requested: Backend::Jev,
                backend_used: Backend::Rules,
                fallback: Some("TYPESAFE_API_KEY is not set".into()),
                catalog: 3,
                candidates: 2,
                stripped: vec!["grill-captain".into()],
                hints: vec![],
                noul: None,
                confidence: Some(0.5),
                scores: vec![],
                selected: vec![],
                note: "nothing\ncleared the floor".into(),
            }),
            install: None,
            refused,
        }
    }

    #[test]
    fn card_names_backend_channel_strip_and_refusal() {
        let card = outcome(Some(("install", "disk\nfull".into()))).card();
        assert!(card.starts_with("BOOT / msn_x / PID 2 L3 / pi / REFUSED\n"));
        assert!(card.contains("PICKER: rules (fallback from jev: TYPESAFE_API_KEY is not set)\n"));
        assert!(card.contains("NOUL: n/a\nCONFIDENCE: 0.50\nCHANNEL: no captain channel (L3)\n"));
        assert!(card.contains("STRIPPED: 1 interactive withheld · grill-captain\n"));
        assert!(card.contains("NEST: (empty · no skill needed)\n"));
        assert!(card.contains("REFUSED: install · disk full\n"));
        assert!(card.contains("NOTE: nothing cleared the floor\n"));
        let ready = outcome(None);
        assert!(ready.card().lines().next().unwrap().ends_with("/ READY"));
        assert!(ready.event().ends_with("pi installed / ready"));
        assert!(ready.activation().is_none());
    }
}
