//! Driven L3 workers (K6, D47): a task has a contract (the captain's intent, the
//! lead's spec, authority implement | report, shape report | act, sources) before it
//! starts. The kernel runs the CPU headless, folds progress into the worker's brief, hands the task to
//! another harness on a low-quota signal, and delivers the result package to the
//! lead's wake queue. `act` tasks wait for workspaces (0.9).
use super::{
    Inner, Kernel, TurnRequest, hot,
    state::{Driven, clip, now_ms},
};
use crate::{Brief, FoldOutcome, MemoryActor};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{fs, sync::Arc, thread};

pub(crate) const GO_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
pub(crate) const NO_GO: &str = "No captain go found for this work: propose it and ask the captain.";

/// Kernel-owned admission receipts survive expiry; model memory is never a receipt.
pub(crate) fn approved_contract(contract: Option<&Value>) -> Result<&Value> {
    let contract = contract.context(NO_GO)?;
    ensure!(
        contract["go_quote"]
            .as_str()
            .or_else(|| contract["go"].as_str())
            .is_some_and(|q| !q.trim().is_empty()),
        NO_GO
    );
    ensure!(
        contract["done_when"]
            .as_str()
            .is_some_and(|d| !d.trim().is_empty()),
        NO_GO
    );
    let source = contract
        .get("go_source")
        .or_else(|| contract.get("go_evidence"));
    ensure!(
        source.is_some_and(|s| matches!(
            s["kind"].as_str(),
            Some("captain_prompt" | "captain_terminal" | "away")
        )),
        NO_GO
    );
    Ok(contract)
}

fn normalized_go(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace(['\'', '’'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn go_matches(quote: &str, prompt: &str) -> bool {
    let quote = normalized_go(quote);
    let prompt = normalized_go(prompt);
    if matches!(
        quote.join(" ").as_str(),
        "go" | "ok go" | "ok" | "yes" | "do it" | "dale" | "si" | "sí" | "hazlo" | "adelante"
    ) {
        quote == prompt
    } else {
        quote.len() >= 2 && prompt.windows(quote.len()).any(|words| words == quote)
    }
}

fn go_evidence(inner: &Inner, quote: &str, project: &str, now: u64) -> Result<Value> {
    if let Some(prompt) = inner.st.captain_prompts.iter().rev().find(|p| {
        p.at <= now
            && now - p.at <= GO_WINDOW_MS
            && p.project.as_deref().is_none_or(|p| p == project)
            && go_matches(quote, &p.text)
    }) {
        return Ok(
            json!({"kind":"captain_prompt","prompt_id":prompt.id,"pid":prompt.pid,"thread":prompt.thread,"project":prompt.project,"at":prompt.at}),
        );
    }
    if let Some(away) = &inner.st.away
        && away.since <= now
        && now - away.since <= GO_WINDOW_MS
        && go_matches(quote, &away.words)
    {
        return Ok(json!({"kind":"away","at":away.since}));
    }
    bail!(NO_GO)
}

#[test]
fn captain_go_normalization_requires_complete_short_replies_or_consecutive_words() {
    for reply in [
        "go", "ok go", "ok", "yes", "do it", "dale", "si", "sí", "hazlo", "adelante",
    ] {
        assert!(go_matches(&format!(" {}!!! ", reply.to_uppercase()), reply));
        assert!(!go_matches(reply, &format!("do not {reply}")), "{reply}");
    }
    assert!(go_matches(
        "BUILD  the, feature!",
        "Please build the feature today"
    ));
    assert!(!go_matches("feature", "Please build the feature today"));
    assert!(!go_matches(
        "build feature",
        "Please build the feature today"
    ));
    assert!(!go_matches(
        "build the feature tomorrow",
        "Please build the feature today"
    ));
    assert!(!go_matches("!!!", "!!!"));
    assert!(!go_matches("captain's", "keep the captain's words"));
    assert!(go_matches("captain’s words", "keep the captain's words"));
}

/// The kernel-wide default L3 model for a harness (UNVRS_L3_<HARNESS>_MODEL).
fn env_model(harness: &str) -> Option<String> {
    std::env::var(format!("UNVRS_L3_{}_MODEL", harness.to_uppercase()))
        .ok()
        .filter(|m| !m.trim().is_empty())
}

/// How a worker's requested model is held, for replies and stop reasons (S8, C11). The
/// driver enforces any requested model as an exact id, so an env default is a pin too.
pub(crate) fn model_terms(harness: &str, pinned: Option<&str>, asked: Option<&str>) -> String {
    match (pinned, asked) {
        (Some(m), _) => format!("model: {m}, pinned; the harness must confirm it before tools"),
        (None, Some(m)) => format!(
            "model: {m} from UNVRS_L3_{}_MODEL, enforced as an exact pin like --model (an alias such as `opus` ends the task on a mismatch; set a full model id or unset it)",
            harness.to_uppercase()
        ),
        (None, None) => format!("model: {harness}'s default (not pinned)"),
    }
}

/// A task pinned with --model keeps that model; it is never carried to another harness.
fn pinned_model(contract: Option<&Value>) -> Option<String> {
    contract
        .and_then(|c| c["model"].as_str().or_else(|| c["resolved_model"].as_str()))
        .map(str::to_owned)
}

/// A task pinned with --effort keeps both its effort and harness.
fn pinned_effort(contract: Option<&Value>) -> Option<String> {
    contract
        .and_then(|c| {
            c["effort"]
                .as_str()
                .or_else(|| c["resolved_effort"].as_str())
        })
        .map(str::to_owned)
}

/// A requested --effort: one of EFFORTS, and only on a harness that can confirm it.
pub(crate) fn parse_effort(raw: Option<&str>, harness: &str) -> Result<Option<String>> {
    let Some(e) = raw.map(str::trim).filter(|e| !e.is_empty()) else {
        return Ok(None);
    };
    let e = e.to_ascii_lowercase();
    ensure!(
        super::EFFORTS.contains(&e.as_str()),
        "--effort takes one of {}, not {e:?}",
        super::EFFORTS.join("|")
    );
    ensure!(
        matches!(harness, "claude" | "codex"),
        "--effort {e} refused on unsupported harness {harness}"
    );
    Ok(Some(e))
}

/// Normalize an explicit harness constraint; no default is chosen here.
pub(crate) fn requested_harness(asked: &[String]) -> Result<Option<String>> {
    let mut named: Vec<String> = asked
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    named.sort();
    named.dedup();
    match named.as_slice() {
        [] => Ok(None),
        [h] if matches!(h.as_str(), "claude" | "codex") => Ok(Some(h.clone())),
        [h] => bail!("Refused: harness {h:?} is not a driven CPU here (claude or codex)"),
        many => bail!(
            "Refused: conflicting harnesses requested ({}); name one",
            many.join(", ")
        ),
    }
}

pub fn other_harness(h: &str) -> &'static str {
    if h == "codex" { "claude" } else { "codex" }
}

/// Parses the last UNVRS-PROGRESS block into (now, next, open, done).
pub fn parse_progress(text: &str) -> Option<(String, String, Vec<String>, Vec<String>)> {
    let start = text.rfind("UNVRS-PROGRESS")?;
    let block = &text[start + "UNVRS-PROGRESS".len()..];
    let block = block.split("END-PROGRESS").next().unwrap_or(block);
    let (mut now, mut next) = (String::new(), String::new());
    let (mut open, mut done) = (vec![], vec![]);
    let mut list: Option<&mut Vec<String>> = None;
    for line in block.lines() {
        let l = line.trim().trim_start_matches('*').trim();
        if let Some(v) = l.strip_prefix("now:") {
            now = v.trim().into();
            list = None;
        } else if let Some(v) = l.strip_prefix("next:") {
            next = v.trim().into();
            list = None;
        } else if l.starts_with("open:") {
            list = Some(&mut open);
        } else if l.starts_with("done:") {
            list = Some(&mut done);
        } else if let Some(item) = l.strip_prefix("- ") {
            let item = item.trim();
            if let Some(list) = list.as_mut()
                && !item.is_empty()
                && !item.eq_ignore_ascii_case("(none)")
                && !item.eq_ignore_ascii_case("none")
            {
                list.push(item.into());
            }
        }
    }
    Some((now, next, open, done))
}

/// Each line of `text` with its byte offset, its raw length and its marker form:
/// trimmed, with markdown emphasis (`*`, `` ` ``, `#`) stripped.
fn marker_lines(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n').map(move |raw| {
        let start = at;
        at += raw.len();
        let marker = raw
            .trim()
            .trim_matches(|c| matches!(c, '*' | '`' | '#'))
            .trim();
        (start, raw.len(), marker)
    })
}

