//! A Herdr-visible seat. ACP owns inference; the bridge owns message admission.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    env,
    io::{BufRead, BufReader, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use uke::signals::CommandTracking;
use uke::{clean, write_json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: &'static str,
    pub executable: String,
    pub args: &'static [&'static str],
    pub model_env: &'static str,
}

pub fn profiles() -> [Profile; 4] {
    [
        Profile {
            id: "pi",
            executable: env::var("UNVRS_ACP").unwrap_or_else(|_| "pi-acp".into()),
            args: &[],
            model_env: "UNVRS_MODEL",
        },
        Profile {
            id: "codex",
            executable: env::var("UNVRS_CODEX_ACP").unwrap_or_else(|_| "codex-acp".into()),
            args: &[],
            model_env: "UNVRS_CODEX_MODEL",
        },
        Profile {
            id: "claude",
            executable: env::var("UNVRS_CLAUDE_ACP").unwrap_or_else(|_| "claude-agent-acp".into()),
            args: &[],
            model_env: "UNVRS_CLAUDE_MODEL",
        },
        Profile {
            id: "cursor",
            executable: env::var("UNVRS_CURSOR_ACP").unwrap_or_else(|_| "agent".into()),
            args: &["acp"],
            model_env: "UNVRS_CURSOR_MODEL",
        },
    ]
}

fn profile(id: &str) -> Result<Profile> {
    profiles()
        .into_iter()
        .find(|profile| profile.id == id)
        .with_context(|| format!("Unknown ACP profile {id}"))
}

