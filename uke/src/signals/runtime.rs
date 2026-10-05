//! Shared durable provenance for the kernel and out-of-process drivers.
use super::{ProcessIdentity, SignalRefusal, Signaller, SpawnLedger, UnixSignaller};
use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::Duration,
};

thread_local! { static CONTEXT: RefCell<Option<(PathBuf, usize)>> = const { RefCell::new(None) }; }

/// Set the kernel home on each request/worker thread, retaining its current owner.
pub fn set_home(home: &Path) {
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let owner = c.as_ref().filter(|(h, _)| h == home).map_or(0, |(_, p)| *p);
        *c = Some((home.to_owned(), owner));
    });
}
pub struct OwnerScope(Option<(PathBuf, usize)>);
impl Drop for OwnerScope {
    fn drop(&mut self) {
        CONTEXT.with(|c| *c.borrow_mut() = self.0.take());
    }
}
pub fn owner_scope(home: &Path, owner: usize) -> OwnerScope {
    OwnerScope(CONTEXT.with(|c| c.replace(Some((home.to_owned(), owner)))))
}
fn context() -> io::Result<(PathBuf, usize)> {
    if let Some(c) = CONTEXT.with(|c| c.borrow().clone()) {
        return Ok(c);
    }
    let owner = std::env::var("UNVRS_DRIVEN_PID")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    #[cfg(test)]
    let home = std::env::temp_dir().join(format!("unvrs-signal-unit-{}", std::process::id()));
    #[cfg(not(test))]
    let home = std::env::var_os("UNVRS_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".unvrs")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "UNVRS home missing"))?;
    Ok((home, owner))
}

#[derive(Clone)]
pub struct LedgerStore {
    dir: PathBuf,
}
fn boundary() -> &'static dyn Signaller {
    #[cfg(test)]
    {
        &UnitSignaller
    }
    #[cfg(not(test))]
    {
        &UnixSignaller
    }
}