/// A FIELD-NOTES header line; `END-FIELD-NOTES` and prose that names the block are not.
fn is_notes_header(marker: &str) -> bool {
    marker == "FIELD-NOTES" || marker == "FIELD-NOTES:"
}

/// Body offset for a bare colon marker, or the end of a colon-less block header.
fn result_body_start(raw: &str, marker: &str) -> Option<usize> {
    raw.trim_start()
        .strip_prefix("UNVRS-RESULT:")
        .map(|rest| raw.len() - rest.len())
        .or_else(|| (marker == "UNVRS-RESULT").then_some(raw.len()))
}

/// The worker's result: the text after the last `UNVRS-RESULT:` or the last bare
/// `UNVRS-RESULT` line, up to an `END-RESULT` or FIELD-NOTES line. None when the reply
/// has neither form. PID 73 wrote the colon-less block and was cut off at the turn
/// limit (tasks/pid-81/REPORT.md).
pub fn parse_result(text: &str) -> Option<String> {
    let body_start = marker_lines(text)
        .filter_map(|(at, len, marker)| {
            result_body_start(&text[at..at + len], marker).map(|offset| at + offset)
        })
        .last()?;
    let body = &text[body_start..];
    let end = marker_lines(body)
        .find(|(_, _, m)| *m == "END-RESULT" || is_notes_header(m))
        .map_or(body.len(), |(at, ..)| at);
    Some(body[..end].trim().to_owned())
}

/// Parses the last FIELD-NOTES block (bullets up to an END- line or UNVRS-RESULT) into
/// its notes; None when the reply has no block. `- none` items are dropped.
pub fn parse_field_notes(text: &str) -> Option<Vec<String>> {
    let (at, len, _) = marker_lines(text)
        .filter(|(_, _, m)| is_notes_header(m))
        .last()?;
    let block = &text[at + len..];
    let end = marker_lines(block)
        .find(|(at, len, m)| {
            m.starts_with("END-") || result_body_start(&block[*at..*at + *len], m).is_some()
        })
        .map_or(block.len(), |(at, ..)| at);
    let block = &block[..end];
    Some(
        block
            .lines()
            .filter_map(|l| l.trim().trim_start_matches('*').trim().strip_prefix("- "))
            .map(str::trim)
            .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("none"))
            .take(20)
            .map(|n| clip(n, 500))
            .collect(),
    )
}

