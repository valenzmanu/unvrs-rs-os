use super::*;

/// A home with a legacy slot `0.8.0` live, as `unvrs install` leaves it.
fn home(name: &str) -> Paths {
    let home = std::env::temp_dir().join(format!("unvrs-deploy-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let legacy = home.join("versions/0.8.0");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("unvrs"), "legacy").unwrap();
    let p = Paths { home };
    link_bin(&p.bin(), &legacy.join("unvrs")).unwrap();
    fs::write(p.versions().join("current"), "0.8.0\n").unwrap();
    p
}

fn opts(reference: &str) -> Opts {
    Opts {
        reference: reference.into(),
        repo: PathBuf::from("/repo"),
        test: true,
        drain_secs: 4,
        force_requeue: false,
        allow_drop: false,
        dry_run: false,
    }
}

/// Refs map to commits `<ref>` padded to 40 hex-ish chars; gates fail for `bad` commits.
#[derive(Default)]
struct Fake {
    calls: Vec<String>,
    busy: Vec<Vec<u64>>,
    bad: Vec<String>,
    fail_build: bool,
    fail_test: bool,
    ancestry_error: Option<String>,
    checked_live: Option<String>,
    /// Slot ids whose plugin refresh fails (a host keeps the old skills).
    bad_plugins: Vec<String>,
}

fn commit(reference: &str) -> String {
    format!("{reference:0<40}")
}

impl Steps for Fake {
    fn resolve(&mut self, _repo: &Path, reference: &str) -> Result<Resolved> {
        self.calls.push(format!("resolve {reference}"));
        Ok(Resolved {
            commit: commit(reference),
            branch: Some(reference.into()),
        })
    }
    fn check_ancestry(
        &mut self,
        _: &Path,
        _: &str,
        _: &Path,
        live_commit: Option<&str>,
    ) -> Result<()> {
        self.calls.push("ancestry".into());
        self.checked_live = live_commit.map(str::to_owned);
        if let Some(error) = &self.ancestry_error {
            bail!("{error}");
        }
        Ok(())
    }
    fn version_at(&mut self, _repo: &Path, _commit: &str) -> Result<String> {
        Ok("0.9.0".into())
    }
    fn checkout(&mut self, _repo: &Path, commit: &str, dir: &Path) -> Result<()> {
        self.calls.push("checkout".into());
        fs::create_dir_all(dir)?;
        fs::write(dir.join("SRC"), commit)?;
        Ok(())
    }
    fn build(&mut self, src: &Path, target: &Path, commit: &str, _r: &str) -> Result<PathBuf> {
        self.calls.push("build".into());
        ensure!(src.join("SRC").is_file(), "no checkout");
        ensure!(!self.fail_build, "cargo build failed");
        let bin = target.join("release/unvrs");
        fs::create_dir_all(bin.parent().unwrap())?;
        fs::write(&bin, format!("bin {commit}"))?;
        Ok(bin)
    }
    fn test(&mut self, _src: &Path, _target: &Path) -> Result<()> {
        self.calls.push("test".into());
        ensure!(!self.fail_test, "cargo test failed");
        Ok(())
    }
    fn drain(&mut self, on: bool) -> Result<Drained> {
        self.calls.push(format!("drain {on}"));
        let busy = if self.busy.len() > 1 {
            self.busy.remove(0)
        } else {
            self.busy.first().cloned().unwrap_or_default()
        };
        Ok(Drained {
            busy,
            ..Drained::default()
        })
    }
    fn census(&mut self) -> Result<usize> {
        Ok(3)
    }
    fn restart(&mut self) -> Result<String> {
        self.calls.push("restart".into());
        Ok("restarted".into())
    }
    fn plugins(&mut self, bin: &Path) -> Result<String> {
        assert_eq!(bin.file_name().unwrap(), "unvrs");
        let slot = bin.parent().unwrap().file_name().unwrap().to_string_lossy();
        self.calls
            .push(format!("plugins {}", &slot[..slot.len().min(20)]));
        ensure!(
            !self.bad_plugins.iter().any(|b| *b == slot),
            "codex skills lack unvrs:conduct"
        );
        Ok("claude: holds x; codex: holds x".into())
    }
    fn gate(&mut self, commit: Option<&str>, before: usize) -> Result<Vec<String>> {
        let c = commit.unwrap_or("legacy");
        self.calls.push(format!("gate {}", &c[..c.len().min(6)]));
        assert_eq!(before, 3);
        ensure!(
            !self.bad.iter().any(|b| b == c),
            "identity: kernel answers as another commit"
        );
        Ok(vec!["kernel ok".into()])
    }
    fn sleep(&mut self, _secs: u64) {
        self.calls.push("sleep".into());
    }
}

fn history(p: &Paths) -> Vec<Value> {
    fs::read_to_string(p.deploy().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn deploy_checks_live_ancestry_before_reuse_and_only_explicit_exceptions_bypass_it() {
    assert!(!Opts::parse(&["candidate"]).unwrap().allow_drop);
    assert!(
        Opts::parse(&["candidate", "--allow-drop"])
            .unwrap()
            .allow_drop
    );
    let p = home("ancestry-pipeline");
    deploy(&p, &mut Fake::default(), &opts("aaa")).unwrap();
    let live = fs::read_link(p.bin()).unwrap();
    // Neither stale marker may override the installed link's manifest.
    fs::write(p.versions().join("current"), "0.8.0\n").unwrap();
    fs::write(p.deploy().join("live.json"), "{\"commit\":\"stale\"}").unwrap();
    let mut allowed = opts("bbb");
    allowed.allow_drop = true;
    allowed.dry_run = true;
    assert_eq!(
        deploy(&p, &mut Fake::default(), &allowed).unwrap()["result"],
        "staged"
    );
    let refusal = format!("would drop live commit {}", commit("aaa"));
    for dry_run in [false, true] {
        for test in [false, true] {
            let mut s = Fake {
                ancestry_error: Some(refusal.clone()),
                ..Default::default()
            };
            let o = Opts {
                dry_run,
                test,
                ..opts("bbb")
            };
            let rec = deploy(&p, &mut s, &o).unwrap();
            assert_eq!(rec["result"], "failed", "{rec}");
            assert!(rec["reason"].as_str().unwrap().contains(&refusal));
            assert_eq!(s.calls, ["resolve bbb", "ancestry"]);
            assert_eq!(s.checked_live, Some(commit("aaa")));
            assert_eq!(fs::read_link(p.bin()).unwrap(), live);
            assert_eq!(p.current().as_deref(), Some("0.8.0"));
        }
    }
    allowed.dry_run = false;
    let mut s = Fake {
        ancestry_error: Some(refusal.clone()),
        ..Default::default()
    };
    let rec = deploy(&p, &mut s, &allowed).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert_eq!(rec["allow_drop"], true);
    assert!(!s.calls.iter().any(|c| c == "ancestry"));
    let mut s = Fake {
        ancestry_error: Some(refusal),
        ..Default::default()
    };
    let rec = rollback(&p, &mut s, 4).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert_eq!(rec["kind"], "rollback");
    assert!(!s.calls.iter().any(|c| c == "ancestry"));
    assert_eq!(fs::read_link(p.bin()).unwrap(), live);
    fs::remove_dir_all(p.home).unwrap();
}

#[test]
fn native_git_guard_names_missing_commits_and_checks_legacy_binary_identity() {
    let p = home("ancestry-git");
    let repo = p.home.join("repo");
    fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=deploy-test",
                "-c",
                "user.email=deploy-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=main"]);
    git(&["commit", "--allow-empty", "-m", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["commit", "--allow-empty", "-m", "live fix one"]);
    let first = git(&["rev-parse", "HEAD"]);
    git(&["commit", "--allow-empty", "-m", "live fix two"]);
    let live = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-b", "candidate", &base]);
    git(&["commit", "--allow-empty", "-m", "candidate work"]);
    let candidate = git(&["rev-parse", "HEAD"]);
    let mut s = real(&p).unwrap();
    let err = s
        .check_ancestry(&repo, &candidate, &p.bin(), Some(&live))
        .unwrap_err()
        .to_string();
    assert!(err.contains(&format!("{first} live fix one")), "{err}");
    assert!(err.contains(&format!("{live} live fix two")), "{err}");
    assert!(err.contains("--allow-drop") && !err.contains(&format!("{base} base")));
    // Exercise the real pipeline: refusal precedes even reading the candidate's Cargo.toml.
    fs::write(
        p.slot("0.8.0").join("manifest.json"),
        json!({"commit":live}).to_string(),
    )
    .unwrap();
    let o = Opts {
        repo: repo.clone(),
        ..opts(&candidate)
    };
    let rec = deploy(&p, &mut s, &o).unwrap();
    assert_eq!(rec["result"], "failed", "{rec}");
    assert!(
        rec["reason"]
            .as_str()
            .unwrap()
            .contains(&format!("{first} live fix one"))
    );
    assert!(!p.deploy().join("checkouts").exists());
    assert_eq!(p.current().as_deref(), Some("0.8.0"));
    s.check_ancestry(&repo, &live, &p.bin(), Some(&live))
        .unwrap();
    git(&["merge", "--no-ff", "--no-edit", "main"]);
    let merged = git(&["rev-parse", "HEAD"]);
    s.check_ancestry(&repo, &merged, &p.bin(), Some(&live))
        .unwrap();
    git(&["checkout", "--orphan", "unrelated"]);
    git(&["commit", "--allow-empty", "-m", "unrelated work"]);
    let unrelated = git(&["rev-parse", "HEAD"]);
    assert!(
        s.check_ancestry(&repo, &unrelated, &p.bin(), Some(&live))
            .is_err()
    );
    let unknown = "f".repeat(40);
    let err = s
        .check_ancestry(&repo, &merged, &p.bin(), Some(&unknown))
        .unwrap_err();
    assert!(format!("{err:#}").contains(&format!("cannot verify live commit {unknown}")));
    // Legacy slots have no manifest: the actual installed binary must identify its commit.
    let bin = p.slot("0.8.0").join("unvrs");
    // A hand-installed external link must also override the stale current manifest.
    let external = p.home.join("external-unvrs");
    fs::write(
        &external,
        format!("#!/bin/sh\nprintf 'commit: {live}\\n'\n"),
    )
    .unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        p.slot("0.8.0").join("manifest.json"),
        json!({"commit":base}).to_string(),
    )
    .unwrap();
    link_bin(&p.bin(), &external).unwrap();
    let rec = deploy(&p, &mut s, &o).unwrap();
    assert_eq!(rec["result"], "failed", "{rec}");
    assert!(
        rec["reason"]
            .as_str()
            .unwrap()
            .contains(&format!("{first} live fix one"))
    );
    assert_eq!(fs::read_link(p.bin()).unwrap(), external);
    link_bin(&p.bin(), &bin).unwrap();
    fs::write(
        &bin,
        format!("#!/bin/sh\nprintf 'version: 0.8.0\\ncommit: {live}\\n'\n"),
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    s.check_ancestry(&repo, &merged, &p.bin(), None).unwrap();
    assert!(s.check_ancestry(&repo, &candidate, &p.bin(), None).is_err());
    fs::write(
        &bin,
        "#!/bin/sh\nprintf 'version: 0.8.0\\ncommit: dev\\n'\n",
    )
    .unwrap();
    assert!(s.check_ancestry(&repo, &merged, &p.bin(), None).is_err());
    assert!(
        s.check_ancestry(&repo, &merged, &p.bin(), Some("--invalid"))
            .is_err()
    );
    s.check_ancestry(&repo, &merged, &p.home.join("no-installed-binary"), None)
        .unwrap();
    fs::remove_file(&bin).unwrap();
    assert!(s.check_ancestry(&repo, &merged, &p.bin(), None).is_err());
    fs::remove_dir_all(p.home).unwrap();
}

#[test]
fn slot_ids_and_cargo_versions() {
    assert_eq!(
        slot_id("0.8.0", "233f9cc79a1e0123456789abcdef0123456789ab"),
        "0.8.0+g233f9cc79a1e"
    );
    let toml = "[workspace]\nmembers = []\n\n[workspace.package]\n# c\nversion = \"0.8.0\"\nedition = \"2024\"\n";
    assert_eq!(cargo_version(toml).as_deref(), Some("0.8.0"));
    assert_eq!(cargo_version("[package]\nversion.workspace = true\n"), None);
    assert_eq!(cargo_version("[dependencies]\nversion = \"1\"\n"), None);
}

#[test]
fn deploy_stages_a_write_once_slot_swaps_and_goes_live() {
    let p = home("live");
    let mut s = Fake {
        busy: vec![vec![7], vec![]],
        ..Fake::default()
    };
    let rec = deploy(&p, &mut s, &opts("aaa")).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    let id = slot_id("0.9.0", &commit("aaa"));
    assert_eq!(rec["slot"], json!(id));
    assert_eq!(
        s.calls,
        [
            "resolve aaa",
            "ancestry",
            "checkout",
            "build",
            "test",
            "drain true",
            "sleep",
            "drain true",
            "restart",
            "plugins 0.9.0+gaaa000000000",
            "gate aaa000"
        ]
    );
    // The slot: binary 0555, manifest with identity and hash; no temp dir or checkout left.
    let slot = p.slot(&id);
    let mode = fs::metadata(slot.join("unvrs"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o555);
    let m = p.manifest(&id).unwrap();
    assert_eq!(m["commit"], json!(commit("aaa")));
    assert_eq!(m["branch"], "aaa");
    assert_eq!(m["tested"], true);
    assert_eq!(m["sha256"].as_str().unwrap().len(), 64);
    let names: Vec<String> = fs::read_dir(p.versions())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
    assert!(!p.deploy().join("checkouts/aaa000000000").exists());
    // Swapped: bin, current, previous, live.json, history, status.
    assert_eq!(fs::read_link(p.bin()).unwrap(), slot.join("unvrs"));
    assert_eq!(p.current().as_deref(), Some(id.as_str()));
    assert_eq!(p.previous().as_deref(), Some("0.8.0"));
    let live = p.live().unwrap();
    assert_eq!(live["slot"], json!(id));
    assert_eq!(live["previous"], "0.8.0");
    assert_eq!(live["commit"], json!(commit("aaa")));
    let h = history(&p);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["result"], "live");
    assert!(h[0].get("log").is_none());
    let status: Value =
        serde_json::from_str(&fs::read_to_string(p.deploy().join("status.json")).unwrap()).unwrap();
    assert_eq!(status["done"], true);
    assert!(status["log"].as_array().unwrap().len() > 5);

    // The same commit again: no rebuild, nothing swapped.
    let mut again = Fake::default();
    let rec = deploy(&p, &mut again, &opts("aaa")).unwrap();
    assert_eq!(rec["result"], "live");
    assert!(rec["reason"].as_str().unwrap().contains("already live"));
    assert_eq!(again.calls, ["resolve aaa", "ancestry"]);
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn a_failed_gate_rolls_back_to_the_previous_slot() {
    let p = home("rollback-auto");
    let mut s = Fake {
        bad: vec![commit("bbb")],
        ..Fake::default()
    };
    let rec = deploy(&p, &mut s, &opts("bbb")).unwrap();
    assert_eq!(rec["result"], "rolled_back", "{rec}");
    assert!(rec["reason"].as_str().unwrap().contains("identity"));
    assert_eq!(
        fs::read_link(p.bin()).unwrap(),
        p.slot("0.8.0").join("unvrs")
    );
    assert_eq!(p.current().as_deref(), Some("0.8.0"));
    assert_eq!(p.previous(), None, "previous restored to what it was");
    assert!(p.live().is_none(), "live.json only names gated slots");
    assert_eq!(
        &s.calls[s.calls.len() - 6..],
        [
            "restart",
            "plugins 0.9.0+gbbb000000000",
            "gate bbb000",
            "restart",
            "plugins 0.8.0",
            "gate legacy"
        ]
    );
    // The staged slot stays (write-once) for a later look.
    assert!(
        p.slot(&slot_id("0.9.0", &commit("bbb")))
            .join("unvrs")
            .is_file()
    );

    // Both gates fail: broken, with the by-hand commands.
    let mut s = Fake {
        bad: vec![commit("bbb"), "legacy".into()],
        ..Fake::default()
    };
    let rec = deploy(&p, &mut s, &opts("bbb")).unwrap();
    assert_eq!(rec["result"], "broken");
    assert!(rec["reason"].as_str().unwrap().contains("by hand: ln -sfn"));
    assert_eq!(
        s.calls[2], "drain true",
        "the existing slot is reused, not rebuilt"
    );
    assert_eq!(history(&p).len(), 2);
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn a_drain_timeout_aborts_without_swapping_unless_forced() {
    let p = home("drain");
    let mut s = Fake {
        busy: vec![vec![9]],
        ..Fake::default()
    };
    let rec = deploy(&p, &mut s, &opts("ccc")).unwrap();
    assert_eq!(rec["result"], "aborted", "{rec}");
    assert_eq!(s.calls.last().unwrap(), "drain false");
    assert!(!s.calls.iter().any(|c| c == "restart"));
    assert_eq!(
        fs::read_link(p.bin()).unwrap(),
        p.slot("0.8.0").join("unvrs")
    );
    assert_eq!(p.current().as_deref(), Some("0.8.0"));

    let mut forced = Fake {
        busy: vec![vec![9]],
        ..Fake::default()
    };
    let o = Opts {
        force_requeue: true,
        ..opts("ccc")
    };
    let rec = deploy(&p, &mut forced, &o).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert!(!forced.calls.iter().any(|c| c == "drain false"));
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn build_or_test_failures_leave_live_and_versions_untouched() {
    let p = home("fail");
    for (fail_build, fail_test) in [(true, false), (false, true)] {
        let mut s = Fake {
            fail_build,
            fail_test,
            ..Fake::default()
        };
        let rec = deploy(&p, &mut s, &opts("ddd")).unwrap();
        assert_eq!(rec["result"], "failed", "{rec}");
        assert!(!s.calls.iter().any(|c| c.starts_with("drain")));
    }
    let names: Vec<String> = fs::read_dir(p.versions())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.contains('+')).count(),
        0,
        "{names:?}"
    );
    assert!(names.iter().all(|n| !n.starts_with('.')));
    assert_eq!(
        fs::read_link(p.bin()).unwrap(),
        p.slot("0.8.0").join("unvrs")
    );
    assert!(!p.deploy().join("checkouts/ddd000000000").exists());
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn dry_run_stages_only_and_no_test_is_recorded() {
    let p = home("dry");
    let mut s = Fake::default();
    let o = Opts {
        dry_run: true,
        test: false,
        ..opts("eee")
    };
    let rec = deploy(&p, &mut s, &o).unwrap();
    assert_eq!(rec["result"], "staged");
    assert_eq!(s.calls, ["resolve eee", "ancestry", "checkout", "build"]);
    let id = slot_id("0.9.0", &commit("eee"));
    assert_eq!(p.manifest(&id).unwrap()["tested"], false);
    assert_eq!(p.current().as_deref(), Some("0.8.0"));
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn one_deploy_at_a_time() {
    let p = home("lock");
    let _held = Lock::take(&p.deploy().join("deploy.lock"), "deploy other").unwrap();
    let err = deploy(&p, &mut Fake::default(), &opts("fff")).unwrap_err();
    assert!(format!("{err:#}").contains("deploy other"), "{err:#}");
    assert!(
        !p.deploy().join("status.json").exists(),
        "the holder's record is not touched"
    );
    drop(_held);
    assert_eq!(
        deploy(&p, &mut Fake::default(), &opts("fff")).unwrap()["result"],
        "live"
    );
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn rollback_returns_to_the_slot_before_the_last_deploy() {
    let p = home("rollback");
    deploy(&p, &mut Fake::default(), &opts("aaa")).unwrap();
    deploy(&p, &mut Fake::default(), &opts("bbb")).unwrap();
    let a = slot_id("0.9.0", &commit("aaa"));
    let b = slot_id("0.9.0", &commit("bbb"));
    assert_eq!(p.live().unwrap()["previous"], json!(a));
    let mut s = Fake::default();
    let rec = rollback(&p, &mut s, 4).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert_eq!(rec["kind"], "rollback");
    assert!(s.calls.contains(&"gate aaa000".to_owned()));
    assert_eq!(fs::read_link(p.bin()).unwrap(), p.slot(&a).join("unvrs"));
    let live = p.live().unwrap();
    assert_eq!(live["slot"], json!(a));
    assert_eq!(live["previous"], json!(b));
    assert_eq!(p.current(), Some(a.clone()));
    assert_eq!(p.previous(), Some(b.clone()));
    let v = versions_json(&p).unwrap();
    assert_eq!(v["slots"].as_array().unwrap().len(), 3);
    assert_eq!(v["history"].as_array().unwrap().len(), 3);
    assert!(v["deploying"].is_null());
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn a_slot_that_appears_meanwhile_wins() {
    let p = home("race");
    let built = p.home.join("built");
    fs::write(&built, "one").unwrap();
    let note = write_slot(&p, "0.9.0+gabc", &built, json!({"id": "0.9.0+gabc"})).unwrap();
    assert!(note.contains("written"), "{note}");
    fs::write(&built, "two").unwrap();
    let note = write_slot(&p, "0.9.0+gabc", &built, json!({})).unwrap();
    assert!(note.contains("not reproducible"), "{note}");
    assert_eq!(
        fs::read_to_string(p.slot("0.9.0+gabc").join("unvrs")).unwrap(),
        "one"
    );
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn versions_text_prints_missing_values_bare() {
    let empty = json!({"bin": null, "current": null, "previous": null, "live": null,
        "slots": [], "history": [], "deploying": null});
    let out = render_versions(&empty);
    assert!(!out.contains("\"null\""), "{out}");
    assert!(out.starts_with("bin: -\nlive: none"), "{out}");
    let one = json!({"bin": "/h/versions/0.8.0+gabc/unvrs", "current": "0.8.0+gabc", "live": {"commit": "abc", "branch": null},
        "slots": [{"id": "0.8.0+gabc", "commit": "abc", "branch": null, "tested": true, "current": true, "previous": false}],
        "history": [], "deploying": null});
    let out = render_versions(&one);
    assert!(
        out.contains("live: \"0.8.0+gabc\" (commit \"abc\", branch -)"),
        "{out}"
    );
    assert!(
        out.contains("  \"0.8.0+gabc\",\"abc\",-,true,current"),
        "{out}"
    );
}

#[test]
fn the_link_names_the_live_slot_when_current_is_stale() {
    // A hand install switched the link to a newer legacy slot but left `current`.
    let p = home("stale-current");
    let hand = p.slot("0.8.0-hand");
    fs::create_dir_all(&hand).unwrap();
    fs::write(hand.join("unvrs"), "hand").unwrap();
    link_bin(&p.bin(), &hand.join("unvrs")).unwrap();
    assert_eq!(p.current().as_deref(), Some("0.8.0"));
    assert_eq!(p.live_slot().as_deref(), Some("0.8.0-hand"));
    let rec = deploy(&p, &mut Fake::default(), &opts("aaa")).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert_eq!(p.previous().as_deref(), Some("0.8.0-hand"));
    assert_eq!(p.live().unwrap()["previous"], "0.8.0-hand");
    // Rollback returns to the slot that really ran, not the stale `current`.
    let rec = rollback(&p, &mut Fake::default(), 4).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert_eq!(fs::read_link(p.bin()).unwrap(), hand.join("unvrs"));
    let _ = fs::remove_dir_all(&p.home);
}

#[test]
fn a_stale_plugin_fails_the_gate_and_rollback_restores_the_old_plugin() {
    // PID 199: the cutover went live while Claude Code and Codex kept the old skills.
    let p = home("plugins");
    let id = slot_id("0.9.0", &commit("ccc"));
    let mut s = Fake {
        bad_plugins: vec![id.clone()],
        ..Fake::default()
    };
    let rec = deploy(&p, &mut s, &opts("ccc")).unwrap();
    assert_eq!(rec["result"], "rolled_back", "{rec}");
    let reason = rec["reason"].as_str().unwrap();
    assert!(
        reason.contains("plugin: codex skills lack unvrs:conduct"),
        "{reason}"
    );
    // The gate never ran on the target: the plugin step failed first.
    assert!(
        !s.calls.contains(&"gate ccc000".to_owned()),
        "{:?}",
        s.calls
    );
    assert_eq!(
        &s.calls[s.calls.len() - 3..],
        ["restart", "plugins 0.8.0", "gate legacy"]
    );
    assert_eq!(p.current().as_deref(), Some("0.8.0"));

    // A good deploy, then `unvrs rollback`: the previous slot's plugin comes back too.
    let mut ok = Fake::default();
    assert_eq!(deploy(&p, &mut ok, &opts("ddd")).unwrap()["result"], "live");
    let mut back = Fake::default();
    let rec = rollback(&p, &mut back, 4).unwrap();
    assert_eq!(rec["result"], "live", "{rec}");
    assert!(
        back.calls.contains(&"plugins 0.8.0".to_owned()),
        "{:?}",
        back.calls
    );

    // A rollback whose plugin refresh fails is reported broken, not live.
    let mut ok = Fake::default();
    assert_eq!(deploy(&p, &mut ok, &opts("ddd")).unwrap()["result"], "live");
    let mut back = Fake {
        bad_plugins: vec!["0.8.0".into()],
        ..Fake::default()
    };
    let rec = rollback(&p, &mut back, 4).unwrap();
    assert_ne!(rec["result"], "live", "{rec}");
    let _ = fs::remove_dir_all(&p.home);
}
