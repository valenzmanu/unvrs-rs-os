//! `unvrs deploy | rollback | versions` (docs/design/deploy-loop.md). Deploy is separate
//! from merge: it builds a commit that already exists in the repo into a write-once slot
//! `versions/<v>+g<sha12>/`, drains the kernel, swaps `bin/unvrs`, gates the new kernel
//! and rolls back on its own when the gate fails. It never writes to the repo.
//!
//! Under `UNVRS_HOME`: `deploy/deploy.lock` (one deploy or rollback at a time),
//! `deploy/status.json` (the running or last run; the Observatory reads it),
//! `deploy/live.json` (the live slot and the one before it), `deploy/history.jsonl`,
//! `deploy/logs/<run>.log`, `deploy/checkouts/<sha12>/`, `deploy/cache/target` (the
//! shared build cache, guarded by `deploy/cache/target.lock`).
use crate::cmd_install::{
    agent_loaded, launchctl, link_bin, mcp_tool, now_stamp, observatory, read_trim, service,
    write_if_changed,
};
use anyhow::{Context, Result, bail, ensure};
use drv_agent::Homes;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::{fs::PermissionsExt, io::AsRawFd},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use uke::{Universe, now_ms, scalar};

pub const USAGE: &str = "usage: unvrs deploy <branch|commit> [--repo <dir>] [--no-test] [--drain-secs n] [--force-requeue] [--allow-drop] [--dry-run]\n       unvrs rollback   (to the slot live before the last deploy; same drain and gate)\n       unvrs versions [--json]\nenv: UNVRS_REPO (default ~/github/unvrs-rs), UNVRS_HOME (~/.unvrs)";

const POLL_SECS: u64 = 2;

/// `Some(result)` for `deploy`, `versions` and `rollback`; `rollback` without a
/// `deploy/live.json` falls back to the legacy `versions/previous` rollback.
pub fn dispatch(args: &[String]) -> Option<Result<()>> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let first = *a.first()?;
    if !matches!(first, "deploy" | "versions" | "rollback") {
        return None;
    }
    if a.iter().any(|x| *x == "--help" || *x == "-h") {
        println!("{USAGE}");
        return Some(Ok(()));
    }
    Some(match (first, &a[1..]) {
        ("deploy", rest) => Opts::parse(rest).and_then(|o| cli_deploy(&o)),
        ("rollback", []) => cli_rollback(),
        ("versions", []) => versions(false),
        ("versions", ["--json"]) => versions(true),
        _ => Err(anyhow::anyhow!("{USAGE}")),
    })
}

// ───────────────────────────── options and paths ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Opts {
    pub reference: String,
    pub repo: PathBuf,
    pub test: bool,
    pub drain_secs: u64,
    pub force_requeue: bool,
    pub allow_drop: bool,
    pub dry_run: bool,
}

impl Opts {
    fn parse(args: &[&str]) -> Result<Opts> {
        let mut o = Opts {
            reference: String::new(),
            repo: default_repo()?,
            test: true,
            drain_secs: 900,
            force_requeue: false,
            allow_drop: false,
            dry_run: false,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match *a {
                "--repo" => o.repo = PathBuf::from(it.next().context(USAGE)?),
                "--drain-secs" => {
                    o.drain_secs = it.next().context(USAGE)?.parse().context(USAGE)?
                }
                "--no-test" => o.test = false,
                "--force-requeue" => o.force_requeue = true,
                "--allow-drop" => o.allow_drop = true,
                "--dry-run" => o.dry_run = true,
                r if !r.starts_with('-') && o.reference.is_empty() => o.reference = r.to_owned(),
                _ => bail!("{USAGE}"),
            }
        }
        ensure!(!o.reference.is_empty(), "{USAGE}");
        Ok(o)
    }
}

fn default_repo() -> Result<PathBuf> {
    if let Some(r) = std::env::var_os("UNVRS_REPO").filter(|r| !r.is_empty()) {
        return Ok(PathBuf::from(r));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME unset")?).join("github/unvrs-rs"))
}

/// The files deploy owns under one home.
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    fn versions(&self) -> PathBuf {
        self.home.join("versions")
    }
    fn deploy(&self) -> PathBuf {
        self.home.join("deploy")
    }
    fn bin(&self) -> PathBuf {
        self.home.join("bin/unvrs")
    }
    fn slot(&self, id: &str) -> PathBuf {
        self.versions().join(id)
    }
    fn current(&self) -> Option<String> {
        read_trim(&self.versions().join("current"))
    }
    /// The slot `bin/unvrs` runs: the link decides, `current` is only a fallback. A hand
    /// install can leave `current` naming another slot than the one the link points to.
    fn live_slot(&self) -> Option<String> {
        let target = fs::read_link(self.bin()).ok()?;
        match (target.parent(), target.file_name()) {
            (Some(dir), Some(name))
                if name == "unvrs" && dir.parent() == Some(&self.versions()) =>
            {
                dir.file_name()?.to_str().map(str::to_owned)
            }
            _ => self.current(),
        }
        .or_else(|| self.current())
    }
    fn previous(&self) -> Option<String> {
        read_trim(&self.versions().join("previous"))
    }
    fn manifest(&self, id: &str) -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(self.slot(id).join("manifest.json")).ok()?).ok()
    }
    /// The commit a slot was built from; `None` for a legacy slot (no manifest).
    fn commit_of(&self, id: &str) -> Option<String> {
        self.manifest(id)?["commit"].as_str().map(str::to_owned)
    }
    fn live(&self) -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(self.deploy().join("live.json")).ok()?).ok()
    }
}

