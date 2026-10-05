//! D61 install surface: `unvrs install | uninstall | doctor | upgrade | rollback |
//! plugin render`. Versioned binaries, the LaunchAgent and the checks live here; the
//! plugin and harness plumbing live in `drv_agent::plugin`.
//!
//! Layout under `UNVRS_HOME` (default `~/.unvrs`):
//! `bin/unvrs` → `versions/<v>/unvrs` (stable path for hooks, MCP, LaunchAgent),
//! `versions/current`, `versions/previous`, `plugin/`, `backup/<stamp>/`,
//! `install.json` (the install record), `kernel/kernel.log`.
use anyhow::{Context, Result, ensure};
use drv_agent::{Homes, plugin_id};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const USAGE: &str = "usage: unvrs install [--no-launchagent] | uninstall | doctor | upgrade [--from <binary>] | rollback | plugin render | plugin sync\nenv: UNVRS_HOME (~/.unvrs), CLAUDE_CONFIG_DIR (~/.claude), CODEX_HOME (~/.codex), UNVRS_LAUNCHAGENTS_DIR (~/Library/LaunchAgents), UNVRS_LAUNCHD_LABEL (dev.unvrs.kernel), UNVRS_OBSERVATORY_PORT (7576)";

/// `Some(result)` when `args` is one of ours; the integrator's dispatch calls this first.
pub fn dispatch(args: &[String]) -> Option<Result<()>> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let ours = matches!(
        a.first(),
        Some(&"install" | &"uninstall" | &"doctor" | &"upgrade" | &"rollback")
    ) || a.starts_with(&["plugin", "render"])
        || a.starts_with(&["plugin", "sync"]);
    if !ours {
        return None;
    }
    if a.iter().any(|x| *x == "--help" || *x == "-h") {
        println!("{USAGE}");
        return Some(Ok(()));
    }
    Some(match a.as_slice() {
        ["install"] => install(true),
        ["install", "--no-launchagent"] => install(false),
        ["uninstall"] => uninstall(),
        ["doctor"] => doctor(),
        ["upgrade"] => upgrade(None),
        ["upgrade", "--from", bin] => upgrade(Some(Path::new(bin))),
        ["rollback"] => rollback(),
        ["plugin", "render"] => render(),
        ["plugin", "sync"] => sync(),
        _ => Err(anyhow::anyhow!("{USAGE}")),
    })
}

// ───────────────────────────── helpers ─────────────────────────────

pub fn now_stamp() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:03}", t.as_secs(), t.subsec_millis())
}
fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}
fn domain() -> String {
    format!("gui/{}", uid())
}
pub(crate) fn service(h: &Homes) -> String {
    format!("gui/{}/{}", uid(), h.label)
}
pub(crate) fn launchctl(args: &[&str]) -> (bool, String) {
    match Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ),
        Err(e) => (false, e.to_string()),
    }
}
pub(crate) fn agent_loaded(h: &Homes) -> bool {
    launchctl(&["print", &service(h)]).0
}
pub(crate) fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
fn record_path(h: &Homes) -> PathBuf {
    h.unvrs.join("install.json")
}
fn read_record(h: &Homes) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(record_path(h)).ok()?).ok()
}
/// Writes only when the bytes differ, so a repeated install changes nothing.
pub(crate) fn write_if_changed(p: &Path, text: &str) -> Result<bool> {
    if fs::read_to_string(p).ok().as_deref() == Some(text) {
        return Ok(false);
    }
    fs::create_dir_all(p.parent().context("parent")?)?;
    let tmp = p.with_extension("unvrs-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, p)?;
    Ok(true)
}

/// `<bin> --version` prints `version: X.Y.Z`.
fn binary_version(bin: &Path) -> Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {} --version", bin.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("version:")
                .map(|v| v.trim().to_owned())
        })
        .filter(|v| !v.is_empty())
        .with_context(|| format!("{} --version printed no `version: X`", bin.display()))
}

/// Copies `from` into `versions/<v>/unvrs` unless identical bytes are already there.
fn stage_version(h: &Homes, from: &Path, v: &str) -> Result<(PathBuf, bool)> {
    let dest = h.unvrs.join("versions").join(v).join("unvrs");
    let from_c = from.canonicalize().unwrap_or_else(|_| from.to_path_buf());
    if dest.canonicalize().ok().as_deref() == Some(from_c.as_path()) {
        return Ok((dest, false));
    }
    let bytes = fs::read(from).with_context(|| format!("read {}", from.display()))?;
    if fs::read(&dest).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok((dest, false));
    }
    fs::create_dir_all(dest.parent().context("parent")?)?;
    let tmp = dest.with_extension("new");
    fs::write(&tmp, &bytes)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, &dest)?;
    Ok((dest, true))
}

