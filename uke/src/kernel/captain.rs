//! Captain-authority path (D21, D61): the prompt hook receives exactly what the captain
//! typed (`$unvrs:<verb>`, `/unvrs:<verb>`, `$l1`, `$l2 …`) and runs it here as the
//! captain, before the model. A model's call through the MCP tool or `unvrs ctl` is a
//! seat call and can never do these.
use super::{
    Inner, Kernel,
    bind::{HookReply, ThreadInfo},
    state::{State, clip, pid_line},
};
use crate::{Filed, MemStore, Tier};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captain {
    L1,
    L2(Option<String>),
    Digest,
    Observe,
    Answer(String, String),
    Approve(String),
    Reopen(String),
    Remember {
        tier: Tier,
        project: Option<String>,
        until: Option<String>,
        text: String,
    },
    Forget(String),
    ProjectList,
    ProjectNew {
        id: String,
        purpose: String,
        sources: Vec<String>,
    },
    Away(Option<(String, u32)>),
    Detach,
    Tree,
    Help(String),
}

pub const VERBS: &[&str] = &[
    "l1", "l2", "digest", "observe", "answer", "approve", "reopen", "remember", "forget",
    "project", "away", "detach", "tree", "help",
];

/// Splits quoted arguments, preserving escapes and apostrophes within words.
pub fn words(s: &str) -> Result<Vec<String>> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            // ponytail: word apostrophes stay literal; use double quotes for adjacent single-quoted shell fragments.
            None | Some('\'')
                if c == '\''
                    && cur.ends_with(|c: char| c.is_alphanumeric())
                    && chars.peek().is_some_and(|c| c.is_alphanumeric()) =>
            {
                cur.push(c)
            }
            Some('"') if c == '\\' && chars.peek().is_some_and(|c| matches!(c, '"' | '\\')) => {
                cur.push(chars.next().unwrap());
            }
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\\' => {
                cur.push(
                    chars
                        .next()
                        .context("Command ends with an incomplete escape")?,
                );
                any = true;
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => cur.push(c),
        }
    }
    ensure!(
        quote.is_none(),
        "Command has an unclosed quote; quote or escape the complete value"
    );
    if !cur.is_empty() || any {
        out.push(cur);
    }
    Ok(out)
}

/// `None` when the prompt is not an UNVRS entry command.
pub fn parse(prompt: &str) -> Option<Captain> {
    let p = unwrap_mention(prompt.trim());
    let (verb, args) = split_entry(&p)?;
    Some(parse_verb(&verb, args.trim()))
}

/// The Codex app sends a picked skill as a Markdown link to its file:
/// `[$unvrs:l1](/…/skills/l1/SKILL.md) args`. Reduce it to `$unvrs:l1 args`.
fn unwrap_mention(p: &str) -> String {
    if let Some(inner) = p.strip_prefix('[')
        && let Some((name, after)) = inner.split_once("](")
        && name.starts_with(['$', '/'])
        && let Some((_, rest)) = after.split_once(')')
    {
        return format!("{name} {}", rest.trim());
    }
    p.to_owned()
}

fn split_entry(p: &str) -> Option<(String, String)> {
    let first = p.split_whitespace().next()?;
    let rest = p[first.len()..].to_owned();
    let lower = first.to_ascii_lowercase();
    for prefix in ["$unvrs:", "/unvrs:"] {
        if let Some(v) = lower.strip_prefix(prefix) {
            return VERBS.contains(&v).then(|| (v.to_owned(), rest));
        }
    }
    if lower == "$unvrs" || lower == "/unvrs" {
        let mut it = rest.trim_start().splitn(2, char::is_whitespace);
        let v = it.next()?.to_ascii_lowercase();
        let r = it.next().unwrap_or("").to_owned();
        return VERBS.contains(&v.as_str()).then_some((v, r));
    }
    // Short and 0.8-card spellings for the binding commands only (other bare names
    // would collide with the captain's own skills).
    let bare = lower.trim_start_matches(['$', '/']);
    if first.starts_with(['$', '/']) {
        let v = match bare {
            "l1" | "l1-unvrs" => "l1",
            "l2" | "l2-unvrs" => "l2",
            _ => return None,
        };
        if bare.ends_with("-unvrs") || first.starts_with('$') {
            return Some((v.into(), rest));
        }
    }
    None
}

