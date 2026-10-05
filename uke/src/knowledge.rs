//! Captain memory (L1) and project memory (L2): notes with a lifetime (D49).
//! Tiers: pinned (never decays), aging (stale after 30 days without reinforcement),
//! perishable (7 days or an explicit expiry). Stale and expired notes are archived,
//! never deleted. Filing inspects before it writes: a fact already known is
//! reinforced, not duplicated. One markdown file per note is the record.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;
pub const AGING_DAYS: u64 = 30;
pub const PERISHABLE_DAYS: u64 = 7;
pub const NOTE_BYTES: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Pinned,
    Aging,
    Perishable,
}
impl Tier {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "pinned" | "pin" => Tier::Pinned,
            "aging" => Tier::Aging,
            "perishable" => Tier::Perishable,
            other => bail!("Unknown tier {other:?}: pinned, aging or perishable"),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Pinned => "pinned",
            Tier::Aging => "aging",
            Tier::Perishable => "perishable",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub tier: Tier,
    /// live · stale · archived
    pub status: String,
    pub text: String,
    pub created: u64,
    pub reinforced: u64,
    #[serde(default)]
    pub expires: Option<u64>,
    /// Perishable: the condition that ends it, in words.
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// captain · pid:N · stow:pid:N
    pub source: String,
    #[serde(skip)]
    pub path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Front {
    id: String,
    tier: Tier,
    status: String,
    created: u64,
    reinforced: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    until: Option<String>,
    #[serde(default)]
    evidence: Vec<String>,
    source: String,
}

impl Note {
    fn read(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        let rest = raw
            .strip_prefix("---\n")
            .with_context(|| format!("Note without front matter: {}", path.display()))?;
        let (front, body) = rest
            .split_once("\n---\n")
            .with_context(|| format!("Note front matter unterminated: {}", path.display()))?;
        let f: Front = toml::from_str(front)
            .with_context(|| format!("Note front matter unreadable: {}", path.display()))?;
        Ok(Note {
            id: f.id,
            tier: f.tier,
            status: f.status,
            text: body.trim().to_owned(),
            created: f.created,
            reinforced: f.reinforced,
            expires: f.expires,
            until: f.until,
            evidence: f.evidence,
            source: f.source,
            path: path.to_path_buf(),
        })
    }
    fn write(&self) -> Result<()> {
        let front = toml::to_string(&Front {
            id: self.id.clone(),
            tier: self.tier,
            status: self.status.clone(),
            created: self.created,
            reinforced: self.reinforced,
            expires: self.expires,
            until: self.until.clone(),
            evidence: self.evidence.clone(),
            source: self.source.clone(),
        })?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, format!("---\n{front}---\n\n{}\n", self.text))?;
        fs::rename(tmp, &self.path)?;
        Ok(())
    }
    /// Live and not past its lifetime at `now`.
    pub fn current(&self, now: u64) -> bool {
        self.status == "live"
            && match self.tier {
                Tier::Pinned => true,
                Tier::Aging => now.saturating_sub(self.reinforced) < AGING_DAYS * DAY_MS,
                Tier::Perishable => self.expires.is_none_or(|e| now < e),
            }
    }
    pub fn line(&self) -> String {
        format!(
            "- [{}] ({}) {}",
            self.id,
            self.tier.as_str(),
            self.text.replace('\n', " ")
        )
    }
}

fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_owned)
        .collect()
}
/// Same fact in other words? Word-set overlap (Jaccard) at or above 0.8, or equal text.
fn same_fact(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    if norm(a) == norm(b) {
        return true;
    }
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return false;
    }
    let inter = x.intersection(&y).count() as f64;
    let union = x.union(&y).count() as f64;
    inter / union >= 0.8
}

/// One memory: `<home>/captain/notes/` or `<home>/projects/<id>/memory/notes/`.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
    prefix: &'static str,
}

/// What `remember` did.
#[derive(Clone, Debug)]
pub enum Filed {
    New(Note),
    /// The fact was already known: its note was reinforced (and its tier raised if asked).
    Reinforced(Note),
}
impl Filed {
    pub fn note(&self) -> &Note {
        match self {
            Filed::New(n) | Filed::Reinforced(n) => n,
        }
    }
}

