//! `unvrs mission` — captain-side mission admit/inspect over uKe's MissionRegistry.
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};
use uke::{Mission, MissionRegistry, scalar};

const DEFAULT_REGISTRY: &str = ".unvrs/missions.json";
const USAGE: &str = "usage: unvrs mission check <file> | admit <file> | list | show <id> [--registry <path>] [--json]";

/// Pull `--registry <path>` and `--json` out of the argument list.
fn flags(args: &[String]) -> Result<(PathBuf, bool, Vec<String>)> {
    let (mut registry, mut json_out, mut rest) = (PathBuf::from(DEFAULT_REGISTRY), false, vec![]);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--registry" => {
                registry = args
                    .next()
                    .context("usage: --registry needs a path")?
                    .into()
            }
            "--json" => json_out = true,
            flag if flag.starts_with("--") => bail!("{USAGE}"),
            positional => rest.push(positional.to_string()),
        }
    }
    Ok((registry, json_out, rest))
}
fn print_mission(mission: &Mission, json_out: bool) {
    if json_out {
        println!(
            "{}",
            json!({
                "mission": mission.id,
                "status": mission.status,
                "investment": mission.investment,
                "owner_pid": mission.owner_pid,
                "outcome": mission.outcome,
                "done_check": mission.done_check,
                "scope": mission.scope,
                "prompt": mission.prompt,
                "pointers": mission.pointers,
                "skills": mission.skills,
                "path": mission.path,
            })
        );
        return;
    }
    println!(
        "mission: {}\nstatus: {}\ninvestment: {}\nowner_pid: {}\npath: {}\noutcome: {}\ndone_check: {}\nscope: {}\nprompt: {}\npointers[{}]: {}\nskills[{}]: {}",
        mission.id,
        mission.status,
        mission.investment,
        mission
            .owner_pid
            .map_or("null".to_string(), |pid| pid.to_string()),
        scalar(&mission.path.display().to_string()),
        scalar(&mission.outcome),
        scalar(&mission.done_check),
        scalar(&mission.scope),
        scalar(&mission.prompt),
        mission.pointers.len(),
        mission.pointers.join(","),
        mission.skills.len(),
        mission.skills.join(","),
    );
}

pub fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}\nfloor: Outcome and Done check must be real text before admit");
        return Ok(());
    }
    let (registry_path, json_out, rest) = flags(args)?;
    match rest.first().map(String::as_str) {
        Some("check") if rest.len() == 2 => {
            let mission = Mission::load(Path::new(&rest[1]))?;
            mission.admit_check()?;
            println!("check: pass");
            print_mission(&mission, json_out);
        }
        Some("admit") if rest.len() == 2 => {
            let mut registry = MissionRegistry::open(&registry_path)?;
            let entry = registry.admit(Path::new(&rest[1]))?;
            let mission = Mission::load(&entry.path)?;
            if json_out {
                println!("{}", json!({"admitted": mission.id, "entry": entry}));
            } else {
                println!(
                    "admitted: {}\nstatus: {}\nupdated: {}\nregistry: {}\nbrief:\n{}",
                    mission.id,
                    entry.status,
                    entry.updated,
                    scalar(&registry_path.display().to_string()),
                    mission.brief()
                );
            }
        }
        Some("list") if rest.len() == 1 => {
            let registry = MissionRegistry::open(&registry_path)?;
            let rows = registry.list();
            if json_out {
                println!(
                    "{}",
                    json!(
                        rows.iter()
                            .map(|(id, e)| json!({"id": id, "status": e.status, "owner_pid": e.owner_pid, "updated": e.updated, "path": e.path}))
                            .collect::<Vec<_>>()
                    )
                );
                return Ok(());
            }
            println!("missions[{}]{{id,status,owner_pid,path}}:", rows.len());
            for (id, entry) in rows {
                println!(
                    "  {id},{},{},{}",
                    entry.status,
                    entry
                        .owner_pid
                        .map_or("null".to_string(), |pid| pid.to_string()),
                    scalar(&entry.path.display().to_string())
                );
            }
        }
        Some("show") if rest.len() == 2 => {
            let registry = MissionRegistry::open(&registry_path)?;
            let entry = registry
                .get(&rest[1])
                .with_context(|| format!("unknown mission {}", rest[1]))?;
            print_mission(&Mission::load(&entry.path)?, json_out);
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
