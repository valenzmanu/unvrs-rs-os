mod cmd_boot;
mod cmd_deploy;
mod cmd_install;
mod cmd_kernel;
mod cmd_mission;
mod cmd_setup;

use anyhow::{Result, bail, ensure};
use drv_agent as seat;
use drv_intf as bridge;
use std::{env, io::IsTerminal, path::Path};
mod cmd_pool;

use uke::scalar;

// M6: temporary wiring for the install matrix; lead reconciles the CLI surface.
mod cmd_agent;

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let full = args.last().is_some_and(|a| a == "--full");
    if full {
        args.pop();
    }
    if let Err(e) = run(args) {
        let message = format!("{e:#}");
        let code = if message.starts_with("usage:") { 2 } else { 1 };
        let chars = message.chars().count();
        let display = if !full && chars > 512 {
            format!(
                "{}… ({chars} chars; append --full for complete error)",
                message.chars().take(512).collect::<String>()
            )
        } else {
            message.clone()
        };
        eprintln!(
            "error:\n  code: {code}\n  message: {}\nhelp[1]:\n  unvrs --help",
            scalar(&display)
        );
        std::process::exit(code);
    }
}
fn help(command: &str) {
    match command {
        "ctl" => println!(
            "usage: unvrs ctl <command>   (the same commands as the MCP tool `unvrs`)\nseats: help, whoami, ctx sources|map <source> [path]|search \"words\" [--source s] [--k n]|get <ref> [--lines a-b],\n  remember [--perishable] [--until words] <fact>, stow \"tier | fact; …\", recall <words>, decide \"question\" --option a --option b,\n  project list|propose <id> <purpose> [--source s], task --intent \"…\" --spec \"…\" --go \"<captain go>\" --done-when \"<one line>\" [--project <id> for L1] [--shape report] [--authority report|implement (default implement)] [--source s] [--kind implement|review|validate|research|design|decide] [--judgment low|high] [--thoroughness low|high] [--on|--harness claude|codex] [--model <id>] [--effort low|medium|high|xhigh|max],\n  stop|cancel <pid> [reason], send <pid|l1|project> <text>, digest, tree, hot\ncaptain's terminal only: approve <id>, answer <id> <words>, forget <id>, away <words> [--cap n]|off, project new <id> <purpose> [--source s],\n  sources add <id> <path|git> <uri> [--purpose words] [--exclude glob]…, seed import <dir>\nidentity: derived from the process tree and the harness thread (CODEX_THREAD_ID / CLAUDE_CODE_SESSION_ID); models cannot run captain-only ops"
        ),
        "observe" => println!(
            "usage: unvrs observe [--short-url [off]]\nthe kernel daemon serves the Observatory at http://unvrs.localhost:7576 (loopback, Host guard; settings writes require page authorization)\n--short-url: one sudo adds http://unvrs.localhost for every browser (hosts entry + loopback 80 → 7576)"
        ),
        "kernel" => println!(
            "usage: unvrs kernel status [--json]|start|stop|run|hot --pid <n>|snapshot\nhome: UNVRS_HOME (default ~/.unvrs): kernel/ (socket, state, journal), captain/, projects/<id>/, sources.toml\nthe LaunchAgent keeps `unvrs kernel run` up after `unvrs install`; hooks start it on demand"
        ),
        "mission" => println!(
            "usage: unvrs mission check|admit <mission.md> | list | show <id>\noptional: --registry <json> --json"
        ),
        "econ" => println!(
            "usage: unvrs econ show | explain <pid> | route --dry-run [--kind implement|review|validate|research|design|decide] [--judgment low|high] [--thoroughness low|high] [--on|--harness claude|codex] [--model <id>] [--effort low|medium|high|xhigh|max]\nReads kernel catalog evidence; dry-run starts no task."
        ),
        "settings" => println!("{}", uke::settings::USAGE),
        "boot" => cmd_boot::help(),
        "pool" => println!("usage: unvrs pool list|resolve [--config <toml>] [--json]"),
        _ => println!(
            "UNVRS {}\ncommands:\n  unvrs setup [--seed <dir> | --no-seed] [--trust-hooks] (the captain's one command: install, PATH, kernel, seed, Codex hook trust after your yes, doctor)\n  unvrs install [--no-launchagent] | uninstall | doctor | upgrade [--from <binary>] | rollback\n  unvrs deploy <branch|commit> [--repo <dir>] [--no-test] [--drain-secs n] [--force-requeue] [--allow-drop] [--dry-run] | versions [--json]\n  unvrs kernel status|start|stop|snapshot\n  unvrs econ show|explain <pid>|route --dry-run [task routing flags]\n  unvrs settings get|set <key> <json-value> [--confirm]|history|revert <change-id> [--confirm]\n  unvrs observe [--short-url]\n  unvrs sources add <id> <path|git> <uri> [--purpose words] [--exclude glob]…\n  unvrs seed import <dir> (the captain's seed: sources.toml, projects.toml, captain.md, projects/<id>/memory.md; re-run to update)\n  unvrs ctl <command> (seats and scripts; `unvrs ctl help`)\n  unvrs mcp (the MCP server the plugin declares: one tool, unvrs(command))\n  unvrs hook <Event> --harness claude|codex (the plugin's hooks)\n  unvrs --version\nin T3 Code and the Codex app: $unvrs:l1, $unvrs:l2 [project], $unvrs:digest, $unvrs:observe, $unvrs:answer, $unvrs:approve,\n  $unvrs:remember, $unvrs:forget, $unvrs:project, $unvrs:away, $unvrs:detach, $unvrs:tree\nlegacy (frozen): unvrs profiles | boot | mission | pool | agent | --demo",
            env!("CARGO_PKG_VERSION")
        ),
    }
}
pub fn available(executable: &Path) -> bool {
    if executable.components().count() > 1 {
        executable.is_file()
    } else {
        env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .any(|p| p.join(executable).is_file())
    }
}
fn doctor() {
    let profiles = seat::profiles();
    let missing = profiles
        .iter()
        .filter(|profile| !available(Path::new(&profile.executable)))
        .count();
    println!("profiles[4]{{id,command,available}}:");
    for profile in &profiles {
        let command = std::iter::once(profile.executable.as_str())
            .chain(profile.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  {},{},{}",
            profile.id,
            scalar(&command),
            available(Path::new(&profile.executable))
        );
    }
    println!(
        "missing: {missing}\nauthentication: unchecked\nhelp[2]:\n  npm install -g pi-acp @agentclientprotocol/codex-acp @agentclientprotocol/claude-agent-acp\n  https://cursor.com/docs/cli/installation"
    );
}