fn parse_verb(verb: &str, args: &str) -> Captain {
    let w = match words(args) {
        Ok(w) => w,
        Err(error) => return Captain::Help(error.to_string()),
    };
    let usage = |u: &str| Captain::Help(format!("usage: $unvrs:{u}"));
    match verb {
        "l1" => Captain::L1,
        "l2" => Captain::L2(w.first().cloned()),
        "digest" => Captain::Digest,
        "observe" => Captain::Observe,
        "tree" => Captain::Tree,
        "detach" => Captain::Detach,
        "help" => Captain::Help(String::new()),
        "answer" => match w.split_first() {
            Some((id, rest)) if !rest.is_empty() => {
                // The captain's exact words: everything after the id, as typed.
                let words = args.trim()[args.trim().find(id.as_str()).unwrap_or(0) + id.len()..]
                    .trim()
                    .to_owned();
                Captain::Answer(id.clone(), words)
            }
            _ => usage("answer <id> <your words>"),
        },
        "approve" => match w.first() {
            Some(id) => Captain::Approve(id.clone()),
            None => usage("approve <id>"),
        },
        "reopen" => match w.first() {
            Some(id) => Captain::Reopen(id.clone()),
            None => usage("reopen <id>"),
        },
        "forget" => match w.first() {
            Some(id) => Captain::Forget(id.clone()),
            None => usage("forget <note id>"),
        },
        "remember" => {
            let mut tier = Tier::Pinned;
            let (mut project, mut until) = (None, None);
            let mut text = vec![];
            let mut it = w.into_iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--pin" | "--pinned" => tier = Tier::Pinned,
                    "--aging" => tier = Tier::Aging,
                    "--perishable" => tier = Tier::Perishable,
                    "--project" => project = it.next(),
                    "--until" => {
                        until = it.next();
                        tier = Tier::Perishable;
                    }
                    _ => text.push(a),
                }
            }
            let text = text.join(" ");
            if text.trim().is_empty() {
                usage("remember [--aging|--perishable] [--project p] <fact>")
            } else {
                Captain::Remember {
                    tier,
                    project,
                    until,
                    text,
                }
            }
        }
        "project" => match w.first().map(String::as_str) {
            None | Some("list") => Captain::ProjectList,
            Some("new") => {
                let mut sources = vec![];
                let mut rest = vec![];
                let mut it = w.into_iter().skip(1);
                while let Some(a) = it.next() {
                    if a == "--source" {
                        if let Some(s) = it.next() {
                            sources.push(s);
                        }
                    } else {
                        rest.push(a);
                    }
                }
                match rest.split_first() {
                    Some((id, purpose)) if !purpose.is_empty() => Captain::ProjectNew {
                        id: id.clone(),
                        purpose: purpose.join(" "),
                        sources,
                    },
                    _ => usage("project new <id> <purpose> [--source s]"),
                }
            }
            _ => usage("project list | project new <id> <purpose> [--source s]"),
        },
        "away" => {
            if w.first().is_some_and(|a| a == "off" || a == "back") || w.is_empty() {
                Captain::Away(None)
            } else {
                let mut cap = 3;
                let mut text = vec![];
                let mut it = w.into_iter();
                while let Some(a) = it.next() {
                    if a == "--cap" {
                        cap = it.next().and_then(|c| c.parse().ok()).unwrap_or(cap);
                    } else {
                        text.push(a);
                    }
                }
                Captain::Away(Some((text.join(" "), cap)))
            }
        }
        _ => Captain::Help(format!("Unknown UNVRS command {verb:?}.")),
    }
}

pub fn tree_text(st: &State) -> String {
    let mut out = String::from("UNVRS crew\n");
    fn walk(st: &State, parent: usize, depth: usize, out: &mut String) {
        for p in st.pids.values().filter(|p| p.parent == parent) {
            if matches!(p.state.as_str(), "ended" | "handed-off") && p.rank == 3 {
                continue;
            }
            let bound = match (&p.thread, p.kind.as_str()) {
                (Some(t), _) => {
                    let app = st.threads.get(t).map(|t| t.app.as_str()).unwrap_or("");
                    format!(" · in a {app} thread")
                }
                (None, "driven") => " · driven".into(),
                (None, _) => " · no thread".into(),
            };
            let seat = match (p.rank, &p.project) {
                (1, _) => " · seat L1".to_owned(),
                (2, Some(pr)) => format!(" · seat L2 {pr}"),
                _ => String::new(),
            };
            let wakes = p.wakes.iter().filter(|w| !w.acked).count();
            out.push_str(&format!(
                "{}{}{seat}{bound}{}\n",
                "  ".repeat(depth + 1),
                pid_line(p),
                if wakes > 0 {
                    format!(" · {wakes} wake(s)")
                } else {
                    String::new()
                }
            ));
            walk(st, p.pid, depth + 1, out);
        }
    }
    walk(st, 0, 0, &mut out);
    out
}

