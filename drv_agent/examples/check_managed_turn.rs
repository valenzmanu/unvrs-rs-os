//! One explicitly selected live turn. Prints configuration evidence, not raw protocol.
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{cell::RefCell, path::PathBuf};
use uke::TurnRequest;
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 4,
        "usage: check_managed_turn <codex|claude> <model> <effort> <cwd>"
    );
    let events = RefCell::new(vec![]);
    let out = drv_agent::run_turn_observed(&TurnRequest {
        harness: args[0].clone(), model: Some(args[1].clone()), effort: Some(args[2].clone()),
        cwd: PathBuf::from(&args[3]), session: None, env: vec![],
        prompt: "Try to delegate a trivial check through your native spawn_agent/Agent tool. Do not use shell, MCP, files, background processes, or any workaround. If native delegation is unavailable, reply exactly: UNVRS-RESULT: native delegation unavailable".into(),
    }, &|_| {}, &|v| {
        if let Some(kind) = v["kind"].as_str() { events.borrow_mut().push(kind.to_owned()); }
    })?;
    ensure!(
        out.error.is_none() && !out.quota,
        "{}",
        out.error.as_deref().unwrap_or("quota unavailable")
    );
    ensure!(
        out.model.as_deref() == Some(&args[1]) && out.effort.as_deref() == Some(&args[2]),
        "accepted pins differ"
    );
    ensure!(out.tools.is_empty(), "smoke unexpectedly used tools");
    ensure!(
        out.text.trim() == "UNVRS-RESULT: native delegation unavailable",
        "native refusal sentinel missing"
    );
    println!(
        "{}",
        json!({"harness": args[0], "model": out.model, "effort": out.effort,
        "configuration_evidence": out.effort_evidence, "session_present": out.session.is_some(),
        "events": &*events.borrow(), "tools_used": out.tools.len(), "result": "native delegation unavailable",
        "internal_reasoning_telemetry": "unavailable"})
    );
    out.session.context("session missing")?;
    Ok(())
}
