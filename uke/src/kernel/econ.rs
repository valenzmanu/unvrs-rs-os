//! Kernel ownership of route receipts, held tasks and catalog refresh.
use super::{Inner, Kernel, now_ms};
use crate::drv_econ::{Config, Decision, Overrides, TaskFacts};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, sync::Arc};

impl Kernel {
    pub(crate) fn econ_command(
        &self,
        inner: &Inner,
        by: Option<usize>,
        args: &[String],
    ) -> Result<String> {
        let value = match args.first().map(String::as_str) {
            Some("show") if args.len() == 1 => json!({
                "config_path": self.u.root().join("econ.toml"),
                "config": Config::load(self.u.root())?,
                "catalogs": inner.st.econ_catalogs,
                "observed_now": now_ms(),
            }),
            Some("explain") if args.len() == 2 => {
                let pid: usize = args[1].parse().context("usage: econ explain <pid>")?;
                let target = inner.st.pid(pid)?;
                if let Some(by) = by {
                    let caller = inner.st.pid(by)?;
                    ensure!(
                        caller.rank == 1
                            || by == pid
                            || target.parent == by
                            || (caller.rank == 2 && caller.project == target.project),
                        "Refused: PID {pid} is outside your project"
                    );
                }
                target
                    .contract
                    .as_ref()
                    .and_then(|c| c.get("econ"))
                    .filter(|v| v.is_object())
                    .cloned()
                    .context("PID has no recorded econ route")?
            }
            Some("route") => {
                let mut contract = json!({});
                let mut asked = vec![];
                let mut dry_run = false;
                let mut it = args.iter().skip(1);
                while let Some(flag) = it.next() {
                    if flag == "--dry-run" {
                        ensure!(!dry_run, "duplicate --dry-run");
                        dry_run = true;
                        continue;
                    }
                    let key = match flag.as_str() {
                        "--on" | "--harness" => "harness",
                        "--model" => "model",
                        "--effort" => "effort",
                        "--kind" => "kind",
                        "--judgment" => "judgment",
                        "--thoroughness" => "thoroughness",
                        _ => anyhow::bail!("usage: unknown econ route flag {flag}"),
                    };
                    let value = it
                        .next()
                        .filter(|v| !v.starts_with("--") && !v.trim().is_empty())
                        .with_context(|| format!("usage: {flag} requires a value"))?;
                    if key == "harness" {
                        asked.push(value.clone());
                    } else {
                        ensure!(contract.get(key).is_none(), "duplicate {flag}");
                        contract[key] = json!(value);
                    }
                }
                ensure!(dry_run, "usage: econ route requires --dry-run");
                serde_json::to_value(self.econ_decision(
                    inner,
                    by.unwrap_or(0),
                    &contract,
                    &asked,
                )?)?
            }
            _ => anyhow::bail!(
                "usage: econ show | explain <pid> | route --dry-run [task routing flags]"
            ),
        };
        Ok(serde_json::to_string_pretty(&value)?)
    }

    pub(crate) fn econ_refresh_seconds(&self) -> u64 {
        Config::load(self.u.root()).map_or(300, |c| c.catalog.refresh_minutes * 60)
    }

    pub(crate) fn refresh_econ(self: &Arc<Self>) {
        let _owner = crate::signals::owner_scope(self.u.root(), 0);
        {
            self.lock().st.econ_refresh_requested = false;
        }
        let cadence = self.econ_refresh_seconds() * 1000;
        self.job_periodic("econ.catalog", cadence, super::PERIODIC_GRACE_MS);
        self.job_begin("econ.catalog", cadence + super::PERIODIC_GRACE_MS);
        let catalogs = self.drivers.catalogs(self.u.root());
        let errors = catalogs
            .iter()
            .filter_map(|(h, c)| c.error.as_ref().map(|e| format!("{h}: {e}")))
            .collect::<Vec<_>>();
        {
            let mut inner = self.lock();
            for (h, c) in &catalogs {
                if c.quota.is_object() {
                    inner.quota.insert(format!("{h}:default"), c.quota.clone());
                }
            }
            inner.st.econ_catalogs = catalogs;
            let seats = inner
                .st
                .seats()
                .iter()
                .filter(|p| p.state == "held")
                .map(|p| p.pid)
                .collect::<Vec<_>>();
            for pid in seats {
                inner.seat_backoff.remove(&pid);
            }
            let harnesses = inner.st.econ_catalogs.keys().cloned().collect::<Vec<_>>();
            self.event(
                &mut inner,
                "econ.catalog",
                json!({"harnesses":harnesses,"errors":errors}),
            );
            self.save(&mut inner);
            let held = inner
                .st
                .pids
                .values()
                .filter(|p| {
                    p.rank == 3
                        && p.state == "held"
                        && p.contract.as_ref().is_some_and(|c| c["econ"].is_object())
                        && p.driven
                            .as_ref()
                            .is_some_and(|d| d.turns == 0 && !d.busy && !d.recovering)
                })
                .map(|p| p.pid)
                .collect::<Vec<_>>();
            for pid in held {
                if let Err(e) = self.retry_econ(&mut inner, pid) {
                    self.event(
                        &mut inner,
                        "econ.error",
                        json!({"pid":pid,"error":format!("{e:#}")}),
                    );
                }
            }
            self.save(&mut inner);
        }
        self.job_finish(
            "econ.catalog",
            (!errors.is_empty()).then(|| errors.join("; ")),
            false,
        );
    }

