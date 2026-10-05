//! One brief (memory-layer §13 R2): the session summary, the swap rehydrate and the
//! handoff package share this schema. Writes are compare-and-swap on a version (D22).
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const BRIEF_DECISIONS: usize = 8;
pub const BRIEF_DONE: usize = 12;
pub const BRIEF_NEXT: usize = 3;
pub const BRIEF_LIST: usize = 12;
pub const BRIEF_ITEM_BYTES: usize = 400;
pub const BRIEF_NARRATIVE_BYTES: usize = 1200;

/// The brief extends the 0.3 `HandoffSummary` (goal, done, open, artifacts); its
/// `next_focus` becomes `next`. Open items carry ids like `[o3]` so a fold can prove
/// that none vanished.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Brief {
    #[serde(default)]
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<String>,
    #[serde(default)]
    pub now: String,
    #[serde(default)]
    pub next: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub open: Vec<String>,
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<String>,
    #[serde(default)]
    pub gotchas: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub narrative: String,
}

/// What a fold worker returns: the new brief plus durable facts that should outlive it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BriefFold {
    pub brief: Brief,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// A fold job records the brief version it read (CAS).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FoldJob {
    pub pid: usize,
    pub version: u64,
    pub reason: String,
    pub brief: Brief,
    pub turns: Vec<String>,
    /// Tail entries the job covers (for draining on threshold compaction).
    pub covered: usize,
    pub drain: bool,
    pub area: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FoldOutcome {
    Applied {
        version: u64,
        graduated: usize,
    },
    /// The brief moved on while the job ran; the job is dropped (re-run on the new brief).
    Discarded {
        read: u64,
        current: u64,
    },
}

pub fn item_id(item: &str) -> Option<&str> {
    let rest = item.trim_start().strip_prefix('[')?;
    let (id, _) = rest.split_once(']')?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(id)
}
fn normalized(item: &str) -> String {
    let text = match item_id(item) {
        Some(id) => item.trim_start()[id.len() + 2..].trim(),
        None => item.trim(),
    };
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

impl Brief {
    pub fn is_empty(&self) -> bool {
        *self == Brief::default()
    }
    /// Gives every open item an id; keeps existing ids.
    pub fn number_open(&mut self) {
        let mut next = self
            .open
            .iter()
            .chain(&self.done)
            .chain(&self.cancelled)
            .filter_map(|i| item_id(i))
            .filter_map(|id| id.strip_prefix('o').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        for item in &mut self.open {
            if item_id(item).is_none() {
                next += 1;
                *item = format!("[o{next}] {}", item.trim());
            }
        }
    }
    /// Open items cannot vanish: each previous open item must be open, done or cancelled
    /// in the new brief (by id, or by text when the previous item had no id).
    pub fn check_open_items(previous: &Brief, next: &Brief) -> Result<()> {
        let ids: BTreeSet<&str> = next
            .open
            .iter()
            .chain(&next.done)
            .chain(&next.cancelled)
            .filter_map(|i| item_id(i))
            .collect();
        let texts: BTreeSet<String> = next
            .open
            .iter()
            .chain(&next.done)
            .chain(&next.cancelled)
            .map(|i| normalized(i))
            .collect();
        let missing: Vec<&String> = previous
            .open
            .iter()
            .filter(|item| match item_id(item) {
                Some(id) => !ids.contains(id) && !texts.contains(&normalized(item)),
                None => !texts.contains(&normalized(item)),
            })
            .collect();
        if !missing.is_empty() {
            bail!(
                "Fold rejected: open items vanished ({}); previous brief kept",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        Ok(())
    }
    /// Bounds every field; returns decisions that overflowed (oldest first) so the
    /// caller can graduate them into notes.
    pub fn bound(&mut self) -> Vec<String> {
        for list in [
            &mut self.next,
            &mut self.decisions,
            &mut self.open,
            &mut self.done,
            &mut self.cancelled,
            &mut self.gotchas,
            &mut self.artifacts,
        ] {
            list.retain(|i| !i.trim().is_empty());
            for item in list.iter_mut() {
                *item = clip(item.trim(), BRIEF_ITEM_BYTES);
            }
        }
        self.goal = clip(self.goal.trim(), BRIEF_ITEM_BYTES);
        self.now = clip(self.now.trim(), BRIEF_ITEM_BYTES);
        self.narrative = clip(self.narrative.trim(), BRIEF_NARRATIVE_BYTES);
        self.next.truncate(BRIEF_NEXT);
        let overflow = self.decisions.len().saturating_sub(BRIEF_DECISIONS);
        let graduated: Vec<String> = self.decisions.drain(..overflow).collect();
        let done_overflow = self.done.len().saturating_sub(BRIEF_DONE);
        self.done.drain(..done_overflow);
        for list in [&mut self.gotchas, &mut self.artifacts, &mut self.cancelled] {
            let over = list.len().saturating_sub(BRIEF_LIST);
            list.drain(..over);
        }
        graduated
    }
    pub fn validate_shape(&self) -> Result<()> {
        ensure!(
            serde_json::to_vec(self)?.len() <= 24000,
            "Brief exceeds 24000 bytes"
        );
        Ok(())
    }
    /// Plain text for recall and the legacy `summary` field.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut line = |label: &str, value: &str| {
            if !value.trim().is_empty() {
                out.push_str(&format!("{label}: {value}\n"));
            }
        };
        line("goal", &self.goal);
        if let Some(m) = &self.mission {
            line("mission", m);
        }
        line("now", &self.now);
        let lists: [(&str, &Vec<String>); 8] = [
            ("next", &self.next),
            ("open", &self.open),
            ("done", &self.done),
            ("decisions", &self.decisions),
            ("gotchas", &self.gotchas),
            ("artifacts", &self.artifacts),
            ("cancelled", &self.cancelled),
            ("", &Vec::new()),
        ];
        for (label, items) in lists {
            if !items.is_empty() {
                out.push_str(&format!("{label}:\n"));
                for i in items {
                    out.push_str(&format!("- {i}\n"));
                }
            }
        }
        if !self.narrative.trim().is_empty() {
            out.push_str(&format!("narrative: {}\n", self.narrative));
        }
        out
    }
}
