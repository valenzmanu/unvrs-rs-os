//! A task's pinned reasoning effort: passed to claude (flag and env), confirmed on the
//! live process and in claude's init, and anything unconfirmed stops the CPU before it
//! works (no silent downgrade).
use std::{fs, os::unix::fs::PermissionsExt, path::Path, thread, time::Duration};
use uke::TurnRequest;

/// A fake `claude -p`: records its argv and CLAUDE_CODE_EFFORT_LEVEL, reports
/// `per_turn_effort_active` (FAKE_ACTIVE: true | false | missing; FAKE_NO_INIT skips
/// the init event), waits, then "works" (touches `worked`) and uses a tool.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
cat >/dev/null
printf '%s\n' "$*" > args
printf '%s\n' "${CLAUDE_CODE_EFFORT_LEVEL:-unset}" > env_effort
active="${FAKE_ACTIVE:-true}"
if [ -z "$FAKE_NO_INIT" ]; then
  if [ "$active" = missing ]; then
    printf '{"type":"system","subtype":"init","tools":["Bash"],"session_id":"s1","model":"claude-opus-5-5"}\n'
  else
    printf '{"type":"system","subtype":"init","tools":["%s"],"session_id":"s1","model":"claude-opus-5-5","per_turn_effort_active":%s}\n' "${FAKE_TOOL:-Bash}" "$active"
  fi
fi
sleep 1
touch worked
printf '{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","name":"Bash","input":{"command":"touch worked"}},{"type":"text","text":"done"}]}}\n'
printf '{"type":"result","session_id":"s1","result":"done"}\n'
"#;

/// A fake `codex exec` that would leave a mark if it were ever started.
const FAKE_CODEX: &str = "#!/bin/sh\ntouch codex-ran\ncat >/dev/null\n";

fn req(dir: &Path, harness: &str, effort: Option<&str>, env: &[(&str, &str)]) -> TurnRequest {
    TurnRequest {
        harness: harness.into(),
        cwd: dir.to_path_buf(),
        session: None,
        prompt: "do the task".into(),
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        model: None,
        effort: effort.map(str::to_owned),
    }
}

fn read(dir: &Path, f: &str) -> String {
    fs::read_to_string(dir.join(f)).unwrap_or_default()
}

fn assert_no_work(dir: &Path) {
    thread::sleep(Duration::from_millis(1500));
    assert!(!dir.join("worked").exists(), "the unconfirmed CPU did work");
}

// One test: UNVRS_CLAUDE_BIN / UNVRS_CODEX_BIN are process-wide, so cases run in sequence.
#[test]
fn pinned_effort_is_passed_verified_and_never_downgraded() {
    let dir = std::env::temp_dir().join(format!("unvrs-effort-pin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (name, body) in [("claude", FAKE_CLAUDE), ("codex", FAKE_CODEX)] {
        let bin = dir.join(name);
        fs::write(&bin, body).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    }
    unsafe {
        std::env::set_var("UNVRS_CLAUDE_BIN", dir.join("claude"));
        std::env::set_var("UNVRS_CODEX_BIN", dir.join("codex"));
        std::env::remove_var("CLAUDE_CODE_EFFORT_LEVEL");
    }

    // (a) pass-through: --effort high and the env pin reach claude, even over an
    // inherited CLAUDE_CODE_EFFORT_LEVEL=low; (b) verified match with evidence.
    let out = drv_agent::run_turn(
        &req(
            &dir,
            "claude",
            Some("high"),
            &[("CLAUDE_CODE_EFFORT_LEVEL", "low")],
        ),
        &|_| {},
    )
    .unwrap();
    assert!(
        read(&dir, "args").contains("--effort high"),
        "argv: {}",
        read(&dir, "args")
    );
    assert_eq!(read(&dir, "env_effort").trim(), "high");
    assert_eq!(out.effort.as_deref(), Some("high"));
    let ev = out.effort_evidence.unwrap_or_default();
    assert!(
        ev.contains("--effort high")
            && ev.contains("CLAUDE_CODE_EFFORT_LEVEL=high")
            && ev.contains("per_turn_effort_active=true"),
        "{ev}"
    );
    assert_eq!(out.tools, vec!["Bash"]);

    // (c) mismatch: claude says the session runs without a per-turn effort (a model
    // that ignores it): killed at init, before any work.
    fs::remove_file(dir.join("worked")).unwrap();
    let err = drv_agent::run_turn(
        &req(&dir, "claude", Some("max"), &[("FAKE_ACTIVE", "false")]),
        &|_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("effort not applied") && err.contains("requested max"),
        "{err}"
    );
    assert_no_work(&dir);

    // (d) cannot confirm: the init has no effort field, or work arrives before init.
    for env in [("FAKE_ACTIVE", "missing"), ("FAKE_NO_INIT", "1")] {
        let err = drv_agent::run_turn(&req(&dir, "claude", Some("high"), &[env]), &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("no silent downgrade"), "{env:?}: {err}");
        let _ = fs::remove_file(dir.join("worked"));
    }

    // Native delegation is removed per run; a harness that keeps Agent is refused.
    assert!(read(&dir, "args").contains("--disallowed-tools Agent,Task"));
    let err = drv_agent::run_turn(
        &req(&dir, "claude", Some("high"), &[("FAKE_TOOL", "Agent")]),
        &|_| {},
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("native delegation restriction not applied"),
        "{err:#}"
    );
    assert_no_work(&dir);

    // Unpinned: nothing passed, nothing enforced, the old behaviour.
    let out = drv_agent::run_turn(
        &req(&dir, "claude", None, &[("FAKE_ACTIVE", "false")]),
        &|_| {},
    )
    .unwrap();
    assert!(!read(&dir, "args").contains("--effort"));
    assert_eq!(read(&dir, "env_effort").trim(), "unset");
    assert_eq!(out.effort, None);

    fs::remove_dir_all(dir).unwrap();
}