fn run(args: Vec<String>) -> Result<()> {
    // `agent` owns its own flags and help; keep it ahead of the shared help walker.
    if args.first().is_some_and(|a| a == "agent") {
        return cmd_agent::run(&args[1..]);
    }
    // Plugin hooks never fail the harness: handled before anything that can error.
    if args.first().is_some_and(|a| a == "hook") {
        cmd_kernel::hook(args[1..].to_vec());
    }
    let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
    match args.first().map(String::as_str) {
        Some("mcp") => return cmd_kernel::mcp(),
        Some("kernel") if !wants_help => return cmd_kernel::kernel(args[1..].to_vec()),
        Some("observe") if !wants_help => return cmd_kernel::observe(args[1..].to_vec()),
        Some("ctl") if args.len() > 1 && !wants_help => return cmd_kernel::ctl(&args[1..]),
        Some("econ") if !wants_help => return cmd_kernel::ctl(&args),
        Some("settings") if !wants_help => return cmd_kernel::settings(&args[1..]),
        Some("sources") if args.len() > 1 && !wants_help => {
            return cmd_kernel::sources(&args[1..]);
        }
        Some("seed") if args.len() > 1 && !wants_help => return cmd_kernel::seed(&args[1..]),
        Some("setup") => return cmd_setup::run(&args[1..]),
        _ => {}
    }
    // Deploy loop: deploy, versions, and rollback through deploy/live.json.
    if let Some(done) = cmd_deploy::dispatch(&args) {
        return done;
    }
    // 0.8 install surface (D61): install, uninstall, doctor, upgrade, rollback, plugin render.
    if let Some(done) = cmd_install::dispatch(&args) {
        return done;
    }
    if args.last().is_some_and(|a| a == "--help" || a == "-h") {
        let path: Vec<&str> = args[..args.len() - 1].iter().map(String::as_str).collect();
        ensure!(
            matches!(
                path.as_slice(),
                [] | ["ctl"
                    | "econ"
                    | "settings"
                    | "observe"
                    | "kernel"
                    | "boot"
                    | "mission"
                    | "pool"
                    | "handoff-smoke"]
            ),
            "usage: unknown help path"
        );
        help(path.last().copied().unwrap_or(""));
        return Ok(());
    }
    match args.first().map(String::as_str) {
        Some("--version") if args.len() == 1 => {
            println!("version: {}", env!("CARGO_PKG_VERSION"));
            println!(
                "commit: {}",
                uke::BUILD_COMMIT.unwrap_or("none (not built by unvrs deploy)")
            );
            println!("ref: {}", uke::BUILD_REF.unwrap_or("none"));
        }
        Some("profiles") if args.len() == 1 => doctor(),
        Some("seat") if args.len() == 1 => seat::run()?,
        Some("boot") => cmd_boot::run(&args[1..])?,
        Some("mission") => cmd_mission::run(&args[1..])?,
        Some("pool") => cmd_pool::run(&args[1..])?,
        Some("handoff-smoke") if args.len() == 1 => bridge::handoff_smoke()?,
        Some("ctl") => cmd_kernel::ctl(&["help".to_owned()])?,
        None if !std::io::stdin().is_terminal() => doctor(),
        _ if args
            .iter()
            .all(|arg| matches!(arg.as_str(), "--demo" | "--fresh")) =>
        {
            let has = |flag: &str| args.iter().any(|arg| arg == flag);
            bridge::run(has("--demo"), has("--fresh"))?
        }
        _ => bail!("usage: unknown argument; run unvrs --help"),
    }
    Ok(())
}