/// Points `bin/unvrs` at `target` atomically (temp symlink + rename).
fn switch_bin(h: &Homes, target: &Path) -> Result<bool> {
    link_bin(&h.bin(), target)
}
pub(crate) fn link_bin(bin: &Path, target: &Path) -> Result<bool> {
    if fs::read_link(bin).ok().as_deref() == Some(target) {
        return Ok(false);
    }
    fs::create_dir_all(bin.parent().context("parent")?)?;
    let tmp = bin.with_extension("next");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, bin)?;
    Ok(true)
}

/// Records `current` (and moves the old current to `previous` when it changes).
fn set_current(h: &Homes, v: &str) -> Result<Option<String>> {
    let dir = h.unvrs.join("versions");
    let cur = read_trim(&dir.join("current"));
    if cur.as_deref() == Some(v) {
        return Ok(None);
    }
    if let Some(old) = &cur {
        write_if_changed(&dir.join("previous"), &format!("{old}\n"))?;
    }
    write_if_changed(&dir.join("current"), &format!("{v}\n"))?;
    Ok(cur)
}

/// Restarts the daemon onto the binary `bin/unvrs` now points at.
fn restart_daemon(h: &Homes) -> String {
    if agent_loaded(h) {
        let (ok, out) = launchctl(&["kickstart", "-k", &service(h)]);
        if ok {
            format!("daemon: restarted ({})", service(h))
        } else {
            format!("daemon: kickstart failed: {}", out.trim())
        }
    } else {
        let _ = Command::new(h.bin())
            .args(["kernel", "stop"])
            .env("UNVRS_HOME", &h.unvrs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        "daemon: no LaunchAgent; kernel stopped, hooks start the new one on demand".into()
    }
}

// ───────────────────────────── LaunchAgent ─────────────────────────────

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// PATH for the daemon: the directories of `claude` and `codex`, then the system dirs.
fn daemon_path() -> String {
    let mut dirs: Vec<String> = vec![];
    for cmd in ["claude", "codex"] {
        if let Some(d) = drv_agent::which(cmd).and_then(|p| p.parent().map(Path::to_path_buf)) {
            dirs.push(d.display().to_string());
        }
    }
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        dirs.push(d.into());
    }
    let mut seen = vec![];
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    dirs.join(":")
}

pub fn plist_text(h: &Homes) -> String {
    let home = h.unvrs.display().to_string();
    let log = h.unvrs.join("kernel/kernel.log").display().to_string();
    let mut env = vec![
        ("UNVRS_HOME".to_owned(), home),
        ("PATH".to_owned(), daemon_path()),
        // Under launchd the kernel never exits for being idle (D60).
        ("UNVRS_DAEMON".to_owned(), "launchd".to_owned()),
    ];
    // Test homes: the daemon must see the same harness copies as the installer.
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "UNVRS_OBSERVATORY_PORT"] {
        if let Ok(v) = std::env::var(key)
            && !v.is_empty()
        {
            env.push((key.into(), v));
        }
    }
    let env_xml: String = env
        .iter()
        .map(|(k, v)| {
            format!(
                "\t\t<key>{}</key>\n\t\t<string>{}</string>\n",
                xml(k),
                xml(v)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{bin}</string>
		<string>kernel</string>
		<string>run</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
{env_xml}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = xml(&h.label),
        bin = xml(&h.bin().display().to_string()),
        log = xml(&log),
    )
}

/// Writes and bootstraps the LaunchAgent; `binary_changed` restarts a loaded daemon.
fn launch_agent(h: &Homes, binary_changed: bool) -> Result<String> {
    fs::create_dir_all(h.unvrs.join("kernel"))?;
    let plist = h.plist();
    let changed = write_if_changed(&plist, &plist_text(h))?;
    let loaded = agent_loaded(h);
    if loaded && !changed {
        if binary_changed {
            return Ok(restart_daemon(h));
        }
        return Ok(format!("launchagent: already loaded ({})", service(h)));
    }
    if loaded {
        launchctl(&["bootout", &service(h)]);
    }
    let (ok, out) = launchctl(&["bootstrap", &domain(), &plist.display().to_string()]);
    ensure!(
        ok,
        "launchctl bootstrap {}: {}",
        plist.display(),
        out.trim()
    );
    Ok(format!(
        "launchagent: {} and loaded ({})",
        if changed {
            "written"
        } else {
            "already written"
        },
        service(h)
    ))
}

fn remove_launch_agent(h: &Homes) -> String {
    let plist = h.plist();
    let loaded = agent_loaded(h);
    if loaded {
        launchctl(&["bootout", &service(h)]);
    }
    let existed = plist.exists();
    let _ = fs::remove_file(&plist);
    match (loaded, existed) {
        (false, false) => format!("launchagent: already removed ({})", h.label),
        _ => format!("launchagent: booted out and deleted ({})", plist.display()),
    }
}

// ───────────────────────────── install ─────────────────────────────

pub fn trust_help() -> &'static str {
    "codex hooks (once, your decision, D61): `unvrs setup` in Terminal asks you, or trust them in Codex:\n  Codex app: Settings > Hooks > the five UNVRS plugin hooks > Trust (or Plugins > UNVRS > Hooks > Trust all).\n  Codex CLI: run `codex`; at \"Hooks need review\" pick \"Trust all and continue\", or type /hooks and trust the UNVRS hooks.\n  Codex re-asks after a plugin update changes a hook. Check with: unvrs doctor"
}