/// `<version>+g<sha12>`: one id names exactly one source tree.
pub fn slot_id(version: &str, commit: &str) -> String {
    format!("{version}+g{}", &commit[..commit.len().min(12)])
}

/// `[workspace.package] version` (or `[package]`) from a Cargo.toml text.
pub fn cargo_version(toml: &str) -> Option<String> {
    let mut section = "";
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if matches!(section, "[workspace.package]" | "[package]")
            && let Some(v) = line.strip_prefix("version")
            && let Some(v) = v.trim_start().strip_prefix('=')
        {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() && !v.contains('{') {
                return Some(v.to_owned());
            }
        }
    }
    None
}

// ───────────────────────────── locks ─────────────────────────────

/// An `flock` held for as long as the value lives.
pub struct Lock(File);

impl Lock {
    /// Non-blocking; the error names the holder written in the lock file.
    pub fn take(path: &Path, holder: &str) -> Result<Lock> {
        fs::create_dir_all(path.parent().context("parent")?)?;
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let mut who = String::new();
            let _ = f.read_to_string(&mut who);
            bail!(
                "another deploy or rollback holds {}: {}",
                path.display(),
                who.trim()
            );
        }
        f.set_len(0)?;
        f.rewind()?;
        writeln!(
            f,
            "os_pid {} · {holder} · since {}",
            std::process::id(),
            now_stamp()
        )?;
        Ok(Lock(f))
    }
    /// Blocking: waits for the holder (the build cache lock).
    pub fn wait(path: &Path) -> Result<Lock> {
        fs::create_dir_all(path.parent().context("parent")?)?;
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: as above.
        ensure!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } == 0,
            "flock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        Ok(Lock(f))
    }
}

