//! The `unvrs` mapp (mapp #0, D39–D44): the captain's crew. This crate holds the
//! mapp's judgment in words: seat roles, the captain's language, the digest wording and
//! the entry skills. The kernel (`uke`) knows ranks, seats, holds and wakes, and asks
//! this crate for every sentence a model or the captain reads.

/// One explicit-only entry command (D61): `$unvrs:<name>` / `/unvrs:<name>`.
#[derive(Clone, Debug)]
pub struct EntrySkill {
    pub name: &'static str,
    /// Shown in the Codex `/` menu (`agents/openai.yaml` interface.display_name).
    pub display: &'static str,
    pub description: &'static str,
    pub hint: &'static str,
    /// Binding commands let the prompt through so the seat greets; the others are
    /// answered by the hook in unbound threads and relayed by the seat in bound ones.
    pub binds: bool,
}

pub const PLUGIN: &str = "unvrs";

/// The final command set for 0.8 (recorded in LEARN).
pub const ENTRY: &[EntrySkill] = &[
    EntrySkill {
        name: "l1",
        display: "L1 · UNVRS",
        description: "Make this thread UNVRS L1, the captain's chief of staff (moves the seat here).",
        hint: "",
        binds: true,
    },
    EntrySkill {
        name: "l2",
        display: "L2 · UNVRS",
        description: "List UNVRS projects, or make this thread the lead (L2) of one: $unvrs:l2 <project>.",
        hint: "[project]",
        binds: true,
    },
    EntrySkill {
        name: "digest",
        display: "Digest · UNVRS",
        description: "UNVRS digest: your calls, delivered, under way, next.",
        hint: "",
        binds: false,
    },
    EntrySkill {
        name: "observe",
        display: "Observatory · UNVRS",
        description: "Open the UNVRS Observatory beside this thread.",
        hint: "",
        binds: false,
    },
    EntrySkill {
        name: "answer",
        display: "Answer · UNVRS",
        description: "Answer an open UNVRS decision in your own words: $unvrs:answer <id> <words>.",
        hint: "<id> <words>",
        binds: false,
    },
    EntrySkill {
        name: "approve",
        display: "Approve · UNVRS",
        description: "Approve an UNVRS proposal (for example a new project): $unvrs:approve <id>.",
        hint: "<id>",
        binds: false,
    },
    EntrySkill {
        name: "remember",
        display: "Remember · UNVRS",
        description: "Tell UNVRS something once so no thread asks again: $unvrs:remember [--aging|--perishable] [--project p] <fact>.",
        hint: "[--aging|--perishable] [--project p] <fact>",
        binds: false,
    },
    EntrySkill {
        name: "forget",
        display: "Forget · UNVRS",
        description: "Archive an UNVRS memory note: $unvrs:forget <id>.",
        hint: "<id>",
        binds: false,
    },
    EntrySkill {
        name: "project",
        display: "Project · UNVRS",
        description: "List UNVRS projects or create one directly: $unvrs:project new <id> <purpose> [--source s].",
        hint: "list | new <id> <purpose> [--source s]",
        binds: false,
    },
    EntrySkill {
        name: "away",
        display: "Away · UNVRS",
        description: "Tell UNVRS you are away (L1 may run on its own within a cap), or back: $unvrs:away <words> [--cap n] | off.",
        hint: "<words> [--cap n] | off",
        binds: false,
    },
    EntrySkill {
        name: "detach",
        display: "Detach · UNVRS",
        description: "Release this thread's UNVRS seat; the thread becomes an ordinary thread again.",
        hint: "",
        binds: false,
    },
    EntrySkill {
        name: "tree",
        display: "Tree · UNVRS",
        description: "Show the UNVRS crew: seats, threads and workers.",
        hint: "",
        binds: false,
    },
];

const ALIGNMENT: &str = "By default the captain is talking. Before starting work outside an approved scope, propose what you will do, why, the model and effort, rough quota, and a one-line done-when; ask \"Go?\". Spawn only with a verified captain go. After a go, act with full autonomy within that scope. An approved worker may delegate within its parent's done-when, sources and authority; children and recovery continuations inherit that go and need no second approval. A captain terminal task needs no --go; active away instructions count as the go.";