pub fn install(with_agent: bool) -> Result<()> {
    install_with(with_agent, true).map(|_| ())
}

/// The install itself; `tail` prints the trust step and first steps (`unvrs setup`
/// prints its own). Returns true when nothing changed.
pub fn install_with(with_agent: bool, tail: bool) -> Result<bool> {
    let h = Homes::from_env()?;
    fs::create_dir_all(&h.unvrs)?;
    let mut lines = vec![];
    // 1. The binary: versions/<v>/unvrs, bin/unvrs → it.
    let exe = std::env::current_exe()?.canonicalize()?;
    let (dest, copied) = stage_version(&h, &exe, VERSION)?;
    let switched = switch_bin(&h, &dest)?;
    let prev = set_current(&h, VERSION)?;
    let binary_changed = copied || switched;
    lines.push(if binary_changed {
        format!(
            "binary: {} -> {}{}",
            h.bin().display(),
            dest.display(),
            prev.map(|p| format!(" (previous {p})")).unwrap_or_default()
        )
    } else {
        format!("binary: already {} ({VERSION})", h.bin().display())
    });
    // 2. Backup before any harness change; the first install's backup is the record.
    let record = read_record(&h);
    let backup_dir = match record.as_ref().and_then(|r| r["backup"].as_str()) {
        Some(b) if Path::new(b).join("manifest.json").is_file() => {
            lines.push(format!("backup: already {b}"));
            PathBuf::from(b)
        }
        _ => {
            let dir = h.unvrs.join("backup").join(now_stamp());
            drv_agent::backup(&h, &dir)?;
            lines.push(format!("backup: {} (manifest.json)", dir.display()));
            dir
        }
    };
    // 3. The plugin.
    let (root, changed) = drv_agent::render_plugin_checked(&h.unvrs)?;
    lines.push(format!(
        "plugin: {} {}",
        if changed {
            "rendered"
        } else {
            "already rendered"
        },
        root.display()
    ));
    // 4. Harnesses.
    let claude = drv_agent::which("claude").is_some();
    let codex = drv_agent::which("codex").is_some();
    if claude {
        lines.push(drv_agent::register_claude(&h, &root)?);
    } else {
        lines.push("claude: not found on PATH (skipped)".into());
    }
    if codex {
        lines.push(drv_agent::register_codex(&h, &root)?);
    } else {
        lines.push("codex: not found on PATH (skipped)".into());
    }
    // 5. The daemon.
    let agent = with_agent || record.as_ref().is_some_and(|r| r["launchagent"] == true);
    if with_agent {
        lines.push(launch_agent(&h, binary_changed)?);
    } else {
        lines.push(
            "launchagent: skipped (--no-launchagent); hooks start the kernel on demand".into(),
        );
    }
    // 6. The record (first install's timestamp and backup kept).
    let rec = json!({
        "version": VERSION,
        "home": h.unvrs,
        "backup": backup_dir,
        "claude": claude || record.as_ref().is_some_and(|r| r["claude"] == true),
        "codex": codex || record.as_ref().is_some_and(|r| r["codex"] == true),
        "launchagent": agent,
        "label": h.label,
        "plist": h.plist(),
        "plugin_id": plugin_id(),
        "claude_dir": h.claude,
        "codex_dir": h.codex,
        "installed_at": record.as_ref().map(|r| r["installed_at"].clone()).unwrap_or_else(|| json!(now_stamp())),
    });
    let rec_changed = write_if_changed(
        &record_path(&h),
        &(serde_json::to_string_pretty(&rec)? + "\n"),
    )?;
    let nothing = !binary_changed
        && !changed
        && !rec_changed
        && lines
            .iter()
            .all(|l| l.contains("already") || l.contains("skipped"));
    for l in &lines {
        println!("{l}");
    }
    if nothing {
        println!("result: already installed; nothing changed");
    } else {
        println!("result: installed");
    }
    if tail {
        if codex {
            println!("{}", trust_help());
        }
        println!(
            "first: type $unvrs:l1 (Codex app, T3 Code) or /unvrs:l1 (Claude Code) in any thread to make it your L1\ncheck: {} doctor",
            h.bin().display()
        );
    }
    Ok(nothing)
}