impl Drop for Lock {
    /// Explicit: a child forked meanwhile shares the open file description until its
    /// exec, and closing our descriptor alone would leave the lock held.
    fn drop(&mut self) {
        // SAFETY: flock on the descriptor this value owns.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

// ───────────────────────────── steps ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Resolved {
    pub commit: String,
    /// The ref when it names a local or remote branch.
    pub branch: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Drained {
    pub busy: Vec<u64>,
    pub folding: u64,
    /// Set when the kernel could not hold new work (not running, or no drain op).
    pub note: Option<String>,
}

/// Everything with side effects outside the home's `versions/` and `deploy/`; the tests
/// swap it for a fake.
pub trait Steps {
    fn resolve(&mut self, repo: &Path, reference: &str) -> Result<Resolved>;
    /// Refuse missing live history; legacy slots obtain identity from their binary.
    fn check_ancestry(
        &mut self,
        repo: &Path,
        candidate: &str,
        live_bin: &Path,
        live_commit: Option<&str>,
    ) -> Result<()>;
    /// The workspace version at `commit` (from its Cargo.toml).
    fn version_at(&mut self, repo: &Path, commit: &str) -> Result<String>;
    fn checkout(&mut self, repo: &Path, commit: &str, dir: &Path) -> Result<()>;
    /// Builds `src` into `target` and returns the release binary.
    fn build(
        &mut self,
        src: &Path,
        target: &Path,
        commit: &str,
        reference: &str,
    ) -> Result<PathBuf>;
    fn test(&mut self, src: &Path, target: &Path) -> Result<()>;
    fn drain(&mut self, on: bool) -> Result<Drained>;
    /// Pids the kernel knows (the smoke compares it across the swap).
    fn census(&mut self) -> Result<usize>;
    /// Stops the old kernel gracefully and starts the one `bin/unvrs` points at.
    fn restart(&mut self) -> Result<String>;
    /// Renders the plugin with the slot's binary `bin` (`plugin render`) and brings the
    /// Claude Code and Codex caches to it; both cache by version, so a swap without this
    /// leaves the hosts on the old skills.
    fn plugins(&mut self, bin: &Path) -> Result<String>;
    /// The health gate; `Err` names the failing check.
    fn gate(&mut self, commit: Option<&str>, pids_before: usize) -> Result<Vec<String>>;
    fn sleep(&mut self, secs: u64);
}

// ───────────────────────────── the run record ─────────────────────────────

/// One deploy or rollback: `status.json` while it runs, a history line when it ends.
struct Run<'a> {
    p: &'a Paths,
    rec: Value,
}

impl<'a> Run<'a> {
    fn start(p: &'a Paths, kind: &str, reference: &str) -> Result<Run<'a>> {
        fs::create_dir_all(p.deploy().join("logs"))?;
        let run = Run {
            p,
            rec: json!({
                "run": format!("{kind}-{}", now_stamp()), "kind": kind, "ref": reference,
                "os_pid": std::process::id(), "started": now_ms(), "phase": "start",
                "done": false, "log": [],
            }),
        };
        run.save()?;
        Ok(run)
    }
    fn set(&mut self, key: &str, v: Value) {
        self.rec[key] = v;
    }
    fn phase(&mut self, phase: &str, note: &str) {
        println!("{phase}: {note}");
        self.rec["phase"] = json!(phase);
        self.rec["updated"] = json!(now_ms());
        if let Some(log) = self.rec["log"].as_array_mut() {
            log.push(json!({"phase": phase, "at": now_ms(), "note": note}));
        }
        let _ = self.save();
    }
    fn save(&self) -> Result<()> {
        let text = serde_json::to_string_pretty(&self.rec)? + "\n";
        write_if_changed(&self.p.deploy().join("status.json"), &text)?;
        Ok(())
    }
    /// Ends the run with `result` (live, staged, failed, aborted, rolled_back, broken).
    fn finish(mut self, result: &str, reason: &str) -> Result<Value> {
        self.phase(result, reason);
        self.rec["done"] = json!(true);
        self.rec["result"] = json!(result);
        self.rec["reason"] = json!(reason);
        self.rec["ended"] = json!(now_ms());
        self.save()?;
        let mut line = self.rec.clone();
        if let Some(o) = line.as_object_mut() {
            o.remove("log");
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.p.deploy().join("history.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(&line)?)?;
        Ok(self.rec)
    }
}

// ───────────────────────────── deploy ─────────────────────────────

/// The whole pipeline. `Err` only when the lock is held (the run record belongs to the
/// holder); every other failure ends in a finished record.
pub fn deploy(p: &Paths, s: &mut dyn Steps, o: &Opts) -> Result<Value> {
    let _lock = Lock::take(
        &p.deploy().join("deploy.lock"),
        &format!("deploy {}", o.reference),
    )?;
    let mut run = Run::start(p, "deploy", &o.reference)?;
    let id = match stage(p, s, o, &mut run) {
        Ok(id) => id,
        Err(e) => return run.finish("failed", &format!("{e:#}; live slot untouched")),
    };
    if o.dry_run {
        return run.finish(
            "staged",
            &format!(
                "{id} staged; `unvrs deploy {}` swaps it with no rebuild",
                o.reference
            ),
        );
    }
    let cur = p.live_slot();
    if cur.as_deref() == Some(id.as_str())
        && fs::read_link(p.bin()).ok() == Some(p.slot(&id).join("unvrs"))
    {
        return run.finish("live", &format!("{id} is already live; nothing swapped"));
    }
    cutover(p, s, run, &id, cur, o.drain_secs, o.force_requeue)
}

/// resolve → ancestry → checkout → build → test → stage; returns the slot id.
/// A slot that exists is reused as it is (write-once).
fn stage(p: &Paths, s: &mut dyn Steps, o: &Opts, run: &mut Run) -> Result<String> {
    run.phase(
        "resolve",
        &format!("{} in {}", o.reference, o.repo.display()),
    );
    let r = s.resolve(&o.repo, &o.reference)?;
    run.set("commit", json!(r.commit));
    run.set("branch", json!(r.branch));
    run.set("allow_drop", json!(o.allow_drop));
    if !o.allow_drop {
        let live = p
            .live_slot()
            .filter(|id| fs::read_link(p.bin()).ok() == Some(p.slot(id).join("unvrs")))
            .and_then(|id| p.commit_of(&id));
        run.phase(
            "ancestry",
            "checking that the candidate contains the live commit",
        );
        s.check_ancestry(&o.repo, &r.commit, &p.bin(), live.as_deref())?;
    }
    let version = s.version_at(&o.repo, &r.commit)?;
    let id = slot_id(&version, &r.commit);
    run.set("slot", json!(id));
    if p.slot(&id).join("manifest.json").is_file() {
        run.phase(
            "stage",
            &format!("{id} exists; reused (slots are write-once)"),
        );
        return Ok(id);
    }
    let src = p
        .deploy()
        .join("checkouts")
        .join(&r.commit[..r.commit.len().min(12)]);
    let result = (|| {
        run.phase("checkout", &format!("{} -> {}", r.commit, src.display()));
        let _ = fs::remove_dir_all(&src);
        s.checkout(&o.repo, &r.commit, &src)?;
        let cache = p.deploy().join("cache");
        // Held from build through copying the artifact, so no other build replaces it.
        let _cache = Lock::wait(&cache.join("target.lock"))?;
        let target = cache.join("target");
        run.phase("build", "cargo build --release --locked -p unvrs");
        let built = s.build(&src, &target, &r.commit, &o.reference)?;
        if o.test {
            run.phase("test", "cargo test --workspace");
            s.test(&src, &target)?;
        }
        let manifest = json!({
            "id": id, "version": version, "commit": r.commit, "ref": o.reference,
            "branch": r.branch, "built_at": now_ms(), "tested": o.test,
            "builder": format!("unvrs deploy {} ({})", env!("CARGO_PKG_VERSION"), uke::BUILD_COMMIT.unwrap_or("dev")),
        });
        let note = write_slot(p, &id, &built, manifest)?;
        run.phase("stage", &note);
        Ok(())
    })();
    let _ = fs::remove_dir_all(&src);
    result.map(|()| id)
}

/// Copies `built` into `versions/<id>/` through a temp dir and one rename. When the slot
/// appeared meanwhile, the existing one wins and a byte difference is reported.
pub fn write_slot(p: &Paths, id: &str, built: &Path, mut manifest: Value) -> Result<String> {
    let dest = p.slot(id);
    let hash = sha256(built)?;
    let existing = |why: &str| -> Result<String> {
        let old = p
            .manifest(id)
            .context("slot exists without manifest.json")?;
        let old = old["sha256"].as_str().unwrap_or("?").to_owned();
        Ok(if old == hash {
            format!("{id} exists with the same bytes ({why})")
        } else {
            format!(
                "{id} exists with sha256 {old}; this build is {hash} (not reproducible); kept the existing slot"
            )
        })
    };
    if dest.exists() {
        return existing("reused");
    }
    let tmp = p
        .versions()
        .join(format!(".{id}.tmp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let write = (|| -> Result<()> {
        fs::copy(built, tmp.join("unvrs")).with_context(|| format!("copy {}", built.display()))?;
        fs::set_permissions(tmp.join("unvrs"), fs::Permissions::from_mode(0o555))?;
        manifest["sha256"] = json!(hash);
        fs::write(
            tmp.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)? + "\n",
        )?;
        Ok(())
    })();
    if let Err(e) = write {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_dir_all(&tmp);
        if dest.exists() {
            return existing("another deploy staged it first");
        }
        return Err(e).with_context(|| format!("stage {}", dest.display()));
    }
    Ok(format!("{id} written ({}, sha256 {hash})", dest.display()))
}

fn sha256(path: &Path) -> Result<String> {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context("run shasum")?;
    ensure!(out.status.success(), "shasum {} failed", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("shasum printed nothing")
}

/// Points `bin/unvrs` at `id` and records `current` and `previous`.
fn point(p: &Paths, id: &str, previous: Option<&str>) -> Result<()> {
    let bin = p.slot(id).join("unvrs");
    ensure!(bin.is_file(), "{} missing", bin.display());
    link_bin(&p.bin(), &bin)?;
    write_if_changed(&p.versions().join("current"), &format!("{id}\n"))?;
    if let Some(prev) = previous {
        write_if_changed(&p.versions().join("previous"), &format!("{prev}\n"))?;
    }
    Ok(())
}

/// drain → swap → gate → live, or rollback to `back` when the gate fails.
fn cutover(
    p: &Paths,
    s: &mut dyn Steps,
    mut run: Run,
    target: &str,
    back: Option<String>,
    drain_secs: u64,
    force_requeue: bool,
) -> Result<Value> {
    let before = s.census().unwrap_or(0);
    run.set("pids_before", json!(before));
    // Drain.
    let mut waited = 0;
    loop {
        let d = match s.drain(true) {
            Ok(d) => d,
            Err(e) => {
                return run.finish("aborted", &format!("drain: {e:#}; nothing swapped"));
            }
        };
        if let Some(note) = &d.note
            && waited == 0
        {
            run.phase("drain", note);
        }
        if d.busy.is_empty() && d.folding == 0 {
            run.phase("drain", &format!("idle after {waited}s"));
            break;
        }
        if waited >= drain_secs {
            if force_requeue {
                run.phase(
                    "drain",
                    &format!(
                        "timed out after {waited}s; --force-requeue: busy {:?} are cut; the new kernel blocks them (restart-unverified) and wakes their leads",
                        d.busy
                    ),
                );
                break;
            }
            let _ = s.drain(false);
            return run.finish(
                "aborted",
                &format!(
                    "drain timed out after {waited}s (busy {:?}, folding {}); nothing swapped, kernel undrained; --force-requeue cuts busy workers (the new kernel blocks them for their leads)",
                    d.busy, d.folding
                ),
            );
        }
        if waited == 0 {
            run.phase(
                "drain",
                &format!("waiting for busy {:?}, folding {}", d.busy, d.folding),
            );
        }
        s.sleep(POLL_SECS);
        waited += POLL_SECS;
    }
    // Swap.
    let old_prev = p.previous();
    run.phase(
        "swap",
        &format!("{} -> {target}", back.as_deref().unwrap_or("none")),
    );
    if let Err(e) = point(p, target, back.as_deref()) {
        let _ = s.drain(false);
        return run.finish("aborted", &format!("swap: {e:#}; kernel undrained"));
    }
    match s.restart() {
        Ok(note) => run.phase("swap", &note),
        Err(e) => run.phase("swap", &format!("restart: {e:#}")),
    }
    // Plugin: the target's skills reach both hosts before anything checks them.
    let plugins = s.plugins(&p.slot(target).join("unvrs"));
    if let Ok(note) = &plugins {
        run.phase("plugin", note);
    }
    // Gate.
    let commit = p.commit_of(target);
    run.phase(
        "gate",
        &format!(
            "expect commit {}",
            commit.as_deref().unwrap_or("(legacy slot, unchecked)")
        ),
    );
    let gated = plugins
        .context("plugin")
        .and_then(|_| s.gate(commit.as_deref(), before));
    let reason = match gated {
        Ok(lines) => {
            let live = json!({
                "slot": target, "commit": commit,
                "branch": p.manifest(target).and_then(|m| m["branch"].as_str().map(str::to_owned)),
                "ref": run.rec["ref"], "since": now_ms(), "previous": back,
                "run": run.rec["run"],
            });
            write_if_changed(
                &p.deploy().join("live.json"),
                &(serde_json::to_string_pretty(&live)? + "\n"),
            )?;
            return run.finish("live", &format!("{target} live; {}", lines.join("; ")));
        }
        Err(e) => format!("gate failed on {target}: {e:#}"),
    };
    // Rollback.
    let Some(back) = back else {
        return run.finish(
            "broken",
            &format!(
                "{reason}; no previous slot to return to; {}",
                manual(p, None)
            ),
        );
    };
    run.phase("rollback", &format!("{reason}; returning to {back}"));
    let restored = point(p, &back, old_prev.as_deref()).map(|()| {
        if old_prev.is_none() {
            let _ = fs::remove_file(p.versions().join("previous"));
        }
    });
    if let Err(e) = restored {
        return run.finish(
            "broken",
            &format!("{reason}; rollback swap: {e:#}; {}", manual(p, Some(&back))),
        );
    }
    if let Err(e) = s.restart() {
        run.phase("rollback", &format!("restart: {e:#}"));
    }
    // The previous slot's own plugin back on both hosts.
    let plugins = s.plugins(&p.slot(&back).join("unvrs"));
    if let Ok(note) = &plugins {
        run.phase("rollback", &format!("plugin: {note}"));
    }
    let gated = plugins
        .context("plugin")
        .and_then(|_| s.gate(p.commit_of(&back).as_deref(), before));
    match gated {
        Ok(_) => run.finish("rolled_back", &format!("{reason}; {back} is live again")),
        Err(e) => run.finish(
            "broken",
            &format!(
                "{reason}; rollback to {back} also failed its gate: {e:#}; {}",
                manual(p, Some(&back))
            ),
        ),
    }
}

/// The exact commands for a by-hand recovery.
fn manual(p: &Paths, slot: Option<&str>) -> String {
    let slot = slot.unwrap_or("<slot>");
    format!(
        "by hand: ln -sfn {} {} && printf '{slot}\\n' > {} && launchctl kickstart -k gui/$(id -u)/dev.unvrs.kernel",
        p.slot(slot).join("unvrs").display(),
        p.bin().display(),
        p.versions().join("current").display()
    )
}

/// `unvrs rollback` through `live.json`: back to the slot live before the last deploy.
pub fn rollback(p: &Paths, s: &mut dyn Steps, drain_secs: u64) -> Result<Value> {
    let _lock = Lock::take(&p.deploy().join("deploy.lock"), "rollback")?;
    let live = p.live().context("no deploy/live.json")?;
    let from = live["slot"]
        .as_str()
        .context("live.json has no slot")?
        .to_owned();
    let to = live["previous"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| p.previous())
        .context("no previous slot recorded")?;
    ensure!(
        p.slot(&to).join("unvrs").is_file(),
        "previous slot {to} has no binary"
    );
    let mut run = Run::start(p, "rollback", &to)?;
    run.set("slot", json!(to));
    run.set("commit", json!(p.commit_of(&to)));
    cutover(p, s, run, &to, Some(from), drain_secs, false)
}

// ───────────────────────────── the real steps ─────────────────────────────

struct Real {
    h: Homes,
    u: Universe,
    log: PathBuf,
}

impl Real {
    fn cmd(&self, mut c: Command, what: &str) -> Result<String> {
        let out = c
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("run {what}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        ensure!(out.status.success(), "{what} failed: {}", text.trim());
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    /// Runs cargo with its output appended to the run log; the error quotes the tail.
    fn cargo(&self, src: &Path, target: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<()> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)?;
        let status = Command::new("cargo")
            .args(args)
            .current_dir(src)
            .env("CARGO_TARGET_DIR", target)
            .envs(envs.iter().copied())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()
            .context("run cargo")?;
        if !status.success() {
            let text = fs::read_to_string(&self.log).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(15).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            bail!(
                "cargo {} failed (log {}):\n{}",
                args.join(" "),
                self.log.display(),
                tail.join("\n")
            );
        }
        Ok(())
    }
    fn table(&self) -> Result<Value> {
        uke::kernel_request(&self.u, &json!({"op": "table"}), Duration::from_secs(5))
    }
    fn kernel_os_pid(&self) -> Option<u64> {
        let text = fs::read_to_string(self.u.kernel_dir().join("kernel.json")).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["os_pid"].as_u64()
    }
    fn wait_kernel(&self, commit: Option<&str>) -> Result<String> {
        let mut last = String::from("no answer");
        for _ in 0..30 {
            match uke::kernel_request(&self.u, &json!({"op": "ping"}), Duration::from_secs(3)) {
                Ok(v) => {
                    let got = v["commit"].as_str();
                    match commit {
                        None => return Ok("answers ping (legacy slot: commit unchecked)".into()),
                        Some(c) if got == Some(c) => return Ok(format!("answers ping as {c}")),
                        Some(_) => last = format!("answers as commit {}", got.unwrap_or("none")),
                    }
                }
                Err(e) => last = format!("{e:#}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        bail!("kernel within 15s: {last}")
    }
}

impl Steps for Real {
    fn resolve(&mut self, repo: &Path, reference: &str) -> Result<Resolved> {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{reference}^{{commit}}"));
        let commit = self
            .cmd(c, &format!("git rev-parse {reference}"))
            .with_context(|| format!("{reference} is no commit in {}", repo.display()))?
            .trim()
            .to_owned();
        ensure!(commit.len() == 40, "git rev-parse printed {commit:?}");
        let mut c = Command::new("git");
        c.arg("-C").arg(repo).args([
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/{reference}"),
            &format!("refs/remotes/*/{reference}"),
        ]);
        let branch = !self.cmd(c, "git for-each-ref")?.trim().is_empty();
        Ok(Resolved {
            commit,
            branch: branch.then(|| reference.to_owned()),
        })
    }
    fn check_ancestry(
        &mut self,
        repo: &Path,
        candidate: &str,
        live_bin: &Path,
        live_commit: Option<&str>,
    ) -> Result<()> {
        let live = match live_commit {
            Some(commit) => commit.to_owned(),
            None => {
                match fs::symlink_metadata(live_bin) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    result => {
                        result.context("read installed binary metadata")?;
                    }
                }
                let mut c = Command::new(live_bin);
                c.arg("--version");
                let version = self.cmd(c, "installed unvrs --version")
                    .context("cannot identify the live commit; use --allow-drop to deploy without ancestry verification")?;
                version.lines().find_map(|line| line.strip_prefix("commit:").map(str::trim))
                    .context("cannot identify the live commit; use --allow-drop to deploy without ancestry verification")?
                    .to_owned()
            }
        };
        ensure!(
            live.len() == 40 && live.bytes().all(|b| b.is_ascii_hexdigit()),
            "cannot verify live commit {live:?}; use --allow-drop to deploy without ancestry verification"
        );
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(repo)
            .args(["log", "--format=%H %s", &live, "--not", candidate, "--"]);
        let missing = self
            .cmd(c, "git log live commits missing from candidate")
            .with_context(|| {
                format!(
                    "cannot verify live commit {live} in {}; fetch its history or use --allow-drop",
                    repo.display()
                )
            })?;
        ensure!(
            missing.trim().is_empty(),
            "refusing deploy {candidate}: it would drop live commits from {live}:\n{}\nmerge the live history, use --allow-drop, or run unvrs rollback",
            missing.trim()
        );
        Ok(())
    }
    fn version_at(&mut self, repo: &Path, commit: &str) -> Result<String> {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(repo)
            .arg("show")
            .arg(format!("{commit}:Cargo.toml"));
        let toml = self.cmd(c, "git show Cargo.toml")?;
        cargo_version(&toml).context("no workspace version in Cargo.toml")
    }
    fn checkout(&mut self, repo: &Path, commit: &str, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let mut git = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["archive", "--format=tar", commit])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .context("run git archive")?;
        let tar = Command::new("tar")
            .arg("-x")
            .arg("-C")
            .arg(dir)
            .stdin(git.stdout.take().context("git stdout")?)
            .status()
            .context("run tar")?;
        let git = git.wait()?;
        ensure!(
            git.success() && tar.success(),
            "git archive {commit} | tar -x failed"
        );
        Ok(())
    }
    fn build(
        &mut self,
        src: &Path,
        target: &Path,
        commit: &str,
        reference: &str,
    ) -> Result<PathBuf> {
        self.cargo(
            src,
            target,
            &["build", "--release", "--locked", "-p", "unvrs"],
            &[
                ("UNVRS_BUILD_COMMIT", commit),
                ("UNVRS_BUILD_REF", reference),
            ],
        )?;
        let bin = target.join("release/unvrs");
        ensure!(bin.is_file(), "{} missing after the build", bin.display());
        Ok(bin)
    }
    fn test(&mut self, src: &Path, target: &Path) -> Result<()> {
        self.cargo(src, target, &["test", "--workspace", "--locked"], &[])
    }
    fn drain(&mut self, on: bool) -> Result<Drained> {
        if !uke::kernel_running(&self.u) {
            return Ok(Drained {
                note: Some("kernel not running; nothing to drain".into()),
                ..Drained::default()
            });
        }
        match uke::kernel_request(
            &self.u,
            &json!({"op": "drain", "on": on}),
            Duration::from_secs(5),
        ) {
            Ok(v) => Ok(Drained {
                busy: v["busy"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .collect(),
                folding: v["folding"].as_u64().unwrap_or(0),
                note: None,
            }),
            // A kernel from before the drain op: wait for its busy workers without holding.
            Err(e) if format!("{e:#}").contains("Unknown kernel op") => {
                let t = self.table()?;
                let busy = t["pids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p["driven"]["busy"] == json!(true))
                    .filter_map(|p| p["pid"].as_u64())
                    .collect();
                Ok(Drained {
                    busy,
                    folding: 0,
                    note: Some("the live kernel has no drain op; waiting for idle without holding new turns".into()),
                })
            }
            Err(e) => Err(e),
        }
    }
    fn census(&mut self) -> Result<usize> {
        if !uke::kernel_running(&self.u) {
            return Ok(0);
        }
        Ok(self.table()?["pids"].as_array().map_or(0, Vec::len))
    }
    fn restart(&mut self) -> Result<String> {
        let old = self.kernel_os_pid();
        if uke::kernel_running(&self.u) {
            uke::kernel_request(&self.u, &json!({"op": "stop"}), Duration::from_secs(5))?;
            // Gone, or already restarted by launchd (KeepAlive) under a new os_pid.
            for _ in 0..60 {
                if !uke::kernel_running(&self.u) || self.kernel_os_pid() != old {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        if agent_loaded(&self.h) {
            // No -k: a kernel launchd already restarted is the new binary; keep it.
            let (ok, out) = launchctl(&["kickstart", &service(&self.h)]);
            ensure!(ok, "launchctl kickstart: {}", out.trim());
            Ok(format!("old kernel stopped; {} started", service(&self.h)))
        } else {
            uke::ensure_running(&self.u, &self.h.bin(), Duration::from_secs(8))?;
            Ok("old kernel stopped; new kernel started (no LaunchAgent)".into())
        }
    }
    fn plugins(&mut self, bin: &Path) -> Result<String> {
        let mut c = Command::new(bin);
        c.args(["plugin", "render"]);
        let mut lines = vec![self.cmd(c, "plugin render")?.trim().to_owned()];
        lines.extend(drv_agent::refresh_hosts(
            &self.h,
            &self.h.unvrs.join("plugin"),
        )?);
        Ok(lines.join("; "))
    }
    fn gate(&mut self, commit: Option<&str>, pids_before: usize) -> Result<Vec<String>> {
        let mut ok = vec![format!("kernel {}", self.wait_kernel(commit)?)];
        ok.push(format!("mcp {}", mcp_tool(&self.h).context("mcp")?));
        let port = std::env::var("UNVRS_OBSERVATORY_PORT").unwrap_or_else(|_| "7576".into());
        if port != "off" && port != "0" {
            let port: u16 = port.parse().context("UNVRS_OBSERVATORY_PORT")?;
            let mut last = None;
            for _ in 0..10 {
                match observatory(port) {
                    Ok(line) => {
                        last = None;
                        ok.push(format!("observatory {line}"));
                        break;
                    }
                    Err(e) => last = Some(e),
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if let Some(e) = last {
                return Err(e.context("observatory"));
            }
        }
        let t = self.table().context("smoke: kernel table")?;
        let pids = t["pids"]
            .as_array()
            .context("smoke: table has no pids")?
            .len();
        ensure!(
            pids >= pids_before,
            "smoke: the kernel knows {pids} pids, {pids_before} before the swap (state lost?)"
        );
        ok.push(format!("smoke {pids} pids (before {pids_before})"));
        Ok(ok)
    }
    fn sleep(&mut self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

fn real(p: &Paths) -> Result<Real> {
    Ok(Real {
        h: Homes::from_env()?,
        u: Universe::at(&p.home)?,
        log: p
            .deploy()
            .join("logs")
            .join(format!("build-{}.log", now_stamp())),
    })
}

/// A driven worker that deploys would wait on its own busy turn, and the swap's
/// `kernel stop` would end the deploy's own process group mid-swap.
fn refuse_inside_a_turn() -> Result<()> {
    if let Some(pid) = std::env::var_os("UNVRS_DRIVEN_PID").filter(|v| !v.is_empty()) {
        bail!(
            "refusing inside the driven turn of pid {}: the drain would wait on this turn and the swap would stop it; run it from the captain's terminal",
            pid.to_string_lossy()
        );
    }
    Ok(())
}

fn report(rec: &Value) -> Result<()> {
    let result = rec["result"].as_str().unwrap_or("?");
    println!("result: {result}");
    if let Some(slot) = rec["slot"].as_str() {
        println!("slot: {slot}");
    }
    println!("reason: {}", scalar(rec["reason"].as_str().unwrap_or("")));
    if !matches!(result, "live" | "staged") {
        std::process::exit(1);
    }
    Ok(())
}

fn cli_deploy(o: &Opts) -> Result<()> {
    if !o.dry_run {
        refuse_inside_a_turn()?;
    }
    let p = Paths {
        home: Homes::from_env()?.unvrs,
    };
    let mut s = real(&p)?;
    report(&deploy(&p, &mut s, o)?)
}

fn cli_rollback() -> Result<()> {
    refuse_inside_a_turn()?;
    let p = Paths {
        home: Homes::from_env()?.unvrs,
    };
    if p.live().is_none() {
        return crate::cmd_install::rollback();
    }
    let mut s = real(&p)?;
    report(&rollback(&p, &mut s, 900)?)
}

// ───────────────────────────── versions ─────────────────────────────

/// Every slot with its identity, the live one, and recent history.
pub fn versions_json(p: &Paths) -> Result<Value> {
    let cur = p.live_slot();
    let prev = p.previous();
    let mut ids: Vec<String> = fs::read_dir(p.versions())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    ids.sort();
    let slots: Vec<Value> = ids
        .iter()
        .map(|id| {
            let m = p.manifest(id).unwrap_or(Value::Null);
            json!({
                "id": id, "commit": m["commit"], "branch": m["branch"], "ref": m["ref"],
                "built_at": m["built_at"], "tested": m["tested"], "sha256": m["sha256"],
                "current": cur.as_deref() == Some(id.as_str()),
                "previous": prev.as_deref() == Some(id.as_str()),
            })
        })
        .collect();
    let history: Vec<Value> = fs::read_to_string(p.deploy().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let recent = history[history.len().saturating_sub(10)..].to_vec();
    let status: Value = fs::read_to_string(p.deploy().join("status.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    // The OS metadata probe rejects invalid or overflowing recorded PIDs.
    let running = status["done"] == json!(false)
        && status["os_pid"]
            .as_u64()
            .and_then(|pid| i64::try_from(pid).ok())
            .is_some_and(|pid| uke::signals::process_exists(pid).is_ok());
    Ok(json!({
        "bin": fs::read_link(p.bin()).ok(), "current": cur, "previous": prev,
        "live": p.live(), "slots": slots, "history": recent,
        "deploying": if running { status } else { Value::Null },
    }))
}

/// `unvrs versions` as text. Strings are quoted scalars; a missing value is a bare `-`.
fn render_versions(v: &Value) -> String {
    use std::fmt::Write as _;
    let s = |x: &Value| match x {
        Value::Null => "-".to_owned(),
        Value::String(t) => scalar(t),
        other => other.to_string(),
    };
    let mut out = String::new();
    let _ = writeln!(out, "bin: {}", s(&v["bin"]));
    if v["current"].is_null() {
        let _ = writeln!(out, "live: none (nothing deployed or installed yet)");
    } else {
        let _ = writeln!(
            out,
            "live: {} (commit {}, branch {})",
            s(&v["current"]),
            s(&v["live"]["commit"]),
            s(&v["live"]["branch"])
        );
    }
    let slots = v["slots"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(
        out,
        "slots[{}]{{id,commit,branch,tested,mark}}:",
        slots.len()
    );
    for sl in &slots {
        let mark = if sl["current"] == json!(true) {
            "current"
        } else if sl["previous"] == json!(true) {
            "previous"
        } else {
            "-"
        };
        let _ = writeln!(
            out,
            "  {},{},{},{},{mark}",
            s(&sl["id"]),
            s(&sl["commit"]),
            s(&sl["branch"]),
            s(&sl["tested"])
        );
    }
    let hist = v["history"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(
        out,
        "history[{}]{{run,ref,slot,result,reason}}:",
        hist.len()
    );
    for r in &hist {
        let _ = writeln!(
            out,
            "  {},{},{},{},{}",
            s(&r["run"]),
            s(&r["ref"]),
            s(&r["slot"]),
            s(&r["result"]),
            s(&r["reason"])
        );
    }
    if !v["deploying"].is_null() {
        let _ = writeln!(
            out,
            "deploying: {} {} ({})",
            s(&v["deploying"]["ref"]),
            s(&v["deploying"]["slot"]),
            s(&v["deploying"]["phase"])
        );
    }
    out
}

fn versions(as_json: bool) -> Result<()> {
    let p = Paths {
        home: Homes::from_env()?.unvrs,
    };
    let v = versions_json(&p)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    print!("{}", render_versions(&v));
    Ok(())
}

#[cfg(test)]
mod tests;