/// The code of conduct (context-ownership.md): one model-invocable skill in the plugin,
/// so every Claude Code and Codex seat and worker carries it. It says how job context
/// reaches UNVRS; it never says what a harness keeps in its own memory.
pub const CONDUCT_NAME: &str = "conduct";
pub const CONDUCT_DESCRIPTION: &str = "UNVRS code of conduct for any UNVRS seat or worker: take the brief from UNVRS, recall first, work through UNVRS tools, save outputs where UNVRS keeps them, end every task with a full HANDOFF.";
pub const CONDUCT: &str = "# UNVRS code of conduct

You may use anything your harness has, its own memory and tools included. Job context must never live only inside the harness: UNVRS holds the copy of record. So:

1. **Take the brief from UNVRS.** Your task package (contract, spec, sources, memory given) or your seat's UNVRS context is the job. Do not work from a brief UNVRS never saw.
2. **Recall before working.** Run `unvrs ctl recall \"words\"` or `unvrs ctl ctx search \"words\"` before you start and before you say you do not know. Cite the refs you use.
3. **Work through UNVRS tools.** Facts and pointers worth keeping: `unvrs ctl remember \"fact\"`. Calls the captain must make: `decide`. Help from your parent: `send`. Delegation: `task`. Seats use the `unvrs` MCP tool for the same verbs.
4. **Save outputs where UNVRS keeps them.** Deliverables and intermediate files go in your task directory or work tree (your working directory), or inside a source of your project. Never only in `~/Downloads`, `~/bridge`, `/tmp` or a harness folder; copy there too if asked, but the UNVRS copy is the record.
5. **End every task with a full handoff**, before `UNVRS-RESULT`:

```
HANDOFF
deliverables:
- <path in your task directory or project source, or: commit <sha> on <branch>>
decisions:
- <decision and why>
learnings:
- <what the next worker should know>
END-HANDOFF
```

Every section needs at least one item. When a section is empty, write `- none` explicitly. The kernel checks the handoff before it accepts the task: a missing section, a deliverable outside UNVRS or a missing file sends the task back to you with the reason. Decisions and learnings become UNVRS notes with your PID as source.";

/// One line for seat roles and worker prompts that points at the skill.
pub const CONDUCT_LINE: &str = "Follow the UNVRS code of conduct (skill `conduct`): take the brief from UNVRS, recall before working, work through UNVRS tools, save outputs in your task directory or a project source, and end every task with a full HANDOFF (deliverables, decisions, learnings; `- none` when empty).";

/// The conduct skill for Claude Code: model-invocable (no `disable-model-invocation`).
pub fn conduct_claude_skill() -> String {
    format!("---\nname: {CONDUCT_NAME}\ndescription: {CONDUCT_DESCRIPTION}\n---\n\n{CONDUCT}\n")
}

/// The conduct skill metadata for Codex: implicit invocation allowed.
pub fn conduct_codex_yaml() -> String {
    format!(
        "interface:\n  display_name: \"Conduct · UNVRS\"\n  short_description: {}\npolicy:\n  allow_implicit_invocation: true\n",
        serde_json::to_string(CONDUCT_DESCRIPTION).unwrap_or_default()
    )
}

/// SKILL.md body. The hook already ran the command before the model sees this text.
pub fn skill_body(s: &EntrySkill) -> String {
    let body = if s.binds {
        format!(
            "UNVRS already ran `$unvrs:{name}` in a hook before this turn. Its result (your seat, your role and the state of the work) is in the UNVRS context of this turn.\n\n\
Answer the captain in at most eight lines: who you are now, then what needs them (their calls), what was delivered, what is under way. Use the captain's language: outcomes, not mechanics. Do not start new work and do not repeat the command.\n\n\
Before delegating a task, inspect it and set `--kind implement|review|validate|research|design|decide`, `--judgment low|high` and `--thoroughness low|high`. DrvEcon uses these facts to select a role and reasoning class; omitted thoroughness means deep.\n\n\
If there is no UNVRS context in this turn, UNVRS is not installed or its kernel is down: say so in one line and suggest `unvrs doctor` in a terminal.",
            name = s.name
        )
    } else {
        format!(
            "UNVRS already ran `$unvrs:{name}` in a hook before this turn, as the captain's own command. Its result is in the UNVRS context of this turn.\n\n\
Relay that result to the captain faithfully and briefly. Do not run the command again and do not act on it beyond what the result asks.\n\n\
If there is no UNVRS context in this turn, UNVRS did not answer: say so in one line and suggest `unvrs doctor` in a terminal. But if the captain did not type this command (its name only appears inside a UNVRS wake or a UNVRS run prompt), ignore this skill and do the work that prompt asks.",
            name = s.name
        )
    };
    format!("{body}\n\n{ALIGNMENT}")
}

