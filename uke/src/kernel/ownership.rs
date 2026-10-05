//! Context ownership (context-ownership.md): a driven task is accepted only when its
//! final reply carries a full HANDOFF whose deliverables live where UNVRS keeps them,
//! and UNVRS holds the task's brief and working record. The parse and the check are
//! pure; the kernel gathers the inputs and acts on the verdict (driven.rs).
use super::{Inner, Kernel, now_ms, state::clip};
use crate::{MemStore, Sources, Tier};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Bounces before a task that keeps failing the check ends blocked.
pub const MAX_BOUNCES: u32 = 3;
pub const FILE: &str = "ownership.json";
const SECTIONS: [&str; 3] = ["deliverables", "decisions", "learnings"];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub deliverables: Vec<String>,
    pub decisions: Vec<String>,
    pub learnings: Vec<String>,
    /// Sections whose header appeared, in order.
    pub sections: Vec<String>,
}

fn marker(line: &str) -> &str {
    line.trim()
        .trim_matches(|c| matches!(c, '*' | '`' | '#'))
        .trim()
}

fn is_none(item: &str) -> bool {
    let t = item.trim().trim_end_matches('.').to_ascii_lowercase();
    t == "none" || t.starts_with("none:") || t.starts_with("none ")
}

/// The last `HANDOFF` block, up to `END-HANDOFF`, a FIELD-NOTES header or the result.
pub fn parse_handoff(text: &str) -> Option<Handoff> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| matches!(marker(l), "HANDOFF" | "HANDOFF:"))?;
    let mut h = Handoff::default();
    let mut current: Option<&str> = None;
    for line in &lines[start + 1..] {
        let m = marker(line);
        if m.starts_with("END-")
            || m == "FIELD-NOTES"
            || m == "FIELD-NOTES:"
            || line.trim_start().starts_with("UNVRS-RESULT")
        {
            break;
        }
        let head = m.trim_end_matches(':').to_ascii_lowercase();
        if let Some(s) = SECTIONS.iter().find(|s| **s == head) {
            current = Some(s);
            h.sections.push((*s).into());
            continue;
        }
        let Some(item) = line
            .trim()
            .trim_start_matches('*')
            .trim()
            .strip_prefix("- ")
            .map(str::trim)
            .filter(|i| !i.is_empty())
        else {
            continue;
        };
        let item: String = item.chars().take(600).collect();
        match current {
            Some("deliverables") => h.deliverables.push(item),
            Some("decisions") => h.decisions.push(item),
            Some("learnings") => h.learnings.push(item),
            _ => {}
        }
    }
    Some(h)
}

/// What the kernel knows about the task besides its final reply.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// The worker's working directory (its task directory).
    pub cwd: PathBuf,
    /// UNVRS-owned roots: the home and the project's path sources.
    pub owned: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    /// `tasks/pid-<n>/contract.json`, when it exists.
    pub contract: Option<PathBuf>,
    /// Turns UNVRS holds (session tail plus cold transcript lines).
    pub record_turns: usize,
    pub record: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliverable {
    pub item: String,
    /// `path`, `commit` or `text`.
    pub kind: String,
    pub resolved: Option<String>,
    pub owned: bool,
    pub exists: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub pass: bool,
    /// One line per failure, for the worker and the Observatory.
    pub leaks: Vec<String>,
    pub handoff: Option<Handoff>,
    pub deliverables: Vec<Deliverable>,
    pub brief: Option<String>,
    pub record_turns: usize,
    pub record: Vec<String>,
    pub owned_roots: Vec<String>,
}

