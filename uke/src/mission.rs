//! Mission MD files and the thin MissionRegistry (uke-design §4).
//!
//! The Markdown file is the document of truth: YAML-ish frontmatter plus `# Heading`
//! sections. The registry is an index only (id → path, status, owner_pid, updated),
//! persisted as one JSON file and refreshed after every mutation.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Draft,
    Admitted,
    Active,
    Done,
    Refused,
    Cancelled,
}
impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Admitted => "admitted",
            Status::Active => "active",
            Status::Done => "done",
            Status::Refused => "refused",
            Status::Cancelled => "cancelled",
        }
    }
}
impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for Status {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim() {
            "draft" => Status::Draft,
            "admitted" => Status::Admitted,
            "active" => Status::Active,
            "done" => Status::Done,
            "refused" => Status::Refused,
            "cancelled" => Status::Cancelled,
            other => bail!("unknown mission status: {other}"),
        })
    }
}

/// How much power the captain is willing to spend. Hint for Econ, not a quality dial.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Investment {
    Cheap,
    #[default]
    Standard,
    Frontier,
}
impl Investment {
    pub fn as_str(&self) -> &'static str {
        match self {
            Investment::Cheap => "cheap",
            Investment::Standard => "standard",
            Investment::Frontier => "frontier",
        }
    }
}
impl fmt::Display for Investment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for Investment {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim() {
            "cheap" => Investment::Cheap,
            "standard" => Investment::Standard,
            "frontier" => Investment::Frontier,
            other => bail!("unknown mission investment: {other}"),
        })
    }
}

/// One mission packet, loaded from its Markdown file.
#[derive(Clone, Debug, Default)]
pub struct Mission {
    pub id: String,
    pub status: Status,
    pub investment: Investment,
    pub owner_pid: Option<usize>,
    pub updated: String,
    pub outcome: String,
    pub done_check: String,
    pub scope: String,
    pub prompt: String,
    pub pointers: Vec<String>,
    /// Optional frontmatter hint `skills: [a, b]`; empty when absent.
    pub skills: Vec<String>,
    pub log: String,
    pub path: PathBuf,
}

/// Placeholder text counts as empty: the admit floor wants real words.
fn blank(s: &str) -> bool {
    let t = s.trim();
    t.is_empty()
        || t.chars()
            .all(|c| matches!(c, '…' | '.' | '-' | '_' | '?' | ' ' | '\n' | '\t'))
        || matches!(
            t.to_ascii_lowercase().trim_end_matches(['.', '!', ':']),
            "todo" | "tbd" | "tba" | "fixme" | "n/a" | "none"
        )
}
/// `a, b` or `[a, b]` → owned trimmed items.
fn items(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !blank(s))
        .map(str::to_string)
        .collect()
}

impl Mission {
    /// Hand-rolled parse: `---` fenced `key: value` frontmatter, then `# Heading` sections.
    pub fn parse(text: &str, path: &Path) -> Result<Mission> {
        let mut mission = Mission {
            path: path.to_path_buf(),
            ..Mission::default()
        };
        let mut lines = text.lines();
        let first = lines.next().map(str::trim).unwrap_or_default();
        ensure!(
            first == "---",
            "mission {} must open with a --- frontmatter fence",
            path.display()
        );
        let mut fenced = false;
        for line in lines.by_ref() {
            if line.trim() == "---" {
                fenced = true;
                break;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "id" => mission.id = value.to_string(),
                "status" if !value.is_empty() => mission.status = value.parse()?,
                "investment" if !value.is_empty() => mission.investment = value.parse()?,
                "owner_pid" => {
                    mission.owner_pid = match value {
                        "" | "null" | "~" => None,
                        pid => Some(pid.parse().with_context(|| {
                            format!("owner_pid must be a number or null, got {pid}")
                        })?),
                    }
                }
                "updated" => mission.updated = value.to_string(),
                "skills" => mission.skills = items(value),
                _ => {}
            }
        }
        ensure!(
            fenced,
            "mission {} frontmatter is not closed by ---",
            path.display()
        );
        let mut section = String::new();
        let mut body: BTreeMap<String, String> = BTreeMap::new();
        for line in lines {
            if let Some(heading) = line.trim().strip_prefix("# ") {
                section = heading.trim().to_ascii_lowercase();
                continue;
            }
            if section.is_empty() {
                continue;
            }
            let slot = body.entry(section.clone()).or_default();
            slot.push_str(line);
            slot.push('\n');
        }
        let take = |key: &str| {
            body.get(key)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        mission.outcome = take("outcome");
        mission.done_check = take("done check");
        mission.scope = take("scope");
        mission.log = take("log");
        for line in take("inputs").lines() {
            let Some(bullet) = line.trim().strip_prefix("- ") else {
                continue;
            };
            match bullet.split_once(':') {
                Some((key, value)) if key.trim() == "prompt" => {
                    mission.prompt = value.trim().to_string()
                }
                Some((key, value)) if key.trim() == "pointers" => {
                    mission.pointers.extend(items(value))
                }
                _ => {}
            }
        }
        Ok(mission)
    }
    pub fn load(path: &Path) -> Result<Mission> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read mission {}", path.display()))?;
        Mission::parse(&text, path)
    }
    /// Admit floor (uke-design §4): id, outcome and done check, all for real.
    pub fn admit_check(&self) -> Result<()> {
        ensure!(
            self.id.starts_with("msn_") && self.id.len() > 4,
            "mission id must start with msn_, got {:?} in {}",
            self.id,
            self.path.display()
        );
        ensure!(
            !blank(&self.outcome),
            "mission {} refused: missing Outcome (what concrete thing exists when done)",
            self.id
        );
        ensure!(
            !blank(&self.done_check),
            "mission {} refused: missing Done check (how a stranger says pass/fail)",
            self.id
        );
        Ok(())
    }
    /// Compact brief; later read by the skill pickers as their state.
    pub fn brief(&self) -> String {
        let mut out = format!(
            "mission: {}\noutcome: {}\ndone check: {}",
            self.id,
            self.outcome.trim(),
            self.done_check.trim()
        );
        if !blank(&self.scope) {
            out.push_str(&format!("\nscope: {}", self.scope.trim()));
        }
        if !blank(&self.prompt) {
            out.push_str(&format!("\nprompt: {}", self.prompt.trim()));
        }
        if !self.pointers.is_empty() {
            out.push_str(&format!("\npointers: {}", self.pointers.join(", ")));
        }
        out
    }
}

