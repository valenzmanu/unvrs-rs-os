//! Durable spawn identities. A PID alone never authorizes a signal.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, io};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub os_pid: u32,
    pub pgid: u32,
    pub parent_pid: u32,
    pub start_tvsec: u64,
    pub start_tvusec: u64,
    /// An exited process may no longer have an executable path in the OS table.
    pub executable: Option<String>,
}

impl ProcessIdentity {
    fn same_process(&self, other: &Self) -> bool {
        self.os_pid == other.os_pid
            && self.pgid == other.pgid
            && self.start_tvsec == other.start_tvsec
            && self.start_tvusec == other.start_tvusec
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnEntry {
    pub identity: ProcessIdentity,
    pub executable: String,
    /// Zero identifies kernel housekeeping; otherwise this is the owning UNVRS PID.
    pub owning_pid: usize,
    pub purpose: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpawnLedger {
    entries: BTreeMap<u32, SpawnEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignalRefusal {
    pub os_pid: i64,
    pub signal: i32,
    pub owning_pid: Option<usize>,
    pub reason: String,
}

impl fmt::Display for SignalRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "signal {} to PID {} refused: {}",
            self.signal, self.os_pid, self.reason
        )
    }
}
impl std::error::Error for SignalRefusal {}

/// Constructed only after a ledger entry matches a fresh OS identity.
/// Fields remain private so callers cannot manufacture an authorized signal.
pub struct VerifiedSignal {
    identity: ProcessIdentity,
    signal: i32,
    group: bool,
}
impl VerifiedSignal {
    pub fn target(&self) -> i32 {
        let pid = self.identity.os_pid as i32;
        if self.group { -pid } else { pid }
    }
    pub fn signal(&self) -> i32 {
        self.signal
    }
}

/// OS boundary; unit tests supply identities and record requests without signalling.
pub trait Signaller: Send + Sync {
    fn own_pid(&self) -> u32;
    fn own_pgid(&self) -> u32;
    fn identity(&self, os_pid: u32) -> io::Result<ProcessIdentity>;
    fn send(&self, signal: VerifiedSignal) -> io::Result<()>;
    /// Prove an owned leader and its entire group have exited; never reap here.
    fn group_exited(&self, _identity: &ProcessIdentity) -> io::Result<bool> {
        Ok(false)
    }
}

pub struct UnixSignaller;
impl Signaller for UnixSignaller {
    fn own_pid(&self) -> u32 {
        std::process::id()
    }
    fn own_pgid(&self) -> u32 {
        unsafe { libc::getpgrp() as u32 }
    }
    fn identity(&self, os_pid: u32) -> io::Result<ProcessIdentity> {
        process_info(os_pid).map(|(identity, _)| identity)
    }
    fn group_exited(&self, identity: &ProcessIdentity) -> io::Result<bool> {
        let pid = super::checked_pid(i64::from(identity.os_pid))?;
        if identity.parent_pid != self.own_pid() || identity.pgid != identity.os_pid {
            return Ok(false);
        }
        match process_info(identity.os_pid) {
            Ok((current, _))
                if identity.same_process(&current) && current.parent_pid == self.own_pid() => {}
            Ok(_) => return Ok(false),
            Err(e) if gone(&e) => return Ok(super::group_members(pid)?.is_empty()),
            Err(e) => return Err(e),
        }
        // WNOWAIT retains the original Child's status and protects against PID reuse.
        match super::child_exited(pid) {
            Ok(false) => return Ok(false),
            Ok(true) => {}
            Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                return Ok(process_info(identity.os_pid).is_err_and(|e| gone(&e))
                    && super::group_members(identity.pgid as i32)?.is_empty());
            }
            Err(e) => return Err(e),
        }
        for pid in super::group_members(identity.pgid as i32)? {
            match process_info(pid) {
                Ok((current, exited)) => {
                    if !exited || (pid == identity.os_pid && !identity.same_process(&current)) {
                        return Ok(false);
                    }
                }
                Err(e) if gone(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
    fn send(&self, request: VerifiedSignal) -> io::Result<()> {
        // Recheck at the syscall boundary as well as at ledger admission.
        let current = self.identity(request.identity.os_pid)?;
        if !request.identity.same_process(&current) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process identity changed before signal",
            ));
        }
        super::checked_pid(i64::from(current.os_pid))?;
        if current.os_pid == self.own_pid()
            || current.pgid == self.own_pgid()
            || current.pgid != current.os_pid
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe signal group",
            ));
        }
        #[cfg(test)]
        {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unit tests must inject a fake Signaller",
            ))
        }
        #[cfg(not(test))]
        if unsafe { libc::kill(request.target(), request.signal()) } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl SpawnLedger {
    pub fn entries(&self) -> &BTreeMap<u32, SpawnEntry> {
        &self.entries
    }

    /// Call immediately after spawn, using the returned Child's ID. Persist before
    /// publishing that PID to a worker or admitting any signal for it.
    pub fn record_spawn(
        &mut self,
        os_pid: u32,
        owning_pid: usize,
        purpose: &str,
        executable: &str,
        signaller: &dyn Signaller,
    ) -> io::Result<()> {
        super::checked_pid(i64::from(os_pid))?;
        let identity = signaller.identity(os_pid)?;
        if identity.os_pid != os_pid
            || identity.pgid != os_pid
            || identity.pgid == signaller.own_pgid()
            || identity.parent_pid != signaller.own_pid()
            || identity.start_tvsec == 0
            || identity.start_tvusec >= 1_000_000
            || purpose.is_empty()
            || executable.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "spawn is not an identified child in its own group",
            ));
        }
        let executable = identity
            .executable
            .clone()
            .unwrap_or_else(|| executable.into());
        if executable.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "spawn executable is empty",
            ));
        }
        self.entries.insert(
            os_pid,
            SpawnEntry {
                identity,
                executable,
                owning_pid,
                purpose: purpose.into(),
            },
        );
        Ok(())
    }

    /// Remove only the same spawn instance, so a late reap cannot erase a reused PID.
    pub fn reaped(&mut self, identity: &ProcessIdentity) -> bool {
        if self
            .entries
            .get(&identity.os_pid)
            .is_some_and(|e| e.identity.same_process(identity))
        {
            self.entries.remove(&identity.os_pid);
            true
        } else {
            false
        }
    }

    pub fn signal_group(
        &mut self,
        os_pid: i64,
        signal: i32,
        signaller: &dyn Signaller,
    ) -> Result<(), SignalRefusal> {
        self.signal(os_pid, signal, true, signaller)
    }
    pub fn signal_pid(
        &mut self,
        os_pid: i64,
        signal: i32,
        signaller: &dyn Signaller,
    ) -> Result<(), SignalRefusal> {
        self.signal(os_pid, signal, false, signaller)
    }

    fn signal(
        &mut self,
        os_pid: i64,
        signal: i32,
        group: bool,
        signaller: &dyn Signaller,
    ) -> Result<(), SignalRefusal> {
        let owning_pid = u32::try_from(os_pid)
            .ok()
            .and_then(|p| self.entries.get(&p))
            .map(|e| e.owning_pid);
        let refusal = |reason: String| SignalRefusal {
            os_pid,
            signal,
            owning_pid,
            reason,
        };
        let pid = super::checked_pid(os_pid).map_err(|e| refusal(e.to_string()))? as u32;
        if pid == signaller.own_pid() || pid == signaller.own_pgid() {
            self.entries.remove(&pid);
            return Err(refusal("target is our process or group".into()));
        }
        let entry = self
            .entries
            .get(&pid)
            .cloned()
            .ok_or_else(|| refusal("target is absent from the spawn ledger".into()))?;
        let current = match signaller.identity(pid) {
            Ok(identity) => identity,
            Err(e) => {
                self.entries.remove(&pid);
                return Err(refusal(format!("cannot reverify identity: {e}")));
            }
        };
        if !entry.identity.same_process(&current) || current.pgid != pid {
            self.entries.remove(&pid);
            return Err(refusal(
                "process start time or group identity changed; ledger entry dropped".into(),
            ));
        }
        let own_child = current.parent_pid == signaller.own_pid();
        if let Err(e) = signaller.send(VerifiedSignal {
            identity: current,
            signal,
            group,
        }) {
            if group && own_child && e.raw_os_error() == Some(libc::EPERM) {
                // macOS can deny a signal before the exiting child becomes waitable.
                // Bound the wait to 50 ms; only a fresh proof of group exit is benign.
                for attempt in 0..=5 {
                    match signaller.group_exited(&entry.identity) {
                        Ok(true) => {
                            // The Child owner reaps; retain its original wait status.
                            eprintln!("debug: signal {signal} to group {pid}: already exited");
                            return Ok(());
                        }
                        Ok(false) if attempt < 5 => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        _ => break,
                    }
                }
            }
            if matches!(
                e.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::NotFound
            ) || e.raw_os_error() == Some(libc::ESRCH)
            {
                self.entries.remove(&pid);
            }
            return Err(refusal(e.to_string()));
        }
        Ok(())
    }
}

