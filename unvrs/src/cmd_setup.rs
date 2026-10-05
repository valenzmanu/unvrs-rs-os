//! `unvrs setup`: the captain's one command (D61, point 6 as amended). From the
//! captain's own terminal only; every step is idempotent, so a re-run is a quick no-op.
//!
//! 1. install or upgrade: binary into `versions/<v>` + `bin/unvrs`, the plugin for Claude
//!    Code and Codex, the LaunchAgent (`cmd_install::install_with`);
//! 2. `unvrs` on PATH: a symlink in `~/.local/bin` when that directory is on PATH, else the
//!    one line for the shell rc (appended only after a yes on a terminal);
//! 3. the kernel started;
//! 4. the captain's seed imported (`~/context/_seed`, `--seed DIR`, `--no-seed`);
//! 5. Codex hook trust, **only with the captain's consent**: the UNVRS hooks are listed and
//!    the captain answers a yes/no prompt in this terminal (or passes `--trust-hooks`); the
//!    trust is written by Codex's own app server (`hooks/list` → `config/batchWrite` of
//!    `hooks.state`), for our plugin's hooks that run our binary only, after a backup of
//!    `config.toml`. No terminal and no flag: skipped, with the manual step printed;
//! 6. doctor, then how to start.
//!
//! Test knobs (never needed by the captain): `UNVRS_LINK_DIR` (default `~/.local/bin`),
//! `UNVRS_SHELL_RC` (default `~/.zshrc`, `~/.bash_profile` under bash), `UNVRS_SEED`.
use crate::cmd_install;
use anyhow::{Context, Result, bail, ensure};
use drv_agent::{Homes, plugin_id};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const USAGE: &str = "usage: unvrs setup [--seed <dir> | --no-seed] [--trust-hooks] [--no-launchagent]\n  the captain's one command, from Terminal: install or upgrade, unvrs on PATH, kernel, seed import\n  (default ~/context/_seed when present), Codex hook trust after your yes (or --trust-hooks), doctor.\n  Safe to re-run. Undo: unvrs uninstall";

struct Opts {
    seed: Option<PathBuf>,
    no_seed: bool,
    trust: bool,
    agent: bool,
}

fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts {
        seed: None,
        no_seed: false,
        trust: false,
        agent: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seed" => o.seed = Some(PathBuf::from(it.next().context(USAGE)?)),
            "--no-seed" => o.no_seed = true,
            "--trust-hooks" => o.trust = true,
            "--no-launchagent" => o.agent = false,
            _ => bail!("{USAGE}"),
        }
    }
    ensure!(!(o.no_seed && o.seed.is_some()), "{USAGE}");
    Ok(o)
}

fn say(s: &str) {
    println!("\n==> {s}");
}

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("HOME unset")
}

