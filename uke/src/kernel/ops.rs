//! Seat operations: what a seat's model may do (through the one MCP tool
//! `unvrs(command)` or `unvrs ctl`). Identity is derived from the process tree and the
//! harness's own thread id, never claimed. Captain-only operations are refused here
//! unless the caller is the captain's own terminal (no harness among its ancestors).
use super::{Inner, Kernel, SettingsFn, SnapshotFn, captain::words, now_ms, procinfo, state::clip};
use crate::{Filed, MemStore, SourceRec, SourceScope, Sources, Tier};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::sync::Arc;

#[path = "stop.rs"]
mod stop;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A bound thread's model (pid, thread key).
    Seat(usize, String),
    /// A driven run's CPU (an L3, or a detached seat run).
    Driven(usize),
    /// A model in a thread that is not an UNVRS seat.
    Unbound,
    /// The captain's terminal.
    Captain,
}

const CAPTAIN_ONLY: &[&str] = &[
    "approve", "answer", "reopen", "forget", "away", "sources", "seed",
];

impl Kernel {
    /// Who is calling: a driven CPU among the ancestors, else the harness's thread (the
    /// hint names it; the harness process must be an ancestor), else the one bound
    /// thread whose harness process is an ancestor.
    pub(crate) fn caller(
        &self,
        inner: &Inner,
        peer: Option<u32>,
        hint: Option<(Option<&str>, &str)>,
    ) -> Result<Caller> {
        let Some(peer) = peer else {
            return Ok(Caller::Unbound);
        };
        let chain: Vec<u32> = std::iter::once(peer)
            .chain(procinfo::ancestors(peer).into_iter().map(|(p, _)| p))
            .collect();
        for os in &chain {
            if let Some(p) = inner.st.pids.values().find(|p| {
                p.driven
                    .as_ref()
                    .is_some_and(|d| d.busy && d.os_pid == Some(*os))
            }) {
                return Ok(Caller::Driven(p.pid));
            }
        }
        // A hint without a harness matches the session on any harness; the thread must
        // still sit in the caller's process tree (S8, C13: no "codex" default).
        if let Some((h, s)) = hint
            && !s.is_empty()
            && let Some(t) = inner.st.threads.values().find(|t| {
                t.bound
                    && t.session == s
                    && h.is_none_or(|h| t.harness == h)
                    && t.os_pid.is_some_and(|o| chain.contains(&o))
            })
            && let Some(pid) = t.pid
        {
            return Ok(Caller::Seat(pid, t.key.clone()));
        }
        let bound: Vec<_> = inner
            .st
            .threads
            .values()
            .filter(|t| t.bound && t.os_pid.is_some_and(|o| chain.contains(&o)))
            .collect();
        match bound.as_slice() {
            [one] => Ok(Caller::Seat(one.pid.unwrap_or(0), one.key.clone())),
            [] if procinfo::under_harness(peer) => Ok(Caller::Unbound),
            [] => Ok(Caller::Captain),
            _ => bail!(
                "Refused: several UNVRS seats share this harness process and the call did not say which thread it came from"
            ),
        }
    }

    /// Socket and ctl settings calls use the same identity boundary.
    fn request_caller(&self, inner: &Inner, req: &Value, peer: Option<u32>) -> Result<Caller> {
        let hint = req["hint"]["session"]
            .as_str()
            .map(|session| (req["hint"]["harness"].as_str(), session));
        let mut who = self.caller(inner, peer, hint)?;
        // MCP and harness thread hints cannot claim captain-terminal authority.
        if who == Caller::Captain && (req["via"] == "mcp" || hint.is_some()) {
            who = Caller::Unbound;
        }
        Ok(who)
    }

    pub(super) fn observatory_callbacks(
        self: &Arc<Self>,
    ) -> Result<(SnapshotFn, SettingsFn, String)> {
        let token = crate::settings::local_token(self.u.root())?;
        let expected = token.clone();
        let k = Arc::clone(self);
        let settings: SettingsFn = Arc::new(move |req, supplied| {
            if matches!(req["op"].as_str(), Some("settings.set" | "settings.revert")) {
                let difference = expected
                    .bytes()
                    .zip(supplied.bytes())
                    .fold(0u8, |a, (b, c)| a | (b ^ c));
                ensure!(
                    expected.len() == supplied.len() && difference == 0,
                    "Refused: Observatory settings authorization failed"
                );
            }
            k.settings_op_as(&mut k.lock(), &req, &Caller::Captain, "observatory")
        });
        let k = Arc::clone(self);
        Ok((Arc::new(move || k.snapshot()), settings, token))
    }

    pub(crate) fn settings_request(&self, req: &Value, peer: Option<u32>) -> Result<Value> {
        let mut inner = self.lock();
        let who = if matches!(req["op"].as_str(), Some("settings.set" | "settings.revert")) {
            self.request_caller(&inner, req, peer)?
        } else {
            Caller::Unbound
        };
        self.settings_op(&mut inner, req, &who)
    }

    fn settings_op(&self, inner: &mut Inner, req: &Value, who: &Caller) -> Result<Value> {
        self.settings_op_as(inner, req, who, "captain")
    }