#[cfg(test)]
type UnitProcesses = std::collections::BTreeMap<u32, (ProcessIdentity, bool)>;
#[cfg(test)]
fn unit_processes() -> &'static std::sync::Mutex<UnitProcesses> {
    static PROCESSES: std::sync::OnceLock<std::sync::Mutex<UnitProcesses>> =
        std::sync::OnceLock::new();
    PROCESSES.get_or_init(Default::default)
}
/// Unit-test boundary: record signals; never call the OS signal API.
#[cfg(test)]
struct UnitSignaller;
#[cfg(test)]
impl Signaller for UnitSignaller {
    fn own_pid(&self) -> u32 {
        UnixSignaller.own_pid()
    }
    fn own_pgid(&self) -> u32 {
        UnixSignaller.own_pgid()
    }
    fn identity(&self, pid: u32) -> io::Result<ProcessIdentity> {
        unit_processes()
            .lock()
            .unwrap()
            .get(&pid)
            .map(|p| Ok(p.0.clone()))
            .unwrap_or_else(|| UnixSignaller.identity(pid))
    }
    fn send(&self, request: super::VerifiedSignal) -> io::Result<()> {
        let pid = request.target().unsigned_abs();
        if let Some(p) = unit_processes().lock().unwrap().get_mut(&pid) {
            if p.1 {
                return Err(io::Error::from_raw_os_error(libc::EPERM));
            }
            p.1 = true;
        }
        Ok(())
    }
    fn group_exited(&self, identity: &ProcessIdentity) -> io::Result<bool> {
        Ok(unit_processes()
            .lock()
            .unwrap()
            .get(&identity.os_pid)
            .is_some_and(|p| p.0 == *identity && p.1))
    }
}
#[cfg(test)]
pub(crate) fn fake_worker(home: &Path, owner: usize) -> io::Result<u32> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1_000_000_000);
    let pid = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let identity = ProcessIdentity {
        os_pid: pid,
        pgid: pid,
        parent_pid: std::process::id(),
        start_tvsec: u64::from(pid),
        start_tvusec: 0,
        executable: Some("fake-worker".into()),
    };
    unit_processes()
        .lock()
        .unwrap()
        .insert(pid, (identity, false));
    LedgerStore::new(home).register(pid, owner, "unit worker", "fake-worker")?;
    Ok(pid)
}
#[cfg(test)]
pub(crate) fn wait_fake_worker(home: &Path, pid: u32) -> io::Result<()> {
    loop {
        let done = unit_processes()
            .lock()
            .unwrap()
            .get(&pid)
            .filter(|p| p.1)
            .map(|p| p.0.clone());
        if let Some(identity) = done {
            LedgerStore::new(home).reaped(&identity)?;
            unit_processes().lock().unwrap().remove(&pid);
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
impl LedgerStore {
    pub fn new(home: &Path) -> Self {
        Self {
            dir: home.join("kernel"),
        }
    }
    fn edit<T>(&self, f: impl FnOnce(&mut SpawnLedger) -> io::Result<T>) -> io::Result<T> {
        fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.dir.join("spawn-ledger.lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let path = self.dir.join("spawn-ledger.json");
        let mut ledger = match fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => SpawnLedger::default(),
            Err(e) => return Err(e),
        };
        // ponytail: one cross-process lock; split by worker only if spawn volume warrants it.
        let result = f(&mut ledger);
        atomic_json(&path, &ledger)?;
        result
    }
    pub fn snapshot(&self) -> io::Result<SpawnLedger> {
        self.edit(|l| Ok(l.clone()))
    }
    pub fn initialize(&self, saved: &SpawnLedger) -> io::Result<()> {
        // Only recover from state when the authoritative sidecar does not exist.
        // Never resurrect entries already removed from an existing ledger.
        self.edit(|l| {
            if l.entries().is_empty() && !self.dir.join("spawn-ledger.json").exists() {
                *l = saved.clone();
            }
            Ok(())
        })
    }
    fn register(
        &self,
        pid: u32,
        owner: usize,
        purpose: &str,
        executable: &str,
    ) -> io::Result<ProcessIdentity> {
        self.edit(|l| {
            l.record_spawn(pid, owner, purpose, executable, boundary())?;
            Ok(l.entries()[&pid].identity.clone())
        })
    }
    fn reaped(&self, identity: &ProcessIdentity) -> io::Result<()> {
        self.edit(|l| {
            l.reaped(identity);
            Ok(())
        })
    }
    pub fn signal(&self, pid: i64, sig: i32, group: bool) -> io::Result<()> {
        self.edit(|l| {
            let result = if group {
                l.signal_group(pid, sig, boundary())
            } else {
                l.signal_pid(pid, sig, boundary())
            };
            if let Err(ref refusal) = result {
                let dir = self.dir.join("signal-refusals");
                fs::create_dir_all(&dir)?;
                let path = dir.join(format!(
                    "{}-{}.json",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(io::Error::other)?
                        .as_nanos()
                ));
                atomic_json(&path, refusal)?;
                eprintln!("watchdog: {refusal}");
            }
            result.map_err(io::Error::other)
        })
    }
    /// The daemon journals first, then acknowledges; a restart may replay a warning.
    pub fn refusals(&self) -> io::Result<Vec<(PathBuf, SignalRefusal)>> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(self.dir.join("signal-refusals")) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for e in entries {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "json") {
                out.push((
                    p.clone(),
                    serde_json::from_slice(&fs::read(p)?).map_err(io::Error::other)?,
                ));
            }
        }
        Ok(out)
    }
}
fn atomic_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    File::open(path.parent().unwrap())?.sync_all()
}
pub(super) fn signal(pid: i64, sig: i32, group: bool) -> io::Result<()> {
    let (home, _) = context()?;
    LedgerStore::new(&home).signal(pid, sig, group)
}