/// `$HOME/…` for display and for the rc line.
fn tilde(p: &Path) -> String {
    match home()
        .ok()
        .and_then(|h| p.strip_prefix(&h).ok().map(Path::to_path_buf))
    {
        Some(rest) => format!("$HOME/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// A yes/no question on the captain's terminal; Enter means yes.
fn ask(q: &str) -> bool {
    print!("{q} [Y/n] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        println!();
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// Captain steps are refused from a model: a harness thread's env or a harness among
/// the process's ancestors (the same rule the kernel applies to `unvrs ctl`).
fn refuse_model() -> Result<()> {
    let env_model = ["CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CLAUDECODE"]
        .iter()
        .any(|k| env::var_os(k).is_some_and(|v| !v.is_empty()));
    if env_model || uke::procinfo::under_harness(std::process::id()) {
        bail!(
            "Refused: `unvrs setup` is the captain's step. Run it from Terminal (or iTerm), not inside a Claude, Codex or T3 Code thread or their built-in terminals: UNVRS refuses captain steps from a model."
        );
    }
    Ok(())
}

fn run_bin(h: &Homes, args: &[&str]) -> Result<String> {
    let out = Command::new(h.bin())
        .args(args)
        .env("UNVRS_HOME", &h.unvrs)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {} {}", h.bin().display(), args.join(" ")))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    ensure!(
        out.status.success(),
        "unvrs {} failed: {}",
        args.join(" "),
        text.trim()
    );
    Ok(text)
}

// ───────────────────────────── PATH ─────────────────────────────

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

fn on_path(dir: &Path) -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .any(|d| d == dir || same_file(&d, dir))
}

/// Moves a stale `unvrs` into this run's `backup/<stamp>-path/` (named after its old
/// path, listed in `moved.txt`); returns where it went.
fn stash(h: &Homes, dir: &mut Option<PathBuf>, file: &Path) -> Result<PathBuf> {
    let d = match dir {
        Some(d) => d.clone(),
        None => {
            let d = h
                .unvrs
                .join("backup")
                .join(format!("{}-path", cmd_install::now_stamp()));
            fs::create_dir_all(&d)?;
            *dir = Some(d.clone());
            d
        }
    };
    let to = d.join(
        file.display()
            .to_string()
            .trim_start_matches('/')
            .replace('/', "_"),
    );
    fs::rename(file, &to)
        .or_else(|_| fs::copy(file, &to).and_then(|_| fs::remove_file(file)))
        .with_context(|| format!("move {} to {}", file.display(), to.display()))?;
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(d.join("moved.txt"))?;
    writeln!(log, "{} -> {}", file.display(), to.display())?;
    Ok(to)
}

/// Every `unvrs` on PATH that is not ours (older builds: a 0.5 `cargo install`, an old
/// copy in `~/.local/bin`), in PATH order.
fn strays(bin: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for d in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let p = d.join("unvrs");
        if p.is_file() && !same_file(&p, bin) && !out.iter().any(|o| same_file(o, &p)) {
            out.push(p);
        }
    }
    out
}

/// `unvrs` on PATH resolves to the stable binary (doctor uses this too).
pub fn path_resolves(bin: &Path) -> Result<String> {
    match drv_agent::which("unvrs") {
        Some(p) if same_file(&p, bin) => {
            Ok(format!("`unvrs` is {} -> {}", p.display(), bin.display()))
        }
        Some(p) => bail!(
            "`unvrs` on PATH is {}, not {} (an older build?); run unvrs setup",
            p.display(),
            bin.display()
        ),
        None => bail!("no `unvrs` on PATH; run unvrs setup (or open a new terminal after it)"),
    }
}

/// Puts `unvrs` on PATH. Returns how to call it afterwards (`unvrs` when it resolves to
/// ours in this shell, else the full path) and whether it resolves.
fn path_step(h: &Homes, tty: bool) -> Result<(String, bool)> {
    let bin = h.bin();
    let full = bin.display().to_string();
    let link_dir = env::var_os("UNVRS_LINK_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map_or_else(|| home().map(|h| h.join(".local/bin")), Ok)?;
    let mut backup = None;
    if on_path(&link_dir) {
        let link = link_dir.join("unvrs");
        match fs::symlink_metadata(&link) {
            Ok(m)
                if m.file_type().is_symlink()
                    && fs::read_link(&link).ok().as_deref() == Some(&bin) =>
            {
                println!("path: already {} -> {full}", link.display());
            }
            Ok(m) => {
                if m.file_type().is_symlink() {
                    fs::remove_file(&link)?;
                } else {
                    // An older unvrs binary: kept in the backup, replaced by the link.
                    let to = stash(h, &mut backup, &link)?;
                    println!("path: moved the old {} to {}", link.display(), to.display());
                }
                std::os::unix::fs::symlink(&bin, &link)?;
                println!("path: {} -> {full}", link.display());
            }
            Err(_) => {
                fs::create_dir_all(&link_dir)?;
                std::os::unix::fs::symlink(&bin, &link)?;
                println!("path: {} -> {full}", link.display());
            }
        }
    } else if path_resolves(&bin).is_ok() {
        println!("path: already on PATH ({full})");
    } else {
        rc_step(&bin, &link_dir, tty)?;
    }
    // Older unvrs builds elsewhere on PATH shadow ours or confuse `which unvrs`.
    let others = strays(&bin);
    if !others.is_empty() {
        let first = drv_agent::which("unvrs");
        println!(
            "path: other `unvrs` on your PATH (not UNVRS {}):",
            env!("CARGO_PKG_VERSION")
        );
        for o in &others {
            let shadows = first.as_deref() == Some(o.as_path());
            println!(
                "  {}{}",
                o.display(),
                if shadows {
                    "   (comes first: `unvrs` runs this one)"
                } else {
                    ""
                }
            );
        }
        if tty
            && ask(&format!(
                "Move {} to {}?",
                if others.len() == 1 { "it" } else { "them" },
                h.unvrs.join("backup").display()
            ))
        {
            for o in &others {
                match stash(h, &mut backup, o) {
                    Ok(to) => println!("path: moved {} to {}", o.display(), to.display()),
                    Err(e) => println!("path: could not move {}: {e:#}", o.display()),
                }
            }
        } else {
            println!(
                "path: left as they are; remove them with: rm {}",
                others
                    .iter()
                    .map(|o| format!("'{}'", o.display()))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    let ok = path_resolves(&bin).is_ok();
    Ok((if ok { "unvrs".into() } else { full }, ok))
}

/// The one line for the shell rc when the link dir is not on PATH; appended after a yes.
fn rc_step(bin: &Path, link_dir: &Path, tty: bool) -> Result<()> {
    let rc = match env::var_os("UNVRS_SHELL_RC").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            let shell = env::var("SHELL").unwrap_or_default();
            home()?.join(if shell.ends_with("bash") {
                ".bash_profile"
            } else {
                ".zshrc"
            })
        }
    };
    let line = format!(
        "export PATH=\"{}:$PATH\"",
        tilde(bin.parent().context("bin dir")?)
    );
    if fs::read_to_string(&rc).is_ok_and(|t| t.lines().any(|l| l.trim() == line)) {
        println!(
            "path: already in {} (open a new terminal to use `unvrs`)",
            rc.display()
        );
        return Ok(());
    }
    println!(
        "path: {} is not on your PATH; this line puts unvrs on it:\n  {line}",
        link_dir.display()
    );
    if tty && ask(&format!("Add it to {}?", rc.display())) {
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&rc)?;
        let text = fs::read_to_string(&rc).unwrap_or_default();
        let sep = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        write!(f, "{sep}\n# UNVRS (unvrs setup)\n{line}\n")?;
        println!(
            "path: added to {} (open a new terminal to use `unvrs`)",
            rc.display()
        );
    } else {
        println!(
            "path: not changed; add the line to {} yourself",
            rc.display()
        );
    }
    Ok(())
}

// ───────────────────────────── seed ─────────────────────────────

fn seed_step(h: &Homes, o: &Opts) -> Result<()> {
    if o.no_seed {
        println!("seed: skipped (--no-seed)");
        return Ok(());
    }
    let dir = match &o.seed {
        Some(d) => {
            ensure!(d.is_dir(), "No seed folder at {}", d.display());
            d.clone()
        }
        None => {
            let d = env::var_os("UNVRS_SEED")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .map_or_else(|| home().map(|h| h.join("context/_seed")), Ok)?;
            if !d.is_dir() {
                println!(
                    "seed: none at {} (skipped; `unvrs setup --seed <dir>` imports another)",
                    d.display()
                );
                return Ok(());
            }
            d
        }
    };
    print!(
        "{}",
        run_bin(h, &["seed", "import", &dir.display().to_string()])?
    );
    Ok(())
}

// ───────────────────────────── Codex hook trust ─────────────────────────────

#[derive(PartialEq)]
enum Trust {
    /// Every UNVRS hook is trusted (now or before).
    Done,
    /// Left to the captain (declined, no terminal, or Codex unavailable).
    Skipped,
}

fn hook_list(h: &Homes, v: &Value) -> Vec<Value> {
    let bin = format!("'{}' hook ", h.bin().display());
    let mut seen = vec![];
    v["result"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|d| d["hooks"].as_array().cloned().unwrap_or_default())
        .filter(|x| x["pluginId"] == plugin_id())
        .filter(|x| x["command"].as_str().is_some_and(|c| c.contains(&bin)))
        .filter(|x| {
            let k = x["key"].as_str().unwrap_or_default().to_owned();
            let new = !k.is_empty() && !seen.contains(&k);
            seen.push(k);
            new
        })
        .collect()
}

fn trusted(x: &Value) -> bool {
    x["trustStatus"] == "trusted" || x["trustStatus"] == "managed"
}

fn manual() {
    println!("{}", cmd_install::trust_help());
    println!("  Or re-run `unvrs setup` in Terminal and answer yes.");
}

fn trust_step(h: &Homes, o: &Opts, tty: bool) -> Result<Trust> {
    if drv_agent::which("codex").is_none() {
        println!("codex: not on PATH; no hooks to trust");
        return Ok(Trust::Done);
    }
    let cwds = json!({"cwds": [h.unvrs]});
    let listed = match drv_agent::codex_app_server(h, &[("hooks/list", cwds.clone())]) {
        Ok(r) => hook_list(h, &r[0]),
        Err(e) => {
            println!("codex hooks: could not ask Codex's app server ({e:#})");
            manual();
            return Ok(Trust::Skipped);
        }
    };
    if listed.is_empty() {
        println!("codex hooks: Codex lists no UNVRS hooks yet");
        manual();
        return Ok(Trust::Skipped);
    }
    let pending: Vec<&Value> = listed.iter().filter(|x| !trusted(x)).collect();
    if pending.is_empty() {
        println!(
            "codex hooks: all {} UNVRS hooks already trusted",
            listed.len()
        );
        return Ok(Trust::Done);
    }
    println!(
        "Codex runs these {} UNVRS hooks ({}) once you trust them:",
        pending.len(),
        plugin_id()
    );
    for x in &pending {
        println!(
            "  {:<17} {}",
            x["eventName"].as_str().unwrap_or("?"),
            x["command"].as_str().unwrap_or("?")
        );
    }
    let yes = if o.trust {
        println!("--trust-hooks: trusting them");
        true
    } else if tty {
        ask(&format!(
            "Trust these {} UNVRS hooks in Codex?",
            pending.len()
        ))
    } else {
        println!(
            "codex hooks: not trusted (no terminal to ask you in; `--trust-hooks` gives consent)"
        );
        manual();
        return Ok(Trust::Skipped);
    };
    if !yes {
        println!("codex hooks: not trusted (your answer)");
        manual();
        return Ok(Trust::Skipped);
    }
    // Backup, then the same write the Codex app makes when you click Trust.
    let config = h.codex.join("config.toml");
    let dir = h
        .unvrs
        .join("backup")
        .join(format!("{}-codex-trust", cmd_install::now_stamp()));
    fs::create_dir_all(&dir)?;
    if config.is_file() {
        fs::copy(&config, dir.join("config.toml"))?;
    }
    let state: serde_json::Map<String, Value> = pending
        .iter()
        .filter_map(|x| {
            Some((
                x["key"].as_str()?.to_owned(),
                json!({"trusted_hash": x["currentHash"].as_str()?}),
            ))
        })
        .collect();
    let r = drv_agent::codex_app_server(
        h,
        &[
            (
                "config/batchWrite",
                json!({"edits": [{"keyPath": "hooks.state", "value": state, "mergeStrategy": "upsert"}]}),
            ),
            ("hooks/list", cwds),
        ],
    )?;
    ensure!(
        r[0].get("error").is_none(),
        "codex config/batchWrite: {}",
        r[0]["error"]
    );
    let after = hook_list(h, &r[1]);
    let left = after.iter().filter(|x| !trusted(x)).count();
    ensure!(
        left == 0 && !after.is_empty(),
        "codex still lists {left} UNVRS hooks as untrusted"
    );
    println!(
        "codex hooks: trusted {} (written by Codex's app server; config.toml backed up to {})",
        state.len(),
        dir.display()
    );
    Ok(Trust::Done)
}

// ───────────────────────────── setup ─────────────────────────────

pub fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let o = parse(args)?;
    refuse_model()?;
    let h = Homes::from_env()?;
    let tty = io::stdin().is_terminal();

    say(&format!(
        "Installing UNVRS {} (binary, plugin for Claude Code and Codex, kernel LaunchAgent)",
        env!("CARGO_PKG_VERSION")
    ));
    let unchanged = cmd_install::install_with(o.agent, false)?;

    say("Command line");
    let (unvrs, on_path) = path_step(&h, tty)?;

    say("Kernel");
    let k = run_bin(&h, &["kernel", "start"])?;
    println!("{}", k.lines().next().unwrap_or("kernel: running"));

    say("Seed");
    seed_step(&h, &o)?;

    say("Codex hook trust (your decision)");
    let trust = trust_step(&h, &o, tty)?;

    say("Doctor");
    let fails = cmd_install::doctor_checks()?;
    // Expected failures: the trust the captain has not given, and PATH until a new
    // terminal reads the rc line.
    let expected = |f: &str| {
        (trust == Trust::Skipped && f == "codex hooks trusted")
            || (!on_path && f == "unvrs on PATH")
    };
    let real: Vec<&String> = fails.iter().filter(|f| !expected(f)).collect();
    if fails.is_empty() {
        println!("doctor: every check passed");
    } else if real.is_empty() {
        let why: Vec<&str> = fails
            .iter()
            .map(|f| match f.as_str() {
                "codex hooks trusted" => "Codex hook trust (yours to give, see above)",
                _ => "unvrs on PATH (open a new terminal)",
            })
            .collect();
        println!("doctor: ok except {}", why.join("; "));
    } else {
        println!(
            "doctor: {} check(s) failed: {}",
            real.len(),
            real.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let port = env::var("UNVRS_OBSERVATORY_PORT").unwrap_or_else(|_| "7576".into());
    say(if real.is_empty() {
        "Ready"
    } else {
        "Almost ready"
    });
    println!(
        "Start:        type $unvrs:l1 in the Codex app or T3 Code (Claude Code: /unvrs:l1); that thread becomes your L1\nObservatory:  http://unvrs.localhost:{port}\nOnce:         restart the Codex app and T3 Code{} so they load the UNVRS plugin\nCheck / undo: {unvrs} doctor · {unvrs} uninstall · re-run {unvrs} setup any time",
        if unchanged {
            " (if you have not since installing)"
        } else {
            ""
        }
    );
    if !real.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flags() {
        let a = |v: &[&str]| parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(a(&[]).is_ok_and(|o| o.agent && !o.trust && !o.no_seed && o.seed.is_none()));
        assert!(a(&["--seed", "/x", "--trust-hooks"]).is_ok_and(|o| o.trust && o.seed.is_some()));
        assert!(a(&["--seed", "/x", "--no-seed"]).is_err());
        assert!(a(&["--seed"]).is_err());
        assert!(a(&["--bogus"]).is_err());
    }
}
