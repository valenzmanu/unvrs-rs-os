//! Harness-native install/activate of a nest plan for pi, codex, claude and cursor.
//!
//! Project-local only: every write lands under the workspace, never in the captain's home.
//! Each profile also gets an activation snippet the seat boot prompt can carry, because no
//! harness guarantees it reads a skill body without being told the path (progressive disclosure).
//!
//! Evidence probed 2026-09-21 against the locally installed CLIs:
//!
//! | profile | native root | evidence |
//! | --- | --- | --- |
//! | `claude` | `<ws>/.claude/skills/<id>/SKILL.md` | claude 2.1.278: `.claude/skills/` and literals such as `.claude/skills/commit/SKILL.md` in the packaged CLI |
//! | `codex` | `<ws>/.agents/skills/<id>/SKILL.md` | codex-cli 0.155.1: `codex debug prompt-input` (no inference) listed a planted skill as `(file: r8/<id>/SKILL.md)` with root `r8 = <ws>/.agents/skills`; this is also the only project root in the OpenAI docs (developers.openai.com/codex/skills → learn.chatgpt.com/docs/build-skills). `<ws>/.codex/skills` worked too (root `r0`) but is undocumented; `~/.codex/skills` is the *global* `$CODEX_HOME/skills` |
//! | `pi` | `<ws>/.pi/skills/<id>/SKILL.md` | pi (`@earendil-works/pi-coding-agent` 0.85.1) `docs/skills.md` § Locations: project scope `.pi/skills/` and `.agents/skills/`, directories containing `SKILL.md` discovered recursively, loaded once the project is trusted (`pi --approve`) |
//! | `cursor` | `<ws>/.cursor/skills/<id>/SKILL.md` | cursor-agent 2026.09.10: `src/utils/skill-path-utils.ts` prefix list starts `.cursor/skills/`, the scaffolder labels it `Project Skill → .cursor/skills/`, and discovery walks directories for `SKILL.md` |
//!
//! All four read the [Agent Skills](https://agentskills.io/specification) `SKILL.md` layout, so no
//! profile needs the Cursor rules/`.mdc` rewrite we were ready for (Cursor CLI 2.4 added skills to
//! the CLI, and its root list even covers `.claude/skills/` and `.codex/skills/`). A profile with
//! no verified native root falls back to prompt activation: the skill is copied to
//! `<ws>/.unvrs/skills/<id>/` and the activation text points the seat at it. No harness offers a
//! plain non-LLM `skills list` subcommand; `codex debug prompt-input` is the closest and the smoke
//! uses it as an INFO-only probe.
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use uke::SkillEntry;

/// One skill placed for one profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstalledSkill {
    pub id: String,
    pub path: PathBuf,
    /// `native` (harness reads this directory itself) or `prompt` (activation text only).
    pub mode: String,
}

/// Per-profile outcome; one row of the install matrix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    pub profile: String,
    pub ok: bool,
    pub installed: Vec<InstalledSkill>,
    /// Snippet for the seat boot prompt: skill id, path, one-line summary.
    pub activation: String,
    pub error: Option<String>,
}

/// Entry file every supported harness expects inside a skill directory.
pub const ENTRY: &str = "SKILL.md";
/// Manifest recording profile → installed skills, relative to the workspace.
pub const MANIFEST: &str = ".unvrs/installed-skills.json";

/// Harness-native skill root for `profile`, or `None` when only prompt activation is verified.
pub fn native_root(profile: &str, workspace: &Path) -> Option<PathBuf> {
    let relative = match profile {
        "pi" => ".pi/skills",
        "codex" => ".agents/skills",
        "claude" => ".claude/skills",
        "cursor" => ".cursor/skills",
        _ => return None,
    };
    Some(workspace.join(relative))
}

fn known(profile: &str) -> bool {
    matches!(profile, "pi" | "codex" | "claude" | "cursor")
}

/// Install `skills` for one profile. Never panics; failures come back as `ok=false`.
pub fn install(profile: &str, skills: &[SkillEntry], workspace: &Path) -> InstallReport {
    match place(profile, skills, workspace) {
        Ok((installed, activation)) => InstallReport {
            profile: profile.to_owned(),
            ok: true,
            installed,
            activation,
            error: None,
        },
        Err(e) => InstallReport {
            profile: profile.to_owned(),
            ok: false,
            installed: Vec::new(),
            activation: String::new(),
            error: Some(format!("{e:#}")),
        },
    }
}