    fn settings_op_as(
        &self,
        inner: &mut Inner,
        req: &Value,
        who: &Caller,
        by: &str,
    ) -> Result<Value> {
        let op = req["op"].as_str().context("Settings operation required")?;
        let write = matches!(op, "settings.set" | "settings.revert");
        if write && who != &Caller::Captain {
            let pid = match who {
                Caller::Seat(p, _) | Caller::Driven(p) => Some(*p),
                _ => None,
            };
            self.event(inner,"refused",json!({"verb":op,"pid":pid,"caller":format!("{who:?}"),"reason":"captain-only settings write from a model"}));
            self.save(inner);
            bail!(
                "Refused: only the captain may change settings. Run unvrs settings from the captain's terminal or use Observatory settings."
            );
        }
        let allowed: &[&str] = match op {
            "settings.get" | "settings.history" => &["op", "hint", "via"],
            "settings.set" => &["op", "key", "value", "confirm", "hint", "via"],
            "settings.revert" => &["op", "change_id", "confirm", "hint", "via"],
            _ => bail!("Unknown settings operation: {op}"),
        };
        for key in req
            .as_object()
            .context("Expected a settings request object")?
            .keys()
        {
            ensure!(
                allowed.contains(&key.as_str()),
                "Unknown settings request field: {key}"
            );
        }
        match op {
            "settings.get" => {
                return Ok(serde_json::to_value(crate::settings::get(
                    self.u.root(),
                    serde_json::to_value(&inner.st.econ_catalogs)?,
                )?)?);
            }
            "settings.history" => {
                return Ok(json!({"history":crate::settings::history(self.u.root())?}));
            }
            _ => {}
        }
        let confirm = match req.get("confirm") {
            None => false,
            Some(Value::Bool(b)) => *b,
            _ => bail!("confirm must be true or false"),
        };
        ensure!(
            !inner.watchdog.active("journal"),
            "Kernel journal is unavailable; settings were not changed"
        );
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.u.journal_path())
            .context("Cannot open the kernel journal; settings were not changed")?;
        let change = match op {
            "settings.set" => crate::settings::set(
                self.u.root(),
                req["key"].as_str().context("Setting key must be text")?,
                req.get("value")
                    .context("Setting value is required")?
                    .clone(),
                confirm,
                by,
            )?,
            "settings.revert" => crate::settings::revert(
                self.u.root(),
                req["change_id"]
                    .as_str()
                    .context("Change ID must be text")?,
                confirm,
                by,
            )?,
            _ => unreachable!(),
        };
        self.event(inner, "config.changed", serde_json::to_value(&change)?);
        ensure!(
            !inner.watchdog.active("journal"),
            "Settings saved as {}, but the kernel journal failed; the change remains in settings history",
            change.change_id
        );
        journal.sync_data().with_context(|| {
            format!(
                "Settings saved as {}, but the kernel journal could not be synced",
                change.change_id
            )
        })?;
        if change.key.starts_with("econ.") {
            inner.st.econ_refresh_requested = true;
        }
        self.save(inner);
        ensure!(
            !inner.dirty,
            "Settings saved as {}, but kernel state could not be saved",
            change.change_id
        );
        Ok(json!({"ok":true,"change":change}))
    }

    pub(crate) fn ctl(self: &Arc<Self>, req: &Value, peer: Option<u32>) -> Result<Value> {
        let argv: Vec<String> = match req["argv"].as_array() {
            Some(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => words(req["command"].as_str().unwrap_or(""))?,
        };
        let verb = argv.first().cloned().unwrap_or_else(|| "help".into());
        let args: Vec<String> = argv.iter().skip(1).cloned().collect();
        let mut inner = self.lock();
        let who = self.request_caller(&inner, req, peer)?;
        if verb == "settings" {
            let value = self.settings_op(&mut inner, &crate::settings::command(&args)?, &who)?;
            return Ok(json!({"text":serde_json::to_string_pretty(&value)?}));
        }
        if matches!(verb.as_str(), "stop" | "cancel" | "kill") {
            // An owner ends a driven L3 (stop.rs); every refusal is journaled there.
            let out = self.stop_verb(&mut inner, &who, &args);
            self.save(&mut inner);
            return out.map(|text| json!({"text": text}));
        }
        let captain_only = CAPTAIN_ONLY.contains(&verb.as_str())
            || (verb == "project" && args.first().is_some_and(|a| a == "new"))
            || (verb == "remember" && args.iter().any(|a| a == "--pin"));
        if captain_only && who != Caller::Captain {
            let pid = match &who {
                Caller::Seat(p, _) | Caller::Driven(p) => Some(*p),
                _ => None,
            };
            self.event(
                &mut inner,
                "refused",
                json!({"verb": verb, "pid": pid, "caller": format!("{who:?}"), "reason": "captain-only op from a model"}),
            );
            self.save(&mut inner);
            bail!(
                "Refused: only the captain may {verb}. Ask the captain to type $unvrs:{verb} themselves."
            );
        }
        if verb == "quota-low" {
            // A test signal (0.7): a driven L3 is handed off to the other harness. It
            // stops another PID's process, so only its owner may send it.
            let p: usize = args
                .iter()
                .skip_while(|a| *a != "--pid")
                .nth(1)
                .and_then(|p| p.parse().ok())
                .context("usage: quota-low --pid <n>")?;
            if let Err(e) = self.may_signal(&inner, &who, p) {
                let by = match &who {
                    Caller::Seat(c, _) | Caller::Driven(c) => Some(*c),
                    _ => None,
                };
                self.event(
                    &mut inner,
                    "refused",
                    json!({"verb": verb, "pid": by, "target": p, "caller": format!("{who:?}"), "reason": "not the target's parent, L1 or the captain"}),
                );
                self.save(&mut inner);
                return Err(e);
            }
            let text = self.quota_low(&mut inner, p)?;
            self.save(&mut inner);
            return Ok(json!({"text": text}));
        }
        let pid = match &who {
            Caller::Seat(p, _) | Caller::Driven(p) => Some(*p),
            Caller::Captain => None,
            Caller::Unbound => bail!(
                "Refused: this thread is not an UNVRS seat. The captain binds a thread with $unvrs:l1 or $unvrs:l2."
            ),
        };
        let text = self.op(&mut inner, &who, pid, &verb, &args)?;
        self.save(&mut inner);
        Ok(json!({"text": text}))
    }

    /// Signals that act on another PID come from its owner: the captain, L1, or the
    /// seat that is the target's parent.
    fn may_signal(&self, inner: &Inner, who: &Caller, target: usize) -> Result<()> {
        let by = match who {
            Caller::Captain => return Ok(()),
            Caller::Unbound => bail!(
                "Refused: this thread is not an UNVRS seat; only PID {target}'s lead, L1 or the captain may signal it"
            ),
            Caller::Seat(p, _) | Caller::Driven(p) => *p,
        };
        let parent = inner.st.pid(target)?.parent;
        let l1 = inner.st.pids.get(&by).is_some_and(|r| r.rank == 1);
        ensure!(
            l1 || parent == by,
            "Refused: PID {target} belongs to PID {parent}; only its lead, L1 or the captain may signal it"
        );
        Ok(())
    }

    /// A lead steers its worker (S4, D53): the message goes to the worker's mailbox and
    /// reaches it at the start of its next turn. Its parent, children or L1 may write
    /// to it; a handed-off worker's mail follows the lineage to the live successor.
    pub(crate) fn mail_worker(
        &self,
        inner: &mut Inner,
        who: &Caller,
        by: usize,
        target: usize,
        text: &str,
    ) -> Result<String> {
        let to_parent = matches!(who, Caller::Seat(p, _) | Caller::Driven(p) if *p == by)
            && inner.st.pid(by)?.parent == target;
        if !to_parent && let Err(e) = self.may_signal(inner, who, target) {
            self.event(
                inner,
                "refused",
                json!({"verb": "send", "pid": by, "target": target, "caller": format!("{who:?}"), "reason": "not the worker's lead or L1"}),
            );
            self.save(inner);
            return Err(e);
        }
        let mut to = target;
        while let Some(next) = inner.st.pid(to)?.handed_to {
            to = next;
        }
        let rec = inner.st.pid(to)?;
        ensure!(
            rec.state == "working",
            "PID {to} is {}; it takes no more messages",
            rec.state
        );
        let project = rec.project.clone();
        let id = inner.st.mail(to, &format!("PID {by}"), text)?;
        self.event(
            inner,
            "mail",
            json!({"pid": by, "to": to, "target": target, "project": project, "id": id, "chars": text.chars().count()}),
        );
        let via = if to == target {
            String::new()
        } else {
            format!(" (PID {target} was handed off to it)")
        };
        Ok(format!(
            "Queued for PID {to}{via} (mail {id}); it reads it at the start of its next turn."
        ))
    }

    fn op(
        self: &Arc<Self>,
        inner: &mut Inner,
        who: &Caller,
        pid: Option<usize>,
        verb: &str,
        args: &[String],
    ) -> Result<String> {
        let rec = pid.and_then(|p| inner.st.pids.get(&p).cloned());
        let rank = rec.as_ref().map_or(0, |r| r.rank);
        let project = rec.as_ref().and_then(|r| r.project.clone());
        let need_seat =
            || -> Result<usize> { pid.context("This operation runs from an UNVRS seat") };
        let flag = |name: &str| -> Vec<String> {
            let mut out = vec![];
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if a == name
                    && let Some(v) = it.next()
                {
                    out.push(v.clone());
                }
            }
            out
        };
        // Positional words (flags and their values removed).
        let positional = |valued: &[&str]| -> Vec<String> {
            let mut out = vec![];
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if valued.contains(&a.as_str()) {
                    it.next();
                } else if !a.starts_with("--") {
                    out.push(a.clone());
                }
            }
            out
        };
        Ok(match verb {
            "help" => self.mapp.help(),
            "econ" => self.econ_command(inner, pid, args)?,
            "whoami" => match (who, &rec) {
                (_, Some(r)) => format!(
                    "PID {} · L{} · {} · harness {}{}",
                    r.pid,
                    r.rank,
                    r.project
                        .as_ref()
                        .map(|p| format!("project {p}"))
                        .unwrap_or_else(|| "all projects".into()),
                    r.harness,
                    match who {
                        Caller::Seat(_, k) => format!(" · thread {k}"),
                        _ => " · driven".into(),
                    }
                ),
                _ => "the captain's terminal".into(),
            },
            "hot" => super::hot::hot_text(self, inner, need_seat()?)?,
            "tree" => super::captain::tree_text(&inner.st),
            "digest" => self.digest_text(inner, false),
            "remember" | "stow" => {
                let p = pid;
                let store = match (&project, rank) {
                    (Some(pr), _) => MemStore::project(self.u.root(), pr),
                    _ => MemStore::captain(self.u.root()),
                };
                let source = match p {
                    Some(p) if verb == "stow" => format!("stow:pid:{p}"),
                    Some(p) => format!("pid:{p}"),
                    None => "captain".into(),
                };
                let items: Vec<(Tier, String)> = if verb == "stow" {
                    positional(&[])
                        .join(" ")
                        .split(['\n', ';'])
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| match l.split_once('|') {
                            Some((t, x)) if Tier::parse(t).is_ok() => {
                                (Tier::parse(t).unwrap_or(Tier::Aging), x.trim().to_owned())
                            }
                            _ => (Tier::Aging, l.trim().to_owned()),
                        })
                        .collect()
                } else {
                    let tier = if args.iter().any(|a| a == "--perishable")
                        || !flag("--until").is_empty()
                    {
                        Tier::Perishable
                    } else if args.iter().any(|a| a == "--pin") {
                        Tier::Pinned
                    } else {
                        Tier::Aging
                    };
                    vec![(tier, positional(&["--until"]).join(" "))]
                };
                ensure!(
                    !items.is_empty(),
                    "usage: remember [--perishable] [--until words] <fact>"
                );
                let until = flag("--until").first().cloned();
                let mut out = vec![];
                for (mut tier, text) in items {
                    if tier == Tier::Pinned && *who != Caller::Captain {
                        tier = Tier::Aging; // pins are the captain's (D48)
                    }
                    let filed = store.remember(
                        &text,
                        tier,
                        &source,
                        Some(&source),
                        until.as_deref(),
                        now_ms(),
                    )?;
                    let n = filed.note();
                    self.event(
                        inner,
                        "remember",
                        json!({"pid": p, "note": n.id, "tier": n.tier.as_str(), "by": source, "project": project, "reinforced": matches!(filed, Filed::Reinforced(_))}),
                    );
                    out.push(format!(
                        "{} {} ({}): {}",
                        if matches!(filed, Filed::Reinforced(_)) {
                            "reinforced"
                        } else {
                            "filed"
                        },
                        n.id,
                        n.tier.as_str(),
                        clip(&n.text, 160)
                    ));
                }
                out.join("\n")
            }
            "recall" => {
                let q = positional(&[]).join(" ");
                let mut stores = vec![];
                match (&project, rank) {
                    (Some(pr), _) => {
                        stores.push((pr.clone(), MemStore::project(self.u.root(), pr)))
                    }
                    _ => {
                        stores.push(("captain".into(), MemStore::captain(self.u.root())));
                        for p in self.project_ids() {
                            stores.push((p.clone(), MemStore::project(self.u.root(), &p)));
                        }
                    }
                }
                let mut out = vec![];
                for (label, s) in stores {
                    for n in s.search(&q)? {
                        out.push(format!(
                            "- [{}] ({label}, {}, {}) {}",
                            n.id,
                            n.tier.as_str(),
                            n.status,
                            clip(&n.text, 240)
                        ));
                    }
                }
                if out.is_empty() {
                    format!("No memory note matches {q:?}. Try `ctx search`.")
                } else {
                    out.join("\n")
                }
            }
            "ctx" => self.ctx_op(inner, pid, args)?,
            "decide" => {
                let p = need_seat()?;
                let q = positional(&["--option", "--until", "--supersedes"]).join(" ");
                // `--supersedes dN`: the new call replaces an open one; check the right to
                // moot it before anything is opened.
                let supersedes = flag("--supersedes").first().cloned();
                if let Some(old) = &supersedes {
                    let h = inner
                        .st
                        .holds
                        .get(old)
                        .with_context(|| format!("No decision {old} to supersede"))?;
                    ensure!(h.status == "open", "Decision {old} is already {}", h.status);
                    ensure!(
                        Self::may_moot(inner, h, Some(p)),
                        "Refused: only the seat that raised {old}, L1 or the captain may supersede it"
                    );
                }
                let until = flag("--until").first().and_then(|u| {
                    u.trim_end_matches('h')
                        .parse::<u64>()
                        .ok()
                        .map(|h| now_ms() + h * 3600 * 1000)
                });
                let id = self.open_hold(
                    inner,
                    "decision",
                    &q,
                    flag("--option"),
                    until,
                    project.clone(),
                    Some(p),
                    None,
                )?;
                let replaced = match &supersedes {
                    Some(old) => {
                        if let Some(h) = inner.st.holds.get_mut(&id) {
                            h.supersedes = Some(old.clone());
                        }
                        self.moot_hold(inner, old, &format!("superseded by {id}"), Some(p))?;
                        format!(" It replaces {old}, now closed as moot.")
                    }
                    None => String::new(),
                };
                format!(
                    "Held as {id}.{replaced} It is in every digest until the captain answers with $unvrs:answer {id} <words>; the answer comes back to you as a wake."
                )
            }
            "moot" => {
                let id = positional(&["--evidence"])
                    .first()
                    .cloned()
                    .context("usage: moot <id> --evidence \"<ref, commit or fact>\"")?;
                self.moot_hold(inner, &id, &flag("--evidence").join(" "), pid)?
            }
            "reopen" => self.reopen_hold(inner, args.first().context("usage: reopen <id>")?)?,
            "project" => match args.first().map(String::as_str) {
                None | Some("list") => self.projects_text(inner),
                Some("propose") => {
                    let p = need_seat()?;
                    ensure!(rank == 1, "Only L1 proposes projects");
                    let rest: Vec<String> = positional(&["--source"]).into_iter().skip(1).collect();
                    let (id, purpose) = rest
                        .split_first()
                        .context("usage: project propose <id> <purpose> [--source s]")?;
                    let hold =
                        self.propose_project(inner, p, id, &purpose.join(" "), &flag("--source"))?;
                    format!(
                        "Proposed {id} as {hold}; it exists only after the captain types $unvrs:approve {hold}."
                    )
                }
                Some("new") => {
                    let rest: Vec<String> = positional(&["--source"]).into_iter().skip(1).collect();
                    let (id, purpose) = rest
                        .split_first()
                        .context("usage: project new <id> <purpose> [--source s]")?;
                    let seat = self.create_project(
                        inner,
                        id,
                        &purpose.join(" "),
                        &flag("--source"),
                        "captain",
                    )?;
                    format!("Project {id} created; its lead seat is PID {seat}.")
                }
                Some(other) => {
                    bail!("usage: project list | propose <id> <purpose> [--source s] (not {other})")
                }
            },
            "task" => {
                let p = if matches!(who, Caller::Captain) {
                    None
                } else {
                    Some(need_seat()?)
                };
                for name in ["--go", "--done-when"] {
                    let values = flag(name);
                    if name == "--go" && !args.iter().any(|a| a == name) {
                        continue; // Kernel admission checks terminal, inheritance and away exceptions.
                    }
                    ensure!(
                        values.len() == 1
                            && args.iter().filter(|a| a.as_str() == name).count() == 1
                            && !values[0].starts_with("--"),
                        "{name} requires exactly one quoted value"
                    );
                }
                let authority = flag("--authority");
                ensure!(
                    authority.len() <= 1
                        && authority.len() == args.iter().filter(|a| *a == "--authority").count(),
                    "--authority takes exactly one value: report|implement"
                );
                let asked: Vec<String> =
                    flag("--on").into_iter().chain(flag("--harness")).collect();
                let mut contract = json!({
                    "intent": flag("--intent").join(" "),
                    "spec": flag("--spec").join(" "),
                    "go": flag("--go").first().cloned(),
                    "done_when": flag("--done-when")[0],
                    "project": flag("--project").first().cloned(),
                    "shape": flag("--shape").first().cloned().unwrap_or_else(|| "report".into()),
                    "sources": flag("--source"),
                    "rigor": flag("--rigor").first().cloned().unwrap_or_else(|| "normal".into()),
                    "model": flag("--model").first().cloned(),
                    "effort": flag("--effort").first().cloned(),
                    "kind": flag("--kind").first().cloned(),
                    "judgment": flag("--judgment").first().cloned(),
                    "thoroughness": flag("--thoroughness").first().cloned(),
                });
                if let Some(authority) = authority.first() {
                    contract["authority"] = json!(authority);
                }
                let new = self.task(inner, p, contract, &asked)?;
                if inner.st.pid(new)?.state == "held" {
                    return Ok(format!(
                        "Task held as L3 PID {new}; {}. One captain call is open; no harness turn started.",
                        inner.st.pid(new)?.next
                    ));
                }
                let on = inner.st.pid(new)?.harness.clone();
                let driven = inner.st.pid(new)?.driven.clone().unwrap_or_default();
                let pinned = inner.st.pid(new)?.contract.as_ref().and_then(|c| {
                    c["model"]
                        .as_str()
                        .or_else(|| c["resolved_model"].as_str())
                        .map(str::to_owned)
                });
                let model =
                    super::driven::model_terms(&on, pinned.as_deref(), driven.model.as_deref());
                let effort = driven.effort.map_or_else(String::new, |e| format!(
                    "; effort: {e}, pinned; the harness must confirm acceptance or the worker stops. Evidence appears in `tree`; internal reasoning telemetry is unavailable"
                ));
                format!(
                    "Task started as L3 PID {new} on {on}; {model}{effort}. Its result package comes back to you as a wake; nobody needs to relay it."
                )
            }
            "send" => {
                let p = need_seat()?;
                let to_s = args
                    .first()
                    .context("usage: send <pid|l1|project> <text>")?;
                let to = match to_s.as_str() {
                    "l1" => inner.st.l1().context("No L1 seat")?,
                    s => match s.parse::<usize>() {
                        Ok(n) => n,
                        Err(_) => inner
                            .st
                            .seat_of(s)
                            .with_context(|| format!("No seat {s}"))?,
                    },
                };
                let target = inner.st.pid(to)?.clone();
                // S8 (C12): a seat or worker writes to L1, its own lead or its own
                // project's seat; only L1 writes anywhere. A worker target goes to its
                // mailbox (S4), which admits only the worker's lead or L1.
                let lead = rec.as_ref().map(|r| r.parent);
                let in_scope = target.rank > 2
                    || rank == 1
                    || target.rank == 1
                    || lead == Some(to)
                    || (project.is_some() && target.project == project);
                if !in_scope {
                    self.event(
                        inner,
                        "refused",
                        json!({"verb": "send", "pid": p, "target": to, "caller": format!("{who:?}"), "reason": "outside the caller's lead, L1 and project"}),
                    );
                    bail!(
                        "Refused: PID {p} may send to L1, its lead or its project's seat, not PID {to}"
                    );
                }
                let text = args[1..].join(" ");
                ensure!(
                    !text.trim().is_empty(),
                    "usage: send <pid|l1|project> <text>"
                );
                if target.rank > 2 {
                    return self.mail_worker(inner, who, p, to, &text);
                }
                let id = self.wake_seat(
                    inner,
                    to,
                    "note",
                    &format!("from PID {p}: {text}"),
                    Some(p),
                    None,
                )?;
                format!("Queued for PID {to} (wake {id}).")
            }
            "approve" => self.approve(inner, args.first().context("usage: approve <id>")?)?,
            "answer" => {
                let (id, rest) = args.split_first().context("usage: answer <id> <words>")?;
                self.close_hold(inner, id, "answered", &rest.join(" "))?
            }
            "forget" => {
                let id = args.first().context("usage: forget <id>")?;
                let mut hit = None;
                for s in std::iter::once(MemStore::captain(self.u.root())).chain(
                    self.project_ids()
                        .iter()
                        .map(|p| MemStore::project(self.u.root(), p)),
                ) {
                    if s.note(id).is_ok() {
                        hit = Some(s.forget(id)?);
                        break;
                    }
                }
                let n = hit.with_context(|| format!("No note {id}"))?;
                format!("Archived {}.", n.id)
            }
            "away" => {
                let w = positional(&["--cap"]).join(" ");
                let cap = flag("--cap")
                    .first()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(3);
                self.set_away(
                    inner,
                    if w.is_empty() || w == "off" {
                        None
                    } else {
                        Some((w, cap))
                    },
                )?
            }
            "seed" => self.seed(inner, args)?,
            "sources" => match args.first().map(String::as_str) {
                Some("add") => {
                    let rest = positional(&[
                        "--purpose",
                        "--exclude",
                        "--sensitivity",
                        "--branch",
                        "--refresh",
                    ]);
                    let (id, kind, uri) = match rest.as_slice() {
                        [_, id, kind, uri, ..] => (id.clone(), kind.clone(), uri.clone()),
                        _ => bail!(
                            "usage: sources add <id> <path|git> <uri> [--purpose words] [--exclude glob]..."
                        ),
                    };
                    let rec = SourceRec {
                        id: id.clone(),
                        kind,
                        uri,
                        purpose: flag("--purpose").join(" "),
                        exclude: flag("--exclude"),
                        sensitivity: flag("--sensitivity")
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "private".into()),
                        branch: flag("--branch").first().cloned(),
                        refresh: flag("--refresh").first().cloned(),
                    };
                    Sources::add(self.u.root(), rec)?;
                    self.event(inner, "source.add", json!({"source": id}));
                    format!("Source {id} registered.")
                }
                _ => bail!(
                    "usage: sources add <id> <path|git> <uri> [--purpose words] [--exclude glob]..."
                ),
            },
            other => bail!("Unknown operation {other:?}. `help` lists them."),
        })
    }

    /// `ctx sources | map <source> [path] | search "<words>" [--source s]… [--k n] |
    /// get <ref> [--lines a-b]`, scoped by rank and project (D58) and journaled.
    fn ctx_op(&self, inner: &mut Inner, pid: Option<usize>, args: &[String]) -> Result<String> {
        let reg = Sources::load(self.u.root())?;
        let rec = pid.and_then(|p| inner.st.pids.get(&p).cloned());
        // Which sources this caller may read.
        let allowed: Vec<SourceScope> = match &rec {
            None => reg
                .list()
                .iter()
                .map(|s| SourceScope {
                    id: s.id.clone(),
                    prefix: None,
                })
                .collect(),
            Some(r) if r.rank == 1 => reg
                .list()
                .iter()
                .map(|s| SourceScope {
                    id: s.id.clone(),
                    prefix: None,
                })
                .collect(),
            Some(r) => {
                let project = r.project.clone().unwrap_or_default();
                let ids: Vec<String> = if r.rank == 2 {
                    self.project(&project)
                        .map(|p| p.sources)
                        .unwrap_or_default()
                } else {
                    r.contract
                        .as_ref()
                        .and_then(|c| c["sources"].as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                };
                let mut v: Vec<SourceScope> = ids
                    .into_iter()
                    .filter(|i| i != "memory")
                    .map(|id| SourceScope { id, prefix: None })
                    .collect();
                v.push(SourceScope {
                    id: "memory".into(),
                    prefix: Some(format!("projects/{project}")),
                });
                v
            }
        };
        let sub = args.first().map(String::as_str).unwrap_or("sources");
        let rest = &args[1.min(args.len())..];
        let valued = ["--source", "--k", "--lines"];
        let mut pos = vec![];
        let mut sources = vec![];
        let (mut k, mut lines) = (8usize, None);
        let mut it = rest.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--source" => sources.extend(it.next().cloned()),
                "--k" => k = it.next().and_then(|v| v.parse().ok()).unwrap_or(8),
                "--lines" => {
                    lines = it.next().and_then(|v| {
                        let (a, b) = v.split_once('-')?;
                        Some((a.parse().ok()?, b.parse().ok()?))
                    })
                }
                x if valued.contains(&x) => {}
                _ => pos.push(a.clone()),
            }
        }
        let refuse = |this: &Kernel, inner: &mut Inner, what: &str, e: &anyhow::Error| {
            this.event(
                inner,
                "ctx.refused",
                json!({"pid": pid, "what": what, "error": format!("{e:#}")}),
            );
        };
        let journal = |this: &Kernel,
                       inner: &mut Inner,
                       source: &str,
                       reference: &str,
                       bytes: usize,
                       op: &str| {
            this.event(
                inner,
                "ctx.read",
                json!({"pid": pid, "op": op, "source": source, "ref": reference, "bytes": bytes}),
            );
        };
        Ok(match sub {
            "sources" => {
                let mut out = String::new();
                for s in reg.list() {
                    if allowed.iter().any(|a| a.id == s.id) {
                        out.push_str(&format!(
                            "- {} ({}, {}): {}\n",
                            s.id, s.kind, s.sensitivity, s.purpose
                        ));
                    }
                }
                if out.is_empty() {
                    "No sources you may read.".into()
                } else {
                    out
                }
            }
            "map" => {
                let id = pos.first().context("usage: ctx map <source> [path]")?;
                let scope = match allowed.iter().find(|a| &a.id == id) {
                    Some(s) => s.clone(),
                    None => {
                        let e = anyhow::anyhow!("Refused: source {id} is not in your scope");
                        refuse(self, inner, id, &e);
                        return Err(e);
                    }
                };
                let a = reg.map(&scope, pos.get(1).map(String::as_str))?;
                journal(self, inner, id, &a.reference, a.bytes, "map");
                format!("{} ({} bytes)\n{}", a.reference, a.bytes, a.text)
            }
            "search" => {
                let q = pos.join(" ");
                ensure!(
                    !q.trim().is_empty(),
                    "usage: ctx search \"<words>\" [--source s] [--k n]"
                );
                let scopes: Vec<SourceScope> = if sources.is_empty() {
                    allowed.clone()
                } else {
                    let mut v = vec![];
                    for s in &sources {
                        match allowed.iter().find(|a| &a.id == s) {
                            Some(a) => v.push(a.clone()),
                            None => {
                                let e = anyhow::anyhow!("Refused: source {s} is not in your scope");
                                refuse(self, inner, s, &e);
                                return Err(e);
                            }
                        }
                    }
                    v
                };
                let hits = reg.search(&q, &scopes, k)?;
                let bytes: usize = hits.iter().map(|h| h.snippet.len()).sum();
                journal(
                    self,
                    inner,
                    &scopes
                        .iter()
                        .map(|s| s.id.clone())
                        .collect::<Vec<_>>()
                        .join(","),
                    &format!("search:{q}"),
                    bytes,
                    "search",
                );
                if hits.is_empty() {
                    format!("No hits for {q:?}.")
                } else {
                    hits.iter()
                        .map(|h| {
                            format!(
                                "- {}#L{} · {}\n  {}",
                                h.reference, h.line, h.title, h.snippet
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "get" => {
                let r = pos.first().context("usage: ctx get <ref> [--lines a-b]")?;
                let a = match reg.fetch(r, lines, &allowed) {
                    Ok(a) => a,
                    Err(e) => {
                        if format!("{e:#}").contains("Refused")
                            || format!("{e:#}").contains("unknown source")
                        {
                            refuse(self, inner, r, &e);
                        }
                        return Err(e);
                    }
                };
                journal(self, inner, &a.source, &a.reference, a.bytes, "get");
                format!(
                    "{}{} ({} bytes){}\n{}",
                    a.reference,
                    if a.stale {
                        " (your ref was stale; this is the current version)"
                    } else {
                        ""
                    },
                    a.bytes,
                    a.next
                        .as_ref()
                        .map(|n| format!(" · next: {n}"))
                        .unwrap_or_default(),
                    a.text
                )
            }
            other => bail!("usage: ctx sources | map | search | get (not {other})"),
        })
    }
}

#[cfg(test)]
#[path = "audit_repro.rs"]
mod audit_repro;

#[cfg(test)]
mod moot_tests {
    //! D46 moot/supersede close (PID40 O3): evidence retires a call; it approves nothing.
    use super::*;
    use crate::kernel::{Drivers, SilentMapp, State, Universe, state::Journal};
    use std::fs;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    struct NoDrivers;
    impl Drivers for NoDrivers {
        fn fold(&self, _: &crate::FoldJob) -> Result<crate::BriefFold> {
            unreachable!()
        }
        fn turn(
            &self,
            _: &crate::kernel::TurnRequest,
            _: &dyn Fn(u32),
        ) -> Result<crate::kernel::TurnResult> {
            unreachable!()
        }
    }

    /// A kernel with L1 (PID 1), an L2 that asks (PID 2) and another L2 (PID 3).
    fn fixture() -> Arc<Kernel> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "unvrs-moot-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let u = Universe::at(&path).unwrap();
        u.init().unwrap();
        let k = Kernel {
            journal: Journal::new(u.journal_path()),
            inner: Mutex::new(Inner {
                st: State::load(&u.state_path()).unwrap(),
                dirty: false,
                mem: crate::MemoryIndex::open(u.root()).unwrap(),
                watchdog: Default::default(),
                folding: Default::default(),
                running: Default::default(),
                recent: Default::default(),
                quota: Default::default(),
                seat_backoff: Default::default(),
                watchers: Default::default(),
            }),
            u,
            drivers: Arc::new(NoDrivers),
            mapp: Arc::new(SilentMapp),
            stop: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            started: now_ms(),
            last_active: AtomicU64::new(now_ms()),
        };
        {
            let mut inner = k.lock();
            for (rank, project) in [(1, None), (2, Some("alpha")), (2, Some("beta"))] {
                let p = inner.st.create(
                    0,
                    rank,
                    "attached",
                    "claude",
                    "seat",
                    Default::default(),
                    None,
                );
                inner.st.pid_mut(p).unwrap().project = project.map(str::to_owned);
            }
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
        }
        Arc::new(k)
    }

    #[test]
    fn settings_observatory_credential_stays_private_and_records_actor() {
        use std::os::unix::fs::PermissionsExt;
        let k = fixture();
        let (_, call, token) = k.observatory_callbacks().unwrap();
        let path = k.u.root().join("kernel/settings.token");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(token.len(), 64);
        assert_eq!(token, crate::settings::local_token(k.u.root()).unwrap());
        let request = json!({"op":"settings.set","key":"econ.catalog.refresh_minutes","value":6});
        for bad in ["", "wrong", &token[..63]] {
            assert!(
                call(request.clone(), bad)
                    .unwrap_err()
                    .to_string()
                    .starts_with("Refused:")
            );
        }
        let read = call(json!({"op":"settings.get"}), "").unwrap();
        assert!(!read.to_string().contains(&token));
        assert!(read["history"].as_array().unwrap().is_empty());
        let saved = call(request, &token).unwrap();
        assert_eq!(saved["change"]["by"], "observatory");
        assert!(!saved.to_string().contains(&token));
        let reverted = call(
            json!({"op":"settings.revert","change_id":saved["change"]["change_id"]}),
            &token,
        )
        .unwrap();
        assert_eq!(reverted["change"]["by"], "observatory");
        assert_eq!(reverted["change"]["after"], 5);
        assert!(
            !std::fs::read_to_string(k.u.journal_path())
                .unwrap()
                .contains(&token)
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(token, crate::settings::local_token(k.u.root()).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(&path).unwrap();
        let target = k.u.root().join("outside.token");
        std::fs::write(&target, &token).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(crate::settings::local_token(k.u.root()).is_err());
        assert_eq!(
            std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn settings_use_live_catalogs_and_refuse_model_writes() {
        let k = fixture();
        {
            let mut inner = k.lock();
            inner.st.econ_catalogs.insert(
                "codex".into(),
                crate::drv_econ::Catalog {
                    signed_in: Some(true),
                    observed_at: 123,
                    ..Default::default()
                },
            );
            let request =
                json!({"op":"settings.set","key":"econ.catalog.refresh_minutes","value":6});
            for who in [
                Caller::Unbound,
                Caller::Driven(42),
                Caller::Seat(1, "claude:s1".into()),
                Caller::Seat(2, "claude:s2".into()),
            ] {
                assert!(
                    k.settings_op(&mut inner, &request, &who)
                        .unwrap_err()
                        .to_string()
                        .starts_with("Refused:")
                );
            }
            assert!(!k.u.root().join("econ.toml").exists());
        }
        let state = k.handle(json!({"op":"settings.get"}), None).unwrap();
        assert_eq!(state["live_catalog"]["codex"]["observed_at"], 123);
        assert_eq!(state["live_catalog"]["codex"]["signed_in"], true);
        assert!(!k.u.root().join("econ.toml").exists());
        assert_eq!(
            k.handle(json!({"op":"settings.history"}), None).unwrap()["history"],
            json!([])
        );
        fs::remove_dir_all(k.u.root()).unwrap();
    }
    #[test]
    fn captain_settings_changes_and_reverts_are_journaled_and_reloaded() {
        let k = fixture();
        let request = json!({"op":"settings.set","key":"econ.catalog.refresh_minutes","value":6});
        let mut inner = k.lock();
        let value = k
            .settings_op(&mut inner, &request, &Caller::Captain)
            .unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["change"]["by"], "captain");
        assert!(inner.st.econ_refresh_requested);
        assert_eq!(
            crate::drv_econ::Config::load(k.u.root())
                .unwrap()
                .catalog
                .refresh_minutes,
            6
        );
        let change_id = value["change"]["change_id"].as_str().unwrap();
        let undo = k
            .settings_op(
                &mut inner,
                &json!({"op":"settings.revert","change_id":change_id}),
                &Caller::Captain,
            )
            .unwrap();
        assert_eq!(undo["change"]["reverts"], change_id);
        assert_eq!(
            crate::drv_econ::Config::load(k.u.root())
                .unwrap()
                .catalog
                .refresh_minutes,
            5
        );
        for invalid in [
            json!({"op":"settings.set","key":"econ.policy.allow_max","value":true,"confirm":"true"}),
            json!({"op":"settings.set","key":"econ.policy.allow_max","value":true,"by":"observatory"}),
        ] {
            assert!(
                k.settings_op(&mut inner, &invalid, &Caller::Captain)
                    .is_err()
            );
        }
        let events: Vec<_> = k
            .journal
            .tail(200)
            .into_iter()
            .filter(|e| e["kind"] == "config.changed")
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["change_id"], change_id);
        assert_eq!(events[0]["before"], 5);
        assert_eq!(events[0]["after"], 6);
        assert_eq!(events[1]["reverts"], change_id);
        assert_eq!(events[0]["by"], "captain");
        assert!(events[1]["seq"].as_u64().unwrap() > events[0]["seq"].as_u64().unwrap());
        drop(inner);
        fs::remove_dir_all(k.u.root()).unwrap();
    }
    #[test]
    fn raw_socket_and_ctl_cannot_bypass_captain_settings_authority() {
        let k = fixture();
        let request = json!({"op":"settings.set","key":"econ.catalog.refresh_minutes","value":6});
        assert!(
            k.handle(request.clone(), None)
                .unwrap_err()
                .to_string()
                .starts_with("Refused:")
        );
        {
            let mut inner = k.lock();
            let pid = inner.st.create(
                1,
                3,
                "driven",
                "codex",
                "settings reader",
                Default::default(),
                None,
            );
            inner.st.pid_mut(pid).unwrap().driven = Some(super::super::Driven {
                busy: true,
                os_pid: Some(std::process::id()),
                ..Default::default()
            });
        }
        assert!(k.handle(request.clone(), Some(std::process::id())).is_err());
        assert!(k.ctl(&json!({"op":"ctl","argv":["settings","set","econ.catalog.refresh_minutes","6"],"via":"ctl"}),Some(std::process::id())).is_err());
        let read = k
            .ctl(
                &json!({"op":"ctl","argv":["settings","get"]}),
                Some(std::process::id()),
            )
            .unwrap();
        assert!(
            serde_json::from_str::<Value>(read["text"].as_str().unwrap()).unwrap()["schema"]
                .is_array()
        );
        // Exercise the existing terminal downgrade without relying on this test's ancestors.
        let peer = Some(u32::MAX);
        assert_eq!(
            k.request_caller(&k.lock(), &json!({"via":"mcp"}), peer)
                .unwrap(),
            Caller::Unbound
        );
        assert_eq!(
            k.request_caller(&k.lock(), &json!({"hint":{"session":"model-thread"}}), peer)
                .unwrap(),
            Caller::Unbound
        );
        assert!(!k.u.root().join("econ.toml").exists());
        fs::remove_dir_all(k.u.root()).unwrap();
    }
    #[test]
    fn settings_refuse_an_unwritable_main_journal_before_changing_files() {
        let k = fixture();
        fs::create_dir(k.u.journal_path()).unwrap();
        let err = k
            .settings_op(
                &mut k.lock(),
                &json!({"op":"settings.set","key":"econ.catalog.refresh_minutes","value":6}),
                &Caller::Captain,
            )
            .unwrap_err();
        assert!(err.to_string().contains("settings were not changed"));
        assert!(!k.u.root().join("econ.toml").exists());
        assert!(crate::settings::history(k.u.root()).unwrap().is_empty());
        fs::remove_dir_all(k.u.root()).unwrap();
    }

    fn run(k: &Arc<Kernel>, pid: Option<usize>, line: &str) -> Result<String> {
        let who = match pid {
            Some(p) => Caller::Seat(p, format!("claude:s{p}")),
            None => Caller::Captain,
        };
        let argv = words(line)?;
        let mut inner = k.lock();
        k.op(&mut inner, &who, pid, &argv[0], &argv[1..])
    }

    #[test]
    fn task_go_requires_a_fresh_captain_prompt_and_preserves_quoted_spec() {
        let k = fixture();
        k.create_project(&mut k.lock(), "alpha", "fixture", &[], "fixture")
            .unwrap();
        let before = k.lock().st.pids.len();
        let command = "task --intent implement --spec inspect --go 'Ok, go' --done-when 'report cites sources'";
        k.lock().st.captain_prompts.clear();
        k.lock().mem.append_turn(2, "Captain: Ok, go").unwrap();
        assert!(
            run(&k, Some(2), command)
                .unwrap_err()
                .to_string()
                .eq(super::super::driven::NO_GO)
        );
        assert_eq!(k.lock().st.pids.len(), before);
        assert!(k.task(&mut k.lock(), Some(2), json!({"intent":"implement", "spec":"inspect", "go":"Ok, go", "done_when":"done", "go_evidence":{"kind":"captain_prompt", "prompt_id":1, "at":now_ms()}}), &[]).is_err());
        assert_eq!(k.lock().st.pids.len(), before);
        k.hook(
            "UserPromptSubmit",
            "claude",
            &json!({"session_id":"unbound", "prompt":"Ok, go"}),
            None,
        )
        .unwrap();
        assert!(k.lock().st.captain_prompts.is_empty());
        {
            let mut inner = k.lock();
            inner.st.threads.insert(
                "claude:alignment".into(),
                super::super::state::ThreadRec {
                    key: "claude:alignment".into(),
                    harness: "claude".into(),
                    session: "alignment".into(),
                    pid: Some(2),
                    bound: true,
                    os_pid: Some(std::process::id()),
                    ..Default::default()
                },
            );
        }
        k.hook(
            "UserPromptSubmit",
            "claude",
            &json!({"session_id":"alignment", "prompt":"Ok, go"}),
            Some(std::process::id()),
        )
        .unwrap();
        let prompt = k.lock().st.captain_prompts[0].clone();
        assert_eq!(prompt.project.as_deref(), Some("alpha"));
        assert_eq!(prompt.text, "Ok, go");
        let stored = State::load(&k.u.state_path()).unwrap();
        assert_eq!(stored.captain_prompts[0].id, prompt.id);
        // A restart retains typed hook evidence; it does not promote memory text.
        k.lock().st = stored;
        for (at, project) in [
            (
                now_ms() - super::super::driven::GO_WINDOW_MS - 1000,
                Some("alpha"),
            ),
            (now_ms() + 60_000, Some("alpha")),
            (prompt.at, Some("beta")),
        ] {
            {
                let mut inner = k.lock();
                inner.st.captain_prompts[0].at = at;
                inner.st.captain_prompts[0].project = project.map(str::to_owned);
            }
            assert!(run(&k, Some(2), command).is_err());
            assert_eq!(k.lock().st.pids.len(), before);
        }
        k.lock().st.captain_prompts[0] = prompt.clone();
        let spec = "keep the captain's words and \"quotes\" intact";
        for line in [
            "task --intent implement --spec 'keep the captain's words and \"quotes\" intact' --go 'OK   GO!!!' --done-when done",
            r#"task --intent implement --spec "keep the captain's words and \"quotes\" intact" --go "Ok, go" --done-when done"#,
        ] {
            k.ctl(&json!({"command":line, "hint":{"harness":"claude","session":"alignment"}, "via":"mcp"}), Some(std::process::id())).unwrap();
            let inner = k.lock();
            let pid = *inner.st.pids.keys().max().unwrap();
            let contract = inner.st.pid(pid).unwrap().contract.as_ref().unwrap();
            assert_eq!(contract["spec"], spec);
            assert_eq!(contract["go_source"]["prompt_id"], prompt.id);
            assert_eq!(contract["go_source"]["kind"], "captain_prompt");
            let disk: Value = serde_json::from_slice(
                &fs::read(
                    k.u.project_dir("alpha")
                        .join(format!("tasks/pid-{pid}/contract.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(disk["spec"], spec);
            assert_eq!(disk["go_source"], contract["go_source"]);
        }
        assert_eq!(
            k.lock().st.pids.len(),
            before + 2,
            "one prompt may authorize several tasks"
        );
        let before = k.lock().st.pids.len();
        assert!(run(&k, Some(2), "task --intent implement --spec 'unterminated").is_err());
        assert_eq!(k.lock().st.pids.len(), before);
        k.hook("UserPromptSubmit", "claude", &json!({"session_id":"alignment", "prompt":"Please IMPLEMENT: the change in the captain's words, now."}), Some(std::process::id())).unwrap();
        run(
            &k,
            Some(2),
            "task --intent implement --spec inspect --go 'implement the change' --done-when done",
        )
        .unwrap();
        assert!(
            run(
                &k,
                Some(2),
                "task --intent implement --spec inspect --go captain --done-when done"
            )
            .is_err()
        );
        assert!(
            k.journal
                .tail(200)
                .iter()
                .any(|e| e["kind"] == "refused" && e["verb"] == "task")
        );
        k.lock().st.captain_prompts.clear();
        run(&k, None, "away 'Build the feature now'").unwrap();
        run(
            &k,
            Some(2),
            "task --intent implement --spec inspect --go 'build the feature' --done-when done",
        )
        .unwrap();
        {
            let inner = k.lock();
            let pid = *inner.st.pids.keys().max().unwrap();
            assert_eq!(
                inner.st.pid(pid).unwrap().contract.as_ref().unwrap()["go_source"]["kind"],
                "away"
            );
        }
        let before = k.lock().st.pids.len();
        k.lock().st.away.as_mut().unwrap().since =
            now_ms() - super::super::driven::GO_WINDOW_MS - 1000;
        assert!(
            run(
                &k,
                Some(2),
                "task --intent implement --spec inspect --go 'build the feature' --done-when done"
            )
            .is_err()
        );
        assert_eq!(k.lock().st.pids.len(), before);
        fs::remove_dir_all(k.u.root()).unwrap();
    }

    #[test]
    fn child_tasks_inherit_go_and_scope_without_widening_permissions() {
        let k = fixture();
        k.create_project(
            &mut k.lock(),
            "alpha",
            "fixture",
            &["memory".into()],
            "fixture",
        )
        .unwrap();
        run(&k, Some(2), "task --intent read --spec inspect --go 'Ok, go' --done-when 'report cites sources' --authority report").unwrap();
        let parent = *k.lock().st.pids.keys().max().unwrap();
        let original = k.lock().st.pid(parent).unwrap().contract.clone().unwrap();
        k.lock().st.captain_prompts.clear(); // Admitted work retains its go after evidence expires.
        let task = "task --intent read --spec 'check one fact' --done-when 'fact is checked'";
        run(&k, Some(parent), task).unwrap();
        let child = *k.lock().st.pids.keys().max().unwrap();
        {
            let inner = k.lock();
            let rec = inner.st.pid(child).unwrap();
            let contract = rec.contract.as_ref().unwrap();
            assert_eq!(rec.parent, parent);
            assert_eq!(contract["go_quote"], original["go_quote"]);
            assert_eq!(
                contract["go_source"]["prompt_id"],
                original["go_source"]["prompt_id"]
            );
            assert_eq!(
                contract["go_source"]["thread"],
                original["go_source"]["thread"]
            );
            assert_eq!(contract["go_source"]["at"], original["go_source"]["at"]);
            assert_eq!(contract["go_source"]["inherited_from"], parent);
            assert_eq!(contract["go_scope"], "report cites sources");
            assert_eq!(contract["done_when"], "fact is checked");
            assert_eq!(contract["authority"], "report");
            assert!(
                super::super::hot::hot_text(&k, &inner, child)
                    .unwrap()
                    .contains("inherited scope: report cites sources")
            );
        }
        run(&k, Some(child), task).unwrap();
        let grandchild = *k.lock().st.pids.keys().max().unwrap();
        assert_eq!(
            k.lock()
                .st
                .pid(grandchild)
                .unwrap()
                .contract
                .as_ref()
                .unwrap()["go_scope"],
            "fact is checked; within: report cites sources"
        );
        let before = k.lock().st.pids.len();
        for flags in [
            " --authority implement",
            " --source memory",
            " --project beta",
            " --go 'do it'",
        ] {
            assert!(
                run(&k, Some(parent), &format!("{task}{flags}")).is_err(),
                "{flags}"
            );
            assert_eq!(k.lock().st.pids.len(), before);
        }
        // Child mail and completion reach a worker parent, whose mailbox is its input.
        k.lock().st.pid_mut(parent).unwrap().state = "working".into();
        run(
            &k,
            Some(child),
            &format!("send {parent} 'judgment needed: evidence'"),
        )
        .unwrap();
        {
            let mut inner = k.lock();
            k.finish(&mut inner, child, "fact checked", "done");
            let mail = &inner.st.pid(parent).unwrap().mailbox;
            assert!(mail.iter().any(|m| m.text == "judgment needed: evidence"));
            assert!(
                mail.iter()
                    .any(|m| m.text.contains("fact checked") && m.text.contains("Result package:"))
            );
        }
        assert_eq!(
            run(&k, Some(child), task).unwrap_err().to_string(),
            super::super::driven::NO_GO
        );
        fs::remove_dir_all(k.u.root()).unwrap();
    }

    #[test]
    fn captain_terminal_and_away_admission_need_no_go_flag_but_require_done_when() {
        let k = fixture();
        k.create_project(&mut k.lock(), "alpha", "fixture", &[], "fixture")
            .unwrap();
        k.lock().st.captain_prompts.clear();
        let task = "task --project alpha --intent inspect --spec inspect --done-when done";
        let before = k.lock().st.pids.len();
        assert_eq!(
            run(&k, Some(2), task).unwrap_err().to_string(),
            super::super::driven::NO_GO
        );
        assert_eq!(k.lock().st.pids.len(), before);
        run(&k, None, task).unwrap();
        let pid = *k.lock().st.pids.keys().max().unwrap();
        {
            let inner = k.lock();
            let contract = inner.st.pid(pid).unwrap().contract.as_ref().unwrap();
            assert_eq!(contract["go_quote"], "inspect");
            assert_eq!(contract["go_source"]["kind"], "captain_terminal");
        }
        // A harness cannot request the terminal exception through MCP or a session hint.
        for req in [
            json!({"command":task,"via":"mcp"}),
            json!({"command":task,"hint":{"session":"unbound"}}),
        ] {
            assert!(k.ctl(&req, Some(std::process::id())).is_err());
        }
        run(
            &k,
            Some(pid),
            "task --intent read --spec inspect --go 'INSPECT!' --done-when checked",
        )
        .unwrap();
        let before = k.lock().st.pids.len();
        assert!(
            run(
                &k,
                None,
                "task --project alpha --intent read --spec inspect"
            )
            .is_err()
        );
        assert_eq!(k.lock().st.pids.len(), before);
        run(&k, None, "away 'Build the feature now'").unwrap();
        run(&k, Some(2), task).unwrap();
        let pid = *k.lock().st.pids.keys().max().unwrap();
        {
            let inner = k.lock();
            let contract = inner.st.pid(pid).unwrap().contract.as_ref().unwrap();
            assert_eq!(contract["go_quote"], "Build the feature now");
            assert_eq!(contract["go_source"]["kind"], "away");
        }
        run(&k, None, "away off").unwrap();
        assert_eq!(
            run(&k, Some(2), task).unwrap_err().to_string(),
            super::super::driven::NO_GO
        );
        fs::remove_dir_all(k.u.root()).unwrap();
    }

    #[test]
    fn task_alignment_fields_are_required_before_creating_a_worker() {
        let k = fixture();
        k.create_project(&mut k.lock(), "alpha", "fixture", &[], "fixture")
            .unwrap();
        let task = "task --intent read --spec inspect";
        let before = k.lock().st.pids.len();
        for flags in [
            "",
            " --go 'Ok, go'",
            " --done-when 'report cites sources'",
            " --go '' --done-when 'report cites sources'",
            " --go '   ' --done-when 'report cites sources'",
            " --go 'Ok, go' --done-when ''",
            " --go 'Ok, go' --done-when '   '",
            " --go 'Ok, go' --done-when 'first\nsecond'",
            " --go 'Ok, go' --done-when 'first\rsecond'",
            " --go 'Ok, go' --done-when 'first\u{0085}second'",
            " --go 'Ok, go' --done-when 'first\u{2028}second'",
            " --go 'Ok, go' --done-when 'first\u{2029}second'",
            " --go --done-when 'report cites sources'",
            " --go 'Ok, go' --done-when",
            " --go 'Ok, go' --done-when --shape report",
            " --go 'Ok, go' --go 'do it' --done-when done",
            " --go 'Ok, go' --done-when done --done-when finished",
            " --go 'Ok, go' --done-when done --go",
            " --go 'Ok, go' --done-when done --done-when",
        ] {
            for pid in [1, 2] {
                assert!(
                    run(&k, Some(pid), &format!("{task} --project alpha{flags}")).is_err(),
                    "PID {pid}: {flags:?}"
                );
                assert_eq!(k.lock().st.pids.len(), before, "refusal created a worker");
            }
        }
        for field in ["go", "done_when"] {
            let mut missing = json!({"project":"alpha", "intent":"read", "spec":"inspect", "go":"Ok, go", "done_when":"report cites sources"});
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                k.task(&mut k.lock(), Some(1), missing, &[])
                    .unwrap_err()
                    .to_string()
                    .contains(if field == "go" {
                        super::super::driven::NO_GO
                    } else {
                        "--done-when"
                    })
            );
            for value in [Value::Null, json!(false), json!(7), json!([]), json!(" ")] {
                let mut contract = json!({"intent":"read", "spec":"inspect", "go":"Ok, go", "done_when":"report cites sources"});
                contract[field] = value;
                let error = k.task(&mut k.lock(), Some(2), contract, &[]).unwrap_err();
                assert!(
                    error.to_string().contains(if field == "go" {
                        super::super::driven::NO_GO
                    } else {
                        "--done-when"
                    }),
                    "{error}"
                );
                assert_eq!(k.lock().st.pids.len(), before);
            }
        }
        assert!(
            !k.u.project_dir("alpha")
                .join(format!("tasks/pid-{}", before + 1))
                .exists()
        );
        let worker = k
            .lock()
            .st
            .create(2, 3, "driven", "codex", "read", Default::default(), None);
        k.lock().st.pid_mut(worker).unwrap().project = Some("alpha".into());
        let before = k.lock().st.pids.len();
        let error = run(
            &k,
            Some(worker),
            &format!("{task} --go 'Ok, go' --done-when done"),
        )
        .unwrap_err();
        assert!(error.to_string() == super::super::driven::NO_GO, "{error}");
        assert_eq!(k.lock().st.pids.len(), before);
        fs::remove_dir_all(k.u.root()).unwrap();
    }

    #[test]
    fn task_authority_is_explicit_independent_of_shape_and_persisted() {
        let k = fixture();
        let task = "task --intent implement --spec change --shape report --go ' Ok, go ' --done-when ' report cites sources '";
        for (flags, expected) in [
            ("", "implement"),
            (" --authority implement", "implement"),
            (" --authority report", "report"),
        ] {
            run(&k, Some(2), &format!("{task}{flags}")).unwrap();
            let inner = k.lock();
            let pid = *inner.st.pids.keys().max().unwrap();
            let rec = inner.st.pid(pid).unwrap();
            assert_eq!(rec.state, "held", "fixture must not launch a driver");
            assert_eq!(rec.contract.as_ref().unwrap()["authority"], expected);
            assert_eq!(rec.contract.as_ref().unwrap()["shape"], "report");
            assert_eq!(rec.contract.as_ref().unwrap()["go_quote"], "Ok, go");
            assert_eq!(
                rec.contract.as_ref().unwrap()["done_when"],
                "report cites sources"
            );
            let disk: Value = serde_json::from_slice(
                &fs::read(
                    k.u.project_dir("alpha")
                        .join(format!("tasks/pid-{pid}/contract.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(disk["authority"], expected);
            assert_eq!(disk["go_quote"], "Ok, go");
            assert_eq!(disk["done_when"], "report cites sources");
            let restored = State::load(&k.u.state_path()).unwrap();
            assert_eq!(
                restored.pid(pid).unwrap().contract.as_ref().unwrap()["authority"],
                expected
            );
            assert_eq!(
                restored.pid(pid).unwrap().contract.as_ref().unwrap()["go_quote"],
                "Ok, go"
            );
            assert_eq!(
                restored.pid(pid).unwrap().contract.as_ref().unwrap()["done_when"],
                "report cites sources"
            );
            let hot = super::super::hot::hot_text(&k, &inner, pid).unwrap();
            assert!(hot.contains(&format!("authority: {expected}\n")), "{hot}");
            assert!(hot.contains("go (captain quote): Ok, go\n"), "{hot}");
            assert!(hot.contains("done when: report cites sources\n"), "{hot}");
            assert!(k.journal.tail(200).iter().any(|e| e["kind"] == "task"
                && e["pid"] == pid
                && e["go_quote"] == "Ok, go"
                && e["done_when"] == "report cites sources"));
        }
        let before = k.lock().st.pids.len();
        for flags in [
            " --authority",
            " --authority invalid",
            " --authority 'report only'",
            " --authority report --authority implement",
            " --authority report --authority",
            " --authority --shape report",
        ] {
            assert!(
                run(&k, Some(2), &format!("{task}{flags}")).is_err(),
                "{flags}"
            );
            assert_eq!(
                k.lock().st.pids.len(),
                before,
                "refused task created a worker"
            );
        }
        for authority in [Value::Null, json!(true), json!("report only")] {
            assert!(
                k.task(
                    &mut k.lock(),
                    Some(2),
                    json!({"intent":"read", "spec":"inspect", "authority":authority, "go":"Ok, go", "done_when":"report cites sources"}),
                    &[],
                )
                .is_err()
            );
            assert_eq!(k.lock().st.pids.len(), before);
        }
        fs::remove_dir_all(k.u.root()).unwrap();
    }

    fn status(k: &Arc<Kernel>, id: &str) -> String {
        k.lock().st.holds[id].status.clone()
    }

    fn closes(k: &Arc<Kernel>, id: &str) -> Vec<Value> {
        k.journal
            .tail(200)
            .into_iter()
            .filter(|e| e["kind"] == "hold.close" && e["hold"] == id)
            .collect()
    }

    #[test]
    fn raiser_moots_its_own_call_with_evidence_and_it_leaves_open_calls() {
        let k = fixture();
        run(&k, Some(2), "decide 'merge the polish branch?'").unwrap();
        let err = run(&k, Some(2), "moot d1").unwrap_err().to_string();
        assert!(err.contains("needs evidence"), "{err}");
        assert_eq!(status(&k, "d1"), "open");

        let text = run(&k, Some(2), "moot d1 --evidence 'merged in 78d4fbc'").unwrap();
        assert!(
            text.contains("as moot") && text.contains("reopen"),
            "{text}"
        );
        let h = k.lock().st.holds["d1"].clone();
        assert_eq!(
            (h.status.as_str(), h.evidence.as_deref(), h.closed_by),
            ("moot", Some("merged in 78d4fbc"), Some(2))
        );
        assert!(h.answer.is_none(), "a moot close records no captain words");
        let c = closes(&k, "d1");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["status"], "moot");
        assert_eq!(c[0]["evidence"], "merged in 78d4fbc");
        assert_eq!(k.snapshot()["calls"].as_array().unwrap().len(), 0);
        let digest = k.digest_facts(&k.lock(), 0);
        assert!(digest["calls"].as_array().unwrap().is_empty());
        // Closed once: a second moot is refused.
        assert!(run(&k, Some(2), "moot d1 --evidence again").is_err());
    }

    #[test]
    fn another_seat_is_refused_and_journaled_but_l1_may_moot() {
        let k = fixture();
        run(&k, Some(2), "decide 'clear build caches?'").unwrap();
        let err = run(&k, Some(3), "moot d1 --evidence 'disk has 36 GB free'")
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Refused") && err.contains("PID 2"), "{err}");
        assert_eq!(status(&k, "d1"), "open");
        assert!(
            k.journal
                .tail(50)
                .iter()
                .any(|e| e["kind"] == "refused" && e["verb"] == "moot" && e["pid"] == 3)
        );
        assert!(closes(&k, "d1").is_empty());

        run(&k, Some(1), "moot d1 --evidence 'disk has 36 GB free'").unwrap();
        assert_eq!(status(&k, "d1"), "moot");
        // The raiser hears that someone else closed its call.
        let inner = k.lock();
        let w = inner.st.pids[&2].wakes.last().expect("raiser woken");
        assert!(
            w.text.contains("PID 1 closed your call d1") && w.text.contains("36 GB"),
            "{}",
            w.text
        );
    }

    #[test]
    fn decide_supersedes_moots_the_old_call_only_with_the_right_to_moot_it() {
        let k = fixture();
        run(&k, Some(2), "decide 'summaries quick fix?'").unwrap();
        let err = run(
            &k,
            Some(3),
            "decide 'summaries lasting fix?' --supersedes d1",
        )
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("Refused"), "{err}");
        assert_eq!(
            k.lock().st.hold_seq,
            1,
            "nothing is opened when the supersede is refused"
        );
        assert!(run(&k, Some(2), "decide 'x' --supersedes d9").is_err());

        let text = run(
            &k,
            Some(2),
            "decide 'summaries lasting fix?' --supersedes d1",
        )
        .unwrap();
        assert!(
            text.contains("Held as d2") && text.contains("replaces d1"),
            "{text}"
        );
        let inner = k.lock();
        let (old, new) = (&inner.st.holds["d1"], &inner.st.holds["d2"]);
        assert_eq!(old.status, "moot");
        assert_eq!(old.evidence.as_deref(), Some("superseded by d2"));
        assert_eq!(new.status, "open");
        assert_eq!(new.question, "summaries lasting fix?");
        assert_eq!(new.supersedes.as_deref(), Some("d1"));
    }

    #[test]
    fn captain_words_overrule_a_moot_and_only_the_captain_reopens() {
        assert!(CAPTAIN_ONLY.contains(&"reopen"));
        let k = fixture();
        run(&k, Some(2), "decide 'keep the renderer?'").unwrap();
        run(
            &k,
            Some(2),
            "moot d1 --evidence 'feat/bridge-renderer is live'",
        )
        .unwrap();
        run(&k, None, "reopen d1").unwrap();
        let h = k.lock().st.holds["d1"].clone();
        assert_eq!(
            (h.status.as_str(), h.evidence, h.closed_by),
            ("open", None, None)
        );
        assert!(k.journal.tail(50).iter().any(|e| e["kind"] == "hold.reopen"
            && e["hold"] == "d1"
            && e["was"] == "feat/bridge-renderer is live"));
        assert!(
            run(&k, None, "reopen d1").is_err(),
            "only moot calls reopen"
        );

        run(&k, Some(1), "moot d1 --evidence 'live again'").unwrap();
        run(&k, None, "answer d1 keep it").unwrap();
        let h = k.lock().st.holds["d1"].clone();
        assert_eq!(
            (h.status.as_str(), h.answer.as_deref()),
            ("answered", Some("keep it"))
        );
        let c = closes(&k, "d1");
        assert_eq!(c.last().unwrap()["overrules_moot"], true);
        // Answered calls stay closed: no moot, no reopen.
        assert!(run(&k, Some(1), "moot d1 --evidence x").is_err());
        assert!(run(&k, None, "reopen d1").is_err());
    }
}