// ───────────────────────────── uninstall ─────────────────────────────

pub fn uninstall() -> Result<()> {
    let h = Homes::from_env()?;
    let record = read_record(&h);
    let mut lines = vec![remove_launch_agent(&h)];
    if record.is_none()
        && !drv_agent::claude_installed(&h)
        && !drv_agent::claude_marketplace_known(&h)
        && !drv_agent::codex_installed(&h)
        && !drv_agent::codex_marketplace_known(&h)
        && !h.plugin_root().exists()
    {
        lines.push("harnesses: already clean".into());
        for l in &lines {
            println!("{l}");
        }
        println!("result: already uninstalled; nothing changed");
        return Ok(());
    }
    // A kernel started on demand by a hook (no LaunchAgent) stops too.
    if h.bin().exists() {
        let _ = Command::new(h.bin())
            .args(["kernel", "stop"])
            .env("UNVRS_HOME", &h.unvrs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    lines.extend(drv_agent::unregister(&h));
    let mut all = true;
    match record.as_ref().and_then(|r| r["backup"].as_str()) {
        Some(b) => {
            let (restored, ok) = drv_agent::restore(&h, Path::new(b))?;
            all = ok;
            lines.extend(restored);
        }
        None => lines.push("backup: no install record; harness files left as they are".into()),
    }
    if h.plugin_root().exists() {
        fs::remove_dir_all(h.plugin_root())?;
        lines.push(format!("plugin: removed {}", h.plugin_root().display()));
    }
    if record.is_some() {
        fs::rename(
            record_path(&h),
            h.unvrs.join(format!("uninstalled-{}.json", now_stamp())),
        )?;
    }
    for l in &lines {
        println!("{l}");
    }
    println!(
        "result: uninstalled; {}; kept {} (kernel state, memory, projects, versions)",
        if all {
            "every harness file matches its pre-install backup"
        } else {
            "some harness files changed since install (see lines above)"
        },
        h.unvrs.display()
    );
    Ok(())
}

// ───────────────────────────── upgrade / rollback ─────────────────────────────

pub fn upgrade(from: Option<&Path>) -> Result<()> {
    let h = Homes::from_env()?;
    let from = match from {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe()?.canonicalize()?,
    };
    let v = binary_version(&from)?;
    let (dest, restaged) = stage_version(&h, &from, &v)?;
    let cur = read_trim(&h.unvrs.join("versions/current"));
    if cur.as_deref() == Some(v.as_str())
        && fs::read_link(h.bin()).ok().as_deref() == Some(dest.as_path())
    {
        if restaged {
            // Same version string, new bytes (a dev rebuild): same slot, new process.
            println!("upgrade: {v} restaged with new bytes ({})", dest.display());
            println!("{}", restart_daemon(&h));
        } else {
            println!("upgrade: already at {v} ({})", h.bin().display());
        }
        return Ok(());
    }
    switch_bin(&h, &dest)?;
    let prev = set_current(&h, &v)?;
    println!(
        "upgrade: {} -> {v} ({} -> {})",
        prev.as_deref().unwrap_or("none"),
        h.bin().display(),
        dest.display()
    );
    println!("{}", restart_daemon(&h));
    println!("rollback: unvrs rollback");
    Ok(())
}

pub fn rollback() -> Result<()> {
    let h = Homes::from_env()?;
    let dir = h.unvrs.join("versions");
    let prev = read_trim(&dir.join("previous")).context("no previous version recorded")?;
    let cur = read_trim(&dir.join("current")).unwrap_or_default();
    let target = dir.join(&prev).join("unvrs");
    ensure!(
        target.is_file(),
        "previous binary missing: {}",
        target.display()
    );
    switch_bin(&h, &target)?;
    write_if_changed(&dir.join("current"), &format!("{prev}\n"))?;
    write_if_changed(&dir.join("previous"), &format!("{cur}\n"))?;
    println!("rollback: {cur} -> {prev} ({})", h.bin().display());
    println!("{}", restart_daemon(&h));
    Ok(())
}

fn render() -> Result<()> {
    let h = Homes::from_env()?;
    let (root, changed) = drv_agent::render_plugin_checked(&h.unvrs)?;
    println!(
        "plugin: {} {}",
        if changed {
            "rendered"
        } else {
            "already rendered"
        },
        root.display()
    );
    Ok(())
}

/// `unvrs plugin sync`: render this binary's plugin, then bring every harness that has it
/// installed to that copy. The deploy runs it after each swap (and rollback).
fn sync() -> Result<()> {
    let h = Homes::from_env()?;
    let (root, changed) = drv_agent::render_plugin_checked(&h.unvrs)?;
    println!(
        "plugin: {} {} ({})",
        if changed {
            "rendered"
        } else {
            "already rendered"
        },
        root.display(),
        drv_agent::rendered_version(&root).unwrap_or_default()
    );
    for l in drv_agent::refresh_hosts(&h, &root)? {
        println!("{l}");
    }
    Ok(())
}

// ───────────────────────────── doctor ─────────────────────────────

struct Report {
    fails: Vec<String>,
}
impl Report {
    fn line(&mut self, check: &str, r: Result<String>) {
        match r {
            Ok(d) => println!("ok {check}: {d}"),
            Err(e) => {
                self.fails.push(check.to_owned());
                println!("FAIL {check}: {}", format!("{e:#}").replace('\n', " "));
            }
        }
    }
}

fn kernel_ping(h: &Homes) -> Result<String> {
    let sock = uke::Universe::at(&h.unvrs)?.socket_path();
    // A LaunchAgent just bootstrapped or kickstarted needs a moment (launchd throttles a
    // job that ran under 10 s before restarting it).
    let mut tries = 0;
    let mut s = loop {
        match UnixStream::connect(&sock) {
            Ok(s) => break s,
            Err(_) if tries < 60 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("cannot connect to {}", sock.display()));
            }
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    s.set_write_timeout(Some(Duration::from_secs(3)))?;
    s.write_all(b"{\"op\":\"ping\"}\n")?;
    let mut line = String::new();
    BufReader::new(s).take(65536).read_line(&mut line)?;
    let v: Value = serde_json::from_str(line.trim()).context("kernel answered no JSON line")?;
    ensure!(
        v.get("ok").is_some_and(|ok| ok != &json!(false)),
        "kernel answered without ok: {}",
        line.trim()
    );
    Ok(format!("answers on {}", sock.display()))
}

fn http_status(port: u16, host: &str) -> Result<u16> {
    let mut s = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(
        s,
        "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: text/html\r\n\r\n"
    )?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .with_context(|| format!("not HTTP: {}", line.trim()))
}

pub(crate) fn observatory(port: u16) -> Result<String> {
    let good = http_status(port, &format!("unvrs.localhost:{port}"))
        .with_context(|| format!("nothing on 127.0.0.1:{port}"))?;
    ensure!(good == 200, "Host unvrs.localhost answered {good}");
    let evil = http_status(port, "evil.example")?;
    ensure!(
        evil != 200,
        "Host evil.example answered 200 (no DNS-rebinding guard)"
    );
    Ok(format!(
        "http://unvrs.localhost:{port}/ answers 200; Host evil.example gets {evil}"
    ))
}

pub(crate) fn mcp_tool(h: &Homes) -> Result<String> {
    let bin = h.bin();
    ensure!(bin.exists(), "{} missing", bin.display());
    let mut cmd = Command::new(&bin);
    cmd.arg("mcp").env("UNVRS_HOME", &h.unvrs);
    let r = drv_agent::jsonrpc_session(
        cmd,
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "unvrs-doctor", "version": VERSION}}),
        &[("tools/list", json!({}))],
        Duration::from_secs(10),
    )
    .map_err(|e| anyhow::anyhow!("`{} mcp` did not answer MCP initialize ({e:#}); does this binary have the `mcp` subcommand?", bin.display()))?;
    let tools: Vec<String> = r[0]["result"]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    ensure!(
        tools.iter().any(|t| t == "unvrs"),
        "tools/list has no `unvrs` tool: {tools:?}"
    );
    Ok(format!("`{} mcp` lists tool unvrs", bin.display()))
}