    pub(crate) fn econ_decision(
        &self,
        inner: &Inner,
        by: usize,
        contract: &Value,
        asked: &[String],
    ) -> Result<Decision> {
        let harness = super::driven::requested_harness(asked)?;
        let overrides = Overrides {
            harness: harness.clone(),
            model: contract["model"]
                .as_str()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_owned),
            effort: super::driven::parse_effort(
                contract["effort"].as_str(),
                harness.as_deref().unwrap_or("codex"),
            )?,
        };
        let task = TaskFacts {
            rank: 3,
            kind: contract["kind"].as_str().map(str::to_owned),
            judgment: contract["judgment"].as_str().map(str::to_owned),
            thoroughness: contract["thoroughness"].as_str().map(str::to_owned),
        };
        crate::drv_econ::route(
            &Config::load(self.u.root())?,
            &task,
            &overrides,
            &inner.st.econ_catalogs,
            by,
            now_ms(),
        )
    }

    pub(crate) fn persist_econ(
        &self,
        inner: &mut Inner,
        pid: usize,
        decision: &Decision,
    ) -> Result<()> {
        let rec = inner.st.pid(pid)?.clone();
        let dir = if rec.rank <= 2 {
            let dir = self.u.root().join(format!("kernel/econ/pid-{pid}"));
            fs::create_dir_all(&dir)?;
            dir
        } else {
            rec.driven
                .as_ref()
                .context("route needs driven task")?
                .cwd
                .clone()
        };
        let mut contract = rec.contract.unwrap_or_else(|| json!({"rank":rec.rank}));
        let mut receipt = serde_json::to_value(decision)?;
        for (field, legacy) in [
            ("go_quote", "go"),
            ("go_source", "go_evidence"),
            ("done_when", "done_when"),
            ("go_scope", "go_scope"),
        ] {
            if let Some(value) = contract.get(field).or_else(|| contract.get(legacy)) {
                receipt[field] = value.clone();
            }
        }
        contract["econ"] = receipt.clone();
        if let Some(chosen) = &decision.chosen {
            contract["harness"] = json!(chosen.harness);
            // These are launch pins, not a claim that the delegator supplied overrides.
            contract["resolved_model"] = json!(chosen.model);
            contract["resolved_effort"] = json!(chosen.effort);
        }
        let temp = dir.join("contract.json.tmp");
        fs::write(&temp, serde_json::to_vec_pretty(&contract)?)?;
        fs::rename(temp, dir.join("contract.json"))?;
        inner.st.pid_mut(pid)?.contract = Some(contract);
        self.event(
            inner,
            "econ.route",
            json!({"pid":pid,"by":if rec.rank <= 2 { pid } else { rec.parent },"decision":receipt}),
        );
        ensure!(
            !inner.watchdog.active("journal"),
            "task retained; route journal persistence failed before launch"
        );
        Ok(())
    }

    pub(crate) fn econ_hold(
        &self,
        inner: &mut Inner,
        pid: usize,
        decision: &Decision,
    ) -> Result<()> {
        let rec = inner.st.pid(pid)?.clone();
        let reason = decision
            .hold_reason
            .as_deref()
            .unwrap_or("route unavailable");
        inner.st.pid_mut(pid)?.state = "held".into();
        inner.st.pid_mut(pid)?.next = format!("held: {reason}");
        if rec
            .contract
            .as_ref()
            .is_some_and(|c| c["econ_hold"].is_string())
        {
            return Ok(());
        }
        let skipped = decision
            .skipped
            .iter()
            .map(|s| {
                format!(
                    "{} {}: {}",
                    s.candidate.harness, s.candidate.model, s.reason
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let question = super::state::clip(
            &format!(
                "PID {pid} is held: {reason}. {skipped}. Restore an eligible route or give the delegator an explicit override; reasoning stays {:?}.",
                decision.reasoning
            ),
            1900,
        );
        let by = if rec.rank <= 2 { pid } else { rec.parent };
        let id = self.open_hold(
            inner,
            "decision",
            &question,
            vec![],
            None,
            rec.project.clone(),
            Some(by),
            None,
        )?;
        inner
            .st
            .pid_mut(pid)?
            .contract
            .as_mut()
            .context("missing task contract")?["econ_hold"] = json!(id);
        self.persist_econ(inner, pid, decision)?;
        let _ = self.wake_seat(inner, by, "blocked", &question, Some(pid), Some(id));
        Ok(())
    }

    /// Executable disappearance is an admission failure for both workers and seats.
    pub(crate) fn econ_ready(&self, inner: &mut Inner, decision: &Decision) -> Decision {
        let Some(chosen) = &decision.chosen else {
            return decision.clone();
        };
        if let Err(error) = self.drivers.harness_ready(&chosen.harness) {
            let reason = format!(
                "{} became unavailable before launch: {error:#}",
                chosen.harness
            );
            self.invalidate_econ(inner, &chosen.harness, &reason);
            let mut held = decision.clone();
            held.chosen = None;
            held.skipped.push(crate::drv_econ::Skipped {
                candidate: crate::drv_econ::Candidate {
                    harness: chosen.harness.clone(),
                    model: chosen.selector.clone(),
                    effort: Some(chosen.effort.clone()),
                    captain_fallback: Some(chosen.captain_fallback),
                },
                reason: reason.clone(),
                catalog_age_ms: Some(chosen.catalog_age_ms),
            });
            held.hold_reason = Some(reason);
            return held;
        }
        decision.clone()
    }

    pub(crate) fn admit_econ_seat(&self, inner: &mut Inner, pid: usize) -> Result<bool> {
        let rec = inner.st.pid(pid)?.clone();
        let prior_hold = rec
            .contract
            .as_ref()
            .and_then(|c| c["econ_hold"].as_str())
            .map(str::to_owned);
        if prior_hold.as_ref().is_some_and(|id| {
            inner
                .st
                .holds
                .get(id)
                .is_some_and(|h| h.status == "answered")
        }) {
            return Ok(false);
        }
        // Preserve an explicit seat env pin; prior thread harness alone is not policy.
        let model = std::env::var(format!("UNVRS_SEAT_{}_MODEL", rec.harness.to_uppercase()))
            .ok()
            .filter(|m| !m.trim().is_empty());
        let overrides = Overrides {
            harness: model.as_ref().map(|_| rec.harness.clone()),
            model,
            effort: None,
        };
        let decision = crate::drv_econ::route(
            &Config::load(self.u.root())?,
            &TaskFacts {
                rank: rec.rank,
                ..Default::default()
            },
            &overrides,
            &inner.st.econ_catalogs,
            pid,
            now_ms(),
        )?;
        let decision = self.econ_ready(inner, &decision);
        let Some(chosen) = &decision.chosen else {
            self.persist_econ(inner, pid, &decision)?;
            self.econ_hold(inner, pid, &decision)?;
            return Ok(false);
        };
        if let Some(id) =
            prior_hold.filter(|id| inner.st.holds.get(id).is_some_and(|h| h.status == "open"))
        {
            self.moot_hold(
                inner,
                &id,
                "fresh catalog admitted the detached seat at the required reasoning class",
                Some(pid),
            )?;
        }
        if let Some(contract) = inner
            .st
            .pid_mut(pid)?
            .contract
            .as_mut()
            .and_then(Value::as_object_mut)
        {
            contract.remove("econ_hold");
        }
        self.persist_econ(inner, pid, &decision)?;
        let rec = inner.st.pid_mut(pid)?;
        rec.harness = chosen.harness.clone();
        rec.state = "idle".into();
        rec.next = "handle pending wakes".into();
        self.save(inner);
        ensure!(
            !inner.dirty,
            "seat retained; state persistence failed before launch"
        );
        Ok(true)
    }

    pub(crate) fn launch_econ(
        self: &Arc<Self>,
        inner: &mut Inner,
        pid: usize,
        decision: &Decision,
    ) -> Result<()> {
        let ready = self.econ_ready(inner, decision);
        let Some(chosen) = &ready.chosen else {
            return self.econ_hold(inner, pid, &ready);
        };
        let decision = &ready;
        self.persist_econ(inner, pid, decision)?;
        let rec = inner.st.pid_mut(pid)?;
        rec.harness = chosen.harness.clone();
        let d = rec.driven.as_mut().context("not a driven worker")?;
        ensure!(
            !d.busy && !d.recovering && d.turns == 0,
            "route admission only starts an untouched task"
        );
        d.model = Some(chosen.model.clone());
        d.effort = Some(chosen.effort.clone());
        rec.state = "working".into();
        rec.next = "start the task".into();
        self.save(inner);
        if inner.dirty {
            inner.st.pid_mut(pid)?.state = "held".into();
            anyhow::bail!("task retained; state persistence failed before launch");
        }
        inner.running.insert(pid);
        let k = Arc::clone(self);
        std::thread::spawn(move || k.worker_loop(pid));
        Ok(())
    }

    fn retry_econ(self: &Arc<Self>, inner: &mut Inner, pid: usize) -> Result<()> {
        let rec = inner.st.pid(pid)?.clone();
        let contract = rec
            .contract
            .as_ref()
            .context("held task contract missing")?;
        let before: Decision = serde_json::from_value(contract["econ"].clone())?;
        let decision = crate::drv_econ::route(
            &Config::load(self.u.root())?,
            &before.inputs,
            &before.overrides,
            &inner.st.econ_catalogs,
            rec.parent,
            now_ms(),
        )?;
        if let Some(chosen) = decision.chosen.as_ref() {
            if let Some(id) = contract["econ_hold"].as_str()
                && inner
                    .st
                    .holds
                    .get(id)
                    .is_some_and(|h| h.status == "answered")
            {
                return Ok(());
            }
            self.launch_econ(inner, pid, &decision)?;
            if inner.st.pid(pid)?.state != "working" {
                return Ok(());
            }
            if let Some(id) = contract["econ_hold"].as_str() {
                let evidence = format!(
                    "fresh harness catalog admitted {} {} effort {} at {}",
                    chosen.harness,
                    chosen.model,
                    chosen.effort,
                    now_ms()
                );
                if inner.st.holds.get(id).is_some_and(|h| h.status == "open") {
                    self.moot_hold(inner, id, &evidence, Some(rec.parent))?;
                }
            }
            self.save(inner);
        }
        // Repeated unavailable reads leave the original call and receipt intact.
        Ok(())
    }

    pub(crate) fn invalidate_econ(&self, inner: &mut Inner, harness: &str, reason: &str) {
        if let Some(c) = inner.st.econ_catalogs.get_mut(harness) {
            c.error = Some(super::state::clip(reason, 300));
        }
        inner.st.econ_refresh_requested = true;
        self.event(
            inner,
            "econ.catalog.invalidated",
            json!({"harness":harness,"reason":super::state::clip(reason,300)}),
        );
    }
}

#[cfg(test)]
pub(crate) fn test_catalogs(
    harnesses: &[&str],
) -> std::collections::BTreeMap<String, crate::drv_econ::Catalog> {
    use crate::drv_econ::{Catalog, Model};
    let at = now_ms();
    harnesses
        .iter()
        .map(|h| {
            (
                (*h).into(),
                Catalog {
                    observed_at: at,
                    signed_in: Some(true),
                    quota_available: Some(true),
                    quota_observed_at: at,
                    models: vec![Model {
                        id: if *h == "codex" {
                            "gpt-6.1-sol"
                        } else {
                            "claude-opus-5-5"
                        }
                        .into(),
                        efforts: ["low", "medium", "high", "xhigh", "max"]
                            .map(str::to_owned)
                            .to_vec(),
                        is_default: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BriefFold, Drivers, FoldJob, SilentMapp, TurnRequest, TurnResult, Universe,
        drv_econ::Catalog,
    };
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    struct Evidence {
        catalogs: Mutex<BTreeMap<String, Catalog>>,
        turns: AtomicUsize,
        ready: AtomicBool,
    }
    impl Drivers for Evidence {
        fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
            anyhow::bail!("no model call in fixture")
        }
        fn catalogs(&self, _: &Path) -> BTreeMap<String, Catalog> {
            self.catalogs.lock().unwrap().clone()
        }
        fn harness_ready(&self, _: &str) -> Result<()> {
            ensure!(
                self.ready.load(Ordering::SeqCst),
                "fixture executable disappeared"
            );
            Ok(())
        }
        fn turn(&self, req: &TurnRequest, _: &dyn Fn(u32)) -> Result<TurnResult> {
            let env = |key: &str| {
                req.env
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v)
                    .unwrap()
            };
            let sidecar = PathBuf::from(env("UNVRS_HOME")).join(format!(
                "kernel/econ/pid-{}/contract.json",
                env("UNVRS_DRIVEN_PID")
            ));
            let receipt = if sidecar.exists() {
                sidecar
            } else {
                req.cwd.join("contract.json")
            };
            let contract: Value = serde_json::from_slice(&fs::read(receipt)?)?;
            ensure!(
                contract["econ"]["chosen"]["model"].as_str() == req.model.as_deref(),
                "receipt must precede turn"
            );
            ensure!(
                contract["econ"]["chosen"]["effort"].as_str() == req.effort.as_deref(),
                "effort receipt must precede turn"
            );
            self.turns.fetch_add(1, Ordering::SeqCst);
            Ok(TurnResult {
                text: "HANDOFF\ndeliverables:\n- none\ndecisions:\n- none\nlearnings:\n- none\nEND-HANDOFF\nUNVRS-RESULT: fixture complete".into(),
                model: req.model.clone(),
                effort: req.effort.clone(),
                effort_evidence: Some("fixture accepted configuration".into()),
                ..Default::default()
            })
        }
    }
    struct Rig {
        k: Arc<Kernel>,
        drivers: Arc<Evidence>,
        root: PathBuf,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn rig(tag: &str) -> Rig {
        let root = std::env::temp_dir().join(format!(
            "econ-admission-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let u = Universe::at(&root).unwrap();
        u.init().unwrap();
        fs::write(root.join("econ.toml"), crate::drv_econ::DEFAULT_CONFIG).unwrap();
        let drivers = Arc::new(Evidence {
            catalogs: Mutex::new(test_catalogs(&["codex", "claude"])),
            turns: AtomicUsize::new(0),
            ready: AtomicBool::new(true),
        });
        let k = Kernel::open(u, drivers.clone(), Arc::new(SilentMapp), now_ms()).unwrap();
        let mut inner = k.lock();
        k.create_project(&mut inner, "proj", "fixture", &[], "fixture")
            .unwrap();
        inner
            .st
            .captain_prompts
            .push(crate::kernel::state::CaptainPrompt {
                id: 1,
                pid: 1,
                thread: "claude:fixture".into(),
                project: None,
                at: now_ms(),
                text: "Ok, go".into(),
            });
        drop(inner);
        k.refresh_econ();
        Rig { k, drivers, root }
    }
    fn task(r: &Rig, mut contract: Value) -> usize {
        contract["go"] = json!("Ok, go");
        contract["done_when"] = json!("report delivered");
        r.k.task(&mut r.k.lock(), Some(2), contract, &[]).unwrap()
    }
    fn settle(r: &Rig, pid: usize) {
        let until = Instant::now() + Duration::from_secs(15);
        while r.k.lock().st.pid(pid).unwrap().state != "ended" {
            assert!(Instant::now() < until, "worker did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn detached_seats_hold_once_preserve_wakes_and_route_judge_with_away_cap() {
        let r = rig("seats");
        {
            let mut inner = r.k.lock();
            inner.st.away = Some(super::super::state::Away {
                cap: 1,
                ..Default::default()
            });
            for pid in [1, 2] {
                inner.st.pid_mut(pid).unwrap().harness.clear();
                r.k.wake_seat(&mut inner, pid, "note", "read one fact", None, None)
                    .unwrap();
            }
            inner.st.econ_catalogs.clear();
        }
        r.k.run_detached_seats();
        r.k.run_detached_seats();
        {
            let inner = r.k.lock();
            assert!(inner.running.is_empty());
            assert_eq!(inner.st.holds.len(), 2);
            assert_eq!(inner.st.away.as_ref().unwrap().runs, 0);
            for pid in [1, 2] {
                let rec = inner.st.pid(pid).unwrap();
                assert_eq!(rec.state, "held");
                assert!(rec.wakes.iter().all(|w| !w.acked && w.delivered.is_none()));
            }
        }
        let restarted = Kernel::open(
            Universe::at(&r.root).unwrap(),
            r.drivers.clone(),
            Arc::new(SilentMapp),
            now_ms(),
        )
        .unwrap();
        restarted.refresh_econ();
        restarted.run_detached_seats();
        let until = Instant::now() + Duration::from_secs(15);
        while !restarted.lock().running.is_empty() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        {
            let inner = restarted.lock();
            assert_eq!(inner.st.away.as_ref().unwrap().runs, 1);
            assert!(inner.st.holds.values().all(|h| h.status == "moot"));
            for pid in [1, 2] {
                let rec = inner.st.pid(pid).unwrap();
                let c = rec.contract.as_ref().unwrap();
                assert_eq!(c["econ"]["role"], "judge");
                assert_eq!(c["econ"]["reasoning"], "deep");
                assert_eq!(c["econ"]["chosen"]["harness"], "claude");
                assert!(c["econ_hold"].is_null());
                let d = rec.driven.as_ref().unwrap();
                assert_eq!(d.actual_model.as_deref(), Some("claude-opus-5-5"));
                assert_eq!(d.actual_effort.as_deref(), Some("high"));
            }
        }
        assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 2);
        // A new held run gets a new call; a mooted call cannot suppress it.
        restarted.lock().st.econ_catalogs.clear();
        restarted
            .wake_seat(
                &mut restarted.lock(),
                2,
                "note",
                "next instruction",
                None,
                None,
            )
            .unwrap();
        restarted.run_detached_seats();
        assert_eq!(restarted.lock().st.holds.len(), 3);
        assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 2);
        // No turn may start without its durable receipt.
        let failed = rig("seat-persist");
        failed
            .k
            .wake_seat(&mut failed.k.lock(), 2, "note", "read", None, None)
            .unwrap();
        let dir = failed.root.join("kernel/econ/pid-2/contract.json");
        fs::create_dir_all(&dir).unwrap();
        failed.k.run_detached_seats();
        assert!(failed.k.lock().running.is_empty());
        assert_eq!(failed.drivers.turns.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn econ_commands_inspect_the_same_route_without_dispatch() {
        let r = rig("cli");
        let args = |words: &[&str]| words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let run = |words: &[&str]| r.k.econ_command(&r.k.lock(), Some(2), &args(words));
        let before = (r.k.lock().st.pids.len(), r.k.journal.tail(100).len());
        let decision: Value = serde_json::from_str(
            &run(&[
                "route",
                "--dry-run",
                "--kind",
                "research",
                "--thoroughness",
                "high",
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(decision["role"], "research");
        assert_eq!(decision["reasoning"], "deep");
        assert_eq!(decision["chosen"]["harness"], "codex");
        assert_eq!(decision["chosen"]["captain_fallback"], true);
        assert!(
            decision["reason"]
                .as_str()
                .unwrap()
                .contains("captain fallback: gpt-6.1-sol at low")
        );
        let pinned: Value = serde_json::from_str(
            &run(&["route", "--dry-run", "--on", "claude", "--effort", "high"]).unwrap(),
        )
        .unwrap();
        assert_eq!(pinned["override_by"], 2);
        assert_eq!(pinned["chosen"]["harness"], "claude");
        assert_eq!(pinned["chosen"]["effort"], "high");
        r.k.lock()
            .st
            .econ_catalogs
            .get_mut("claude")
            .unwrap()
            .quota_available = None;
        let judge: Value =
            serde_json::from_str(&run(&["route", "--dry-run", "--kind", "decide"]).unwrap())
                .unwrap();
        assert_eq!(judge["role"], "judge");
        assert_eq!(judge["reasoning"], "deep");
        assert_eq!(judge["chosen"]["model"], "claude-opus-5-5");
        assert!(
            judge["reason"]
                .as_str()
                .unwrap()
                .contains("quota unknown, admitted")
        );
        let show: Value = serde_json::from_str(&run(&["show"]).unwrap()).unwrap();
        assert!(show["config"]["profiles"]["judge"].is_array());
        assert!(show["catalogs"]["codex"]["models"].is_array());
        for bad in [
            vec!["route"],
            vec!["route", "--dry-run", "--kind"],
            vec!["route", "--dry-run", "--kind", "unknown"],
            vec!["route", "--dry-run", "--thoroughness", "medium"],
            vec!["route", "--dry-run", "--model", " "],
            vec!["route", "--dry-run", "--on", "codex", "--harness", "claude"],
            vec!["route", "--dry-run", "--kind", "review", "--kind", "design"],
            vec!["route", "--dry-run", "--typo", "x"],
            vec!["show", "extra"],
            vec!["explain", "oops"],
        ] {
            assert!(run(&bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            before,
            (r.k.lock().st.pids.len(), r.k.journal.tail(100).len())
        );
        assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 0);
        let pid = task(&r, json!({"intent":"read","spec":"report"}));
        settle(&r, pid);
        let receipt: Value =
            serde_json::from_str(&run(&["explain", &pid.to_string()]).unwrap()).unwrap();
        assert_eq!(receipt["reasoning"], "deep");
        assert_eq!(receipt["go_quote"], "Ok, go");
        assert_eq!(receipt["done_when"], "report delivered");
        assert_eq!(receipt["go_source"]["thread"], "claude:fixture");
        assert_eq!(receipt["go_source"]["prompt_id"], 1);
        assert!(receipt["go_source"]["at"].as_u64().is_some());
        let contract: Value = serde_json::from_slice(
            &fs::read(
                r.root
                    .join(format!("projects/proj/tasks/pid-{pid}/contract.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(contract["econ"], receipt);
        for field in ["go_quote", "go_source", "done_when"] {
            assert_eq!(receipt[field], contract[field]);
        }
        assert!(
            r.k.journal
                .tail(200)
                .iter()
                .any(|e| e["kind"] == "econ.route" && e["pid"] == pid && e["decision"] == receipt)
        );
        let _: Decision = serde_json::from_value(receipt).unwrap(); // Retry still reads the route.
        r.k.create_project(&mut r.k.lock(), "other", "fixture", &[], "fixture")
            .unwrap();
        let other = r.k.lock().st.seat_of("other").unwrap();
        assert!(
            r.k.econ_command(
                &r.k.lock(),
                Some(other),
                &args(&["explain", &pid.to_string()])
            )
            .is_err()
        );
        assert!(
            r.k.econ_command(&r.k.lock(), Some(1), &args(&["explain", &pid.to_string()]))
                .is_ok()
        );
    }

    #[test]
    fn fresh_route_persists_before_launch_and_reports_accepted_axes() {
        for quick in [false, true] {
            let r = rig(if quick {
                "quick-receipt"
            } else {
                "fallback-receipt"
            });
            r.drivers
                .catalogs
                .lock()
                .unwrap()
                .get_mut("codex")
                .unwrap()
                .models
                .push(crate::drv_econ::Model {
                    id: "gpt-6.1-luna".into(),
                    efforts: vec!["medium".into(), "high".into()],
                    ..Default::default()
                });
            r.k.refresh_econ();
            let (model, effort, role) = if quick {
                ("gpt-6.1-luna", "medium", "research_quick")
            } else {
                ("gpt-6.1-sol", "low", "research")
            };
            let pid = task(
                &r,
                json!({"intent":"read one fact","spec":"report","kind":"research","thoroughness": if quick { "low" } else { "high" }}),
            );
            settle(&r, pid);
            let result: Value = serde_json::from_slice(
                &fs::read(
                    r.root
                        .join(format!("projects/proj/tasks/pid-{pid}/result.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(result["actual_model"], model);
            assert_eq!(result["actual_effort"], effort);
            let contract = &result["contract"];
            assert_eq!(contract["econ"]["role"], role);
            assert_eq!(contract["econ"]["inputs"]["kind"], "research");
            assert_eq!(contract["econ"]["chosen"]["captain_fallback"], !quick);
            assert_eq!(
                contract["econ"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("captain fallback: gpt-6.1-sol at low"),
                !quick
            );
            assert!(contract["model"].is_null());
            assert_eq!(contract["resolved_model"], model);
            assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 1);
            assert_eq!(
                r.k.journal
                    .tail(100)
                    .iter()
                    .filter(|e| e["kind"] == "econ.route" && e["pid"] == pid)
                    .count(),
                1
            );
        }
    }
    #[test]
    fn research_sonnet_high_receipt_precedes_kernel_launch() {
        let r = rig("sonnet-receipt");
        r.drivers
            .catalogs
            .lock()
            .unwrap()
            .get_mut("claude")
            .unwrap()
            .models
            .push(crate::drv_econ::Model {
                id: "claude-sonnet-5-5".into(),
                efforts: vec!["low".into(), "high".into()],
                ..Default::default()
            });
        r.k.refresh_econ();
        let pid = task(
            &r,
            json!({"intent":"research","spec":"report","kind":"research"}),
        );
        settle(&r, pid);
        let inner = r.k.lock();
        let rec = inner.st.pid(pid).unwrap();
        let contract = rec.contract.as_ref().unwrap();
        assert_eq!(contract["econ"]["role"], "research");
        assert_eq!(contract["econ"]["chosen"]["model"], "claude-sonnet-5-5");
        assert_eq!(contract["econ"]["chosen"]["effort"], "high");
        assert_eq!(contract["econ"]["chosen"]["captain_fallback"], false);
        assert_eq!(
            rec.driven.as_ref().unwrap().actual_model.as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            rec.driven.as_ref().unwrap().actual_effort.as_deref(),
            Some("high")
        );
        assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unavailable_route_survives_restart_opens_one_call_and_resumes_same_pid() {
        let r = rig("hold");
        for c in r.drivers.catalogs.lock().unwrap().values_mut() {
            c.quota_available = Some(false);
        }
        r.k.refresh_econ();
        let pid = task(&r, json!({"intent":"read","spec":"report"}));
        assert_eq!(r.k.lock().st.pid(pid).unwrap().state, "held");
        assert_eq!(r.k.snapshot()["workers"][0]["econ"]["role"], "worker");
        assert_eq!(r.k.snapshot()["workers"][0]["state"], "held");
        let snapshot = r.k.snapshot();
        let worker = &snapshot["workers"][0];
        assert_eq!(worker["go_quote"], "Ok, go");
        assert_eq!(worker["done_when"], "report delivered");
        assert_eq!(worker["go_source"]["prompt_id"], 1);
        for field in ["go_quote", "go_source", "done_when"] {
            assert_eq!(worker[field], worker["econ"][field]);
        }
        assert!(r.k.lock().running.is_empty());
        assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 0);
        assert_eq!(r.k.lock().st.holds.len(), 1);
        r.k.refresh_econ();
        r.k.refresh_econ();
        assert_eq!(r.k.lock().st.holds.len(), 1);
        let resumed = Kernel::open(
            Universe::at(&r.root).unwrap(),
            r.drivers.clone(),
            Arc::new(SilentMapp),
            now_ms(),
        )
        .unwrap();
        assert_eq!(resumed.lock().st.pid(pid).unwrap().state, "held");
        assert_eq!(
            resumed.snapshot()["workers"][0]["go_source"],
            worker["go_source"]
        );
        assert_eq!(resumed.lock().st.holds.len(), 1);
        *r.drivers.catalogs.lock().unwrap() = test_catalogs(&["codex", "claude"]);
        resumed.refresh_econ();
        let until = Instant::now() + Duration::from_secs(15);
        while resumed.lock().st.pid(pid).unwrap().state != "ended" {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 1);
        assert_eq!(resumed.lock().st.pids.len(), 3);
        assert_eq!(
            resumed.lock().st.holds.values().next().unwrap().status,
            "moot"
        );
    }
    #[test]
    fn failed_receipt_or_journal_never_starts_a_turn_and_refusal_requests_refresh() {
        let missing = rig("binary-disappeared");
        missing.drivers.ready.store(false, Ordering::SeqCst);
        let pid = task(&missing, json!({"intent":"read","spec":"report"}));
        assert_eq!(missing.k.lock().st.pid(pid).unwrap().state, "held");
        assert_eq!(missing.k.lock().st.holds.len(), 1);
        assert_eq!(missing.drivers.turns.load(Ordering::SeqCst), 0);
        assert!(
            missing
                .k
                .lock()
                .st
                .pid(pid)
                .unwrap()
                .contract
                .as_ref()
                .unwrap()["econ"]["chosen"]
                .is_null()
        );
        assert!(
            missing
                .k
                .lock()
                .st
                .pid(pid)
                .unwrap()
                .contract
                .as_ref()
                .unwrap()["econ"]["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["reason"].as_str().unwrap().contains("before launch"))
        );
        for journal in [false, true] {
            let r = rig(if journal { "journal" } else { "contract" });
            if journal {
                fs::remove_file(r.k.u.journal_path()).unwrap();
                fs::create_dir(r.k.u.journal_path()).unwrap();
            } else {
                fs::create_dir_all(r.root.join("projects/proj/tasks/pid-3/contract.json")).unwrap();
            }
            assert!(
                r.k.task(
                    &mut r.k.lock(),
                    Some(2),
                    json!({"intent":"read","spec":"report","go":"Ok, go","done_when":"report delivered"}),
                    &[]
                )
                .is_err()
            );
            assert_eq!(r.drivers.turns.load(Ordering::SeqCst), 0);
            assert!(r.k.lock().running.is_empty());
            r.k.invalidate_econ(&mut r.k.lock(), "codex", "recorded model refusal");
            assert!(r.k.lock().st.econ_refresh_requested);
            assert!(r.k.lock().st.econ_catalogs["codex"].error.is_some());
        }
    }
}
