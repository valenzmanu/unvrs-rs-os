//! `unvrs boot`: admit a mission, nest a seat (DrvBoot), install the plan (DrvAgent), then report
//! ready or refused. Every decision lands in the Obs journal.
use anyhow::{Context, Result, bail, ensure};
use drv_agent as install;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use uke as boot;
use uke as pool;
use uke::{Backend, BootConfig, JevHttp, Mission, MissionRegistry, Seat, Status, scalar};

const PROFILES: [&str; 4] = ["pi", "codex", "claude", "cursor"];

pub fn help() {
    println!(
        "usage: unvrs boot <mission.md> [--profile pi|codex|claude|cursor|all] [--picker rules|jev] [--seat l1|l2|l2-focused|l3]\noptional: --workspace <dir> --pools <toml> --registry <json> --config <boot.toml> --journal <jsonl> --require-harness --plan-only (stop after the nest plan; nothing installed) --json\npath: admit (outcome + done check) → Boot nest plan (≤3 skills; interactive skills need a captain channel) → Agent install → ready | refused\ndefaults: profile pi; seat l2; picker from boot config (rules); workspace .; state under <workspace>/.unvrs/\njev: TYPESAFE_API_KEY from environment or .env; missing key or error falls back to rules unless on_error = \"refuse\"\nhelp[2]:\n  unvrs mission --help\n  unvrs pool --help"
    );
}

struct Options {
    mission: PathBuf,
    profiles: Vec<String>,
    picker: Option<Backend>,
    seat: Seat,
    workspace: PathBuf,
    pools: Option<PathBuf>,
    registry: Option<PathBuf>,
    config: Option<PathBuf>,
    journal: Option<PathBuf>,
    require_harness: bool,
    plan_only: bool,
    json: bool,
}

fn options(args: &[String]) -> Result<Options> {
    let mut o = Options {
        mission: PathBuf::new(),
        profiles: vec!["pi".into()],
        picker: None,
        seat: Seat::parse("l2")?,
        workspace: PathBuf::from("."),
        pools: None,
        registry: None,
        config: None,
        journal: None,
        require_harness: false,
        plan_only: false,
        json: false,
    };
    let mut mission = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .with_context(|| format!("usage: {arg} needs a value"))
        };
        match arg.as_str() {
            "--profile" => {
                let profile = value()?;
                o.profiles = if profile == "all" {
                    PROFILES.iter().map(|p| p.to_string()).collect()
                } else {
                    ensure!(
                        PROFILES.contains(&profile.as_str()),
                        "usage: profile must be pi|codex|claude|cursor|all"
                    );
                    vec![profile.clone()]
                };
            }
            "--picker" => o.picker = Some(Backend::parse(value()?)?),
            "--seat" => o.seat = Seat::parse(value()?)?,
            "--workspace" => o.workspace = value()?.into(),
            "--pools" => o.pools = Some(value()?.into()),
            "--registry" => o.registry = Some(value()?.into()),
            "--config" => o.config = Some(value()?.into()),
            "--journal" => o.journal = Some(value()?.into()),
            "--require-harness" => o.require_harness = true,
            "--plan-only" => o.plan_only = true,
            "--json" => o.json = true,
            flag if flag.starts_with("--") => {
                bail!("usage: unknown flag {flag}; run unvrs boot --help")
            }
            _ => ensure!(
                mission.replace(PathBuf::from(arg)).is_none(),
                "usage: one mission file per boot"
            ),
        }
    }
    o.mission = mission.context("usage: unvrs boot <mission.md>; run unvrs boot --help")?;
    Ok(o)
}

