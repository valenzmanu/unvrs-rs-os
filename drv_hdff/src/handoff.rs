//! Compact in-kernel work transfer; serialized only at the transport boundary.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffSummary {
    pub work_id: String,
    #[serde(default)]
    pub from_harness: String,
    #[serde(default)]
    pub from_pid: usize,
    #[serde(default)]
    pub from_rank: u8,
    pub to_rank: u8,
    pub to_pid: Option<usize>,
    pub to_harness: Option<String>,
    pub next_focus: String,
    pub goal: String,
    pub done: Vec<String>,
    pub open: Vec<String>,
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub suggested_skills: Vec<String>,
    #[serde(default)]
    pub created_at: u64,
}
impl HandoffSummary {
    pub fn new(from_pid: usize, to_rank: u8, harness: &str, focus: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            work_id: format!("work-{}", now.as_nanos()),
            from_harness: String::new(),
            from_pid,
            from_rank: 0,
            to_rank,
            to_pid: None,
            to_harness: Some(harness.into()),
            next_focus: focus.into(),
            goal: focus.into(),
            done: vec![],
            open: vec![],
            artifacts: vec![],
            suggested_skills: vec![],
            created_at: now.as_secs(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!((1..=3).contains(&self.to_rank), "to_rank must be 1, 2 or 3");
        ensure!(
            !self.work_id.trim().is_empty()
                && !self.next_focus.trim().is_empty()
                && !self.goal.trim().is_empty(),
            "work_id, next_focus and goal are required"
        );
        ensure!(
            matches!(
                self.to_harness.as_deref(),
                None | Some("pi" | "codex" | "claude" | "cursor")
            ),
            "Harness must be pi, codex, claude or cursor"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= 24000,
            "Summary exceeds 24000 bytes; link artifacts, redact secrets, omit transcripts"
        );
        Ok(())
    }
    pub fn card(&self) -> String {
        format!(
            "HANDOFF / {} / {} → {} / PID {} L{} → PID {} L{} / owner={}\nFOCUS: {}\nGOAL: {}\nDONE: {}\nOPEN: {}\nARTIFACTS: {}\nSUGGESTED SKILLS: {}\nCREATED: {}",
            self.work_id,
            if self.from_harness.is_empty() {
                "unknown"
            } else {
                &self.from_harness
            },
            self.to_harness.as_deref().unwrap_or("pi"),
            self.from_pid,
            self.from_rank,
            self.to_pid.unwrap_or(0),
            self.to_rank,
            self.to_pid.unwrap_or(0),
            self.next_focus,
            self.goal,
            self.done.join("; "),
            self.open.join("; "),
            self.artifacts.join("; "),
            self.suggested_skills.join("; "),
            self.created_at
        )
    }
}

impl uke::HandoffTarget for HandoffSummary {
    fn target_pid(&self) -> Option<usize> {
        self.to_pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_profiles_validate_and_card_names_route_and_owner() {
        for harness in ["pi", "codex", "claude", "cursor"] {
            let mut summary = HandoffSummary::new(2, 3, harness, "continue");
            summary.from_harness = "pi".into();
            summary.to_pid = Some(4);
            summary.validate().unwrap();
            let card = summary.card();
            assert!(card.contains(&format!("pi → {harness}")));
            assert!(card.contains("owner=4"));
        }
    }
}