fn place(
    profile: &str,
    skills: &[SkillEntry],
    workspace: &Path,
) -> Result<(Vec<InstalledSkill>, String)> {
    ensure!(known(profile), "Unknown ACP profile {profile}");
    ensure!(
        !workspace.as_os_str().is_empty(),
        "Workspace path is required"
    );
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        ensure!(
            workspace != Path::new(&home),
            "Refusing to install into the captain's home directory"
        );
    }
    if skills.is_empty() {
        return Ok((Vec::new(), String::new()));
    }
    fs::create_dir_all(workspace)
        .with_context(|| format!("Workspace unavailable: {}", workspace.display()))?;
    let (root, mode) = match native_root(profile, workspace) {
        Some(root) => (root, "native"),
        None => (workspace.join(".unvrs/skills"), "prompt"),
    };
    let mut installed = Vec::new();
    for skill in skills {
        ensure!(
            !skill.id.is_empty()
                && Path::new(&skill.id)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                && !skill.id.contains('/'),
            "Unsafe skill id {}",
            skill.id
        );
        let entry = skill.dir.join(ENTRY);
        ensure!(
            entry.is_file(),
            "Skill {} has no {ENTRY} in {}",
            skill.id,
            skill.dir.display()
        );
        let target = root.join(&skill.id);
        if target.exists() {
            // Idempotent: replace this skill only; siblings keep their place.
            fs::remove_dir_all(&target)
                .with_context(|| format!("Stale skill not replaceable: {}", target.display()))?;
        }
        copy_tree(&skill.dir, &target)
            .with_context(|| format!("Install of skill {} failed", skill.id))?;
        installed.push(InstalledSkill {
            id: skill.id.clone(),
            path: target,
            mode: mode.to_owned(),
        });
    }
    manifest(workspace, profile, &installed)?;
    let activation = activation(profile, mode, skills, &installed, workspace);
    Ok((installed, activation))
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("Cannot create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("Cannot read {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)
                .with_context(|| format!("Cannot copy into {}", target.display()))?;
        }
        // Symlinks are skipped: skills must be self-contained inside the workspace.
    }
    Ok(())
}

/// Short activation text: how to reach each skill plus one line of why.
fn activation(
    profile: &str,
    mode: &str,
    skills: &[SkillEntry],
    installed: &[InstalledSkill],
    workspace: &Path,
) -> String {
    let mut text = format!(
        "UNVRS skills for {profile} ({mode}, {} total):\n",
        installed.len()
    );
    for (skill, placed) in skills.iter().zip(installed) {
        let path = placed
            .path
            .strip_prefix(workspace)
            .unwrap_or(&placed.path)
            .join(ENTRY);
        let summary = if skill.summary.is_empty() {
            "no summary"
        } else {
            &skill.summary
        };
        text.push_str(&format!(
            "- {} · {} · {summary}\n",
            skill.id,
            path.display()
        ));
    }
    text.push_str("Read a skill's SKILL.md in full before acting on a task it covers.");
    text
}

fn manifest(workspace: &Path, profile: &str, installed: &[InstalledSkill]) -> Result<()> {
    let path = workspace.join(MANIFEST);
    let parent = path.parent().context("Manifest has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("Cannot create {}", parent.display()))?;
    let mut all = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(|value| value.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    all[profile] = serde_json::to_value(installed)?;
    fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&all)?))
        .with_context(|| format!("Cannot write {}", path.display()))?;
    Ok(())
}

