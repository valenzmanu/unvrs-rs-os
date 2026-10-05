//! Timeout cleanup uses real signals only against this test's own harness children.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};
use uke::TurnRequest;

#[test]
fn timeout_reaps_chatty_and_closed_stdout_harnesses() {
    let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/headless-timeout-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    // This integration-test binary has one test; environment changes cannot race another test.
    unsafe {
        std::env::set_var("UNVRS_TURN_IDLE_SECS", "1");
        std::env::set_var("UNVRS_TURN_MAX_SECS", "1");
    }
    let path = home.join("partial");
    fs::write(&path, r#"#!/usr/bin/env python3
import json,sys,time
partials='--include-partial-messages' in sys.argv
sys.stdin.read()
def emit(v): print(json.dumps(v),flush=True)
emit({'type':'system','subtype':'init','session_id':'s1','model':'claude-test','tools':['Write']})
for i in range(5):
 if partials:emit({'type':'stream_event','session_id':'s1','event':{'type':'content_block_delta','index':0,'delta':{'type':'input_json_delta','partial_json':'opaque-secret'}}})
 time.sleep(.3)
emit({'type':'assistant','message':{'model':'claude-test','content':[{'type':'tool_use','name':'Write','input':{'secret':'opaque-secret'}},{'type':'text','text':'done'}]}})
emit({'type':'user','message':{'content':[{'type':'tool_result','is_error':False}]}})
emit({'type':'result','session_id':'s1','result':'done'})
"#).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    unsafe {
        std::env::set_var("UNVRS_CLAUDE_BIN", &path);
        std::env::set_var("UNVRS_TURN_MAX_SECS", "10");
    }
    let events = std::cell::RefCell::new(vec![]);
    let out = drv_agent::run_turn_observed(
        &TurnRequest {
            harness: "claude".into(),
            cwd: home.clone(),
            session: None,
            prompt: "task".into(),
            model: Some("claude-test".into()),
            effort: None,
            env: vec![
                ("UNVRS_HOME".into(), home.display().to_string()),
                ("UNVRS_DRIVEN_PID".into(), "162".into()),
            ],
        },
        &|_| {},
        &|v| {
            if !v.is_null() {
                events.borrow_mut().push(v.clone());
            }
        },
    )
    .unwrap();
    assert_eq!(out.text, "done");
    assert_eq!(out.tools, vec!["Write"]);
    let events = events.borrow();
    assert_eq!(events.iter().filter(|v| v["kind"] == "delta").count(), 5);
    assert_eq!(
        events
            .iter()
            .filter(|v| v["kind"] == "tool.started")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|v| v["kind"] == "tool.completed")
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&*events)
            .unwrap()
            .contains("opaque-secret")
    );
    unsafe {
        std::env::set_var("UNVRS_TURN_MAX_SECS", "1");
    }
    for (name, script) in [
        ("chatty", "while :; do echo '{}'; done"),
        ("closed", "exec 1>&-; sleep 30"),
    ] {
        let path = home.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        unsafe {
            std::env::set_var("UNVRS_CLAUDE_BIN", &path);
        }
        let req = TurnRequest {
            harness: "claude".into(),
            cwd: home.clone(),
            session: None,
            prompt: "x".into(),
            model: None,
            effort: None,
            env: vec![
                ("UNVRS_HOME".into(), home.display().to_string()),
                ("UNVRS_DRIVEN_PID".into(), "149".into()),
            ],
        };
        let pid = std::cell::Cell::new(0);
        let started = Instant::now();
        let error = drv_agent::run_turn(&req, &|p| pid.set(p)).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(uke::signals::process_exists(i64::from(pid.get())).is_err());
        assert!(
            uke::signals::LedgerStore::new(&home)
                .snapshot()
                .unwrap()
                .entries()
                .is_empty()
        );
    }
    fs::remove_dir_all(home).unwrap();
}
