use drv_hdff::{CompactionWorker, PiCompactor};
use std::{fs, os::unix::fs::PermissionsExt};
use uke::CompactionJob;

#[test]
fn a6_worker_uses_a_separate_process_and_selected_model() {
    let dir = std::env::temp_dir().join(format!("unvrs-compactor-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let executable = dir.join("pi-fixture");
    fs::write(
        &executable,
        r#"#!/bin/sh
cat >/dev/null
while [ "$#" -gt 0 ]; do
  case "$1" in --model) shift; chosen="$1";; esac
  shift
done
printf '{"summary":"worker pid %s","notes":["--model %s --no-session"]}' "$$" "$chosen"
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = PiCompactor {
        executable: executable.display().to_string(),
        model: "fixture-model".into(),
    };
    let fold = worker
        .fold(&CompactionJob {
            pid: 1,
            scope: Default::default(),
            summary: String::new(),
            turns: vec!["a durable decision".into()],
        })
        .unwrap();
    assert_ne!(fold.summary, format!("worker pid {}", std::process::id()));
    assert!(fold.notes[0].contains("--model fixture-model"));
    assert!(fold.notes[0].contains("--no-session"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires authenticated Luna on Pi"]
fn live_luna_compaction() {
    let fold=PiCompactor::default().fold(&CompactionJob{pid:77,scope:Default::default(),summary:String::new(),turns:vec!["The captain chose Markdown files as the memory record. The lexical index is disposable. Open: finish the memory voyage; done check is A1-A11 exit zero.".into()]}).unwrap();
    assert!(!fold.summary.is_empty());
    assert!(!fold.notes.is_empty());
}

/// A12 live: the real compaction worker (Luna on Pi) returns the brief schema and keeps
/// every open item's id. Opt-in: `cargo test -p drv_hdff --test compaction -- --ignored`.
#[test]
#[ignore]
fn live_luna_brief_fold_returns_the_schema() {
    let job = uke::FoldJob {
        pid: 1,
        version: 3,
        reason: "detach".into(),
        brief: uke::Brief {
            goal: "Write a blog post in five steps".into(),
            now: "step 3: waiting on the captain for the title".into(),
            open: vec!["[o1] title needed from the captain".into()],
            done: vec!["[o2] outline".into(), "[o3] intro".into()],
            ..Default::default()
        },
        turns: vec![
            "Captain: The title is 'Flying with owned context'. Step 3 is done; step 4 (body) is next.".into(),
            "codex: Step 3 done. Next: the body.".into(),
        ],
        covered: 2,
        drain: false,
        area: None,
    };
    let fold = (0..3)
        .find_map(
            |_| match drv_hdff::PiCompactor::default().fold_brief(&job) {
                Ok(f) => Some(f),
                Err(e) => {
                    eprintln!("brief worker attempt failed: {e:#}");
                    None
                }
            },
        )
        .expect("three brief worker attempts failed");
    let b = fold.brief;
    assert!(!b.goal.is_empty() && !b.now.is_empty(), "{b:?}");
    uke::Brief::check_open_items(&job.brief, &b).unwrap();
    let all = [b.open.clone(), b.done.clone()].concat().join(" ");
    assert!(
        all.contains("[o1]") && all.to_lowercase().contains("owned context"),
        "{b:?}"
    );
}