impl Kernel {
    pub(crate) fn captain(
        self: &Arc<Self>,
        inner: &mut Inner,
        info: &ThreadInfo,
        cmd: Captain,
    ) -> Result<HookReply> {
        let key = info.key.as_str();
        let bound = inner
            .st
            .threads
            .get(key)
            .filter(|t| t.bound)
            .and_then(|t| t.pid);
        let op = format!("{cmd:?}")
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_lowercase();
        let result: Result<(HookReply, bool)> = (|| {
            // (reply, handled as a relay for bound seats)
            Ok(match cmd.clone() {
                Captain::L1 => {
                    let l1 = match inner.st.l1() {
                        Some(l1) => l1,
                        None => self.create_l1(inner),
                    };
                    let ctx = self.claim(inner, info, l1, "captain $unvrs:l1")?;
                    (
                        HookReply {
                            context: Some(ctx),
                            system: Some("UNVRS: this thread is now L1.".into()),
                            ..Default::default()
                        },
                        false,
                    )
                }
                Captain::L2(None) => (block(self.projects_text(inner)), false),
                Captain::L2(Some(p)) => {
                    let seat = inner.st.seat_of(&p).with_context(|| {
                        format!("No project {p:?}. Projects:\n{}", self.projects_text(inner))
                    })?;
                    let ctx = self.claim(inner, info, seat, "captain $unvrs:l2")?;
                    (
                        HookReply {
                            context: Some(ctx),
                            system: Some(format!("UNVRS: this thread now leads {p} (L2).")),
                            ..Default::default()
                        },
                        false,
                    )
                }
                Captain::Digest => (block(self.digest_text(inner, true)), true),
                Captain::Observe => {
                    let url = self.observatory_url();
                    let t = info.os_pid.map(super::procinfo::app_of).unwrap_or_default();
                    let opened = self.open_observatory(&t, &info.session, &url);
                    (block(format!("Observatory: {url}\n{opened}")), true)
                }
                Captain::Answer(id, words) => (
                    block(self.close_hold(inner, &id, "answered", &words)?),
                    true,
                ),
                Captain::Approve(id) => (block(self.approve(inner, &id)?), true),
                Captain::Reopen(id) => (block(self.reopen_hold(inner, &id)?), true),
                Captain::Remember {
                    tier,
                    project,
                    until,
                    text,
                } => {
                    let project = project.or_else(|| {
                        bound.and_then(|p| inner.st.pids.get(&p).and_then(|r| r.project.clone()))
                    });
                    let store = match &project {
                        Some(p) => {
                            ensure!(inner.st.seat_of(p).is_some(), "No project {p:?}");
                            MemStore::project(self.u.root(), p)
                        }
                        None => MemStore::captain(self.u.root()),
                    };
                    let filed = store.remember(
                        &text,
                        tier,
                        "captain",
                        Some(&format!("captain in {key}")),
                        until.as_deref(),
                        super::now_ms(),
                    )?;
                    let n = filed.note();
                    self.event(
                        inner,
                        "remember",
                        json!({"note": n.id, "tier": n.tier.as_str(), "by": "captain", "project": project, "reinforced": matches!(filed, Filed::Reinforced(_))}),
                    );
                    (
                        block(format!(
                            "{} {} ({}{}): {}",
                            if matches!(filed, Filed::Reinforced(_)) {
                                "Already known; reinforced"
                            } else {
                                "Remembered"
                            },
                            n.id,
                            n.tier.as_str(),
                            project
                                .as_ref()
                                .map(|p| format!(", project {p}"))
                                .unwrap_or_else(|| ", captain memory".into()),
                            clip(&n.text, 300)
                        )),
                        true,
                    )
                }
                Captain::Forget(id) => {
                    let n = self.forget_note(inner, &id)?;
                    (
                        block(format!(
                            "Archived {} (kept on disk, out of every hot set): {}",
                            n.id,
                            clip(&n.text, 200)
                        )),
                        true,
                    )
                }
                Captain::ProjectList => (block(self.projects_text(inner)), true),
                Captain::ProjectNew {
                    id,
                    purpose,
                    sources,
                } => {
                    let seat = self.create_project(inner, &id, &purpose, &sources, "captain")?;
                    (
                        block(format!(
                            "Project {id} created with its lead seat (PID {seat}). Type $unvrs:l2 {id} in any thread to lead it."
                        )),
                        true,
                    )
                }
                Captain::Away(words) => (block(self.set_away(inner, words)?), true),
                Captain::Detach => {
                    let text = match bound {
                        Some(pid) => {
                            self.detach(inner, key, "captain $unvrs:detach", None);
                            inner.st.threads.remove(key);
                            format!(
                                "This thread left its UNVRS seat (PID {pid}); it is an ordinary thread again."
                            )
                        }
                        None => {
                            inner.st.threads.remove(key);
                            "This thread is not an UNVRS seat.".into()
                        }
                    };
                    (block(text), false)
                }
                Captain::Tree => (block(tree_text(&inner.st)), true),
                Captain::Help(msg) => (block(format!("{msg}\n{}", self.mapp.help())), false),
            })
        })();
        let (mut reply, relay) = match result {
            Ok(r) => r,
            Err(e) => (block(format!("UNVRS refused: {e:#}")), false),
        };
        // In a bound seat the result is relayed by the seat (it learns what the captain
        // did); in any other thread the hook answers and the model never runs.
        if relay
            && bound.is_some()
            && let Some(text) = reply.block.take()
        {
            reply.context = Some(format!(
                "UNVRS ran the captain's command ({op}) before this turn. Result:\n{text}"
            ));
            if let Some(t) = inner.st.threads.get_mut(key) {
                t.pending_prompt = None;
                t.turn_open = true;
            }
        }
        if bound.is_some()
            && reply.block.is_none()
            && let Some(t) = inner.st.threads.get_mut(key)
        {
            t.turn_open = true;
        }
        self.event(
            inner,
            "captain",
            json!({"op": op, "pid": bound, "thread": key, "ok": reply.block.as_deref().is_none_or(|b| !b.starts_with("UNVRS refused"))}),
        );
        Ok(reply)
    }