fn expand(home: Option<&Path>, p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

/// The path a deliverable names: a backticked span when there is one, else the first
/// word that looks like a path.
fn path_token(item: &str) -> Option<String> {
    let clean = |w: &str| {
        w.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>'))
            .trim_end_matches(['.', ',', ';', ':'])
            .to_owned()
    };
    let looks = |w: &str| {
        !w.contains("://")
            && (w.starts_with('/')
                || w.starts_with("~/")
                || w.starts_with("./")
                || w.contains('/')
                || Path::new(w)
                    .extension()
                    .zip(Path::new(w).file_stem())
                    .is_some_and(|(e, stem)| {
                        let e = e.to_string_lossy();
                        (2..=5).contains(&e.len())
                            && e.chars().all(|c| c.is_ascii_alphanumeric())
                            && e.chars().any(|c| c.is_ascii_alphabetic())
                            && stem.len() >= 2
                    }))
    };
    let mut parts = item.split('`');
    parts.next();
    let quoted: Vec<String> = parts.step_by(2).map(clean).collect();
    quoted
        .into_iter()
        .chain(item.split_whitespace().map(clean))
        .find(|w| !w.is_empty() && looks(w))
}

fn commit_sha(item: &str) -> Option<String> {
    let lower = item.to_ascii_lowercase();
    let rest = lower
        .trim_start_matches(['`', '*'])
        .strip_prefix("commit")?;
    rest.split(|c: char| !c.is_ascii_hexdigit())
        .find(|w| w.len() >= 7)
        .map(str::to_owned)
}

/// Git repositories a worker may have committed in: its directory and direct children.
fn repos(cwd: &Path) -> Vec<PathBuf> {
    let mut out = vec![cwd.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(cwd) {
        out.extend(
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.join(".git").exists()),
        );
    }
    out
}

fn commit_exists(cwd: &Path, sha: &str) -> bool {
    repos(cwd).iter().any(|r| {
        Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// Canonical form of `p`, resolving its nearest existing ancestor when it is missing
/// (so `/var` and `/private/var` compare equal for a file not written yet).
fn canonical(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let mut rest = vec![];
    let mut at = p;
    while let Some(parent) = at.parent() {
        rest.push(at.file_name().unwrap_or_default().to_owned());
        if let Ok(c) = parent.canonicalize() {
            return rest.iter().rev().fold(c, |acc, part| acc.join(part));
        }
        at = parent;
    }
    p.to_path_buf()
}

pub fn check(text: &str, inputs: &Inputs) -> Check {
    let owned: Vec<PathBuf> = inputs.owned.iter().map(|p| canonical(p)).collect();
    let mut c = Check {
        owned_roots: owned.iter().map(|p| p.display().to_string()).collect(),
        brief: inputs.contract.as_ref().map(|p| p.display().to_string()),
        record_turns: inputs.record_turns,
        record: inputs
            .record
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        ..Default::default()
    };
    if inputs.contract.is_none() {
        c.leaks
            .push("brief not held: UNVRS has no contract for this task".into());
    }
    if inputs.record_turns == 0 {
        c.leaks
            .push("record not captured: UNVRS holds no turn of this task's conversation".into());
    }
    let Some(h) = parse_handoff(text) else {
        c.leaks.push(
            "handoff missing: end with a HANDOFF block (deliverables, decisions, learnings) before UNVRS-RESULT".into(),
        );
        return c;
    };
    for (name, items) in [
        ("deliverables", &h.deliverables),
        ("decisions", &h.decisions),
        ("learnings", &h.learnings),
    ] {
        if !h.sections.iter().any(|s| s == name) {
            c.leaks
                .push(format!("handoff incomplete: no `{name}:` section"));
        } else if items.is_empty() {
            c.leaks.push(format!(
                "handoff incomplete: `{name}:` is empty (write `- none` when there is nothing)"
            ));
        }
    }
    for item in h.deliverables.iter().filter(|i| !is_none(i)) {
        let mut d = Deliverable {
            item: item.clone(),
            ..Default::default()
        };
        if let Some(sha) = commit_sha(item) {
            d.kind = "commit".into();
            d.resolved = Some(sha.clone());
            d.exists = commit_exists(&inputs.cwd, &sha);
            d.owned = d.exists;
            if !d.exists {
                c.leaks.push(format!(
                    "deliverable missing: commit {sha} is not in a repository under {}",
                    inputs.cwd.display()
                ));
            }
        } else if let Some(token) = path_token(item) {
            d.kind = "path".into();
            let raw = expand(inputs.home.as_deref(), &token);
            let found = if raw.is_absolute() {
                Some(raw.clone()).filter(|p| p.exists())
            } else {
                repos(&inputs.cwd)
                    .into_iter()
                    .map(|r| r.join(&raw))
                    .find(|p| p.exists())
            };
            let path = found.clone().unwrap_or_else(|| {
                if raw.is_absolute() {
                    raw.clone()
                } else {
                    inputs.cwd.join(&raw)
                }
            });
            let resolved = canonical(&path);
            d.exists = found.is_some();
            d.owned = owned.iter().any(|root| resolved.starts_with(root));
            d.resolved = Some(resolved.display().to_string());
            if !d.owned {
                c.leaks.push(format!("deliverable outside UNVRS: {token}"));
            } else if !d.exists {
                c.leaks.push(format!("deliverable missing: {token}"));
            }
        } else {
            d.kind = "text".into();
            d.owned = true;
            d.exists = true;
        }
        c.deliverables.push(d);
    }
    c.handoff = Some(h);
    c.pass = c.leaks.is_empty();
    c
}

/// Decisions and learnings worth a note (`none` items dropped).
pub fn notes(h: &Handoff) -> Vec<(&'static str, String)> {
    h.decisions
        .iter()
        .map(|d| ("decision", d))
        .chain(h.learnings.iter().map(|l| ("learning", l)))
        .filter(|(_, t)| !is_none(t))
        .map(|(k, t)| (k, t.clone()))
        .collect()
}

/// What the gate decided for a reply that carried a result.
pub enum Gate {
    /// Accepted; the notes filed from the handoff are in ownership.json.
    Accept,
    /// Sent back to the worker with the reasons; the task keeps running.
    Bounce,
    /// Failed the check `MAX_BOUNCES` times: end blocked with this result.
    Block(String),
}

impl Kernel {
    fn ownership_inputs(&self, inner: &Inner, pid: usize) -> Inputs {
        let Ok(rec) = inner.st.pid(pid) else {
            return Inputs::default();
        };
        let cwd = rec
            .driven
            .as_ref()
            .map(|d| d.cwd.clone())
            .unwrap_or_default();
        let mut owned = vec![self.u.root().to_path_buf()];
        if let Some(project) = rec.project.as_deref() {
            let sources = Sources::load(self.u.root()).ok();
            for id in self.project(project).map(|p| p.sources).unwrap_or_default() {
                if let Some(src) = sources.as_ref().and_then(|s| s.get(&id))
                    && src.kind == "path"
                    && !src.uri.is_empty()
                {
                    owned.push(PathBuf::from(&src.uri));
                }
            }
        }
        let contract = rec
            .project
            .as_deref()
            .map(|p| {
                self.u
                    .project_dir(p)
                    .join(format!("tasks/pid-{pid}/contract.json"))
            })
            .filter(|p| p.is_file())
            // A continuation on another harness keeps its contract in the PID record.
            .or_else(|| {
                rec.contract
                    .as_ref()
                    .map(|_| PathBuf::from(format!("kernel PID record {pid} (contract)")))
            });
        let session = inner.mem.session(pid).unwrap_or_default();
        let cold = std::fs::read_to_string(&session.cold_transcript)
            .map(|t| t.lines().count())
            .unwrap_or(0);
        let mut record = vec![];
        if let Some(dir) = session.cold_transcript.parent() {
            record.push(dir.join("session.json"));
        }
        if cold > 0 {
            record.push(session.cold_transcript.clone());
        }
        Inputs {
            cwd,
            owned,
            home: std::env::var_os("HOME").map(PathBuf::from),
            contract,
            record_turns: session.tail.len() + cold,
            record,
        }
    }

    /// Writes `tasks/pid-<n>/ownership.json` (the Observatory reads it).
    fn ownership_write(&self, inner: &Inner, pid: usize, record: &Value) {
        if let Some(project) = inner.st.pid(pid).ok().and_then(|r| r.project.clone()) {
            let dir = self
                .u
                .project_dir(&project)
                .join(format!("tasks/pid-{pid}"));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                dir.join(FILE),
                serde_json::to_vec_pretty(record).unwrap_or_default(),
            );
        }
    }

    /// The context-ownership gate for a driven reply that carries `UNVRS-RESULT`.
    pub(crate) fn ownership_gate(&self, inner: &mut Inner, pid: usize, text: &str) -> Gate {
        let inputs = self.ownership_inputs(inner, pid);
        let c = check(text, &inputs);
        let project = inner.st.pid(pid).ok().and_then(|r| r.project.clone());
        let bounces = inner
            .st
            .pid(pid)
            .ok()
            .and_then(|r| r.driven.as_ref())
            .map_or(0, |d| d.ownership_bounces);
        let mut history: Vec<Value> = project
            .as_ref()
            .and_then(|p| {
                std::fs::read(
                    self.u
                        .project_dir(p)
                        .join(format!("tasks/pid-{pid}/{FILE}")),
                )
                .ok()
            })
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["history"].as_array().cloned())
            .unwrap_or_default();
        let (verdict, gate, filed) = if c.pass {
            let mut filed = vec![];
            if let (Some(project), Some(h)) = (&project, &c.handoff) {
                let store = MemStore::project(self.u.root(), project);
                let source = format!("pid:{pid}");
                for (kind, body) in notes(h) {
                    let text = inner.mem.redact(&format!("{kind} (PID {pid}): {body}"));
                    match store.remember(&text, Tier::Aging, &source, Some(&source), None, now_ms())
                    {
                        Ok(f) => {
                            let n = f.note();
                            filed.push(format!("{}: {kind}", n.id));
                            self.event(
                                inner,
                                "remember",
                                json!({"pid": pid, "note": n.id, "tier": n.tier.as_str(), "by": source, "project": project, "kind": kind, "from": "handoff"}),
                            );
                        }
                        Err(e) => self.event(
                            inner,
                            "remember",
                            json!({"pid": pid, "error": format!("{e:#}"), "from": "handoff"}),
                        ),
                    }
                }
            }
            ("accepted", Gate::Accept, filed)
        } else if bounces < MAX_BOUNCES {
            let n = bounces + 1;
            if let Ok(r) = inner.st.pid_mut(pid)
                && let Some(d) = r.driven.as_mut()
            {
                d.ownership_bounces = n;
            }
            let words = self.mapp.ownership_bounce(&c.leaks, n, MAX_BOUNCES);
            let _ = inner
                .st
                .mail(pid, "kernel (context-ownership check)", &words);
            ("bounced", Gate::Bounce, vec![])
        } else {
            let why = format!(
                "blocked: the context-ownership check failed {} times: {}",
                bounces + 1,
                clip(&c.leaks.join("; "), 1200)
            );
            ("blocked", Gate::Block(why), vec![])
        };
        history.push(json!({"at": now_ms(), "verdict": verdict, "pass": c.pass, "leaks": c.leaks}));
        let record = json!({
            "pid": pid, "project": project, "at": now_ms(), "verdict": verdict,
            "pass": c.pass, "bounces": bounces + u32::from(verdict == "bounced"),
            "max_bounces": MAX_BOUNCES, "check": c, "notes": filed, "history": history,
        });
        self.ownership_write(inner, pid, &record);
        self.event(
            inner,
            "ownership.check",
            json!({"pid": pid, "project": project, "verdict": verdict, "pass": c.pass, "leaks": c.leaks, "notes": filed}),
        );
        gate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "uke-ownership-{tag}-{}-{}",
            std::process::id(),
            crate::now_ms()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn inputs(home: &Path, cwd: &Path) -> Inputs {
        Inputs {
            cwd: cwd.into(),
            owned: vec![home.into()],
            home: Some(home.parent().unwrap().into()),
            contract: Some(cwd.join("contract.json")),
            record_turns: 3,
            record: vec![],
        }
    }

    const GOOD: &str = "work\nHANDOFF\ndeliverables:\n- `report.md`\ndecisions:\n- kept the parser pure, so it is testable\nlearnings:\n- none\nEND-HANDOFF\nFIELD-NOTES\n- none\nEND-NOTES\nUNVRS-RESULT: done";

    #[test]
    fn parses_the_last_block_and_stops_at_its_end() {
        let h = parse_handoff(&format!("HANDOFF\ndeliverables:\n- old\n{GOOD}")).unwrap();
        assert_eq!(h.deliverables, vec!["`report.md`"]);
        assert_eq!(h.decisions.len(), 1);
        assert_eq!(h.learnings, vec!["none"]);
        assert_eq!(h.sections, vec!["deliverables", "decisions", "learnings"]);
        assert!(parse_handoff("UNVRS-RESULT: x").is_none());
        let bold = parse_handoff("**HANDOFF**\n**deliverables:**\n- a.md\n").unwrap();
        assert_eq!(bold.deliverables, vec!["a.md"]);
    }

    #[test]
    fn owned_existing_deliverable_with_explicit_none_passes() {
        let home = dir("pass");
        let cwd = home.join("projects/p/tasks/pid-9");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join("report.md"), "r").unwrap();
        let c = check(GOOD, &inputs(&home, &cwd));
        assert!(c.pass, "{:?}", c.leaks);
        assert_eq!(c.deliverables[0].kind, "path");
        assert_eq!(
            notes(c.handoff.as_ref().unwrap()),
            vec![(
                "decision",
                "kept the parser pure, so it is testable".to_owned()
            )]
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn each_leak_is_named() {
        let home = dir("fail");
        let cwd = home.join("projects/p/tasks/pid-9");
        fs::create_dir_all(&cwd).unwrap();
        let outside = std::env::temp_dir().join(format!("uke-outside-{}.md", std::process::id()));
        fs::write(&outside, "x").unwrap();
        let text = format!(
            "HANDOFF\ndeliverables:\n- {}\n- missing.md\ndecisions:\nEND-HANDOFF\nUNVRS-RESULT: x",
            outside.display()
        );
        let mut i = inputs(&home, &cwd);
        i.record_turns = 0;
        i.contract = None;
        let c = check(&text, &i);
        assert!(!c.pass);
        let all = c.leaks.join("\n");
        for needle in [
            "deliverable outside UNVRS",
            "deliverable missing: missing.md",
            "`decisions:` is empty",
            "no `learnings:` section",
            "record not captured",
            "brief not held",
        ] {
            assert!(all.contains(needle), "{needle} in {all}");
        }
        assert!(
            check("UNVRS-RESULT: x", &inputs(&home, &cwd)).leaks[0].starts_with("handoff missing")
        );
        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn commits_and_prose_deliverables() {
        assert_eq!(
            commit_sha("commit 3741e14 on task/pid-198-x").as_deref(),
            Some("3741e14")
        );
        assert_eq!(
            path_token("the report in `docs/a.md`, updated").as_deref(),
            Some("docs/a.md")
        );
        assert_eq!(
            path_token("~/bridge/files/x.png").as_deref(),
            Some("~/bridge/files/x.png")
        );
        assert_eq!(path_token("the answer is in the result text"), None);
        assert_eq!(path_token("e.g. version v0.8 works"), None);
        let home = dir("commit");
        let cwd = home.join("t");
        fs::create_dir_all(&cwd).unwrap();
        let c = check(
            "HANDOFF\ndeliverables:\n- commit deadbeefcafe on main\n- the summary in the result\ndecisions:\n- none\nlearnings:\n- none\nEND-HANDOFF",
            &inputs(&home, &cwd),
        );
        assert!(!c.pass);
        assert!(c.leaks[0].contains("commit deadbeefcafe"));
        assert_eq!(c.deliverables[1].kind, "text");
        let _ = fs::remove_dir_all(home);
    }
}
