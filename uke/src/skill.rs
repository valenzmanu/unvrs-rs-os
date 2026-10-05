//! Catalogue entry shared by pools (producer), DrvBoot (picker) and DrvAgent (installer).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillEntry {
    /// Pool id from the pool config, e.g. `mattpocock`.
    pub pool: String,
    /// Skill id, unique inside its pool (the skill directory name).
    pub id: String,
    /// One-line description from the skill's frontmatter.
    pub summary: String,
    /// Interactive: only useful on a seat with a captain channel.
    pub needs_captain: bool,
    /// Resolved directory on disk holding `SKILL.md` and its resources.
    pub dir: PathBuf,
}

/// Kernel operations projected as native skills by DrvAgent.
pub const MEMORY_CATALOGUE: &[(&str, &str, &str)] = &[
    (
        "remember",
        "Keep a durable universe note",
        "Run `unvrs ctl remember 'a self-contained fact, decision or pointer'`. This writes an unpinned note attributed to your PID in the active area. Never store secrets. Only the captain may pin, retire, delete, settle or switch scope.",
    ),
    (
        "recall",
        "Find prior work across the universe",
        "Before answering that you do not know or do not remember, run `unvrs ctl recall 'keywords'`. Search covers all scopes, notes, PID summaries, cold transcripts, missions and handoffs. Read a returned pointer for detail. Your harness memory is a cache; universe files are the record.",
    ),
];