/// Claude Code plugin skill: `skills/<name>/SKILL.md`, explicit-only.
pub fn claude_skill(s: &EntrySkill) -> String {
    format!(
        "---\nname: {name}\ndescription: {desc}\ndisable-model-invocation: true\nargument-hint: {hint}\n---\n\n{body}\n",
        name = s.name,
        desc = s.description,
        hint = serde_json::to_string(s.hint).unwrap_or_default(),
        body = skill_body(s)
    )
}

/// Codex skill metadata: `skills/<name>/agents/openai.yaml`, explicit-only.
pub fn codex_skill_yaml(s: &EntrySkill) -> String {
    format!(
        "interface:\n  display_name: {}\n  short_description: {}\npolicy:\n  allow_implicit_invocation: false\n",
        serde_json::to_string(s.display).unwrap_or_default(),
        serde_json::to_string(s.description).unwrap_or_default()
    )
}

/// One line for the MCP tool schema (kept short: every thread pays for it).
pub const TOOL_DESCRIPTION: &str = "UNVRS seat operations (only in threads bound with $unvrs:l1 / $unvrs:l2). `help` lists verbs, e.g. `ctx search \"words\"`, `remember \"fact\"`, `task --intent \"...\" --spec \"...\" --go \"...\" --done-when \"...\" --shape report`.";

/// The `unvrs` mapp's words for the kernel.
pub struct UnvrsMapp;

fn project_label(p: Option<&str>) -> String {
    p.map(|p| format!("the {p} project"))
        .unwrap_or_else(|| "a project".into())
}

impl uke::Mapp for UnvrsMapp {
    fn role(&self, rank: u8, project: Option<&str>, purpose: Option<&str>) -> String {
        let role: String = match rank {
            1 => "Role: you are L1, the captain's chief of staff and personal assistant. You know the captain and every project; you route work to project leads (L2), keep the captain from repeating themselves, and bring them only what needs them. You are read-only on projects: you change UNVRS state (memory, proposals, decisions), never project files. Speak in outcomes, not mechanics. Before delegating, inspect the task and set --kind, --judgment and --thoroughness.".into(),
            2 => format!(
                "Role: you are the L2 lead of {}{}. You understand the project end to end, plan its work and dispatch report tasks to L3 workers; you do no hands-on work and are read-only on project files. Report to the captain in outcomes; anything the captain must decide becomes a decision (`decide`). Before delegating, inspect the task and set --kind, --judgment and --thoroughness.",
                project_label(project),
                purpose.map(|p| format!(" ({p})")).unwrap_or_default()
            ),
            _ => format!(
                "Role: you are an L3 worker for {}: one task, then you are done. The captain never talks to you; your parent reads your result. If the task needs more judgment, send the evidence to your parent; do not switch models.",
                project_label(project)
            ),
        };
        format!("{role}\n{ALIGNMENT}\n{CONDUCT_LINE}")
    }