impl Kernel {
    /// None is the trusted captain terminal; workers inherit their admitted scope.
    pub(crate) fn task(
        self: &Arc<Self>,
        inner: &mut Inner,
        by: Option<usize>,
        contract: Value,
        asked: &[String],
    ) -> Result<usize> {
        let terminal = by.is_none();
        let by = by.unwrap_or_else(|| inner.st.l1().unwrap_or_else(|| self.create_l1(inner)));
        let rec = inner.st.pid(by)?.clone();
        let selected = contract["project"].as_str().filter(|p| !p.is_empty());
        let project = match rec.rank {
            1 => selected.context("L1 task needs --project <id>")?.to_owned(),
            _ => {
                let owned = rec.project.clone().context("seat without a project")?;
                ensure!(
                    selected.is_none_or(|p| p == owned),
                    "Task must use its own project"
                );
                owned
            }
        };
        super::crew::valid_project_id(&project)?;
        let configured = self.project(&project);
        ensure!(
            rec.rank != 1 || configured.is_some(),
            "Unknown project {project}"
        );
        let intent = contract["intent"].as_str().unwrap_or("").trim().to_owned();
        let spec = contract["spec"].as_str().unwrap_or("").trim().to_owned();
        ensure!(
            !intent.is_empty(),
            "A task needs the captain's intent in their words (--intent)"
        );
        ensure!(!spec.is_empty(), "A task needs your spec (--spec)");
        let done_when = contract["done_when"]
            .as_str()
            .context("--done-when requires one nonempty line")?
            .trim();
        ensure!(
            !done_when.is_empty()
                && !done_when.contains(['\n', '\r', '\u{0085}', '\u{2028}', '\u{2029}']),
            "--done-when requires one nonempty line"
        );
        let shape = contract["shape"].as_str().unwrap_or("report");
        match shape {
            "report" => {}
            "act" => bail!("act tasks need workspaces (0.9); dispatch a report task instead"),
            other => bail!("Unknown shape {other:?}: report or act"),
        }
        let authority = match contract.get("authority") {
            None if rec.rank == 3 => rec
                .contract
                .as_ref()
                .and_then(|c| c["authority"].as_str())
                .unwrap_or("report"),
            None => "implement",
            Some(value) => value
                .as_str()
                .context("--authority takes report|implement")?,
        };
        ensure!(
            matches!(authority, "report" | "implement"),
            "--authority takes report|implement, not {authority:?}"
        );
        let allowed = configured.map(|p| p.sources).unwrap_or_default();
        let sources: Vec<String> = contract["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect();
        for s in &sources {
            ensure!(
                allowed.contains(s),
                "Source {s:?} is not one of {project}'s sources ({})",
                allowed.join(", ")
            );
        }
        let admission = (|| -> Result<(String, Value, Option<String>)> {
            let requested = contract.get("go");
            let quote = requested.and_then(Value::as_str).map(str::trim);
            if requested.is_some_and(|v| !v.is_null()) {
                ensure!(quote.is_some_and(|q| !q.is_empty()), NO_GO);
            }
            if terminal {
                return Ok((
                    quote.unwrap_or(&intent).to_owned(),
                    json!({"kind":"captain_terminal","at":now_ms()}),
                    None,
                ));
            }
            if rec.rank == 3 {
                let parent = approved_contract(rec.contract.as_ref())?;
                let go = parent["go_quote"]
                    .as_str()
                    .or_else(|| parent["go"].as_str())
                    .filter(|g| !g.trim().is_empty())
                    .context(NO_GO)?;
                let source = parent
                    .get("go_source")
                    .or_else(|| parent.get("go_evidence"))
                    .filter(|s| {
                        matches!(
                            s["kind"].as_str(),
                            Some("captain_prompt" | "captain_terminal" | "away")
                        )
                    })
                    .context(NO_GO)?;
                let scope = parent["done_when"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .context(NO_GO)?;
                ensure!(matches!(rec.state.as_str(), "working" | "held"), NO_GO);
                ensure!(
                    quote.is_none_or(|q| normalized_go(q) == normalized_go(go)),
                    NO_GO
                );
                ensure!(
                    parent["authority"] == "implement" || authority == "report",
                    "Child task cannot exceed its parent's authority"
                );
                ensure!(
                    sources.iter().all(|s| parent["sources"]
                        .as_array()
                        .is_some_and(|allowed| allowed.iter().any(|a| a.as_str() == Some(s)))),
                    "Child task cannot exceed its parent's sources"
                );
                let mut source = source.clone();
                source["inherited_from"] = json!(by);
                let scope = match parent["go_scope"].as_str() {
                    Some(outer) => format!("{scope}; within: {outer}"),
                    None => scope.to_owned(),
                };
                return Ok((go.to_owned(), source, Some(scope)));
            }
            let quote = quote
                .or_else(|| inner.st.away.as_ref().map(|a| a.words.as_str()))
                .context(NO_GO)?;
            Ok((
                quote.to_owned(),
                go_evidence(inner, quote, &project, now_ms())?,
                None,
            ))
        })();
        let (go, go_source, go_scope) = match admission {
            Ok(admission) => admission,
            Err(error) => {
                self.event(
                    inner,
                    "refused",
                    json!({"verb":"task","pid":by,"reason":error.to_string()}),
                );
                self.save(inner);
                return Err(error);
            }
        };
        let decision = match self.econ_decision(inner, by, &contract, asked) {
            Ok(d) => d,
            Err(e) => {
                self.event(
                    inner,
                    "refused",
                    json!({"verb":"task","pid":by,"asked":asked,"reason":format!("{e:#}")}),
                );
                return Err(e);
            }
        };
        let harness = decision
            .chosen
            .as_ref()
            .map_or("pending", |c| c.harness.as_str());
        let requested = decision.overrides.harness.is_some();
        let model = decision.overrides.model.clone();
        let effort = decision.overrides.effort.clone();
        let text = format!("{intent}\n\nSpec: {spec}");
        let pid = self.create_worker_locked(
            inner,
            by,
            &text,
            harness,
            decision.chosen.as_ref().map(|c| c.model.as_str()),
            decision.chosen.as_ref().map(|c| c.effort.as_str()),
            if rec.rank == 1 { "l1" } else { "lead" },
        )?;
        inner.st.pid_mut(pid)?.project = Some(project.clone());
        let dir = self
            .u
            .project_dir(&project)
            .join(format!("tasks/pid-{pid}"));
        fs::create_dir_all(&dir)?;
        let mut contract = json!({"intent": intent, "spec": spec, "shape": shape, "sources": sources,
            "go_quote": go, "done_when": done_when, "go_source": go_source, "go_scope": go_scope,
            "rigor": contract["rigor"].as_str().unwrap_or("normal"), "authority": authority,
            "harness": harness, "harness_requested": requested,
            "kind":contract["kind"], "judgment":contract["judgment"], "thoroughness":contract["thoroughness"]});
        if let Some(m) = &model {
            contract["model"] = json!(m);
        }
        if let Some(e) = &effort {
            contract["effort"] = json!(e);
        }
        fs::write(
            dir.join("contract.json"),
            serde_json::to_vec_pretty(&contract)?,
        )?;
        let r = inner.st.pid_mut(pid)?;
        r.contract = Some(contract.clone());
        if let Some(d) = r.driven.as_mut() {
            d.cwd = dir;
        }
        self.event(
            inner,
            "task",
            json!({"pid": pid, "parent": by, "project": project, "intent": clip(&intent, 300), "go_quote": go, "done_when": done_when, "go_source": go_source, "go_scope": go_scope, "shape": shape, "authority": authority, "sources": sources, "harness": harness, "harness_requested": requested, "model": model, "effort": effort}),
        );
        let admitted = if decision.chosen.is_some() {
            self.launch_econ(inner, pid, &decision)
        } else {
            self.econ_hold(inner, pid, &decision)
        };
        self.save(inner);
        admitted?;
        Ok(pid)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_worker_locked(
        self: &Arc<Self>,
        inner: &mut Inner,
        parent: usize,
        task: &str,
        harness: &str,
        model: Option<&str>,
        effort: Option<&str>,
        by: &str,
    ) -> Result<usize> {
        ensure!(
            matches!(harness, "claude" | "codex" | "pending"),
            "Driven L3 CPUs in this voyage: claude or codex"
        );
        ensure!(
            !task.trim().is_empty() && task.len() <= 8000,
            "Task must contain 1–8000 bytes"
        );
        let prec = inner.st.pid(parent)?.clone();
        let pid = inner.st.create(
            parent,
            3,
            "driven",
            harness,
            task,
            prec.scope.clone(),
            prec.mission.clone(),
        );
        inner.st.pid_mut(pid)?.project = prec.project.clone();
        inner
            .mem
            .set_scope(MemoryActor::Captain, pid, prec.scope.clone())?;
        let brief = Brief {
            goal: task.into(),
            mission: prec.mission.clone(),
            now: "not started".into(),
            next: vec!["start the task".into()],
            open: vec![format!("[o1] {}", clip(task, 300))],
            ..Default::default()
        };
        inner.mem.write_brief(pid, "spawn", brief)?;
        let cwd = self.u.root().to_path_buf();
        let rec = inner.st.pid_mut(pid)?;
        rec.state = "held".into();
        rec.next = "await route admission".into();
        rec.driven = Some(Driven {
            cpu: "headless".into(),
            cwd,
            model: model.map(str::to_owned).or_else(|| env_model(harness)),
            effort: effort.map(str::to_owned),
            ..Default::default()
        });
        let model = rec.driven.as_ref().and_then(|d| d.model.clone());
        self.event(
            inner,
            "spawn",
            json!({"pid": pid, "rank": 3, "seat": "driven", "harness": harness, "parent": parent, "task": clip(task, 300), "by": by, "model": model, "effort": effort}),
        );
        Ok(pid)
    }

    pub(crate) fn start_worker(self: &Arc<Self>, pid: usize) {
        {
            let mut inner = self.lock();
            if !inner.running.insert(pid) {
                return;
            }
        }
        let k = Arc::clone(self);
        thread::spawn(move || k.worker_loop(pid));
    }

    fn worker_prompt(&self, inner: &Inner, pid: usize) -> Result<String> {
        let rec = inner.st.pid(pid)?;
        let d = rec.driven.as_ref().context("not a driven seat")?;
        // Mail from the lead (S4); worker_loop marks it read after a successful turn.
        let unread: Vec<String> = rec
            .mailbox
            .iter()
            .filter(|m| !m.read)
            .map(|m| format!("- from {}: {}", m.from, m.text))
            .collect();
        if d.turns == 0 {
            let mut package = hot::hot_text(self, inner, pid)?;
            if let Some(p) = &d.preamble {
                package = format!("{p}\n\n{package}");
            }
            if !unread.is_empty() {
                package = format!("{package}\n\nNew messages:\n{}", unread.join("\n"));
            }
            let from = rec.handed_from.map(|f| {
                (
                    f,
                    inner
                        .st
                        .pid(f)
                        .map(|r| r.harness.clone())
                        .unwrap_or_default(),
                )
            });
            let authority = rec
                .contract
                .as_ref()
                .and_then(|c| c["authority"].as_str())
                .unwrap_or("implement");
            Ok(self.mapp.worker_prompt(
                pid,
                &rec.harness,
                authority,
                &package,
                &super::unvrs_cmd(),
                from,
            ))
        } else {
            Ok(self.mapp.worker_continue(
                if rec.next.is_empty() {
                    "see your last progress block"
                } else {
                    &rec.next
                },
                &unread.join("\n"),
            ))
        }
    }

    pub(crate) fn worker_loop(self: Arc<Self>, pid: usize) {
        let _owner = crate::signals::owner_scope(self.u.root(), pid);
        loop {
            let (req, mail_through) = {
                let mut inner = self.lock();
                let Ok(rec) = inner.st.pid(pid).cloned() else {
                    inner.running.remove(&pid);
                    return;
                };
                if rec.state != "working"
                    || rec
                        .driven
                        .as_ref()
                        .is_none_or(|d| d.cpu != "headless" || d.busy || d.recovering)
                {
                    inner.running.remove(&pid);
                    return;
                }
                if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
                    // Parked between turns for a deploy; the next kernel requeues it.
                    inner.running.remove(&pid);
                    return;
                }
                if let Err(error) = approved_contract(rec.contract.as_ref()) {
                    inner.running.remove(&pid);
                    self.event(
                        &mut inner,
                        "refused",
                        json!({"verb":"worker.start","pid":pid,"reason":error.to_string()}),
                    );
                    self.finish(&mut inner, pid, &error.to_string(), "blocked");
                    self.save(&mut inner);
                    return;
                }
                if rec.quota_low {
                    inner.running.remove(&pid);
                    self.handoff_or_fail(&mut inner, pid);
                    self.save(&mut inner);
                    return;
                }
                let prompt = match self.worker_prompt(&inner, pid) {
                    Ok(p) => hot::quiet_mentions(&p),
                    Err(e) => {
                        inner.running.remove(&pid);
                        self.event(
                            &mut inner,
                            "error",
                            json!({"pid": pid, "error": format!("{e:#}")}),
                        );
                        return;
                    }
                };
                let d = rec.driven.clone().unwrap_or_default();
                let mail_through = inner.st.mail_seq;
                if let Ok(r) = inner.st.pid_mut(pid)
                    && let Some(d) = r.driven.as_mut()
                {
                    d.busy = true;
                }
                self.save(&mut inner);
                (
                    TurnRequest {
                        harness: rec.harness.clone(),
                        cwd: d.cwd.clone(),
                        session: d.session.clone(),
                        prompt,
                        env: self.driven_env(pid),
                        model: d.model.clone(),
                        effort: d.effort.clone(),
                    },
                    mail_through,
                )
            };
            let k = Arc::clone(&self);
            let result = self.driver_turn(pid, &req, &move |os| {
                let mut inner = k.lock();
                if let Ok(r) = inner.st.pid_mut(pid)
                    && let Some(d) = r.driven.as_mut()
                {
                    d.os_pid = Some(os);
                }
                // A stop that arrived before the process existed signals it now.
                if inner.st.pid(pid).is_ok_and(|r| r.state == "stopping") {
                    let _ = crate::signals::signal_group(i64::from(os), libc::SIGTERM);
                }
                k.save(&mut inner);
            });
            // Read outside the lock (git on a big tree may take a moment), and only
            // for a turn that returned: a failed turn goes to recovery unmarked.
            let head = result
                .as_ref()
                .ok()
                .and_then(|_| super::recover::work_head(&req.cwd, self.u.root()));
            let mut inner = self.lock();
            let Ok(rec) = inner.st.pid(pid).cloned() else {
                inner.running.remove(&pid);
                return;
            };
            if let Ok(r) = inner.st.pid_mut(pid)
                && let Some(d) = r.driven.as_mut()
            {
                d.busy = false;
                d.turns += 1;
            }
            if rec.state == "stopping" {
                // Its owner stopped it during the turn. The driver has returned, so
                // the process group is reaped: checkpoint and end, never continue.
                if let Err(e) = &result {
                    self.event(
                        &mut inner,
                        "turn",
                        json!({"pid": pid, "harness": rec.harness, "error": format!("{e:#}")}),
                    );
                }
                let why = rec
                    .driven
                    .as_ref()
                    .and_then(|d| d.stop_request.clone())
                    .unwrap_or_default();
                self.recover(inner, pid, super::recover::Stop::Cancelled(why));
                return;
            }
            let refused_model = result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .or_else(|| result.as_ref().ok().and_then(|o| o.error.clone()));
            if let Some(reason) = refused_model.filter(|e| {
                e.to_ascii_lowercase().contains("model")
                    || e.to_ascii_lowercase().contains("effort")
            }) {
                self.invalidate_econ(&mut inner, &rec.harness, &reason);
            }
            if let Ok(out) = &result
                && ((req.model.is_some() && out.model.is_some() && req.model != out.model)
                    || (req.effort.is_some() && out.effort.is_some() && req.effort != out.effort))
            {
                self.invalidate_econ(
                    &mut inner,
                    &rec.harness,
                    "harness refused the routed model or effort",
                );
            }
            let handed = rec.state != "working";
            match result {
                Ok(out) => {
                    if let Some(error) = &out.error {
                        let stop = super::recover::classify_error(error);
                        if matches!(stop, super::recover::Stop::CleanupUnconfirmed(_)) {
                            self.recover(inner, pid, stop);
                            return;
                        }
                    }
                    if let Some(rate) = &out.rate {
                        let key = format!("{}:rate", rec.harness);
                        inner.quota.insert(key, rate.clone());
                    }
                    if let Ok(r) = inner.st.pid_mut(pid)
                        && let Some(d) = r.driven.as_mut()
                    {
                        if out.session.is_some() {
                            d.session = out.session.clone();
                        }
                        for tool in &out.tools {
                            d.remember_tool(tool);
                        }
                        if out.model.is_some() {
                            d.actual_model = out.model.clone();
                        }
                        if d.effort.is_some() {
                            d.actual_effort = out.effort.clone();
                            d.effort_evidence = out.effort_evidence.clone();
                        }
                    }
                    let turn_no = rec.driven.as_ref().map_or(0, |d| d.turns + 1);
                    if let Err(e) = inner.mem.append_turn(
                        pid,
                        &format!(
                            "{} turn {turn_no}{}:\n{}",
                            rec.harness,
                            if out.tools.is_empty() {
                                String::new()
                            } else {
                                format!(" (tools: {})", clip(&out.tools.join(" ; "), 600))
                            },
                            out.text.trim()
                        ),
                    ) {
                        self.event(
                            &mut inner,
                            "memory.append",
                            json!({"pid":pid,"error":format!("{e:#}")}),
                        );
                    }
                    self.event(
                        &mut inner,
                        "turn",
                        json!({"pid": pid, "harness": rec.harness, "turn": turn_no, "tools": out.tools.iter().take(12).collect::<Vec<_>>(), "quota": out.quota, "error": out.error, "model": out.model, "effort": out.effort, "effort_evidence": out.effort_evidence}),
                    );
                    // The driver kills a mismatching CPU itself; this also catches a
                    // driver that reports the model but does not enforce it.
                    if let Some(asked) = &req.model
                        && !handed
                        && out.model.as_ref() != Some(asked)
                    {
                        self.recover(
                            inner,
                            pid,
                            super::recover::Stop::ModelMismatch(format!(
                                "model mismatch: requested {asked}, {} confirmed {}{}",
                                rec.harness,
                                out.model.as_deref().unwrap_or("nothing"),
                                if pinned_model(rec.contract.as_ref()).is_none() {
                                    format!(" ({})", model_terms(&rec.harness, None, Some(asked)))
                                } else {
                                    String::new()
                                }
                            )),
                        );
                        return;
                    }
                    // Same for effort: a pinned effort the driver did not confirm this
                    // turn ends the task (never a silent downgrade).
                    if let Some(asked) = &req.effort
                        && !handed
                        && out.effort.as_ref() != Some(asked)
                    {
                        self.recover(
                            inner,
                            pid,
                            super::recover::Stop::EffortMismatch(format!(
                                "effort not verified: requested {asked}, {} confirmed {}",
                                rec.harness,
                                out.effort.as_deref().unwrap_or("nothing")
                            )),
                        );
                        return;
                    }
                    if handed {
                        inner.running.remove(&pid);
                        self.save(&mut inner);
                        return;
                    }
                    if out.error.is_none() {
                        let mut delivered = vec![];
                        if let Ok(r) = inner.st.pid_mut(pid) {
                            for m in r
                                .mailbox
                                .iter_mut()
                                .filter(|m| !m.read && m.id <= mail_through)
                            {
                                m.read = true;
                                delivered.push(m.id);
                            }
                        }
                        if !delivered.is_empty() {
                            self.event(
                                &mut inner,
                                "mail.delivered",
                                json!({"pid": pid, "ids": delivered}),
                            );
                        }
                    }
                    if let Some((now, next, open, done)) = parse_progress(&out.text) {
                        let mut brief = inner.mem.session(pid).map(|s| s.brief).unwrap_or_default();
                        brief.now = now.clone();
                        brief.next = if next.is_empty() {
                            vec![]
                        } else {
                            vec![next.clone()]
                        };
                        brief.open = open;
                        brief.done = done;
                        match inner.mem.write_brief(pid, "progress", brief) {
                            Ok(FoldOutcome::Applied { version, .. }) => {
                                if let Ok(r) = inner.st.pid_mut(pid) {
                                    r.next = next.clone();
                                }
                                self.event(
                                    &mut inner,
                                    "progress",
                                    json!({"pid": pid, "now": clip(&now, 200), "next": clip(&next, 200), "version": version}),
                                );
                            }
                            Ok(FoldOutcome::Discarded { read, current }) => self.event(
                                &mut inner,
                                "fold",
                                json!({"pid": pid, "reason": "progress", "result": "discarded", "read": read, "current": current}),
                            ),
                            Err(e) => self.event(
                                &mut inner,
                                "fold",
                                json!({"pid": pid, "reason": "progress", "result": "rejected", "error": format!("{e:#}")}),
                            ),
                        }
                    }
                    let quota = out.quota || inner.st.pid(pid).is_ok_and(|r| r.quota_low);
                    if quota {
                        inner.running.remove(&pid);
                        self.handoff_or_fail(&mut inner, pid);
                        self.save(&mut inner);
                        return;
                    }
                    if let Some(error) = &out.error {
                        let stop = super::recover::classify_error(error);
                        self.recover(inner, pid, stop);
                        return;
                    }
                    // Field notes are filed on exit (mapp-unvrs §7, S7); a block written
                    // after the result is not part of the result (parse_result cuts it).
                    if let Some(result) = parse_result(&out.text) {
                        // Context ownership (context-ownership.md §4): accept, send back
                        // with the reasons, or end blocked after MAX_BOUNCES.
                        match self.ownership_gate(&mut inner, pid, &out.text) {
                            super::ownership::Gate::Accept => {}
                            super::ownership::Gate::Bounce => {
                                self.save(&mut inner);
                                continue;
                            }
                            super::ownership::Gate::Block(why) => {
                                self.finish(&mut inner, pid, &why, "blocked");
                                inner.running.remove(&pid);
                                self.save(&mut inner);
                                return;
                            }
                        }
                        let notes = match parse_field_notes(&out.text) {
                            Some(n) if n.is_empty() => {
                                vec!["none: the worker had nothing to file".into()]
                            }
                            Some(n) => n,
                            None => vec!["none: the final reply had no FIELD-NOTES block".into()],
                        };
                        self.finish_with_notes(&mut inner, pid, &result, "done", notes);
                        inner.running.remove(&pid);
                        self.save(&mut inner);
                        return;
                    }
                    // No turn budget: an attempt ends only when turns stop changing
                    // the brief and the work tree's HEAD.
                    let brief = inner.mem.session(pid).map(|s| s.brief).unwrap_or_default();
                    let mark = super::recover::progress_mark(&brief, head.as_deref());
                    let idle = inner
                        .st
                        .pid_mut(pid)
                        .ok()
                        .and_then(|r| r.driven.as_mut())
                        .map_or(0, |d| {
                            d.idle_turns = if d.turn_mark.as_deref() == Some(mark.as_str()) {
                                d.idle_turns + 1
                            } else {
                                0
                            };
                            d.turn_mark = Some(mark);
                            d.idle_turns
                        });
                    if idle >= super::recover::stall_turns() {
                        self.recover(inner, pid, super::recover::Stop::NoProgress(idle));
                        return;
                    }
                }
                Err(e) => {
                    self.event(
                        &mut inner,
                        "turn",
                        json!({"pid": pid, "harness": rec.harness, "error": format!("{e:#}")}),
                    );
                    if handed {
                        inner.running.remove(&pid);
                        return;
                    }
                    let stop = super::recover::classify_error(&format!("{e:#}"));
                    if !matches!(stop, super::recover::Stop::CleanupUnconfirmed(_))
                        && inner.st.pid(pid).is_ok_and(|r| r.quota_low)
                    {
                        inner.running.remove(&pid);
                        self.handoff_or_fail(&mut inner, pid);
                        self.save(&mut inner);
                        return;
                    }
                    self.recover(inner, pid, stop);
                    return;
                }
            }
            self.save(&mut inner);
        }
    }

    /// A stop without a result (worker-recovery.md): checkpoint outside the lock, get a
    /// recovery summary without it when the brief never advanced, then continue in
    /// place with a fresh session or escalate. Consumes the lock the loop held.
    pub(crate) fn recover(
        self: &Arc<Self>,
        mut inner: std::sync::MutexGuard<'_, Inner>,
        pid: usize,
        stop: super::recover::Stop,
    ) {
        let _owner = crate::signals::owner_scope(self.u.root(), pid);
        self.stop_event(&mut inner, pid, stop.label(), stop.retryable());
        if matches!(stop, super::recover::Stop::CleanupUnconfirmed(_)) {
            if let Ok(r) = inner.st.pid_mut(pid)
                && let Some(d) = r.driven.as_mut()
            {
                d.attempts.push(json!({"pid":pid, "attempt":d.attempt, "stop":stop.label(), "turns":d.turns, "checkpoint_skipped":true}));
                d.recovering = false;
            }
            self.finish(&mut inner, pid, &format!("blocked: {}; no checkpoint or continuation because writer cleanup is unconfirmed", stop.describe()), "blocked");
            inner.running.remove(&pid);
            self.save(&mut inner);
            return;
        }
        let cwd = inner
            .st
            .pid(pid)
            .ok()
            .and_then(|r| r.driven.as_ref())
            .map(|d| d.cwd.clone())
            .unwrap_or_else(|| self.u.root().to_path_buf());
        if let Ok(r) = inner.st.pid_mut(pid)
            && let Some(d) = r.driven.as_mut()
        {
            d.recovering = true;
        }
        self.save(&mut inner);
        drop(inner);
        let job = format!("checkpoint:{pid}");
        let budget = super::recover::checkpoint_budget().as_millis() as u64;
        self.job_begin(&job, budget * super::recover::CHECKPOINT_COMMANDS + 5_000);
        let checkpoint = super::recover::git_checkpoint(&cwd, self.u.root(), pid, stop.label());
        self.job_finish(
            &job,
            checkpoint.as_ref().err().map(|e| format!("{e:#}")),
            false,
        );
        let mut inner = self.lock();
        let mut saved = self.checkpoint_locked(&mut inner, pid, &stop, checkpoint);
        self.save(&mut inner);
        drop(inner);
        if saved.needs_summary && stop.retryable() && saved.checkpoint_error.is_none() {
            self.recovery_summary(pid, &stop);
        }
        let mut inner = self.lock();
        saved.brief_version = inner
            .mem
            .session(pid)
            .map(|s| s.brief_version)
            .unwrap_or(saved.brief_version);
        let mut continued = false;
        if inner.st.pid(pid).is_ok_and(|r| r.state == "working") {
            continued = self.continue_or_escalate(&mut inner, pid, &stop, &saved);
        } else if inner.st.pid(pid).is_ok_and(|r| r.state == "stopping") {
            self.finish_stopped(&mut inner, pid);
        }
        if let Ok(r) = inner.st.pid_mut(pid)
            && let Some(d) = r.driven.as_mut()
        {
            d.recovering = false;
        }
        self.save(&mut inner);
        if continued {
            // Same PID, fresh session: the loop's next turn sends the full package.
            let k = Arc::clone(self);
            thread::spawn(move || k.worker_loop(pid));
        } else {
            inner.running.remove(&pid);
        }
    }

    /// Ends a task its owner stopped (`stop`, ops/stop.rs) after its checkpoint.
    pub(crate) fn finish_stopped(&self, inner: &mut Inner, pid: usize) {
        let why = inner
            .st
            .pid(pid)
            .ok()
            .and_then(|r| r.driven.as_ref())
            .and_then(|d| d.stop_request.clone())
            .unwrap_or_else(|| "stopped by its owner".into());
        self.finish(inner, pid, &format!("cancelled: {why}"), "cancelled");
    }

    /// Low-quota handoff from the worker loop; a task that cannot move (e.g. pinned to
    /// a model on this harness) ends as failed instead of stalling.
    fn handoff_or_fail(self: &Arc<Self>, inner: &mut Inner, pid: usize) {
        if let Err(e) = self.handoff_locked(inner, pid, None, "low quota") {
            self.finish(
                inner,
                pid,
                &format!("failed: low quota, no handoff: {e:#}"),
                "failed",
            );
        }
    }

    /// Ends a task: the result package goes next to its contract and the lead gets a
    /// wake pointing at it (no relay by the captain).
    pub(crate) fn finish(&self, inner: &mut Inner, pid: usize, result: &str, how: &str) {
        let notes = vec![format!(
            "none: the task ended {how} before the worker filed notes"
        )];
        self.finish_with_notes(inner, pid, result, how, notes);
    }

    /// `finish` with the worker's field notes; they go into result.json and the lead's
    /// wake. A missing note list is recorded as `none: <reason>`, never `[]`.
    pub(crate) fn finish_with_notes(
        &self,
        inner: &mut Inner,
        pid: usize,
        result: &str,
        how: &str,
        notes: Vec<String>,
    ) {
        let notes: Vec<String> = notes.iter().map(|n| inner.mem.redact(n)).collect();
        let Ok(rec) = inner.st.pid(pid).cloned() else {
            return;
        };
        if let Ok(r) = inner.st.pid_mut(pid) {
            r.state = "ended".into();
            r.result = Some(clip(result, 4000));
        }
        let origin = rec
            .handed_from
            .map(|f| format!(" (continued from PID {f})"))
            .unwrap_or_default();
        let brief = inner.mem.session(pid).map(|s| s.brief).unwrap_or_default();
        let mut reference = None;
        if let Some(project) = &rec.project {
            let dir = self.u.project_dir(project).join(format!("tasks/pid-{pid}"));
            let path = dir.join("result.json");
            let package = json!({"pid": pid, "parent": rec.parent, "project": project,
                "harness": rec.harness, "how": how, "contract": rec.contract,
                "model": rec.driven.as_ref().and_then(|d| d.model.clone()),
                "actual_model": rec.driven.as_ref().and_then(|d| d.actual_model.clone()),
                "effort": rec.driven.as_ref().and_then(|d| d.effort.clone()),
                "actual_effort": rec.driven.as_ref().and_then(|d| d.actual_effort.clone()),
                "effort_evidence": rec.driven.as_ref().and_then(|d| d.effort_evidence.clone()),
                "session": rec.driven.as_ref().and_then(|d| d.session.clone()),
                "tools": rec.driven.as_ref().map(|d| d.last_tools.iter().rev().take(8).map(|t| t.split(':').next().unwrap_or("tool")).collect::<Vec<_>>()).unwrap_or_default(),
                "last_activity": rec.driven.as_ref().and_then(|d| d.last_activity),
                "attempt": rec.driven.as_ref().map(|d| d.attempt),
                "lineage": rec.driven.as_ref().map(|d| d.lineage.unwrap_or(pid)),
                "last_stop": rec.driven.as_ref().and_then(|d| d.last_stop.clone()),
                "attempts": rec.driven.as_ref().map(|d| d.attempts.clone()).unwrap_or_default(),
                "result": inner.mem.redact(result), "brief": brief, "handed_from": rec.handed_from,
                "field_notes": notes, "finished": now_ms(),
                "ownership": fs::read(dir.join(super::ownership::FILE)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())});
            if fs::create_dir_all(&dir).is_ok()
                && fs::write(
                    &path,
                    serde_json::to_vec_pretty(&package).unwrap_or_default(),
                )
                .is_ok()
            {
                reference = Some(path.display().to_string());
            }
        }
        // A retryable stop never ends a task here any more (recover.rs); "limit" and
        // driver failures arrive as "blocked" with what was tried.
        let kind = match how {
            "done" => "done",
            "blocked" => "blocked",
            _ => "failed",
        };
        let intent = rec
            .contract
            .as_ref()
            .and_then(|c| c["intent"].as_str())
            .unwrap_or(&rec.task)
            .to_owned();
        let model = rec
            .driven
            .as_ref()
            .map(|d| {
                let effort = super::state::effort_label(d)
                    .map(|e| format!(" · {e}"))
                    .unwrap_or_default();
                format!(" · {}{effort}", super::state::model_label(d))
            })
            .unwrap_or_default();
        let filed: Vec<&str> = notes
            .iter()
            .map(String::as_str)
            .filter(|n| !n.starts_with("none:"))
            .collect();
        let filed = if filed.is_empty() {
            String::new()
        } else {
            format!("\nField notes: {}", clip(&filed.join("; "), 800))
        };
        let text = format!(
            "L3 PID {pid} on {}{model}{origin} finished \"{}\" ({how}): {}{filed}",
            rec.harness,
            clip(&intent, 160),
            clip(result, 1500)
        );
        if inner.st.pids.get(&rec.parent).is_some_and(|p| p.rank <= 2) {
            let _ = self.wake_seat(inner, rec.parent, kind, &text, Some(pid), reference.clone());
        } else {
            let mut parent = rec.parent;
            while let Some(next) = inner.st.pids.get(&parent).and_then(|p| p.handed_to) {
                parent = next;
            }
            let package = reference
                .as_ref()
                .map(|r| format!("\nResult package: {r}"))
                .unwrap_or_default();
            let _ = inner
                .st
                .mail(parent, &format!("PID {pid}"), &format!("{text}{package}"));
        }
        self.event(
            inner,
            "result",
            json!({"pid": pid, "parent": rec.parent, "harness": rec.harness, "model": rec.driven.as_ref().and_then(|d| d.model.clone()), "actual_model": rec.driven.as_ref().and_then(|d| d.actual_model.clone()), "effort": rec.driven.as_ref().and_then(|d| d.effort.clone()), "actual_effort": rec.driven.as_ref().and_then(|d| d.actual_effort.clone()), "how": how, "result": clip(result, 400), "intent": clip(&intent, 200), "package": reference}),
        );
    }

    /// Low-quota signal: a driven L3 is handed off; an attached seat only gets a
    /// suggestion in L1's hot set (the kernel never moves an attached seat). A task
    /// pinned to its model or effort cannot move, so it ends instead (worker-recovery.md).
    /// The caller is already authorized (ops.rs `may_signal`).
    pub(crate) fn quota_low(self: &Arc<Self>, inner: &mut Inner, pid: usize) -> Result<String> {
        let rec = inner.st.pid(pid)?.clone();
        self.event(
            inner,
            "quota",
            json!({"pid": pid, "rank": rec.rank, "harness": rec.harness, "signal": "low"}),
        );
        if rec.kind != "driven" {
            return Ok(format!(
                "PID {pid} (L{}) is a seat; the captain moves it with $unvrs:l1 or $unvrs:l2 in another app",
                rec.rank
            ));
        }
        ensure!(
            rec.state == "working",
            "PID {pid} is {}; nothing to hand off",
            rec.state
        );
        let pin = pinned_model(rec.contract.as_ref())
            .map(|m| format!("model {m}"))
            .or_else(|| pinned_effort(rec.contract.as_ref()).map(|e| format!("effort {e}")));
        let other = other_harness(&rec.harness);
        let busy = rec.driven.as_ref().is_some_and(|d| d.busy);
        inner.st.pid_mut(pid)?.quota_low = true;
        if busy && let Some(os) = rec.driven.as_ref().and_then(|d| d.os_pid) {
            let _ = crate::signals::signal_group(i64::from(os), libc::SIGTERM);
        }
        let running = busy || inner.running.contains(&pid);
        Ok(match (&pin, busy, running) {
            (Some(pin), true, _) => format!(
                "PID {pid} is low on {}; its turn was stopped. It is pinned to {pin} on {}, so it will not move to {other}: the task ends as failed",
                rec.harness, rec.harness
            ),
            (Some(pin), false, true) => format!(
                "PID {pid} is low on {}; it is pinned to {pin} on {}, so it will not move to {other}: the task ends as failed after its current step",
                rec.harness, rec.harness
            ),
            (Some(pin), false, false) => {
                self.handoff_or_fail(inner, pid);
                format!(
                    "PID {pid} is low on {}; it is pinned to {pin} on {}, so it did not move to {other}: the task ended as failed",
                    rec.harness, rec.harness
                )
            }
            (None, true, _) => format!(
                "PID {pid} is low on {}; its turn was stopped and the task is being handed off to {other}",
                rec.harness
            ),
            (None, false, false) => {
                let new = self.handoff_locked(inner, pid, None, "low quota")?;
                format!("PID {pid} handed off to PID {new} on {other}")
            }
            (None, false, true) => format!("PID {pid} will hand off after its current step"),
        })
    }

    /// Rank changes and CPU changes of a driven seat are handoffs: new PID, the brief as
    /// package (bounded, pointers, every open item), one journal event old → new.
    pub(crate) fn handoff_locked(
        self: &Arc<Self>,
        inner: &mut Inner,
        pid: usize,
        to: Option<&str>,
        reason: &str,
    ) -> Result<usize> {
        let rec = inner.st.pid(pid)?.clone();
        approved_contract(rec.contract.as_ref())?;
        ensure!(
            rec.kind == "driven" && rec.rank == 3,
            "Handoff moves driven L3 work; attached seats move with $unvrs:l1 or $unvrs:l2"
        );
        ensure!(
            matches!(rec.state.as_str(), "working" | "idle"),
            "PID {pid} is {}",
            rec.state
        );
        let to = to.unwrap_or(other_harness(&rec.harness)).to_owned();
        ensure!(
            matches!(to.as_str(), "claude" | "codex"),
            "Handoff target must be claude or codex"
        );
        let pinned = pinned_model(rec.contract.as_ref());
        if let Some(m) = &pinned {
            ensure!(
                to == rec.harness,
                "PID {pid} is pinned to model {m} on {}; a handoff to {to} would substitute the model",
                rec.harness
            );
        }
        let effort = pinned_effort(rec.contract.as_ref());
        if let Some(e) = &effort {
            ensure!(
                to == rec.harness,
                "PID {pid} is pinned to effort {e} on {}; a handoff to {to} would change its harness",
                rec.harness
            );
        }
        if rec
            .contract
            .as_ref()
            .is_some_and(|c| c["harness_requested"] == true)
        {
            ensure!(
                to == rec.harness,
                "PID {pid} was dispatched on {} as requested; a handoff to {to} would change its harness",
                rec.harness
            );
        }
        let prior = rec.driven.clone().unwrap_or_default();
        ensure!(
            !prior.recovering,
            "PID {pid} is checkpointing or summarizing; wait for recovery to finish"
        );
        ensure!(
            !prior.busy,
            "PID {pid} has an active driver; wait for its turn to finish and its process group to be reaped before handoff"
        );
        if let Some(max) = prior
            .max_continuations
            .or_else(super::recover::max_continuations)
        {
            ensure!(
                prior.attempt < max,
                "PID {pid} reached its continuation ceiling ({max})"
            );
        }
        let brief = inner.mem.session(pid)?.brief;
        let package = hot::hot_text(self, inner, pid)?;
        let new = inner.st.create(
            rec.parent,
            3,
            "driven",
            &to,
            &rec.task,
            rec.scope.clone(),
            rec.mission.clone(),
        );
        inner
            .mem
            .set_scope(MemoryActor::Captain, new, rec.scope.clone())?;
        inner
            .mem
            .write_brief(new, "handoff package", brief.clone())?;
        let work = format!("pid{pid}-to-pid{new}-{}", now_ms());
        let path = self.u.root().join(format!("handoffs/{work}.json"));
        fs::create_dir_all(path.parent().context("handoff dir")?)?;
        let mut value = json!({
            "work_id": work, "from_pid": pid, "to_pid": new, "rank": 3,
            "from_harness": rec.harness, "to_harness": to, "reason": reason,
            "created": now_ms(), "brief": brief, "package": package,
        });
        value["package"] = json!(inner.mem.redact(value["package"].as_str().unwrap_or("")));
        let bytes = serde_json::to_vec_pretty(&value)?;
        ensure!(bytes.len() <= 32000, "Handoff package exceeds 32000 bytes");
        fs::write(&path, &bytes)?;
        {
            let old = inner.st.pid_mut(pid)?;
            old.state = "handed-off".into();
            old.handed_to = Some(new);
        }
        inner.st.carry_mail(pid, new)?;
        let cwd = rec
            .driven
            .as_ref()
            .map(|d| d.cwd.clone())
            .unwrap_or_else(|| self.u.root().to_path_buf());
        {
            let n = inner.st.pid_mut(new)?;
            n.handed_from = Some(pid);
            n.state = "working".into();
            n.next = rec.next.clone();
            n.driven = Some(Driven {
                cpu: "headless".into(),
                cwd,
                model: pinned.clone().or_else(|| env_model(&to)),
                effort: effort.clone(),
                attempt: prior.attempt + 1,
                lineage: Some(prior.lineage.unwrap_or(pid)),
                stalls: prior.stalls,
                progress_mark: prior.progress_mark,
                attempts: prior.attempts,
                max_continuations: prior.max_continuations,
                ..Default::default()
            });
        }
        {
            let n = inner.st.pid_mut(new)?;
            n.project = rec.project.clone();
            n.contract = rec.contract.clone();
        }
        for child in inner.st.pids.values_mut().filter(|p| p.parent == pid) {
            child.parent = new;
        }
        self.event(
            inner,
            "handoff",
            json!({"from_pid": pid, "to_pid": new, "rank": 3, "from_harness": rec.harness, "to_harness": to, "reason": reason, "package": path, "bytes": bytes.len(), "open": brief.open.len(), "done": brief.done.len()}),
        );
        inner.running.insert(new);
        let k = Arc::clone(self);
        thread::spawn(move || k.worker_loop(new));
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_is_a_known_level_on_supported_harnesses() {
        assert_eq!(parse_effort(None, "claude").unwrap(), None);
        assert_eq!(parse_effort(Some(" "), "codex").unwrap(), None);
        assert_eq!(
            parse_effort(Some("High"), "claude").unwrap().as_deref(),
            Some("high")
        );
        for e in super::super::EFFORTS {
            assert_eq!(parse_effort(Some(e), "claude").unwrap().as_deref(), Some(e));
        }
        assert!(
            parse_effort(Some("ultra"), "claude")
                .unwrap_err()
                .to_string()
                .contains("low|medium|high|xhigh|max")
        );
        assert_eq!(
            parse_effort(Some("high"), "codex").unwrap().as_deref(),
            Some("high")
        );
        assert!(parse_effort(Some("high"), "pi").is_err());
    }

    #[test]
    fn the_requested_harness_is_chosen_or_refused() {
        let s = |v: &[&str]| v.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>();
        assert_eq!(requested_harness(&[]).unwrap(), None);
        assert_eq!(
            requested_harness(&s(&["Codex"])).unwrap(),
            Some("codex".into())
        );
        assert_eq!(
            requested_harness(&s(&["codex", "claude", "codex"]))
                .unwrap_err()
                .to_string(),
            "Refused: conflicting harnesses requested (claude, codex); name one"
        );
        assert!(
            requested_harness(&s(&["pi"]))
                .unwrap_err()
                .to_string()
                .contains("not a driven CPU")
        );
    }

    /// A kernel whose drivers report which harnesses are installed; turns fail at once.
    mod dispatch {
        use super::super::super::Kernel;
        use crate::{BriefFold, Drivers, FoldJob, SilentMapp, TurnRequest, TurnResult, Universe};
        use anyhow::{Result, bail};
        use serde_json::{Value, json};
        use std::{
            fs,
            path::PathBuf,
            sync::Arc,
            thread,
            time::{Duration, Instant},
        };

        struct Installed(&'static [&'static str]);
        impl Drivers for Installed {
            fn fold(&self, _: &FoldJob) -> Result<BriefFold> {
                bail!("no fold worker")
            }
            fn turn(&self, req: &TurnRequest, _: &dyn Fn(u32)) -> Result<TurnResult> {
                bail!("{} is not installed or not on PATH", req.harness)
            }
            fn harness_ready(&self, h: &str) -> Result<()> {
                if self.0.contains(&h) {
                    Ok(())
                } else {
                    bail!("{h} is not installed or not on the kernel's PATH")
                }
            }
            fn catalogs(
                &self,
                _: &std::path::Path,
            ) -> std::collections::BTreeMap<String, crate::drv_econ::Catalog> {
                crate::kernel::econ::test_catalogs(self.0)
            }
        }

        struct Rig {
            k: Arc<Kernel>,
            root: PathBuf,
        }
        impl Drop for Rig {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        fn rig(tag: &str, installed: &'static [&'static str]) -> Rig {
            let root = std::env::temp_dir().join(format!(
                "unvrs-dispatch-{tag}-{}-{}",
                std::process::id(),
                crate::kernel::now_ms()
            ));
            fs::create_dir_all(&root).unwrap();
            let u = Universe::at(&root).unwrap();
            u.init().unwrap();
            let k = Kernel::open(
                u,
                Arc::new(Installed(installed)),
                Arc::new(SilentMapp),
                crate::kernel::now_ms(),
            )
            .unwrap();
            k.refresh_econ();
            {
                let mut inner = k.lock();
                let l1 = inner
                    .st
                    .create(0, 1, "attached", "claude", "", Default::default(), None);
                let l2 = inner
                    .st
                    .create(l1, 2, "attached", "claude", "", Default::default(), None);
                inner.st.pid_mut(l2).unwrap().project = Some("proj".into());
                inner
                    .st
                    .captain_prompts
                    .push(crate::kernel::state::CaptainPrompt {
                        id: 1,
                        pid: l1,
                        thread: "claude:fixture".into(),
                        project: None,
                        at: crate::kernel::now_ms(),
                        text: "Ok, go".into(),
                    });
            }
            Rig { k, root }
        }

        fn dispatch(r: &Rig, asked: &[&str]) -> Result<usize> {
            let asked: Vec<String> = asked.iter().map(|h| (*h).to_owned()).collect();
            let mut inner = r.k.lock();
            r.k.task(
                &mut inner,
                Some(2),
                json!({"intent": "check it", "spec": "look", "go":"Ok, go", "done_when":"report delivered"}),
                &asked,
            )
        }

        fn refused(r: &Rig) -> Vec<Value> {
            r.k.journal
                .tail(2000)
                .into_iter()
                .filter(|e| e["kind"] == "refused" || e["event"] == "refused")
                .collect()
        }

        fn wait_ended(r: &Rig, pid: usize) {
            let until = Instant::now() + Duration::from_secs(20);
            while !{
                let inner = r.k.lock();
                inner.st.pid(pid).unwrap().state == "ended" && inner.running.is_empty()
            } {
                assert!(Instant::now() < until, "PID {pid} did not end");
                thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn unavailable_codex_is_held_loudly_never_moved_to_claude() {
            let r = rig("no-codex", &["claude"]);
            let pid = dispatch(&r, &["codex"]).unwrap();
            assert_eq!(r.k.lock().st.pid(pid).unwrap().state, "held");
            assert!(r.k.lock().running.is_empty());
            assert_eq!(r.k.lock().st.holds.len(), 1);
            assert_eq!(
                r.k.lock().st.pid(pid).unwrap().contract.as_ref().unwrap()["econ"]["overrides"]["harness"],
                "codex"
            );
            let err = dispatch(&r, &["codex", "claude"]).unwrap_err().to_string();
            assert!(err.contains("conflicting"), "{err}");
            assert_eq!(refused(&r).len(), 1);
            assert_eq!(r.k.lock().st.pids.len(), 3);
        }

        #[test]
        fn a_requested_harness_is_honoured_recorded_and_never_handed_across() {
            let r = rig("codex", &["claude", "codex"]);
            let pid = dispatch(&r, &["codex"]).unwrap();
            let contract: Value = serde_json::from_slice(
                &fs::read(
                    r.k.u
                        .project_dir("proj")
                        .join(format!("tasks/pid-{pid}/contract.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(contract["harness"], "codex");
            assert_eq!(contract["harness_requested"], true);
            wait_ended(&r, pid);
            let mut inner = r.k.lock();
            assert_eq!(inner.st.pid(pid).unwrap().harness, "codex");
            // The missing-binary turn blocked it; make it movable again and try a
            // low-quota handoff: refused, because codex was requested.
            let rec = inner.st.pid_mut(pid).unwrap();
            rec.state = "working".into();
            if let Some(d) = rec.driven.as_mut() {
                d.attempt = 0;
            }
            let err =
                r.k.handoff_locked(&mut inner, pid, None, "low quota")
                    .unwrap_err()
                    .to_string();
            assert!(
                err.contains("pinned to model") && err.contains("codex"),
                "{err}"
            );
            assert_eq!(inner.st.pids.len(), 3, "no successor was created");
            drop(inner);
            // An unnamed harness follows the worker profile and is not an explicit override.
            let pid = dispatch(&r, &[]).unwrap();
            wait_ended(&r, pid);
            let inner = r.k.lock();
            let rec = inner.st.pid(pid).unwrap();
            assert_eq!(rec.harness, "codex");
            assert_eq!(rec.contract.as_ref().unwrap()["harness_requested"], false);
        }
    }

    #[test]
    fn a_pinned_effort_survives_in_the_contract() {
        assert_eq!(
            pinned_effort(Some(&json!({"effort": "high"}))).as_deref(),
            Some("high")
        );
        assert_eq!(pinned_effort(Some(&json!({}))), None);
        assert_eq!(pinned_effort(None), None);
    }
}