pub fn run(args: &[String]) -> Result<()> {
    let o = options(args)?;
    std::fs::create_dir_all(&o.workspace)?;
    let state = o.workspace.join(".unvrs");
    let journal = o
        .journal
        .clone()
        .unwrap_or(state.join("boot-journal.jsonl"));
    let log = |kind: &str, fields: Value| {
        uke::append_journal(&journal, kind, &fields).context("Obs journal write failed")
    };

    // 1 · admit
    let mut registry =
        MissionRegistry::open(&o.registry.clone().unwrap_or(state.join("missions.json")))?;
    let mission = match Mission::load(&o.mission).and_then(|mission| {
        registry.admit(&o.mission)?;
        Ok(mission)
    }) {
        Ok(mission) => mission,
        Err(e) => {
            let reason = format!("{e:#}");
            log(
                "boot.refused",
                json!({"mission": o.mission, "stage": "admit", "reason": reason}),
            )?;
            return refuse(&o, None, "admit", &reason);
        }
    };
    log(
        "mission.admitted",
        json!({"mission": mission.id, "path": o.mission}),
    )?;

    // 2 · nest plan
    let mut config = BootConfig::load(&o.config.clone().unwrap_or(state.join("boot.toml")))?;
    if let Some(picker) = o.picker {
        config.backend = picker;
    }
    let catalog = pool::catalog(
        &crate::cmd_pool::config_path(o.pools.as_deref())?,
        &pool::default_cache_dir()?,
    )?;
    let jev = (config.backend == Backend::Jev)
        .then(|| JevHttp::discover(&std::env::current_dir().unwrap_or_default()))
        .flatten();
    let (plan, record) = match boot::nest(
        &mission.brief(),
        &mission.skills,
        &catalog,
        o.seat,
        &config,
        jev,
    ) {
        Ok(nested) => nested,
        Err(e) => {
            let reason = format!("{e:#}");
            log(
                "boot.refused",
                json!({"mission": mission.id, "stage": "pick", "reason": reason}),
            )?;
            registry.set_status(&mission.id, Status::Refused, None)?;
            return refuse(&o, Some(&mission.id), "pick", &reason);
        }
    };
    let mut pick = serde_json::to_value(&record)?;
    pick["mission"] = json!(mission.id);
    log("boot.pick", pick.clone())?;

    if o.plan_only {
        if o.json {
            println!(
                "{}",
                json!({"mission": mission.id, "state": "planned", "pick": pick, "journal": journal})
            );
        } else {
            summary(&o, &mission.id, &record);
            println!("journal: {}\nstate: planned", journal.display());
        }
        return Ok(());
    }

    // 3 · install per profile, then ready or refuse
    let mut reports = Vec::new();
    let mut failures = Vec::new();
    for profile in &o.profiles {
        install::install_memory_skills(profile, &o.workspace)?;
        let report = install::install(profile, &plan.skills, &o.workspace);
        let verified = install::verify(&report).map_err(|e| format!("{e:#}"));
        let harness = harness_available(profile);
        let error = report
            .error
            .clone()
            .or(verified.err())
            .or((o.require_harness && !harness)
                .then(|| format!("{profile} harness executable not found")));
        let ok = report.ok && error.is_none();
        let mut row = serde_json::to_value(&report)?;
        row["mission"] = json!(mission.id);
        row["ok"] = json!(ok);
        row["error"] = json!(error);
        row["harness_available"] = json!(harness);
        log("agent.install", row.clone())?;
        if let Some(error) = error {
            failures.push(format!("{profile}: {error}"));
        }
        reports.push(row);
    }
    let ready = failures.is_empty();
    let reason = failures.join("; ");
    registry.set_status(
        &mission.id,
        if ready {
            Status::Active
        } else {
            Status::Refused
        },
        None,
    )?;
    log(
        if ready { "boot.ready" } else { "boot.refused" },
        json!({"mission": mission.id, "stage": "install", "profiles": o.profiles, "selected": record.selected, "reason": reason}),
    )?;

    if o.json {
        println!(
            "{}",
            json!({"mission": mission.id, "state": if ready {"ready"} else {"refused"}, "reason": reason,
                "pick": pick, "install": reports, "journal": journal})
        );
    } else {
        summary(&o, &mission.id, &record);
        println!(
            "install[{}]{{profile,ok,harness_available,paths}}:",
            reports.len()
        );
        for row in &reports {
            let paths: Vec<&str> = row["installed"]
                .as_array()
                .map(|a| a.iter().filter_map(|s| s["path"].as_str()).collect())
                .unwrap_or_default();
            println!(
                "  {},{},{},{}",
                row["profile"].as_str().unwrap_or("?"),
                row["ok"],
                row["harness_available"],
                scalar(&paths.join(" "))
            );
        }
        println!("journal: {}", journal.display());
        if ready {
            println!("state: ready");
        }
    }
    if ready {
        Ok(())
    } else {
        refuse(&o, Some(&mission.id), "install", &reason)
    }
}

fn summary(o: &Options, mission: &str, record: &boot::BootRecord) {
    println!(
        "mission: {}\nseat: L{}{}",
        mission,
        o.seat.rank,
        if record.captain_channel {
            " (captain channel)"
        } else {
            ""
        }
    );
    println!(
        "picker: {}{}",
        record.backend_used.as_str(),
        record
            .fallback
            .as_ref()
            .map(|f| format!(
                " (fallback from {}: {})",
                record.backend_requested.as_str(),
                scalar(f)
            ))
            .unwrap_or_default()
    );
    println!(
        "candidates: {} of {} (stripped needs_captain: {})",
        record.candidates,
        record.catalog,
        record.stripped.len()
    );
    let number = |n: Option<f64>| n.map_or("n/a".into(), |n| format!("{n:.2}"));
    println!(
        "noul: {}\nconfidence: {}\nnote: {}",
        number(record.noul),
        number(record.confidence),
        scalar(&record.note)
    );
    println!("selected[{}]:", record.selected.len());
    for id in &record.selected {
        println!("  {id}");
    }
}

/// Refusal is visible on stdout (for the captain) and fails the process (for scripts).
fn refuse(o: &Options, mission: Option<&str>, stage: &str, reason: &str) -> Result<()> {
    if !o.json {
        println!(
            "mission: {}\nstate: refused\nstage: {stage}\nreason: {}",
            mission.unwrap_or("unadmitted"),
            scalar(reason)
        );
    } else if stage != "install" {
        println!(
            "{}",
            json!({"mission": mission, "state": "refused", "stage": stage, "reason": reason})
        );
    }
    bail!("Boot refused at {stage}: {reason}")
}

fn harness_available(profile: &str) -> bool {
    drv_agent::profiles()
        .iter()
        .find(|p| p.id == profile)
        .is_some_and(|p| crate::available(Path::new(&p.executable)))
}