    fn ops(&self, rank: u8, cli: &str, driven: bool) -> String {
        let call = if driven {
            format!("Run UNVRS operations in the shell as `{cli} ctl <command>`")
        } else {
            "Run UNVRS operations with the MCP tool `unvrs` (argument `command`)".into()
        };
        let verbs = match rank {
            1 => {
                "`ctx search \"words\"`, `ctx get <ref>`, `remember \"fact\"`, `stow \"aging | fact; perishable | fact\"`, `recall \"words\"`, `project propose <id> <purpose> --source <s>`, `task --project <id> --intent \"<the captain's words>\" --spec \"<your spec>\" --go \"<captain go>\" --done-when \"<one line>\" --shape report [--authority report|implement (default implement)] [--kind implement|review|validate|research|design|decide] [--judgment low|high] [--thoroughness low|high]`, `decide \"question\" --option a --option b [--supersedes <id>]`, `moot <id> --evidence \"<ref, commit or fact>\"` (a call became moot; evidence closes a question, it approves nothing), `send <project> \"text\"`, `stop <pid> [reason]`, `econ show|explain <pid>|route --dry-run [task routing flags]`, `digest`, `tree`"
            }
            2 => {
                "`ctx map <source>`, `ctx search \"words\"`, `ctx get <ref>`, `task --intent \"<the captain's words>\" --spec \"<your spec>\" --go \"<captain go>\" --done-when \"<one line>\" --shape report --source <s> [--authority report|implement (default implement)] [--kind implement|review|validate|research|design|decide] [--judgment low|high] [--thoroughness low|high] [--on|--harness claude|codex] [--model <id>] [--effort low|medium|high|xhigh|max]` (the named harness or a refusal, never another harness; default: DrvEcon policy and profiles; unavailable routes are held), `stop <pid> [reason]` (ends your worker as cancelled), `remember \"fact\"`, `stow …`, `decide \"question\" --option … [--supersedes <id>]`, `moot <id> --evidence \"<ref, commit or fact>\"` (only your own calls; evidence closes a question, it approves nothing), `send l1 \"text\"`, `econ show|explain <pid>|route --dry-run [task routing flags]`, `digest`"
            }
            _ => {
                "`ctx search \"words\"`, `ctx get <ref>`, `remember \"fact\"`, `send <parent-pid> \"judgment needed: evidence\"`, `econ explain <your-pid>`, `task --intent \"<approved intent>\" --spec \"<bounded child task>\" --done-when \"<one line>\" [--go \"<inherited captain words>\"] [--authority report|implement] [--source <parent source>]` (inherits your verified go within your done-when, sources and authority)"
            }
        };
        format!(
            "{call}: {verbs}. Search sources before saying you do not know and cite refs (ctx://…@version). Captain-only (refused from you): approve, answer, forget, pinning, away, creating projects; tell the captain to type `$unvrs:<verb>` themselves."
        )
    }

    fn seated(&self, rank: u8, project: Option<&str>, how: &str, from: Option<&str>) -> String {
        let seat = match rank {
            1 => "L1".to_owned(),
            _ => format!("the L2 lead of {}", project.unwrap_or("its project")),
        };
        match how {
            "moved" | "swapped" => format!(
                "UNVRS moved {seat} into this thread from a {} thread. Same seat, same memory: continue where the work stands below and do not ask the captain for anything answered here.",
                from.unwrap_or("previous")
            ),
            "rebound" => {
                format!("UNVRS: this thread is {seat} again. Continue where the work stands below.")
            }
            _ => format!("UNVRS: this thread is now {seat}."),
        }
    }

    fn detached(&self, rank: u8, project: Option<&str>, to: &str) -> String {
        let seat = match rank {
            1 => "L1".to_owned(),
            _ => format!("the {} lead", project.unwrap_or("project")),
        };
        let cmd = match (rank, project) {
            (1, _) => "$unvrs:l1".to_owned(),
            (_, Some(p)) => format!("$unvrs:l2 {p}"),
            _ => "$unvrs:l2".into(),
        };
        format!(
            "UNVRS: {seat} moved to a {to} thread, so this thread is no longer an UNVRS seat. Tell the captain that in one line before anything else, and do not act as {seat}. To bring it back here, the captain types {cmd}."
        )
    }

    fn worker_prompt(
        &self,
        pid: usize,
        harness: &str,
        authority: &str,
        package: &str,
        cli: &str,
        handed_from: Option<(usize, String)>,
    ) -> String {
        let arrival = handed_from
            .map(|(p, h)| format!("This task was HANDED OFF to you from PID {p} ({h}), which stopped mid-task. The package says what is done: continue from `next` and do not redo anything under done.\n"))
            .unwrap_or_default();
        let authority = if authority == "implement" {
            "Authority: implement. Implement the task within its stated scope; you may edit files and run the required commands."
        } else {
            "Authority: report. Read and analyze; do not modify project or live state. You may write report and checkpoint files in your task directory."
        };
        format!(
            "You are UNVRS L3 worker PID {pid} on {harness}, driven by the kernel.\n{arrival}TASK PACKAGE (from UNVRS; it is your whole context):\n{package}\n\nHow UNVRS drives you:\n- Do ONE completed step per reply, then stop; starting a command includes polling it to completion, verifying its result, or terminating it. Never return a running execution session as the next step; transient sessions cannot cross UNVRS replies. UNVRS sends \"continue\" for the next.\n- {authority} Output shape controls the deliverable, not authority.\n- Read the sources in your contract with `{cli} ctl ctx search \"words\"`, `{cli} ctl ctx map <source>` and `{cli} ctl ctx get <ref>`; cite the refs you used.\n- End EVERY reply with this block, lists updated, ids kept:\nUNVRS-PROGRESS\nnow: <the step you just did>\nnext: <the next step>\nopen:\n- [o1] <item still to do>\ndone:\n- [o2] <finished item, with its result>\nEND-PROGRESS\n- When the whole task is finished, write the full handoff after the block (every section needs an item; write `- none` when a section is empty):\nHANDOFF\ndeliverables:\n- <path in your task directory or a project source, or: commit <sha> on <branch>>\ndecisions:\n- <decision and why>\nlearnings:\n- <what the next worker should know>\nEND-HANDOFF\nthen file field notes for the project (bugs, friction, surprises or changes you saw; `- none` if nothing):\nFIELD-NOTES\n- <bug|friction|change|fact>: <note, with refs>\nEND-NOTES\nthen write `UNVRS-RESULT: <the report, with refs>` last. The kernel checks the handoff before it accepts the task and sends it back with the reason when it fails.\n- {CONDUCT_LINE}\n- Commit a checkpoint after every meaningful step (git, in your work tree) and keep REPORT.md's \"next step\" current, so a fresh worker can continue if you are cut off.\n- If the task needs more judgment, message your delegator with `{cli} ctl send <parent-pid> \"judgment needed: evidence\"`; do not switch models.\n- Do not ask questions; nobody will answer.\n\nDo the first step now."
        )
    }