/// Re-check on disk that a report's promises hold: every path has a non-empty entry file.
pub fn verify(report: &InstallReport) -> Result<()> {
    if !report.ok {
        bail!(
            "Install of {} failed: {}",
            report.profile,
            report.error.as_deref().unwrap_or("unknown error")
        );
    }
    ensure!(
        report.installed.is_empty() == report.activation.is_empty(),
        "Profile {} reported {} skills with {} activation text",
        report.profile,
        report.installed.len(),
        if report.activation.is_empty() {
            "no"
        } else {
            "some"
        }
    );
    for skill in &report.installed {
        ensure!(
            skill.path.is_dir(),
            "Installed skill {} is missing at {}",
            skill.id,
            skill.path.display()
        );
        let entry = skill.path.join(ENTRY);
        let size = fs::metadata(&entry)
            .with_context(|| format!("Missing {}", entry.display()))?
            .len();
        ensure!(size > 0, "Empty {}", entry.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unvrs-install-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn fake(root: &Path, id: &str) -> SkillEntry {
        let dir = root.join(id);
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        fs::write(
            dir.join(ENTRY),
            format!("---\nname: {id}\ndescription: Fake {id}.\n---\n\nbody\n"),
        )
        .unwrap();
        fs::write(dir.join("references/notes.md"), "notes").unwrap();
        SkillEntry {
            pool: "local".into(),
            id: id.into(),
            summary: format!("Fake {id}."),
            needs_captain: false,
            dir,
        }
    }

    #[test]
    fn every_profile_lands_in_its_native_root() {
        let root = workspace("matrix");
        let pool = root.join("pool");
        let skills = [fake(&pool, "alpha"), fake(&pool, "beta")];
        for (profile, expected) in [
            ("pi", ".pi/skills"),
            ("codex", ".agents/skills"),
            ("claude", ".claude/skills"),
            ("cursor", ".cursor/skills"),
        ] {
            let ws = root.join(profile);
            let report = install(profile, &skills, &ws);
            verify(&report).unwrap();
            assert_eq!(report.installed.len(), 2, "{profile}");
            assert_eq!(report.installed[0].mode, "native");
            assert_eq!(report.installed[0].path, ws.join(expected).join("alpha"));
            // Resources travel; .git does not.
            assert!(
                ws.join(expected)
                    .join("alpha/references/notes.md")
                    .is_file()
            );
            assert!(!ws.join(expected).join("alpha/.git").exists());
            assert!(report.activation.contains("alpha"));
            assert!(report.activation.contains("Fake beta."));
            let text = fs::read_to_string(ws.join(MANIFEST)).unwrap();
            assert!(text.contains(profile) && text.contains("beta"), "{text}");
        }
    }

    #[test]
    fn reinstall_is_idempotent_and_leaves_siblings() {
        let root = workspace("idempotent");
        let pool = root.join("pool");
        let alpha = fake(&pool, "alpha");
        let beta = fake(&pool, "beta");
        let ws = root.join("ws");
        assert!(install("claude", &[alpha.clone(), beta.clone()], &ws).ok);
        let stale = ws.join(".claude/skills/alpha/stale.md");
        fs::write(&stale, "leftover").unwrap();
        let again = install("claude", &[alpha], &ws);
        verify(&again).unwrap();
        assert_eq!(again.installed.len(), 1);
        assert!(!stale.exists(), "stale resource survived reinstall");
        assert!(ws.join(".claude/skills/beta").is_dir(), "sibling removed");
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(ws.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(manifest["claude"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unknown_profile_and_missing_entry_fail_softly() {
        let root = workspace("refusals");
        let pool = root.join("pool");
        let good = fake(&pool, "alpha");
        let ws = root.join("ws");
        let bogus = install("bogus", std::slice::from_ref(&good), &ws);
        assert!(!bogus.ok && bogus.error.unwrap().contains("bogus"));
        assert!(verify(&install("bogus", &[good], &ws)).is_err());
        assert!(native_root("bogus", &ws).is_none());

        let empty = pool.join("hollow");
        fs::create_dir_all(&empty).unwrap();
        let hollow = SkillEntry {
            pool: "local".into(),
            id: "hollow".into(),
            summary: String::new(),
            needs_captain: false,
            dir: empty,
        };
        let report = install("pi", &[hollow], &ws);
        assert!(!report.ok);
        assert!(report.error.unwrap().contains("SKILL.md"));
        assert!(!ws.join(".pi/skills/hollow").exists());
    }

    #[test]
    fn escaping_ids_and_home_are_refused() {
        let root = workspace("safety");
        let pool = root.join("pool");
        let mut escape = fake(&pool, "alpha");
        escape.id = "../outside".into();
        let report = install("codex", &[escape], &root.join("ws"));
        assert!(!report.ok && report.error.unwrap().contains("Unsafe skill id"));
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            let home = install("codex", &[fake(&pool, "beta")], Path::new(&home));
            assert!(!home.ok);
            assert!(home.error.unwrap().contains("home directory"));
        }
    }

    #[test]
    fn empty_plan_is_a_clean_pass() {
        let ws = workspace("empty");
        let report = install("cursor", &[], &ws);
        verify(&report).unwrap();
        assert!(report.ok && report.installed.is_empty() && report.activation.is_empty());
        assert!(
            !ws.join(MANIFEST).exists(),
            "nothing installed, no manifest"
        );
        assert!(!ws.join(".cursor").exists());
    }

    #[test]
    fn prompt_mode_copies_under_unvrs() {
        // No profile needs prompt mode today; keep the fallback honest by driving it directly.
        let root = workspace("prompt");
        let skills = [fake(&root.join("pool"), "alpha")];
        let ws = root.join("ws");
        fs::create_dir_all(ws.join(".unvrs/skills")).unwrap();
        let target = ws.join(".unvrs/skills/alpha");
        copy_tree(&skills[0].dir, &target).unwrap();
        let installed = [InstalledSkill {
            id: "alpha".into(),
            path: target.clone(),
            mode: "prompt".into(),
        }];
        let text = activation("nova", "prompt", &skills, &installed, &ws);
        assert!(text.contains(".unvrs/skills/alpha/SKILL.md"), "{text}");
        assert!(text.starts_with("UNVRS skills for nova (prompt, 1 total)"));
        verify(&InstallReport {
            profile: "nova".into(),
            ok: true,
            installed: installed.to_vec(),
            activation: text,
            error: None,
        })
        .unwrap();
    }
}
