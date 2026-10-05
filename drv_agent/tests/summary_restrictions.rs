//! Isolated exec summaries keep subscription/no-config/no-agent restrictions and
//! reject every tool/error item before accepting summary text.
use std::{fs, os::unix::fs::PermissionsExt};
use uke::TurnRequest;
const FAKE: &str = r#"#!/usr/bin/env python3
import os,sys,json,time
for key in ['OPENAI_API_KEY','OPENAI_BASE_URL','ANTHROPIC_API_KEY','CODEX_API_KEY','CODEX_ACCESS_TOKEN','OPENAI_FEDERATION_RULE_ID','OPENAI_IDENTITY_TOKEN_FILE','OPENAI_WORKLOAD_IDENTITY_CONTEXT']: assert key not in os.environ
assert 'cli_auth_credentials_store="file"' in sys.argv
if sys.argv[1:3]==['login','status']:
 print('private fixture key fragment' if os.environ.get('FAKE_AUTH') else 'Logged in using ChatGPT',file=sys.stderr)
 sys.exit(0)
assert 'forced_login_method="chatgpt"' not in sys.argv
for flag in ['--ignore-user-config','--ignore-rules','agents.enabled=false','features.multi_agent=false','features.multi_agent_v2=false','model_provider="openai"','features.shell_tool=false','features.skip_host_skill_discovery=true']:
 assert flag in sys.argv,flag
for key in ['OPENAI_API_KEY','OPENAI_BASE_URL','ANTHROPIC_API_KEY']: assert key not in os.environ
assert sys.argv[sys.argv.index('-m')+1]=='gpt-5.6-luna'
if os.environ.get("SCHEMA"):
 assert "--output-schema" in sys.argv
 schema=json.load(open(sys.argv[sys.argv.index("--output-schema")+1]))
 assert schema["type"]=="object"
else: assert "--output-schema" not in sys.argv
sys.stdin.read()
print(json.dumps({"type":"item.completed","item":{"type":"agent_message","text":"intermediate progress"}}),flush=True)
kind=os.environ.get('ITEM','agent_message')
item={'type':kind,'text':'UNVRS_SUMMARY_READY','message':'model rerouted: pinned -> other'}
print(json.dumps({'type':'item.started','item':item}),flush=True)
if kind!='agent_message': time.sleep(.3);open('worked','w').write('bad')
print(json.dumps({'type':'item.completed','item':item}),flush=True)
"#;
#[test]
fn summaries_disable_native_agents_and_fail_closed_on_unknown_items() {
    let dir =
        std::env::temp_dir().join(format!("unvrs-summary-restrictions-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("codex");
    fs::write(&bin, FAKE).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    unsafe {
        std::env::set_var("UNVRS_CODEX_BIN", &bin);
    }
    let req = |kind: &str| TurnRequest {
        harness: "codex".into(),
        cwd: dir.clone(),
        session: None,
        prompt: "summary".into(),
        model: Some("gpt-5.6-luna".into()),
        effort: None,
        env: vec![
            ("UNVRS_HOME".into(), dir.display().to_string()),
            ("UNVRS_DRIVEN_PID".into(), "149".into()),
            ("ITEM".into(), kind.into()),
            ("OPENAI_API_KEY".into(), "fixture".into()),
            ("OPENAI_BASE_URL".into(), "fixture".into()),
            ("CODEX_API_KEY".into(), "fixture".into()),
            ("CODEX_ACCESS_TOKEN".into(), "fixture".into()),
            ("OPENAI_FEDERATION_RULE_ID".into(), "fixture".into()),
            ("OPENAI_IDENTITY_TOKEN_FILE".into(), "fixture".into()),
            ("OPENAI_WORKLOAD_IDENTITY_CONTEXT".into(), "fixture".into()),
        ],
    };
    let mut bad_auth = req("agent_message");
    bad_auth.env.push(("FAKE_AUTH".into(), "1".into()));
    let auth_error = drv_agent::run_summary(&bad_auth).unwrap_err().to_string();
    assert!(auth_error.contains("ChatGPT auth mode") && !auth_error.contains("private fixture"));
    assert_eq!(
        drv_agent::run_summary(&req("agent_message")).unwrap(),
        "UNVRS_SUMMARY_READY"
    );
    let schema_path = dir.join("schema.json");
    fs::write(&schema_path, r#"{"type":"object"}"#).unwrap();
    let mut constrained = req("agent_message");
    constrained.env.push(("SCHEMA".into(), "1".into()));
    assert_eq!(
        drv_agent::run_summary_with_schema(&constrained, Some(&schema_path)).unwrap(),
        "UNVRS_SUMMARY_READY"
    );
    for kind in [
        "collab_tool_call",
        "mcp_tool_call",
        "web_search",
        "todo_list",
        "command_execution",
        "file_change",
        "dynamic_tool_call",
        "error",
        "unknown",
    ] {
        let err = drv_agent::run_summary(&req(kind)).unwrap_err();
        assert!(err.to_string().contains("unexpected item"), "{kind}: {err}");
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(!dir.join("worked").exists());
    fs::remove_dir_all(dir).unwrap();
}