/// Every plugin skill doctor expects each harness to list: the entry commands and
/// the code of conduct.
fn entry_names() -> Vec<String> {
    mapp_unvrs::ENTRY
        .iter()
        .map(|s| s.name)
        .chain([mapp_unvrs::CONDUCT_NAME])
        .map(|name| format!("{}:{}", drv_agent::PLUGIN, name))
        .collect()
}

pub fn doctor() -> Result<()> {
    let fails = doctor_checks()?;
    if !fails.is_empty() {
        println!("result: {} check(s) failed", fails.len());
        std::process::exit(1);
    }
    println!("result: every check passed");
    Ok(())
}

/// Every doctor line, printed; returns the names of the checks that failed.
pub fn doctor_checks() -> Result<Vec<String>> {
    let h = Homes::from_env()?;
    let mut r = Report { fails: vec![] };
    // Binary.
    r.line(
        "binary",
        (|| {
            let bin = h.bin();
            ensure!(
                bin.exists(),
                "{} missing (run unvrs install)",
                bin.display()
            );
            let v = binary_version(&bin)?;
            let target = fs::read_link(&bin)
                .map(|t| t.display().to_string())
                .unwrap_or_default();
            Ok(format!("{} {v} -> {target}", bin.display()))
        })(),
    );
    // LaunchAgent.
    r.line(
        "launchagent",
        (|| {
            ensure!(h.plist().is_file(), "{} missing", h.plist().display());
            let (ok, out) = launchctl(&["print", &service(&h)]);
            ensure!(ok, "{} not loaded", service(&h));
            let state = out
                .lines()
                .find_map(|l| l.trim().strip_prefix("state = "))
                .unwrap_or("?");
            Ok(format!("{} loaded, state {state}", service(&h)))
        })(),
    );
    r.line("kernel", kernel_ping(&h));
    r.line("unvrs on PATH", crate::cmd_setup::path_resolves(&h.bin()));
    // Claude.
    r.line(
        "claude plugin",
        (|| {
            ensure!(
                drv_agent::claude_installed(&h),
                "{} not in {}",
                plugin_id(),
                h.claude.join("plugins/installed_plugins.json").display()
            );
            ensure!(
                drv_agent::claude_enabled(&h),
                "{} not enabled in {}",
                plugin_id(),
                h.claude.join("settings.json").display()
            );
            Ok(format!(
                "{} installed and enabled (user scope)",
                plugin_id()
            ))
        })(),
    );
    r.line(
        "codex plugin",
        (|| {
            ensure!(
                drv_agent::codex_installed(&h),
                "{} not in {}",
                plugin_id(),
                h.codex.join("config.toml").display()
            );
            ensure!(
                drv_agent::codex_enabled(&h),
                "{} disabled in config.toml",
                plugin_id()
            );
            Ok(format!("{} installed and enabled", plugin_id()))
        })(),
    );
    // Both harnesses cache the plugin by version; a stale cache hides new skills.
    let root = h.unvrs.join("plugin");
    r.line(
        "plugin cache",
        (|| {
            let claude = drv_agent::claude_fresh(&h, &root)?;
            let codex = drv_agent::codex_fresh(&h, &root)?;
            ensure!(claude == codex, "claude holds {claude}, codex {codex}");
            Ok(format!(
                "claude and codex hold the rendered {claude} ({})",
                root.display()
            ))
        })(),
    );
    // Codex surfaces through its own app server.
    let codex = if drv_agent::which("codex").is_some() {
        fs::create_dir_all(&h.unvrs)?;
        let cwd = json!([h.unvrs]);
        drv_agent::codex_app_server(
            &h,
            &[
                ("skills/list", json!({"cwds": cwd, "forceReload": true})),
                ("hooks/list", json!({"cwds": cwd})),
            ],
        )
    } else {
        Err(anyhow::anyhow!("codex not on PATH"))
    };
    let codex = codex
        .as_ref()
        .map_err(|e| anyhow::anyhow!("codex app-server: {e:#}"));
    r.line(
        "codex skills",
        (|| {
            let res = codex.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
            let listed: Vec<String> = res[0]["result"]["data"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|d| d["skills"].as_array().cloned().unwrap_or_default())
                .filter(|s| s["pluginId"] == plugin_id())
                .filter_map(|s| s["name"].as_str().map(str::to_owned))
                .collect();
            let missing: Vec<_> = entry_names()
                .into_iter()
                .filter(|n| !listed.contains(n))
                .collect();
            ensure!(
                missing.is_empty(),
                "skills/list lacks {}",
                missing.join(", ")
            );
            Ok(format!(
                "skills/list has {} unvrs: skills ($unvrs:l1 …)",
                listed.len()
            ))
        })(),
    );
    r.line(
        "codex hooks trusted",
        (|| {
            let res = codex.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
            let hooks: Vec<Value> = res[1]["result"]["data"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|d| d["hooks"].as_array().cloned().unwrap_or_default())
                .filter(|x| x["pluginId"] == plugin_id())
                .collect();
            let mut keys: Vec<&str> = hooks.iter().filter_map(|x| x["key"].as_str()).collect();
            keys.sort();
            keys.dedup();
            ensure!(!keys.is_empty(), "hooks/list shows no {} hooks", plugin_id());
            let pending: Vec<String> = hooks
                .iter()
                .filter(|x| x["trustStatus"] != "trusted" && x["trustStatus"] != "managed")
                .map(|x| format!("{} {}", x["eventName"].as_str().unwrap_or("?"), x["trustStatus"].as_str().unwrap_or("?")))
                .collect();
            ensure!(
                pending.is_empty(),
                "{} of {} need your trust ({}); Codex app: Settings > Hooks > Trust; Codex CLI: /hooks",
                pending.len(),
                keys.len(),
                pending.join(", ")
            );
            Ok(format!("{} plugin hooks trusted", keys.len()))
        })(),
    );
    // Claude: the init line of a headless run (killed before any model turn).
    r.line(
        "claude commands",
        (|| {
            ensure!(drv_agent::which("claude").is_some(), "claude not on PATH");
            let init = drv_agent::claude_init(&h, "/unvrs:tree")?;
            let cmds: Vec<String> = init["slash_commands"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_owned))
                .collect();
            let missing: Vec<_> = entry_names()
                .into_iter()
                .filter(|n| !cmds.contains(n))
                .collect();
            ensure!(
                missing.is_empty(),
                "init slash_commands lack /{}",
                missing.join(", /")
            );
            let mcp = init["mcp_servers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| {
                    m["name"] == format!("plugin:{}:{}", drv_agent::PLUGIN, drv_agent::MCP_SERVER)
                })
                .and_then(|m| m["status"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "absent".into());
            Ok(format!(
                "init has /unvrs:l1 and {} more; mcp plugin:unvrs:unvrs {mcp}",
                cmds.iter()
                    .filter(|c| c.starts_with("unvrs:"))
                    .count()
                    .saturating_sub(1)
            ))
        })(),
    );
    r.line("mcp tool", mcp_tool(&h));
    let port: u16 = std::env::var("UNVRS_OBSERVATORY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7576);
    r.line("observatory", observatory(port));
    Ok(r.fails)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_names_the_stable_binary_and_escapes() {
        let h = Homes {
            unvrs: PathBuf::from("/u/a&b"),
            claude: PathBuf::from("/c"),
            codex: PathBuf::from("/x"),
            launch_agents: PathBuf::from("/l"),
            label: "dev.unvrs.test".into(),
        };
        let p = plist_text(&h);
        assert!(p.contains("<string>/u/a&amp;b/bin/unvrs</string>"));
        assert!(p.contains("<string>kernel</string>\n\t\t<string>run</string>"));
        assert!(p.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(p.contains("/u/a&amp;b/kernel/kernel.log"));
    }
    #[test]
    fn dispatch_leaves_other_commands_alone() {
        assert!(dispatch(&["kernel".into(), "status".into()]).is_none());
        assert!(dispatch(&["plugin".into(), "install".into()]).is_none());
        assert!(dispatch(&["doctor".into(), "--help".into()]).is_some());
    }
}
