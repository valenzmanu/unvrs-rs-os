//! Harness memory is the harness's own cache: DrvAgent never reads or imports it
//! (memory-layer.md §10, context-ownership.md). This module only writes the
//! seat-local hot-set fallback and installs the memory skills.
use anyhow::{Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uke::{GENERATED_END, GENERATED_START};

/// Every current profile accepts ACP turn input. A non-ACP adapter can read this
/// seat-local fallback at startup; it never touches shared harness memory.
pub fn write_hot_region(universe: &Path, pid: usize, hot: &str) -> Result<PathBuf> {
    ensure!(pid > 0, "PID starts at 1");
    let dir = universe.join(format!("sessions/pid-{pid}"));
    fs::create_dir_all(&dir)?;
    let path = dir.join("harness-memory.md");
    let old = fs::read_to_string(&path).unwrap_or_default();
    let mut outside = String::new();
    let mut generated = false;
    for line in old.lines() {
        if line.contains(GENERATED_START) {
            generated = true;
            continue;
        }
        if line.contains(GENERATED_END) {
            generated = false;
            continue;
        }
        if !generated {
            outside.push_str(line);
            outside.push('\n');
        }
    }
    fs::write(
        &path,
        format!("{outside}{GENERATED_START}\n{hot}\n{GENERATED_END}\n"),
    )?;
    Ok(path)
}
pub fn install_memory_skills(profile: &str, universe: &Path) -> Result<()> {
    let mut skills = vec![];
    for &(id, summary, body) in uke::MEMORY_CATALOGUE {
        let dir = universe.join("catalogue").join(id);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {summary}\n---\n\n{body}\n"),
        )?;
        skills.push(uke::SkillEntry {
            pool: "unvrs".into(),
            id: id.into(),
            summary: summary.into(),
            needs_captain: false,
            dir,
        });
    }
    crate::verify(&crate::install(profile, &skills, universe))
}
