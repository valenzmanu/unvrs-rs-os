//! Signals require durable spawn provenance and a fresh OS identity.
mod ledger;
mod runtime;
pub use ledger::{
    ProcessIdentity, SignalRefusal, Signaller, SpawnEntry, SpawnLedger, UnixSignaller,
    VerifiedSignal,
};
pub use runtime::{CommandTracking, LedgerStore, OwnerScope, TrackedChild, owner_scope, set_home};
#[cfg(test)]
pub(crate) use runtime::{fake_worker, wait_fake_worker};
use std::{io, process::ExitStatus};

fn checked_pid(pid: i64) -> io::Result<libc::pid_t> {
    match libc::pid_t::try_from(pid) {
        Ok(pid) if pid > 1 => Ok(pid),
        _ => Err(io::Error::from_raw_os_error(libc::EINVAL)),
    }
}

// WNOWAIT proves child ownership without releasing the PID for reuse.
fn child_exited(pid: libc::pid_t) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { info.si_pid() } != 0)
    }
}

/// Destructive requests require durable spawn provenance and a fresh OS identity.
pub fn signal_group(pgid: i64, sig: i32) -> io::Result<()> {
    runtime::signal(pgid, sig, true)
}
pub fn signal_pid(pid: i64, sig: i32) -> io::Result<()> {
    runtime::signal(pid, sig, false)
}
/// Liveness observation never sends a signal to an unowned process.
pub fn process_exists(pid: i64) -> io::Result<()> {
    let pid = checked_pid(pid)?;
    UnixSignaller.identity(pid as u32).map(|_| ())
}
/// Read OS group membership without sending a signal.
fn group_members(pgid: i32) -> io::Result<Vec<u32>> {
    #[cfg(target_os = "macos")]
    {
        // proc_listpids returns a byte count; grow until the snapshot fits.
        let mut pids = vec![0i32; 64];
        loop {
            let bytes = (pids.len() * std::mem::size_of::<i32>()) as i32;
            let n = unsafe {
                libc::proc_listpids(
                    2, /* PROC_PGRP_ONLY: sys/proc_info.h */
                    pgid as u32,
                    pids.as_mut_ptr().cast(),
                    bytes,
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if n < bytes {
                return Ok(pids
                    .into_iter()
                    .filter(|p| *p > 1)
                    .map(|p| p as u32)
                    .collect());
            }
            pids.resize(pids.len() * 2, 0);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut pids = Vec::new();
        for e in std::fs::read_dir("/proc")? {
            if let Some(pid) = e?.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
                if UnixSignaller
                    .identity(pid)
                    .is_ok_and(|i| i.pgid == pgid as u32)
                {
                    pids.push(pid);
                }
            }
        }
        Ok(pids)
    }
}
pub fn probe_group(pgid: i64) -> io::Result<()> {
    let pgid = checked_pid(pgid)?;
    if pgid == unsafe { libc::getpgrp() } {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    if group_members(pgid)?.is_empty() {
        Err(io::Error::from_raw_os_error(libc::ESRCH))
    } else {
        Ok(())
    }
}

/// Compatibility entry point; only registered children can be reaped here.
pub fn try_wait_group(child: &mut TrackedChild) -> io::Result<Option<ExitStatus>> {
    child.try_wait()
}