    fn worker_continue(&self, next: &str, mail: &str) -> String {
        format!(
            "continue: do the next step ({next}).{}\nEnd with the UNVRS-PROGRESS block. When the whole task is finished, add the HANDOFF block (deliverables, decisions, learnings; closed by END-HANDOFF), then the FIELD-NOTES block (closed by END-NOTES), and then write `UNVRS-RESULT: <the report, with refs>` last.",
            if mail.is_empty() {
                String::new()
            } else {
                format!("\nNew messages:\n{mail}")
            }
        )
    }

    fn seat_run(
        &self,
        rank: u8,
        project: Option<&str>,
        hot: &str,
        wakes: &str,
        cli: &str,
    ) -> String {
        let seat = match rank {
            1 => "L1 (the captain is away; stay within the captain's away words)".to_owned(),
            _ => format!("the L2 lead of {}", project.unwrap_or("its project")),
        };
        let work = match rank {
            1 => {
                format!(": route project work to its lead with `{cli} ctl send <project> \"text\"`")
            }
            _ => format!(
                ": plan it and dispatch report tasks with `{cli} ctl task --intent \"<the captain's words>\" --spec \"<your spec>\" --go \"<captain go>\" --done-when \"<one line>\" --shape report`, or answer from the sources yourself when that is enough"
            ),
        };
        format!(
            "UNVRS runs you on your own: no thread holds your seat, and these wakes need handling. You are {seat}. This prompt is your whole UNVRS context; UNVRS is running (captain commands are quoted here without their `$`, and none of them is for you to run).\n\n{hot}\n\n## Wakes to handle now\n{wakes}\n\nHandle them in this reply. A note (a `send` from another seat) is a request for work: act within its verified go and your role{work}. If it lacks a go, propose the work and ask the captain \"Go?\". For the other wakes, read what they point to (`{cli} ctl ctx get` or the file named). Keep what matters with `{cli} ctl remember` and hold anything the captain must decide with `{cli} ctl decide`. Reply with the outcome in two to four sentences for the captain: what you did and set in motion (outcome language, no mechanics)."
        )
    }

