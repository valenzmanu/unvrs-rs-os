//! Real signals are confined to children spawned by this integration-test binary.
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};
use uke::signals::{CommandTracking, LedgerStore, Signaller, UnixSignaller, owner_scope};

#[cfg(target_os = "macos")]
fn wait_exited_unreaped(pid: u32) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        if unsafe { info.si_pid() } == pid as i32 {
            break;
        }
        assert!(std::time::Instant::now() < until, "child did not exit");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn zombie_group_signal_is_already_exited_without_refusal() {
    let home = std::env::temp_dir().join(format!("signal-zombie-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let _scope = owner_scope(&home, 157);
    let store = LedgerStore::new(&home);
    for tracked_call in [false, true] {
        let mut child = Command::new("sh")
            .args(["-c", "read ignored; exit 17"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn_owned()
            .unwrap();
        let pid = child.id();
        let identity = UnixSignaller.identity(pid).unwrap();
        assert!(!UnixSignaller.group_exited(&identity).unwrap());
        child.stdin.take();
        wait_exited_unreaped(pid);
        eprintln!("own child {pid} exited in group {pid}, verified without reaping");
        assert!(UnixSignaller.group_exited(&identity).unwrap());
        if tracked_call {
            child.signal_group(libc::SIGKILL).unwrap();
            assert!(
                store.snapshot().unwrap().entries().is_empty(),
                "tracked signal must reap"
            );
        } else {
            store.signal(i64::from(pid), libc::SIGKILL, true).unwrap();
        }
        assert!(store.refusals().unwrap().is_empty());
        // Shared signalling must retain the original exit status for the Child owner.
        assert_eq!(child.wait().unwrap().code(), Some(17));
        child.signal_group(libc::SIGKILL).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(17));
        assert!(store.snapshot().unwrap().entries().is_empty());
        assert!(UnixSignaller.identity(pid).is_err());
        assert!(store.refusals().unwrap().is_empty());
    }
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn durable_ownership_refusals_and_child_group_cleanup() {
    let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../target/signal-integration-{}",
        std::process::id()
    ));
    fs::create_dir_all(&home).unwrap();
    let _scope = owner_scope(&home, 149);
    let store = LedgerStore::new(&home);
    for pid in [
        -1,
        0,
        1,
        i64::from(UnixSignaller.own_pid()),
        i64::from(UnixSignaller.own_pgid()),
    ] {
        assert!(store.signal(pid, libc::SIGTERM, true).is_err());
    }
    assert_eq!(store.refusals().unwrap().len(), 5);

    // An actual child without a spawn entry is still refused. Close stdin to end
    // it naturally; this test never falls back to an untracked kill.
    let mut outsider = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    assert!(
        store
            .signal(i64::from(outsider.id()), libc::SIGTERM, true)
            .is_err()
    );
    assert!(outsider.try_wait().unwrap().is_none());
    outsider.stdin.take();
    assert!(outsider.wait().unwrap().success());

    let mut child = Command::new("sleep").arg("30").spawn_owned().unwrap();
    let pid = child.id();
    let ledger = store.snapshot().unwrap();
    let entry = &ledger.entries()[&pid];
    assert_eq!(entry.owning_pid, 149);
    assert_eq!(entry.identity.pgid, pid);
    assert!(entry.identity.start_tvsec > 0);
    assert!(!entry.executable.is_empty());
    let state = uke::State {
        spawn_ledger: ledger,
        ..Default::default()
    };
    state.save(&home.join("kernel/state.json")).unwrap();
    let saved = uke::State::load(&home.join("kernel/state.json")).unwrap();
    let restarted = LedgerStore::new(&home);
    restarted.initialize(&saved.spawn_ledger).unwrap();
    restarted
        .signal(i64::from(pid), libc::SIGTERM, true)
        .unwrap();
    assert!(!child.wait().unwrap().success());
    assert!(!restarted.snapshot().unwrap().entries().contains_key(&pid));
    // A stale state snapshot must never restore an already-reaped entry.
    restarted.initialize(&saved.spawn_ledger).unwrap();
    assert!(!restarted.snapshot().unwrap().entries().contains_key(&pid));

    // Keep the exited leader unreaped until its descendants have been killed.
    let mut leader = Command::new("sh")
        .args(["-c", "sleep 30 & echo $!; exit 0"])
        .stdout(Stdio::piped())
        .spawn_owned()
        .unwrap();
    let group = leader.id();
    let mut line = String::new();
    BufReader::new(leader.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.trim().parse::<u32>().unwrap() > 1);
    #[cfg(target_os = "macos")]
    {
        wait_exited_unreaped(group);
        let identity = UnixSignaller.identity(group).unwrap();
        assert!(
            !UnixSignaller.group_exited(&identity).unwrap(),
            "a zombie leader cannot hide live descendants"
        );
    }
    assert!(leader.wait().unwrap().success());
    assert!(!store.snapshot().unwrap().entries().contains_key(&group));
    #[cfg(target_os = "macos")]
    {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while uke::signals::probe_group(i64::from(group)).is_ok()
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(uke::signals::probe_group(i64::from(group)).is_err());
    }

    // The original checkpoint timeout regression now runs as an integration
    // test: git and its filter are spawned by this binary in a tracked group.
    let wt = home.join("wt");
    fs::create_dir_all(&wt).unwrap();
    for args in [
        vec!["init", "-q", "-b", "work"],
        vec!["config", "filter.hang.clean", "sleep 3; touch escaped; cat"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&wt)
                .args(args)
                .output_owned()
                .unwrap()
                .status
                .success()
        );
    }
    fs::write(wt.join(".gitattributes"), "*.txt filter=hang\n").unwrap();
    fs::write(wt.join("work.txt"), "valuable work").unwrap();
    unsafe {
        std::env::set_var("UNVRS_CHECKPOINT_SECS", "1");
    }
    let started = std::time::Instant::now();
    let error = uke::git_checkpoint(&wt, &home, 149, "timeout").expect_err("filter must time out");
    assert!(format!("{error:#}").contains("timed out"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    std::thread::sleep(std::time::Duration::from_millis(3100));
    assert!(!wt.join("escaped").exists(), "filter survived timeout");
    assert_eq!(
        fs::read_to_string(wt.join("work.txt")).unwrap(),
        "valuable work"
    );
    assert!(store.snapshot().unwrap().entries().is_empty());
    fs::remove_dir_all(home).unwrap();
}
