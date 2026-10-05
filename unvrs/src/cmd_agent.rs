//! `unvrs agent install`: drive the DrvAgent install matrix from the shell (M6 evidence surface).
use anyhow::{Context, Result, bail, ensure};
use drv_agent as install;
use drv_agent::InstallReport;
use std::path::{Path, PathBuf};
use uke::{SkillEntry, scalar};

const PROFILES: [&str; 4] = ["pi", "codex", "claude", "cursor"];

/// `install --profile <p|all> --skill-dir <dir> [...] --workspace <dir> [--json]`
pub fn run(args: &[String]) -> Result<()> {
    if args.first().is_none_or(|a| a == "--help" || a == "-h") {
        println!(
            "usage: unvrs agent install --profile <pi|codex|claude|cursor|all> --skill-dir <dir> [--skill-dir <dir>] --workspace <dir> [--json]\nresult: one install row per profile; harness-native skill dir or prompt activation\nnative: pi .pi/skills · codex .agents/skills · claude .claude/skills · cursor .cursor/skills\nmanifest: <workspace>/.unvrs/installed-skills.json\nhelp[1]:\n  unvrs --help"
        );
        return Ok(());
    }
    ensure!(args[0] == "install", "usage: unvrs agent install --help");
    let mut profile = String::new();
    let mut workspace = PathBuf::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut json = false;
    let mut rest = &args[1..];
    while let Some(flag) = rest.first() {
        let value = |name: &str| -> Result<&String> {
            rest.get(1)
                .with_context(|| format!("usage: {name} needs a value"))
        };
        match flag.as_str() {
            "--json" => {
                json = true;
                rest = &rest[1..];
                continue;
            }
            "--profile" => profile = value("--profile")?.clone(),
            "--workspace" => workspace = PathBuf::from(value("--workspace")?),
            "--skill-dir" => dirs.push(PathBuf::from(value("--skill-dir")?)),
            other => bail!("usage: unknown flag {other}; unvrs agent install --help"),
        }
        rest = &rest[2..];
    }
    ensure!(!profile.is_empty(), "usage: --profile is required");
    ensure!(
        !workspace.as_os_str().is_empty(),
        "usage: --workspace is required"
    );
    let profiles: Vec<&str> = if profile == "all" {
        PROFILES.to_vec()
    } else {
        vec![profile.as_str()]
    };
    let skills = dirs
        .iter()
        .map(|dir| entry(dir))
        .collect::<Result<Vec<_>>>()?;
    let reports: Vec<InstallReport> = profiles
        .iter()
        .map(|p| install::install(p, &skills, &workspace))
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print(&reports);
    }
    let failed = reports.iter().filter(|r| !r.ok).count();
    ensure!(failed == 0, "{failed} of {} installs failed", reports.len());
    for report in &reports {
        install::verify(report)?;
    }
    Ok(())
}

/// Build a catalogue entry from a skill directory: id is the directory name, summary its
/// frontmatter `description` when one is cheaply available.
fn entry(dir: &Path) -> Result<SkillEntry> {
    let id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("usage: skill dir has no name: {}", dir.display()))?
        .to_owned();
    let text = std::fs::read_to_string(dir.join(install::ENTRY)).unwrap_or_default();
    Ok(SkillEntry {
        pool: "local".into(),
        id,
        summary: description(&text),
        needs_captain: false,
        dir: dir.to_path_buf(),
    })
}
fn description(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    lines
        .take_while(|line| line.trim() != "---")
        .find_map(|line| line.trim().strip_prefix("description:"))
        .map(|value| value.trim().trim_matches(['"', '\''].as_slice()).to_owned())
        .unwrap_or_default()
}
fn print(reports: &[InstallReport]) {
    println!("install[{}]{{profile,ok,mode,paths}}:", reports.len());
    for report in reports {
        let mode = report
            .installed
            .first()
            .map(|s| s.mode.as_str())
            .unwrap_or("none");
        let paths = report
            .installed
            .iter()
            .map(|s| s.path.display().to_string())
            .collect::<Vec<_>>()
            .join(";");
        println!(
            "  {},{},{mode},{}",
            report.profile,
            report.ok,
            scalar(&paths)
        );
        if let Some(error) = &report.error {
            println!("    error: {}", scalar(error));
        }
    }
    println!("help[1]:\n  unvrs agent install --help");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontmatter_description_is_optional() {
        assert_eq!(
            description("---\nname: a\ndescription: \"Does a thing.\"\n---\nbody"),
            "Does a thing."
        );
        assert_eq!(description("# no frontmatter\n"), "");
        assert_eq!(description("---\nname: a\n---\n"), "");
    }
    #[test]
    fn install_requires_profile_and_workspace() {
        for args in [
            vec!["install"],
            vec!["install", "--profile", "claude"],
            vec!["install", "--workspace", "/tmp"],
            vec!["install", "--profile"],
            vec!["install", "--bogus"],
            vec!["wat"],
        ] {
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            assert!(run(&args).is_err(), "{args:?}");
        }
    }
}