    fn digest(&self, facts: &serde_json::Value) -> String {
        let list = |k: &str| facts[k].as_array().cloned().unwrap_or_default();
        let mut out = String::from("UNVRS digest\n");
        let calls = list("calls");
        out.push_str("\nYour calls\n");
        if calls.is_empty() {
            out.push_str("- nothing needs you\n");
        }
        for c in &calls {
            let age = c["age_s"].as_u64().unwrap_or(0);
            out.push_str(&format!(
                "- {} ({}{}, open {}{}): {}{}\n",
                c["id"].as_str().unwrap_or(""),
                c["kind"].as_str().unwrap_or(""),
                c["project"]
                    .as_str()
                    .map(|p| format!(", {p}"))
                    .unwrap_or_default(),
                human_age(age),
                if c["overdue"] == true {
                    ", overdue"
                } else {
                    ""
                },
                c["question"].as_str().unwrap_or(""),
                c["options"]
                    .as_array()
                    .filter(|o| !o.is_empty())
                    .map(|o| format!(
                        " [{}]",
                        o.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ))
                    .unwrap_or_default(),
            ));
        }
        if !calls.is_empty() {
            out.push_str(
                "  (answer with $unvrs:answer <id> <your words>, or $unvrs:approve <id>)\n",
            );
        }
        out.push_str("\nDelivered\n");
        let delivered = list("delivered");
        if delivered.is_empty() {
            out.push_str("- nothing new\n");
        }
        for d in delivered.iter().rev().take(10) {
            out.push_str(&format!("- {}\n", d["text"].as_str().unwrap_or("")));
        }
        out.push_str("\nUnder way\n");
        let under = list("under_way");
        if under.is_empty() {
            out.push_str("- nothing running\n");
        }
        for u in &under {
            out.push_str(&format!(
                "- {}: {} ({} so far)\n",
                u["project"].as_str().unwrap_or("L1"),
                u["intent"].as_str().unwrap_or(""),
                human_age(u["elapsed_s"].as_u64().unwrap_or(0))
            ));
        }
        out.push_str("\nNext\n");
        let next = list("next");
        if next.is_empty() {
            out.push_str("- no plan recorded yet\n");
        }
        for n in &next {
            out.push_str(&format!(
                "- {}: {}\n",
                n["project"].as_str().unwrap_or("L1"),
                n["next"].as_str().unwrap_or("")
            ));
        }
        if let Some(a) = facts["away"].as_object() {
            out.push_str(&format!(
                "\nYou are away: \"{}\"\n",
                a.get("words").and_then(|w| w.as_str()).unwrap_or("")
            ));
        }
        out
    }