/// Index row: the file stays the document of truth.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub status: Status,
    #[serde(default)]
    pub owner_pid: Option<usize>,
    #[serde(default)]
    pub updated: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    missions: BTreeMap<String, Entry>,
}

/// Thin JSON-file-backed mission index owned by uKe core.
#[derive(Debug)]
pub struct MissionRegistry {
    path: PathBuf,
    index: Index,
    /// Rewrite `status:`/`owner_pid:`/`updated:` in the mission MD on mutation.
    /// Off by default so smoke fixtures stay pristine.
    pub write_back: bool,
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ` without pulling a date crate.
pub fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = 400 * era + yoe + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl MissionRegistry {
    /// Open (or start) the index at `path`, creating parent directories.
    pub fn open(path: &Path) -> Result<MissionRegistry> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let index = match fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
                .with_context(|| format!("mission registry {} is corrupt", path.display()))?,
            _ => Index::default(),
        };
        Ok(MissionRegistry {
            path: path.to_path_buf(),
            index,
            write_back: false,
        })
    }
    /// Load a mission file, enforce the admit floor, record it as `admitted`.
    /// A failed check records nothing.
    pub fn admit(&mut self, mission_path: &Path) -> Result<Entry> {
        let mission = Mission::load(mission_path)?;
        mission.admit_check()?;
        let updated = now();
        let entry = Entry {
            path: mission_path.to_path_buf(),
            status: Status::Admitted,
            owner_pid: mission.owner_pid,
            updated: updated.clone(),
        };
        self.index
            .missions
            .insert(mission.id.clone(), entry.clone());
        self.persist()?;
        self.stamp(&entry)?;
        Ok(entry)
    }
    pub fn set_status(
        &mut self,
        id: &str,
        status: Status,
        owner_pid: Option<usize>,
    ) -> Result<Entry> {
        let entry = self
            .index
            .missions
            .get_mut(id)
            .with_context(|| format!("unknown mission {id}"))?;
        entry.status = status;
        entry.owner_pid = owner_pid;
        entry.updated = now();
        let entry = entry.clone();
        self.persist()?;
        self.stamp(&entry)?;
        Ok(entry)
    }
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.index.missions.get(id)
    }
    pub fn list(&self) -> Vec<(&String, &Entry)> {
        self.index.missions.iter().collect()
    }
    pub fn registry_path(&self) -> &Path {
        &self.path
    }
    /// temp + rename so a crash never leaves half an index.
    fn persist(&self) -> Result<()> {
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_string_pretty(&self.index)? + "\n")
            .with_context(|| format!("cannot write {}", temp.display()))?;
        fs::rename(&temp, &self.path)
            .with_context(|| format!("cannot replace {}", self.path.display()))?;
        Ok(())
    }
    /// Rewrite the mission MD frontmatter to match the row (only when `write_back`).
    fn stamp(&self, entry: &Entry) -> Result<()> {
        if !self.write_back {
            return Ok(());
        }
        let text = fs::read_to_string(&entry.path)
            .with_context(|| format!("cannot read mission {}", entry.path.display()))?;
        let mut out = String::with_capacity(text.len());
        let mut fence = 0usize;
        for line in text.lines() {
            if line.trim() == "---" {
                fence += 1;
            }
            let key = line.split_once(':').map(|(k, _)| k.trim()).unwrap_or("");
            let replacement = match (fence, key) {
                (1, "status") => Some(format!("status: {}", entry.status)),
                (1, "owner_pid") => Some(match entry.owner_pid {
                    Some(pid) => format!("owner_pid: {pid}"),
                    None => "owner_pid: null".to_string(),
                }),
                (1, "updated") => Some(format!("updated: {}", entry.updated)),
                _ => None,
            };
            out.push_str(replacement.as_deref().unwrap_or(line));
            out.push('\n');
        }
        fs::write(&entry.path, out)
            .with_context(|| format!("cannot rewrite {}", entry.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "---\nid: msn_demo\nstatus: draft\ninvestment: frontier\nowner_pid: null\nupdated: 2026-09-21\nskills: [test-driven-development, brainstorming]\n---\n\n# Outcome\nA parser exists.\n\n# Done check\ncargo test passes.\n\n# Scope\nIn: the parser. Out: the CLI.\n\n# Inputs\n- prompt: Write the mission parser.\n- pointers: uke/src/lib.rs, docs/design/uke-design.md\n\n# Log\n…\n";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uke-mission-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn parses_frontmatter_and_sections() {
        let m = Mission::parse(GOOD, Path::new("msn_demo.md")).unwrap();
        assert_eq!(m.id, "msn_demo");
        assert_eq!(m.status, Status::Draft);
        assert_eq!(m.investment, Investment::Frontier);
        assert_eq!(m.owner_pid, None);
        assert_eq!(m.outcome, "A parser exists.");
        assert_eq!(m.done_check, "cargo test passes.");
        assert_eq!(m.prompt, "Write the mission parser.");
        assert_eq!(m.pointers.len(), 2);
        assert_eq!(m.skills, ["test-driven-development", "brainstorming"]);
        assert!(m.log.is_empty() || blank(&m.log));
        assert!(m.brief().contains("cargo test passes."));
        m.admit_check().unwrap();
    }
    #[test]
    fn owner_pid_and_status_round_trip() {
        let text = GOOD.replace("owner_pid: null", "owner_pid: 7");
        let m = Mission::parse(&text, Path::new("x.md")).unwrap();
        assert_eq!(m.owner_pid, Some(7));
        assert_eq!("admitted".parse::<Status>().unwrap(), Status::Admitted);
        assert_eq!(Status::Cancelled.to_string(), "cancelled");
        assert!("wat".parse::<Status>().is_err());
        assert!("wat".parse::<Investment>().is_err());
    }
    #[test]
    fn admit_floor_rejects_placeholders_and_bad_ids() {
        for (bad, needle) in [
            (GOOD.replace("A parser exists.", "…"), "Outcome"),
            (GOOD.replace("cargo test passes.", "TODO"), "Done check"),
            (
                GOOD.replace("# Done check\ncargo test passes.\n", ""),
                "Done check",
            ),
            (GOOD.replace("id: msn_demo", "id: demo"), "msn_"),
        ] {
            let m = Mission::parse(&bad, Path::new("bad.md")).unwrap();
            let err = format!("{:#}", m.admit_check().unwrap_err());
            assert!(err.contains(needle), "{err} missing {needle}");
        }
        assert!(Mission::parse("no fence here\n", Path::new("bad.md")).is_err());
    }
    #[test]
    fn registry_round_trip() {
        let file = temp("msn_demo.md");
        fs::write(&file, GOOD).unwrap();
        let registry_path = file.with_file_name("missions.json");
        let _ = fs::remove_file(&registry_path);
        let mut registry = MissionRegistry::open(&registry_path).unwrap();
        let entry = registry.admit(&file).unwrap();
        assert_eq!(entry.status, Status::Admitted);
        registry
            .set_status("msn_demo", Status::Active, Some(3))
            .unwrap();
        // Fixture untouched while write_back is off.
        assert_eq!(fs::read_to_string(&file).unwrap(), GOOD);
        let reopened = MissionRegistry::open(&registry_path).unwrap();
        let row = reopened.get("msn_demo").unwrap();
        assert_eq!((row.status, row.owner_pid), (Status::Active, Some(3)));
        assert_eq!(reopened.list().len(), 1);
        assert!(reopened.get("msn_missing").is_none());
    }
    #[test]
    fn write_back_stamps_frontmatter_and_bad_mission_records_nothing() {
        let file = temp("msn_wb.md");
        fs::write(&file, GOOD.replace("id: msn_demo", "id: msn_wb")).unwrap();
        let bad = temp("msn_bad.md");
        fs::write(&bad, GOOD.replace("A parser exists.", "TODO")).unwrap();
        let registry_path = file.with_file_name("missions-wb.json");
        let _ = fs::remove_file(&registry_path);
        let mut registry = MissionRegistry::open(&registry_path).unwrap();
        registry.write_back = true;
        registry.admit(&file).unwrap();
        let stamped = fs::read_to_string(&file).unwrap();
        assert!(stamped.contains("status: admitted"), "{stamped}");
        assert!(registry.admit(&bad).is_err());
        assert!(registry.get("msn_demo").is_none());
        assert_eq!(registry.list().len(), 1);
        assert!(now().ends_with('Z') && now().len() == 20);
    }
}