/// Owns the original Child so no caller can bypass registration or reaping.
pub struct TrackedChild {
    child: Option<Child>,
    store: LedgerStore,
    identity: ProcessIdentity,
    status: Option<ExitStatus>,
    pub stdin: Option<std::process::ChildStdin>,
    pub stdout: Option<std::process::ChildStdout>,
    pub stderr: Option<std::process::ChildStderr>,
}
impl TrackedChild {
    pub fn id(&self) -> u32 {
        self.identity.os_pid
    }
    pub fn signal_group(&mut self, signal: i32) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.store.signal(i64::from(self.id()), signal, true)?;
        self.try_wait()?;
        Ok(())
    }
    pub fn terminate(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGKILL)
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            self.store.reaped(&self.identity)?;
            return Ok(Some(status));
        }
        if !super::child_exited(self.id() as i32)? {
            return Ok(None);
        }
        // Retain the leader's PID until descendant cleanup has been authorized.
        // Tests use the fake boundary; no unit-test build performs a real signal.
        if super::group_members(self.id() as i32)?
            .iter()
            .any(|p| *p != self.id())
        {
            self.store
                .signal(i64::from(self.id()), libc::SIGKILL, true)?;
        }
        let status = self.child.as_mut().unwrap().wait()?;
        self.status = Some(status);
        self.store.reaped(&self.identity)?;
        Ok(Some(status))
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        loop {
            if let Some(s) = self.try_wait()? {
                return Ok(s);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        fn read(
            pipe: Option<impl Read + Send + 'static>,
        ) -> thread::JoinHandle<io::Result<Vec<u8>>> {
            thread::spawn(move || {
                let mut b = Vec::new();
                if let Some(mut p) = pipe {
                    p.read_to_end(&mut b)?;
                }
                Ok(b)
            })
        }
        let out = read(self.stdout.take());
        let err = read(self.stderr.take());
        let status = self.wait()?;
        Ok(Output {
            status,
            stdout: out
                .join()
                .map_err(|_| io::Error::other("stdout reader panicked"))??,
            stderr: err
                .join()
                .map_err(|_| io::Error::other("stderr reader panicked"))??,
        })
    }
    /// Long-lived kernel/notification children retain provenance and a background reaper.
    pub fn detach(self) {
        thread::spawn(move || {
            let mut child = self;
            let _ = child.wait();
        });
    }
}
impl Drop for TrackedChild {
    fn drop(&mut self) {
        if self.status.is_none() {
            self.stdin.take();
            if self.try_wait().is_ok_and(|s| s.is_some()) {
                return;
            }
            if self.terminate().is_ok() {
                let _ = self.wait();
            } else {
                // The child may have exited between the first probe and signal.
                // Reap it now if possible; keep live refused targets asynchronous.
                let _ = self.try_wait();
            }
            if self.status.is_none()
                && let Some(mut child) = self.child.take()
            {
                let store = self.store.clone();
                let identity = self.identity.clone();
                thread::spawn(move || {
                    if child.wait().is_ok() {
                        let _ = store.reaped(&identity);
                    }
                });
            }
        }
    }
}

pub trait CommandTracking {
    fn spawn_owned(&mut self) -> io::Result<TrackedChild>;
    fn output_owned(&mut self) -> io::Result<Output>;
    fn status_owned(&mut self) -> io::Result<ExitStatus>;
}
impl CommandTracking for Command {
    fn spawn_owned(&mut self) -> io::Result<TrackedChild> {
        let (mut home, mut owner) = context()?;
        for (k, v) in self.get_envs() {
            if k == "UNVRS_HOME"
                && let Some(v) = v
            {
                home = v.into();
            }
            if k == "UNVRS_DRIVEN_PID" {
                owner = v
                    .and_then(|v| v.to_str())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
            }
        }
        let store = LedgerStore::new(&home);
        // Fail before spawning if durable storage cannot be opened.
        store.snapshot()?;
        let executable = self.get_program().to_string_lossy().into_owned();
        let purpose = format!("command: {}", executable);
        self.process_group(0);
        let mut child = self.spawn()?;
        let identity = match store.register(child.id(), owner, &purpose, &executable) {
            Ok(identity) => identity,
            Err(e) => {
                // Registration failures never expose a PID and never authorize a kill.
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err(e);
            }
        };
        Ok(TrackedChild {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child: Some(child),
            store,
            identity,
            status: None,
        })
    }
    fn output_owned(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.spawn_owned()?.wait_with_output()
    }
    fn status_owned(&mut self) -> io::Result<ExitStatus> {
        self.spawn_owned()?.wait()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_fake_worker_and_corrupt_store_fail_closed() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/signal-store-unit-{}",
            std::process::id()
        ));
        let _scope = owner_scope(&home, 149);
        let store = LedgerStore::new(&home);
        let pid = fake_worker(&home, 149).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.entries()[&pid].owning_pid, 149);
        // A new store instance reloads durable provenance. The fake boundary records
        // this request, allowing the synthetic worker to be reaped without OS signals.
        LedgerStore::new(&home)
            .signal(i64::from(pid), libc::SIGTERM, true)
            .unwrap();
        wait_fake_worker(&home, pid).unwrap();
        store.initialize(&snapshot).unwrap();
        assert!(store.snapshot().unwrap().entries().is_empty());
        assert!(store.signal(-1, libc::SIGTERM, true).is_err());
        assert_eq!(store.refusals().unwrap().len(), 1);

        fs::write(home.join("kernel/spawn-ledger.json"), b"corrupt ledger").unwrap();
        let marker = home.join("never-spawned");
        assert!(Command::new("touch").arg(&marker).spawn_owned().is_err());
        assert!(!marker.exists());
        assert!(store.signal(i64::from(pid), libc::SIGTERM, true).is_err());
        fs::remove_dir_all(home).unwrap();
    }
}