    fn event_text(&self, kind: &str, e: &serde_json::Value) -> Option<String> {
        let s = |k: &str| e[k].as_str().unwrap_or("").to_owned();
        let project = e["project"]
            .as_str()
            .map(|p| format!("{p}: "))
            .unwrap_or_default();
        Some(match kind {
            "ownership.check" => {
                let leaks: Vec<&str> = e["leaks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str())
                    .collect();
                match e["verdict"].as_str().unwrap_or("") {
                    "accepted" => format!(
                        "{project}worker PID {} passed the context-ownership check; UNVRS holds its brief, record, deliverables and learnings",
                        e["pid"]
                    ),
                    "bounced" => format!(
                        "{project}worker PID {} was sent back by the context-ownership check: {}",
                        e["pid"],
                        leaks.join("; ")
                    ),
                    _ => format!(
                        "{project}worker PID {} is blocked: it kept failing the context-ownership check ({})",
                        e["pid"],
                        leaks.join("; ")
                    ),
                }
            }
            "driver.event" => {
                let event = &e["event"];
                let label = format!("{project}worker PID {}", e["pid"]);
                match event["kind"].as_str().unwrap_or("") {
                    "accepted" => format!(
                        "{label}: {} accepted model {}{}",
                        s("harness"),
                        event["model"].as_str().unwrap_or("default"),
                        event["effort"]
                            .as_str()
                            .map(|effort| format!(", effort {effort}"))
                            .unwrap_or_default()
                    ),
                    "tool.started" => format!(
                        "{label}: {} started",
                        event["tool"].as_str().unwrap_or("tool")
                    ),
                    "tool.completed" => format!("{label}: tool completed"),
                    "progress" => format!(
                        "{label}: {}",
                        event["text"].as_str().unwrap_or("progress reported")
                    ),
                    "failure" => format!(
                        "{label}: {}",
                        event["error"].as_str().unwrap_or("driver failed")
                    ),
                    "turn.completed" if event["error"].is_string() => format!(
                        "{label}: {}",
                        event["error"].as_str().unwrap_or("turn failed")
                    ),
                    "turn.settling" => format!(
                        "{label}: completing unfinished commands (attempt {}/{}, {} commands)",
                        event["attempt"], event["max"], event["count"]
                    ),
                    "turn.completed" => format!("{label}: turn completed"),
                    _ => return None,
                }
            }
            "fold" => format!(
                "memory fold PID {} {} (read {}, version {})",
                e["pid"],
                s("result"),
                e["read"],
                e["version"]
                    .as_u64()
                    .or_else(|| e["current"].as_u64())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".into())
            ),
            "watchdog" => format!("kernel needs attention: {}", s("error")),
            "summary.route" if e["error"].is_string() => {
                format!("summary route {} failed: {}", s("job"), s("error"))
            }
            "summary.route" => format!("summary route {} {} succeeded", s("job"), s("phase")),
            "job" if e["job"] == "summary:startup" && e["error"].is_null() => {
                "summary startup checks completed".into()
            }
            "result" => format!(
                "{project}report {} \"{}\"",
                if s("how") == "done" {
                    "delivered"
                } else {
                    "failed"
                },
                s("intent")
            ),
            "outcome" => {
                // The last paragraph is the outcome; preambles about tools and skills go.
                let text = s("text");
                let last = text
                    .split("\n\n")
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .last()
                    .unwrap_or("")
                    .to_owned();
                format!("{project}lead handled its wakes: {last}")
            }
            "hold.open" => format!("{project}needs your call {}: {}", s("hold"), s("question")),
            "hold.close" if e["status"] == "moot" => format!(
                "{project}closed {} as moot: {} (overrule with $unvrs:answer {} <your words>)",
                s("hold"),
                s("evidence"),
                s("hold")
            ),
            "hold.close" => format!("{project}decided {}: \"{}\"", s("hold"), s("words")),
            "hold.reopen" => format!("{project}reopened {}: {}", s("hold"), s("question")),
            "project.create" => format!("project {} created", e["project"].as_str().unwrap_or("")),
            "remember" if e["by"] == "captain" => format!("{project}remembered {}", s("note")),
            "move" => format!(
                "{} moved from a {} thread to a {} thread",
                if e["rank"] == 1 {
                    "L1".into()
                } else {
                    format!("{} lead", e["project"].as_str().unwrap_or("project"))
                },
                s("from_app"),
                s("to_app")
            ),
            "handoff" => format!(
                "{project}work handed from {} to {}",
                s("from_harness"),
                s("to_harness")
            ),
            "task" => format!("{project}task started: {}", s("intent")),
            "mail" => format!(
                "{project}PID {} sent a message to its worker PID {}",
                e["pid"], e["to"]
            ),
            "refused" => format!(
                "refused a model's {}: {}",
                s("verb"),
                match s("reason") {
                    r if r.is_empty() || r.starts_with("captain-only") => "captain only".into(),
                    r => r,
                }
            ),
            "away" => "you went away".into(),
            "back" => "you are back".into(),
            _ => return None,
        })
    }

    fn ownership_bounce(&self, leaks: &[String], bounce: u32, max: u32) -> String {
        format!(
            "UNVRS did not accept your result: the context-ownership check failed ({bounce}/{max}). Fix each item, then reply again with the full HANDOFF block (deliverables, decisions, learnings; `- none` when empty), FIELD-NOTES and `UNVRS-RESULT` last. Move or copy deliverables into your task directory or a project source; see the `conduct` skill.\n{}",
            leaks
                .iter()
                .map(|l| format!("  - {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    fn help(&self) -> String {
        let mut out = String::from("UNVRS commands (type them yourself; models cannot):\n");
        for s in ENTRY {
            out.push_str(&format!(
                "  $unvrs:{}{} · {}\n",
                s.name,
                if s.hint.is_empty() {
                    String::new()
                } else {
                    format!(" {}", s.hint)
                },
                s.description
            ));
        }
        out.push_str("Seats use the MCP tool `unvrs` (or `unvrs ctl`): ctx sources|map|search|get, remember, stow, recall, decide [--supersedes id], moot <id> --evidence, task [--authority report|implement (default implement)], stop, send, project list|propose, digest, tree, whoami, settings get|history (writes are captain-only), econ show|explain <pid>|route --dry-run [task routing flags].");
        out
    }
}

fn human_age(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_authority_controls_actions_independent_of_report_shape() {
        use uke::Mapp;
        for authority in ["implement", "report", "report only"] {
            let prompt = UnvrsMapp.worker_prompt(
                7,
                "codex",
                authority,
                "shape: report\nspec: implement the change",
                "/task/unvrs",
                Some((6, "codex".into())),
            );
            assert!(prompt.contains("shape: report"));
            assert!(prompt.contains("HANDED OFF"));
            assert!(prompt.contains("Output shape controls the deliverable, not authority."));
            assert!(!prompt.contains("This is a REPORT task"));
            if authority == "implement" {
                assert!(prompt.contains("Authority: implement."));
                assert!(prompt.contains("you may edit files and run the required commands"));
                assert!(!prompt.contains("do not modify project or live state"));
            } else {
                assert!(prompt.contains("Authority: report."));
                assert!(prompt.contains("do not modify project or live state"));
                assert!(!prompt.contains("you may edit files and run the required commands"));
            }
        }
        assert!(
            UnvrsMapp
                .ops(1, "unvrs", false)
                .contains("--authority report|implement")
        );
        assert!(
            UnvrsMapp
                .ops(2, "unvrs", false)
                .contains("--authority report|implement")
        );
        for rank in [1, 2, 3] {
            let ops = UnvrsMapp.ops(rank, "unvrs", false);
            assert!(ops.contains("--go"));
            assert!(ops.contains("--done-when"));
            let role = UnvrsMapp.role(rank, Some("proj"), None);
            assert!(role.contains(ALIGNMENT));
        }
        for entry in ENTRY {
            assert!(skill_body(entry).contains(ALIGNMENT));
        }
        assert!(TOOL_DESCRIPTION.contains("--go"));
        assert!(TOOL_DESCRIPTION.contains("--done-when"));
    }

    #[test]
    fn summary_receipts_publish_success_and_failure() {
        use uke::Mapp;
        let mapp = super::UnvrsMapp;
        for phase in ["startup", "request"] {
            let event = serde_json::json!({"job":"summary:claude=claude-sonnet-5-5", "phase":phase, "error":null});
            let text = mapp.event_text("summary.route", &event).unwrap();
            assert!(text.contains(phase) && text.contains("succeeded"));
        }
        let event = serde_json::json!({"job":"summary:codex=gpt-5.6-luna", "phase":"request", "error":"invalid brief JSON"});
        assert!(
            mapp.event_text("summary.route", &event)
                .unwrap()
                .contains("failed: invalid brief JSON")
        );
    }

    #[test]
    fn moot_close_reads_as_moot_with_its_evidence_not_as_a_decision() {
        use uke::Mapp;
        let mapp = super::UnvrsMapp;
        let e = serde_json::json!({"hold":"d5", "status":"moot", "evidence":"replaced by d7", "project":"unvrs-rs"});
        let t = mapp.event_text("hold.close", &e).unwrap();
        assert!(
            t.contains("closed d5 as moot: replaced by d7")
                && t.contains("$unvrs:answer d5")
                && !t.contains("decided"),
            "{t}"
        );
        let e = serde_json::json!({"hold":"d4", "status":"answered", "words":"yes"});
        assert!(
            mapp.event_text("hold.close", &e)
                .unwrap()
                .contains("decided d4: \"yes\"")
        );
        let e = serde_json::json!({"hold":"d5", "question":"merge polish?"});
        assert!(
            mapp.event_text("hold.reopen", &e)
                .unwrap()
                .contains("reopened d5")
        );
    }

    #[test]
    fn conduct_is_model_invocable_and_never_steers_harness_memory() {
        use uke::Mapp;
        assert!(!conduct_claude_skill().contains("disable-model-invocation"));
        assert!(conduct_claude_skill().starts_with("---\nname: conduct\n"));
        assert!(conduct_codex_yaml().contains("allow_implicit_invocation: true"));
        for needle in [
            "HANDOFF",
            "deliverables:",
            "decisions:",
            "learnings:",
            "- none",
            "recall",
            "remember",
        ] {
            assert!(CONDUCT.contains(needle), "{needle}");
        }
        let lower = CONDUCT.to_lowercase();
        for banned in [
            "memory.md",
            "auto-memory",
            "save to your memory",
            "add to your memory",
        ] {
            assert!(!lower.contains(banned), "{banned}");
        }
        for rank in [1, 2, 3] {
            assert!(UnvrsMapp.role(rank, Some("p"), None).contains(CONDUCT_LINE));
        }
        let prompt = UnvrsMapp.worker_prompt(7, "codex", "implement", "pkg", "unvrs", None);
        assert!(prompt.contains("HANDOFF\ndeliverables:") && prompt.contains("END-HANDOFF"));
        assert!(UnvrsMapp.worker_continue("x", "").contains("HANDOFF"));
    }

    #[test]
    fn every_entry_is_explicit_only_in_both_harnesses() {
        assert!(ENTRY.len() <= 12);
        for s in ENTRY {
            assert!(claude_skill(s).contains("disable-model-invocation: true"));
            assert!(codex_skill_yaml(s).contains("allow_implicit_invocation: false"));
        }
    }
}