    /// A new L1 seat (PID 1 in a fresh home).
    pub(crate) fn create_l1(&self, inner: &mut Inner) -> usize {
        let pid = inner
            .st
            .create(0, 1, "attached", "", "L1", Default::default(), None);
        self.event(inner, "spawn", json!({"pid": pid, "rank": 1, "seat": "l1"}));
        pid
    }

    /// Captain memory or a project's: finds the note by id.
    fn forget_note(&self, inner: &mut Inner, id: &str) -> Result<crate::MemNote> {
        let mut stores = vec![MemStore::captain(self.u.root())];
        for p in self.project_ids() {
            stores.push(MemStore::project(self.u.root(), &p));
        }
        let found: Vec<MemStore> = stores.into_iter().filter(|s| s.note(id).is_ok()).collect();
        match found.as_slice() {
            [one] => {
                let n = one.forget(id)?;
                self.event(inner, "forget", json!({"note": id, "by": "captain"}));
                Ok(n)
            }
            [] => bail!("No memory note {id}"),
            _ => bail!(
                "Note id {id} exists in several projects; archive it from that project's L2 thread"
            ),
        }
    }
}

fn block(text: String) -> HookReply {
    HookReply {
        block: Some(text),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L1: &str = "/Users/c/.codex/plugins/cache/unvrs-local/unvrs/0.8.0/skills/l1/SKILL.md";
    const L2: &str = "/Users/c/.codex/plugins/cache/unvrs-local/unvrs/0.8.0/skills/l2/SKILL.md";

    #[test]
    fn command_words_preserve_apostrophes_quotes_and_escapes() {
        for command in [
            "--spec 'keep the captain's words and \"quotes\" intact' --go ok",
            r#"--spec "keep the captain's words and \"quotes\" intact" --go ok"#,
        ] {
            assert_eq!(
                words(command).unwrap(),
                vec![
                    "--spec",
                    "keep the captain's words and \"quotes\" intact",
                    "--go",
                    "ok"
                ]
            );
        }
        assert_eq!(words("captain's word").unwrap(), vec!["captain's", "word"]);
        assert_eq!(
            words(r#"'two words' "" one\ word"#).unwrap(),
            vec!["two words", "", "one word"]
        );
        assert!(words("--spec 'unfinished").is_err());
        assert!(words("--spec unfinished\\").is_err());
    }

    #[test]
    fn codex_app_skill_mention_is_the_entry_command() {
        // Exactly what the Codex app sends when the captain picks a skill (trailing space).
        assert_eq!(parse(&format!("[$unvrs:l1]({L1}) ")), Some(Captain::L1));
        assert_eq!(parse(&format!("[$unvrs:l1]({L1})")), Some(Captain::L1));
        assert_eq!(
            parse(&format!("[$unvrs:l2]({L2}) acme")),
            Some(Captain::L2(Some("acme".into())))
        );
        assert_eq!(
            parse(&format!("[$unvrs:l2]({L2}) ")),
            Some(Captain::L2(None))
        );
        assert_eq!(
            parse("$unvrs:l2 acme"),
            Some(Captain::L2(Some("acme".into())))
        );
    }

    #[test]
    fn reopen_is_a_typed_captain_command() {
        assert_eq!(
            parse("$unvrs:reopen d5"),
            Some(Captain::Reopen("d5".into()))
        );
        assert_eq!(
            parse("/unvrs:reopen d5"),
            Some(Captain::Reopen("d5".into()))
        );
        assert!(matches!(parse("$unvrs:reopen"), Some(Captain::Help(_))));
    }

    #[test]
    fn ordinary_markdown_links_are_not_commands() {
        assert_eq!(parse("[the docs](https://example.com) please read"), None);
        assert_eq!(parse("[$other:skill](/x/SKILL.md) hi"), None);
    }
}