fn gone(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH) || error.kind() == io::ErrorKind::NotFound
}

fn process_info(os_pid: u32) -> io::Result<(ProcessIdentity, bool)> {
    let pid = super::checked_pid(i64::from(os_pid))?;
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                1,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if n != size {
            let error = io::Error::last_os_error();
            return Err(if n <= 0 && error.raw_os_error().is_some_and(|e| e != 0) {
                error
            } else {
                io::Error::new(io::ErrorKind::InvalidData, "incomplete OS process identity")
            });
        }
        if info.pbi_pid != os_pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "OS process identity mismatched PID",
            ));
        }
        Ok((
            ProcessIdentity {
                os_pid,
                pgid: info.pbi_pgid,
                parent_pid: info.pbi_ppid,
                start_tvsec: info.pbi_start_tvsec,
                start_tvusec: info.pbi_start_tvusec,
                executable: crate::procinfo::path(os_pid),
            },
            info.pbi_status == libc::SZOMB,
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let boot = std::fs::read_to_string("/proc/stat")?;
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid system clock tick frequency",
            ));
        }
        let mut identity = linux_identity(os_pid, &stat, &boot, hz as u64)?;
        identity.executable = crate::procinfo::path(os_pid);
        let exited = stat
            .rsplit_once(')')
            .is_some_and(|(_, tail)| tail.split_whitespace().next() == Some("Z"));
        Ok((identity, exited))
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_identity(os_pid: u32, stat: &str, boot: &str, hz: u64) -> io::Result<ProcessIdentity> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed process start identity",
        )
    };
    let (head, tail) = stat.rsplit_once(')').ok_or_else(invalid)?;
    let recorded_pid = head
        .split_whitespace()
        .next()
        .ok_or_else(invalid)?
        .parse::<u32>()
        .map_err(|_| invalid())?;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let number = |at: usize| -> io::Result<u64> {
        fields
            .get(at)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())
    };
    let boot: u64 = boot
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let ticks = number(19)?;
    if recorded_pid != os_pid || hz == 0 {
        return Err(invalid());
    }
    Ok(ProcessIdentity {
        os_pid,
        pgid: u32::try_from(number(2)?).map_err(|_| invalid())?,
        parent_pid: u32::try_from(number(1)?).map_err(|_| invalid())?,
        start_tvsec: boot.checked_add(ticks / hz).ok_or_else(invalid)?,
        start_tvusec: (ticks % hz).checked_mul(1_000_000).ok_or_else(invalid)? / hz,
        executable: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeSignaller {
        identity: Mutex<Option<ProcessIdentity>>,
        sent: Mutex<Vec<(i32, i32)>>,
        fail_send: bool,
        send_error: Option<i32>,
        exited: bool,
        send_calls: std::sync::atomic::AtomicUsize,
        exit_after_checks: Option<usize>,
        exit_checks: std::sync::atomic::AtomicUsize,
    }
    impl Signaller for FakeSignaller {
        fn own_pid(&self) -> u32 {
            20
        }
        fn own_pgid(&self) -> u32 {
            10
        }
        fn identity(&self, _: u32) -> io::Result<ProcessIdentity> {
            self.identity
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ESRCH))
        }
        fn send(&self, signal: VerifiedSignal) -> io::Result<()> {
            self.send_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = self.send_error {
                return Err(io::Error::from_raw_os_error(error));
            }
            if self.fail_send {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "identity changed at syscall boundary",
                ));
            }
            self.sent
                .lock()
                .unwrap()
                .push((signal.target(), signal.signal()));
            Ok(())
        }
        fn group_exited(&self, _: &ProcessIdentity) -> io::Result<bool> {
            let check = self
                .exit_checks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.exited || self.exit_after_checks.is_some_and(|after| check >= after))
        }
    }
    fn new_fake() -> FakeSignaller {
        FakeSignaller {
            identity: Mutex::new(Some(ProcessIdentity {
                os_pid: 42,
                pgid: 42,
                parent_pid: 20,
                start_tvsec: 100,
                start_tvusec: 7,
                executable: Some("/usr/bin/worker".into()),
            })),
            sent: Default::default(),
            fail_send: false,
            send_error: None,
            exited: false,
            send_calls: Default::default(),
            exit_after_checks: None,
            exit_checks: Default::default(),
        }
    }
    fn recorded(fake: &FakeSignaller) -> SpawnLedger {
        let mut ledger = SpawnLedger::default();
        ledger
            .record_spawn(42, 149, "worker turn", "/usr/bin/worker", fake)
            .unwrap();
        ledger
    }

    #[test]
    fn ledger_refuses_invalid_unknown_reused_and_regrouped_pids_without_real_signals() {
        let fake = new_fake();
        let mut ledger = recorded(&fake);
        for pid in [-1, 0, 1, 10, 20, 999, i64::from(u32::MAX), i64::MAX] {
            assert!(ledger.signal_group(pid, libc::SIGTERM, &fake).is_err());
            assert!(ledger.signal_pid(pid, libc::SIGTERM, &fake).is_err());
        }
        assert!(fake.sent.lock().unwrap().is_empty());
        ledger.signal_group(42, libc::SIGTERM, &fake).unwrap();
        ledger.signal_pid(42, libc::SIGKILL, &fake).unwrap();
        assert_eq!(
            *fake.sent.lock().unwrap(),
            vec![(-42, libc::SIGTERM), (42, libc::SIGKILL)]
        );
        fake.sent.lock().unwrap().clear();
        for change in [
            "start seconds",
            "start microseconds",
            "group",
            "pid",
            "gone",
        ] {
            *fake.identity.lock().unwrap() = new_fake().identity.into_inner().unwrap();
            ledger = recorded(&fake);
            let mut identity = fake.identity.lock().unwrap();
            match change {
                "start seconds" => identity.as_mut().unwrap().start_tvsec += 1,
                "start microseconds" => identity.as_mut().unwrap().start_tvusec += 1,
                "group" => identity.as_mut().unwrap().pgid = 41,
                "pid" => identity.as_mut().unwrap().os_pid = 43,
                "gone" => *identity = None,
                _ => unreachable!(),
            }
            drop(identity);
            assert!(
                ledger.signal_group(42, libc::SIGTERM, &fake).is_err(),
                "{change}"
            );
            assert!(ledger.entries().is_empty(), "{change}");
            assert!(fake.sent.lock().unwrap().is_empty(), "{change}");
        }
        let mut late = new_fake();
        let mut ledger = recorded(&late);
        late.fail_send = true;
        assert!(ledger.signal_group(42, libc::SIGTERM, &late).is_err());
        assert!(ledger.entries().is_empty());
        assert!(late.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn eperm_is_benign_only_for_a_proven_exited_owned_group() {
        for (error, group, exited, parent, benign) in [
            (libc::EPERM, true, true, 20, true),
            (libc::EPERM, true, false, 20, false),
            (libc::EPERM, false, true, 20, false),
            (libc::EPERM, true, true, 99, false),
            (libc::EACCES, true, true, 20, false),
        ] {
            let mut fake = new_fake();
            let mut ledger = recorded(&fake);
            fake.send_error = Some(error);
            fake.exited = exited;
            fake.identity.lock().unwrap().as_mut().unwrap().parent_pid = parent;
            let result = if group {
                ledger.signal_group(42, libc::SIGKILL, &fake)
            } else {
                ledger.signal_pid(42, libc::SIGKILL, &fake)
            };
            assert_eq!(
                result.is_ok(),
                benign,
                "{error} group={group} exited={exited} parent={parent}"
            );
            if let Err(refusal) = result {
                assert_eq!(refusal.owning_pid, Some(149));
                assert_eq!(
                    refusal.reason,
                    io::Error::from_raw_os_error(error).to_string()
                );
            }
            // Neither suppression nor permission refusal consumes the owner's wait status.
            assert!(ledger.entries().contains_key(&42));
            assert!(fake.sent.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn eperm_waits_for_exit_proof_without_resending_or_reaping() {
        let mut fake = new_fake();
        let mut ledger = recorded(&fake);
        fake.send_error = Some(libc::EPERM);
        fake.exit_after_checks = Some(1);
        ledger.signal_group(42, libc::SIGKILL, &fake).unwrap();
        assert_eq!(
            fake.exit_checks.load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        assert_eq!(
            fake.send_calls.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(fake.sent.lock().unwrap().is_empty());
        assert!(ledger.entries().contains_key(&42));
    }

    #[test]
    fn ledger_survives_state_serialization_and_reap_checks_spawn_identity() {
        let fake = new_fake();
        let mut state = crate::State {
            spawn_ledger: recorded(&fake),
            ..Default::default()
        };
        let encoded = serde_json::to_string(&state).unwrap();
        state = serde_json::from_str(&encoded).unwrap();
        let entry = state.spawn_ledger.entries()[&42].clone();
        assert_eq!(
            (entry.owning_pid, entry.purpose.as_str()),
            (149, "worker turn")
        );
        state
            .spawn_ledger
            .signal_group(42, libc::SIGTERM, &fake)
            .unwrap();
        let mut wrong = entry.identity.clone();
        wrong.start_tvusec += 1;
        assert!(!state.spawn_ledger.reaped(&wrong));
        assert!(state.spawn_ledger.reaped(&entry.identity));
        assert!(state.spawn_ledger.entries().is_empty());
        let old: crate::State =
            serde_json::from_str(r#"{"next_pid":1,"pids":{},"threads":{},"mail_seq":0,"seq":0}"#)
                .unwrap();
        assert!(old.spawn_ledger.entries().is_empty());
    }

    #[test]
    fn registration_refuses_foreign_children_and_shared_groups() {
        let fake = new_fake();
        for field in ["parent", "group", "start", "fraction"] {
            *fake.identity.lock().unwrap() = new_fake().identity.into_inner().unwrap();
            let mut identity = fake.identity.lock().unwrap();
            match field {
                "parent" => identity.as_mut().unwrap().parent_pid = 99,
                "group" => identity.as_mut().unwrap().pgid = 10,
                "start" => identity.as_mut().unwrap().start_tvsec = 0,
                "fraction" => identity.as_mut().unwrap().start_tvusec = 1_000_000,
                _ => unreachable!(),
            }
            drop(identity);
            let mut ledger = SpawnLedger::default();
            assert!(
                ledger
                    .record_spawn(42, 149, "worker", "/worker", &fake)
                    .is_err()
            );
            assert!(ledger.entries().is_empty());
            assert!(fake.sent.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn linux_start_identity_parses_names_with_spaces_and_parentheses() {
        let stat = "42 (a tricky ) name) S 20 42 42 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 12345";
        let identity = linux_identity(42, stat, "cpu 1 2\nbtime 100\n", 100).unwrap();
        assert_eq!(
            (
                identity.parent_pid,
                identity.pgid,
                identity.start_tvsec,
                identity.start_tvusec
            ),
            (20, 42, 223, 450_000)
        );
        assert!(linux_identity(43, stat, "btime 100", 100).is_err());
        assert!(linux_identity(42, stat, "btime 100", 0).is_err());
        assert!(linux_identity(42, "42 (short) S", "btime 100", 100).is_err());
    }
}