struct Cpu(uke::signals::TrackedChild);
fn emit(socket: &mut UnixStream, kind: &str, text: &str) -> Result<()> {
    write_json(socket, &json!({"event":kind,"text":text}))
}
pub fn run() -> Result<()> {
    let home = env::var("UNVRS_HOME").context("ACP requires UNVRS_HOME")?;
    let owner: usize = env::var("UNVRS_DRIVEN_PID")
        .or_else(|_| env::var("UNVRS_PID"))
        .context("ACP requires its owning UNVRS PID")?
        .parse()?;
    anyhow::ensure!(owner > 0, "ACP requires a nonzero owning UNVRS PID");
    let _owner = uke::signals::owner_scope(std::path::Path::new(&home), owner);
    let mut socket = UnixStream::connect(env::var("UNVRS_SOCKET")?)?;
    write_json(
        &mut socket,
        &json!({"op":"seat","token":env::var("UNVRS_TOKEN")?}),
    )?;
    println!(
        "UNVRS / {} / ACP\nLive flight recorder. Return to the BRIDGE tab to talk.\n",
        env::var("UNVRS_ROLE")?
    );
    let result = flight(&mut socket);
    if let Err(ref e) = result {
        let _ = emit(&mut socket, "error", &format!("{e:#}"));
        eprintln!("{e:#}");
    }
    result
}
fn flight(socket: &mut UnixStream) -> Result<()> {
    let profile = profile(&env::var("UNVRS_HARNESS").unwrap_or_else(|_| "pi".into()))?;
    let mut command = Command::new(&profile.executable);
    command.args(profile.args);
    if profile.id == "codex" {
        command.env("INITIAL_AGENT_MODE", "agent-full-access");
    }
    let mut cpu = Cpu(command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn_owned()
        .context("ACP launch failed; run unvrs doctor")?);
    emit(socket, "cpu", &cpu.0.id().to_string())?;
    let mut input = cpu.0.stdin.take().context("ACP stdin unavailable")?;
    let output = cpu.0.stdout.take().context("ACP stdout unavailable")?;
    let (tx, rx) = mpsc::channel();
    let output_tx = tx.clone();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => {
                    if output_tx.send((true, v)).is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("ACP non-JSON output: {e}"),
            }
        }
        let _ = output_tx.send((true, json!({"eof":true})));
    });
    let reader = socket.try_clone()?;
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && tx.send((false, v)).is_err()
            {
                return;
            }
        }
        let _ = tx.send((false, json!({"op":"shutdown"})));
    });
    write_json(
        &mut input,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"unvrs","version":"0.6.0"}}}),
    )?;
    let mut started = Instant::now();
    let mut session = String::new();
    let mut ready = false;
    let mut next_id = 10;
    let mut active_scope: Option<Value> = None;
    let mut pending_prompt: Option<Value> = None;
    let mut session_request_id = 2;
    let mut model_request_id = 3;
    let mut tools: HashMap<String, (String, String)> = HashMap::new();
    loop {
        let (acp, v) = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(v) => v,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !ready && started.elapsed() > Duration::from_secs(45) {
                    bail!("ACP boot timed out after 45s");
                }
                continue;
            }
            Err(_) => break,
        };
        if !acp {
            match v["op"].as_str().unwrap_or("") {
                "shutdown" => break,
                "prompt" if ready => {
                    let changed = active_scope
                        .as_ref()
                        .is_some_and(|scope| *scope != v["scope"]);
                    active_scope = Some(v["scope"].clone());
                    if changed {
                        pending_prompt = Some(v);
                        ready = false;
                        started = Instant::now();
                        new_session(&mut input, 5)?;
                    } else {
                        send_prompt(&mut input, &session, &mut next_id, &v)?;
                    }
                }
                "cancel" if ready => {
                    write_json(
                        &mut input,
                        &json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}),
                    )?;
                    println!("\nCANCEL REQUESTED");
                }
                "permission" => write_json(
                    &mut input,
                    &json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":v["outcome"]}}),
                )?,
                _ => {}
            }
            continue;
        }
        if v["eof"] == true {
            bail!("ACP exited; inspect the seat recorder for diagnostics");
        }
        if v.get("error").is_some() {
            let text = v["error"].to_string();
            if !ready {
                bail!("ACP boot refused: {text}");
            }
            emit(socket, "turn_error", &text)?;
            continue;
        }
        match v.get("result").and_then(|_| v["id"].as_u64()) {
            Some(1) => {
                if profile.id == "cursor"
                    && let Some(method) = auth_method(&v["result"])
                {
                    write_json(
                        &mut input,
                        &json!({"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"methodId":method}}),
                    )?;
                    session_request_id = 3;
                    model_request_id = 4;
                } else {
                    new_session(&mut input, session_request_id)?;
                }
            }
            Some(2) if session_request_id == 3 => new_session(&mut input, session_request_id)?,
            Some(id) if id == session_request_id => {
                configuration(socket, &v["result"])?;
                session = v["result"]["sessionId"]
                    .as_str()
                    .context("ACP omitted sessionId")?
                    .to_owned();
                if let Ok(model) = env::var(profile.model_env) {
                    write_json(
                        &mut input,
                        &json!({"jsonrpc":"2.0","id":model_request_id,"method":"session/set_model","params":{"sessionId":session,"modelId":model}}),
                    )?;
                } else {
                    ready = true;
                }
                emit(
                    socket,
                    "model",
                    v["result"]["models"]["currentModelId"]
                        .as_str()
                        .unwrap_or("unknown"),
                )?;
                if ready {
                    emit(socket, "ready", &session)?;
                }
            }
            Some(id) if id == model_request_id => {
                ready = true;
                emit(socket, "ready", &session)?;
                emit(socket, "model", &env::var(profile.model_env)?)?;
            }
            Some(5) => {
                session = v["result"]["sessionId"]
                    .as_str()
                    .context("ACP omitted rebind sessionId")?
                    .into();
                if let Ok(model) = env::var(profile.model_env) {
                    write_json(
                        &mut input,
                        &json!({"jsonrpc":"2.0","id":6,"method":"session/set_model","params":{"sessionId":session,"modelId":model}}),
                    )?;
                } else {
                    ready = true;
                }
            }
            Some(6) => ready = true,
            Some(id) if id >= 10 && v.get("result").is_some() => {
                emit(
                    socket,
                    "done",
                    v["result"]["stopReason"].as_str().unwrap_or("end_turn"),
                )?;
                tools.clear();
                println!("\n\n[ TURN COMPLETE ]");
            }
            _ => {}
        }
        if ready && let Some(prompt) = pending_prompt.take() {
            emit(socket, "rebound", &session)?;
            send_prompt(&mut input, &session, &mut next_id, &prompt)?;
        }
        if v["method"] == "session/update" {
            let u = &v["params"]["update"];
            match u["sessionUpdate"].as_str().unwrap_or("") {
                "agent_message_chunk" => {
                    let text = u["content"]["text"].as_str().unwrap_or("");
                    print!("{}", clean(text));
                    std::io::stdout().flush()?;
                    emit(socket, "chunk", text)?;
                }
                "tool_call" | "tool_call_update" => {
                    let id = u["toolCallId"].as_str().unwrap_or("tool").to_owned();
                    let entry = tools
                        .entry(id)
                        .or_insert_with(|| ("tool".into(), String::new()));
                    if let Some(title) = u["title"].as_str() {
                        entry.0 = title.into();
                    }
                    let status = u["status"].as_str().unwrap_or("running");
                    if entry.1 != status {
                        entry.1 = status.into();
                        let text = format!("{} · {}", entry.0, status);
                        println!("\n[ {} ]", clean(&text));
                        emit(socket, "tool", &text)?;
                    }
                    if let Some(output) = u["_meta"]["terminal_output"]["data"].as_str() {
                        print!("{}", clean(output));
                        std::io::stdout().flush()?;
                        emit(socket, "chunk", output)?;
                    }
                }
                "config_option_update" => configuration(socket, u)?,
                "current_mode_update" if profile.id != "codex" => emit(
                    socket,
                    "effort",
                    u["currentModeId"].as_str().unwrap_or("unknown"),
                )?,
                "usage_update" => emit(socket, "usage", &u.to_string())?,
                _ => {}
            }
        } else if v["method"] == "session/request_permission" {
            let option = permission_option(&v)
                .context("ACP offered no allow option under full-access policy")?;
            write_json(
                &mut input,
                &json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"selected","optionId":option}}}),
            )?;
        } else if v.get("method").is_some() && v.get("id").is_some() {
            write_json(
                &mut input,
                &json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32601,"message":"Client capability not supported"}}),
            )?;
        }
    }
    Ok(())
}