impl Store {
    pub fn captain(home: &Path) -> Self {
        Self {
            dir: home.join("captain/notes"),
            prefix: "c",
        }
    }
    pub fn project(home: &Path, project: &str) -> Self {
        Self {
            dir: home.join(format!("projects/{project}/memory/notes")),
            prefix: "p",
        }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn notes(&self) -> Result<Vec<Note>> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Ok(vec![]);
        };
        let mut out = vec![];
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                out.push(Note::read(&p)?);
            }
        }
        // Filing order: c2 before c10 (ids are numbered, not lexical).
        let num = |id: &str| {
            id.trim_start_matches(|c: char| c.is_ascii_alphabetic())
                .parse::<u64>()
                .unwrap_or(u64::MAX)
        };
        out.sort_by_key(|n| (n.created, num(&n.id), n.id.clone()));
        Ok(out)
    }
    pub fn note(&self, id: &str) -> Result<Note> {
        ensure!(
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "Invalid note id {id:?}"
        );
        Note::read(&self.dir.join(format!("{id}.md"))).with_context(|| format!("No note {id}"))
    }
    fn next_id(&self) -> Result<String> {
        let n = self
            .notes()?
            .iter()
            .filter_map(|n| n.id.strip_prefix(self.prefix)?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Ok(format!("{}{}", self.prefix, n + 1))
    }
    /// Files a fact: inspect, then update or write.
    pub fn remember(
        &self,
        text: &str,
        tier: Tier,
        source: &str,
        evidence: Option<&str>,
        until: Option<&str>,
        now: u64,
    ) -> Result<Filed> {
        let text = text.trim();
        ensure!(
            !text.is_empty() && text.len() <= NOTE_BYTES,
            "A memory note holds 1–{NOTE_BYTES} bytes"
        );
        if let Some(mut n) = self
            .notes()?
            .into_iter()
            .find(|n| n.status == "live" && same_fact(&n.text, text))
        {
            n.reinforced = now;
            if tier < n.tier {
                n.tier = tier; // pinned < aging < perishable: only ever raised
                n.expires = None;
            }
            let ev = evidence.unwrap_or(source).to_owned();
            if !n.evidence.contains(&ev) {
                n.evidence.push(ev);
                if n.evidence.len() > 8 {
                    n.evidence.remove(0);
                }
            }
            n.write()?;
            return Ok(Filed::Reinforced(n));
        }
        let id = self.next_id()?;
        let n = Note {
            path: self.dir.join(format!("{id}.md")),
            id,
            tier,
            status: "live".into(),
            text: text.into(),
            created: now,
            reinforced: now,
            expires: (tier == Tier::Perishable).then_some(now + PERISHABLE_DAYS * DAY_MS),
            until: until.map(str::to_owned),
            evidence: evidence.map(|e| vec![e.to_owned()]).unwrap_or_default(),
            source: source.into(),
        };
        n.write()?;
        Ok(Filed::New(n))
    }
    /// Archives a note (never deletes).
    pub fn forget(&self, id: &str) -> Result<Note> {
        let mut n = self.note(id)?;
        n.status = "archived".into();
        n.write()?;
        Ok(n)
    }
    /// Stale aging notes and expired perishable ones leave the hot set (archived).
    pub fn sweep(&self, now: u64) -> Result<Vec<(String, String)>> {
        let mut out = vec![];
        for mut n in self.notes()? {
            if n.status == "live" && !n.current(now) {
                n.status = if n.tier == Tier::Aging {
                    "stale"
                } else {
                    "archived"
                }
                .into();
                n.write()?;
                out.push((n.id.clone(), n.status.clone()));
            }
        }
        Ok(out)
    }
    /// Always-loaded part within `budget` bytes: pinned first, then aging and perishable,
    /// newest reinforcement first; the rest as a count.
    pub fn hot(&self, budget: usize, now: u64) -> String {
        let mut notes: Vec<Note> = self
            .notes()
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.current(now))
            .collect();
        notes.sort_by(|a, b| a.tier.cmp(&b.tier).then(b.reinforced.cmp(&a.reinforced)));
        let mut out = String::new();
        let mut left = 0;
        for n in &notes {
            let line = format!("{}\n", n.line());
            if out.len() + line.len() > budget {
                left += 1;
                continue;
            }
            out.push_str(&line);
        }
        if left > 0 {
            out.push_str(&format!(
                "({left} more notes; search them with `ctx search` on the memory source)\n"
            ));
        }
        out
    }
    pub fn search(&self, query: &str) -> Result<Vec<Note>> {
        let q = words(query);
        Ok(self
            .notes()?
            .into_iter()
            .filter(|n| {
                let w = words(&n.text);
                !q.is_empty() && q.iter().all(|t| w.iter().any(|x| x.contains(t.as_str())))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiers_reinforce_and_decay() {
        let dir = std::env::temp_dir().join(format!("unvrs-knowledge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let s = Store::captain(&dir);
        let t0 = 1_000_000_000_000;
        let a = s
            .remember(
                "Invoices go through Stripe, never PayPal",
                Tier::Pinned,
                "captain",
                None,
                None,
                t0,
            )
            .unwrap();
        assert!(matches!(a, Filed::New(_)));
        let b = s
            .remember(
                "invoices go through   Stripe, never PayPal",
                Tier::Aging,
                "pid:1",
                Some("thread x"),
                None,
                t0 + 5,
            )
            .unwrap();
        assert!(
            matches!(b, Filed::Reinforced(ref n) if n.tier == Tier::Pinned && n.evidence == vec!["thread x".to_string()])
        );
        s.remember(
            "The deploy needs the VPN",
            Tier::Aging,
            "pid:1",
            None,
            None,
            t0,
        )
        .unwrap();
        s.remember(
            "Dentist on Friday",
            Tier::Perishable,
            "captain",
            None,
            Some("after Friday"),
            t0,
        )
        .unwrap();
        assert_eq!(s.notes().unwrap().len(), 3);
        let hot = s.hot(4000, t0 + DAY_MS);
        assert!(hot.contains("Stripe") && hot.contains("VPN") && hot.contains("Dentist"));
        let later = t0 + 31 * DAY_MS;
        let swept = s.sweep(later).unwrap();
        assert_eq!(swept.len(), 2);
        let hot = s.hot(4000, later);
        assert!(hot.contains("Stripe") && !hot.contains("VPN") && !hot.contains("Dentist"));
        assert_eq!(s.notes().unwrap().len(), 3, "archived, never deleted");
        s.forget("c1").unwrap();
        assert!(!s.hot(4000, later).contains("Stripe"));
        assert!(s.hot(20, t0).contains("more notes") || s.hot(20, t0).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
