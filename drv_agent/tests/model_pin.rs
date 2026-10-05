//! A task's pinned model: passed to the harness, read back from what it reports, and a
//! mismatch stops the CPU before it does any work (no silent substitution).
use std::{fs, os::unix::fs::PermissionsExt, path::Path, thread, time::Duration};
use uke::TurnRequest;

/// A fake `claude -p`: records its argv, reports `FAKE_RAN` (else the --model it got) in
/// `system/init`, waits, then "works" (touches `worked`) and uses a tool.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
cat >/dev/null
printf '%s\n' "$*" > args
chosen=claude-default
while [ "$#" -gt 0 ]; do
  case "$1" in --model) shift; chosen="$1";; esac
  shift
done
ran="${FAKE_RAN:-$chosen}"
printf '{"type":"system","subtype":"init","tools":["Bash"],"session_id":"s1","model":"%s"}\n' "$ran"
sleep 1
touch worked
printf '{"type":"assistant","message":{"model":"%s","content":[{"type":"tool_use","name":"Bash","input":{"command":"touch worked"}},{"type":"text","text":"done"}]}}\n' "${FAKE_RAN_LATER:-$ran}"
printf '{"type":"result","session_id":"s1","result":"done"}\n'
"#;

fn req(dir: &Path, model: Option<&str>, env: &[(&str, &str)]) -> TurnRequest {
    TurnRequest {
        harness: "claude".into(),
        cwd: dir.to_path_buf(),
        session: None,
        prompt: "do the task".into(),
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        model: model.map(str::to_owned),
        effort: None,
    }
}

// One test: UNVRS_CLAUDE_BIN is process-wide, so the cases run in sequence.
#[test]
fn pinned_model_is_passed_verified_and_never_substituted() {
    let dir = std::env::temp_dir().join(format!("unvrs-model-pin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("claude");
    fs::write(&bin, FAKE_CLAUDE).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    unsafe { std::env::set_var("UNVRS_CLAUDE_BIN", &bin) };

    // (a) --model reaches the harness and (b) a matching model is the receipt.
    let out = drv_agent::run_turn(&req(&dir, Some("claude-opus-5-5"), &[]), &|_| {}).unwrap();
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(args.contains("--model claude-opus-5-5"), "argv: {args}");
    assert_eq!(out.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(out.tools, vec!["Bash"]);
    assert_eq!(out.text, "done");

    // (c) the harness reports another model: killed at init, before any work.
    fs::remove_file(dir.join("worked")).unwrap();
    let err = drv_agent::run_turn(
        &req(
            &dir,
            Some("claude-opus-5-5"),
            &[("FAKE_RAN", "claude-sonnet-5")],
        ),
        &|_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("requested claude-opus-5-5") && err.contains("reported claude-sonnet-5"),
        "{err}"
    );
    thread::sleep(Duration::from_millis(1500));
    assert!(!dir.join("worked").exists(), "the mismatched CPU did work");

    // (d) init matches but a later message runs on another model (a fallback): the
    // turn fails before that message's tool call is taken as done.
    let err = drv_agent::run_turn(
        &req(
            &dir,
            Some("claude-opus-5-5"),
            &[("FAKE_RAN_LATER", "claude-haiku-4-5-20251001")],
        ),
        &|_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("reported claude-haiku-4-5-20251001"), "{err}");

    // Aliases and variants are not the pinned id: exact match only.
    let err = drv_agent::run_turn(
        &req(
            &dir,
            Some("claude-opus-5-5"),
            &[("FAKE_RAN", "claude-opus-5-5[1m]")],
        ),
        &|_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("model mismatch"), "{err}");

    // Nothing pinned: the harness default runs and is still reported.
    let out = drv_agent::run_turn(&req(&dir, None, &[]), &|_| {}).unwrap();
    assert!(
        !fs::read_to_string(dir.join("args"))
            .unwrap()
            .contains("--model")
    );
    assert_eq!(out.model.as_deref(), Some("claude-default"));

    fs::remove_dir_all(dir).unwrap();
}