fn send_prompt(input: &mut impl Write, session: &str, next_id: &mut u64, v: &Value) -> Result<()> {
    let body = v["text"].as_str().unwrap_or("");
    let role = env::var("UNVRS_ROLE").unwrap_or_default();
    let body = format!(
        "You are UNVRS {role}. The human is the captain. Use unvrs ctl for recall, remember, handoff and rank-limited delegation.\n\n{body}"
    );
    let text = match v["hot"].as_str() {
        Some(hot) => format!(
            "[UNVRS hot set · session context, not a new request; use recall for everything else]\n{hot}\n\n{body}"
        ),
        None => body,
    };
    println!("\nCAPTAIN / MAILBOX > {}\n", clean(&text));
    write_json(
        input,
        &json!({"jsonrpc":"2.0","id":*next_id,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}}),
    )?;
    *next_id += 1;
    Ok(())
}

fn permission_option(v: &Value) -> Option<&str> {
    let options = v["params"]["options"].as_array()?;
    options
        .iter()
        .find(|o| allowed(o, "always"))
        .or_else(|| options.iter().find(|o| allowed(o, "once")))?["optionId"]
        .as_str()
}
fn allowed(option: &Value, duration: &str) -> bool {
    ["kind", "optionId"].iter().any(|key| {
        option[*key]
            .as_str()
            .is_some_and(|value| value.replace('-', "_") == format!("allow_{duration}"))
    })
}
fn auth_method(result: &Value) -> Option<&str> {
    let method = result["authMethods"].as_array()?.first()?;
    method["id"]
        .as_str()
        .or_else(|| method["methodId"].as_str())
}
fn new_session(input: &mut impl Write, id: u64) -> Result<()> {
    write_json(
        input,
        &json!({"jsonrpc":"2.0","id":id,"method":"session/new","params":{"cwd":env::current_dir()?,"mcpServers":[]}}),
    )
}
fn configuration(socket: &mut UnixStream, value: &Value) -> Result<()> {
    if let Some(options) = value["configOptions"].as_array() {
        for option in options {
            let category = option["category"].as_str().unwrap_or("");
            let id = option["id"].as_str().unwrap_or("");
            let kind = if category == "model" {
                "model"
            } else if category == "thought_level" || id.contains("effort") {
                "effort"
            } else {
                continue;
            };
            emit(
                socket,
                kind,
                option["currentValue"].as_str().unwrap_or("unknown"),
            )?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_access_uses_named_allow_option() {
        let v = json!({"params":{"options":[{"kind":"reject_once","optionId":"no"},{"kind":"allow_once","optionId":"yes"}]}});
        assert_eq!(permission_option(&v), Some("yes"));
        assert_eq!(permission_option(&json!({})), None);
        let cursor = json!({"params":{"options":[{"kind":"reject_once","optionId":"reject-once"},{"optionId":"allow-always"}]}});
        assert_eq!(permission_option(&cursor), Some("allow-always"));
    }
    #[test]
    fn profiles_include_cursor_acp_argument() {
        let profiles = profiles();
        assert_eq!(
            profiles.iter().map(|p| p.id).collect::<Vec<_>>(),
            ["pi", "codex", "claude", "cursor"]
        );
        assert_eq!(
            profiles[3].executable,
            env::var("UNVRS_CURSOR_ACP").unwrap_or_else(|_| "agent".into())
        );
        assert_eq!(profiles[3].args, ["acp"]);
    }
}
