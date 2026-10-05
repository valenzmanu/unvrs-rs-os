//! A12 mechanical brief rules through the public API (memory-layer §13 R2, D22):
//! compare-and-swap, open items cannot vanish, decisions graduate into notes, the
//! previous brief goes to the cold file.
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use uke::{Brief, BriefFold, FoldOutcome, MemoryIndex};

struct Universe(PathBuf);
impl Universe {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("uke-brief-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Universe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn brief(open: &[&str], decisions: usize) -> Brief {
    Brief {
        goal: "ship the release".into(),
        now: "writing notes".into(),
        open: open.iter().map(|s| s.to_string()).collect(),
        decisions: (0..decisions)
            .map(|i| format!("decision {i} — reason {i}"))
            .collect(),
        ..Default::default()
    }
}

#[test]
fn a12_concurrent_folds_on_one_version_apply_once_and_discard_once() {
    let u = Universe::new("cas");
    let mut m = MemoryIndex::open(&u.0).unwrap();
    m.append_turn(1, "Captain: start").unwrap();
    let a = m.fold_job(1, "detach", false).unwrap().unwrap();
    let b = m.fold_job(1, "swap", false).unwrap().unwrap();
    assert_eq!(a.version, b.version);
    let first = m
        .apply_fold(
            &a,
            BriefFold {
                brief: brief(&["waiting on QA"], 0),
                notes: vec![],
            },
        )
        .unwrap();
    let second = m
        .apply_fold(
            &b,
            BriefFold {
                brief: brief(&[], 0),
                notes: vec![],
            },
        )
        .unwrap();
    assert_eq!(
        first,
        FoldOutcome::Applied {
            version: 1,
            graduated: 0
        }
    );
    assert_eq!(
        second,
        FoldOutcome::Discarded {
            read: 0,
            current: 1
        }
    );
    let s = m.session(1).unwrap();
    assert_eq!(s.brief_version, 1);
    assert_eq!(s.brief.open, vec!["[o1] waiting on QA".to_string()]);
}

#[test]
fn a12_a_fold_that_drops_an_open_item_is_rejected_and_the_brief_stays() {
    let u = Universe::new("open");
    let mut m = MemoryIndex::open(&u.0).unwrap();
    m.write_brief(1, "seed", brief(&["waiting on QA", "pick a title"], 0))
        .unwrap();
    let before = m.session(1).unwrap();
    let job = m.fold_job(1, "x", false).unwrap();
    let job = job.unwrap_or_else(|| {
        m.append_turn(1, "turn").unwrap();
        m.fold_job(1, "x", false).unwrap().unwrap()
    });
    let err = m
        .apply_fold(
            &job,
            BriefFold {
                brief: brief(&["[o1] waiting on QA"], 0),
                notes: vec![],
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("[o2] pick a title"), "{err}");
    let after = m.session(1).unwrap();
    assert_eq!(after.brief, before.brief);
    assert_eq!(after.brief_version, before.brief_version);
    // Moving it to done (same id) is fine.
    let mut ok = brief(&["[o1] waiting on QA"], 0);
    ok.done = vec!["[o2] title picked: Owned context".into()];
    assert!(matches!(
        m.apply_fold(
            &job,
            BriefFold {
                brief: ok,
                notes: vec![]
            }
        )
        .unwrap(),
        FoldOutcome::Applied { .. }
    ));
}

#[test]
fn a12_decision_overflow_becomes_notes_and_the_previous_brief_goes_cold() {
    let u = Universe::new("overflow");
    let mut m = MemoryIndex::open(&u.0).unwrap();
    m.write_brief(1, "seed", brief(&[], 2)).unwrap();
    let outcome = m.write_brief(1, "fold", brief(&[], 11)).unwrap();
    assert_eq!(
        outcome,
        FoldOutcome::Applied {
            version: 2,
            graduated: 3
        }
    );
    let s = m.session(1).unwrap();
    assert_eq!(s.brief.decisions.len(), uke::BRIEF_DECISIONS);
    assert_eq!(s.brief.decisions[0], "decision 3 — reason 3");
    let notes = m.notes().unwrap();
    let graduated: Vec<_> = notes
        .iter()
        .filter(|n| n.body.starts_with("Decision: decision ") && n.source == "compaction:pid:1")
        .collect();
    assert_eq!(graduated.len(), 3);
    assert!(graduated.iter().all(|n| !n.pinned));
    let cold = fs::read_to_string(u.0.join("sessions/pid-1/briefs.jsonl")).unwrap();
    assert_eq!(cold.lines().count(), 1);
    assert!(cold.contains("\"version\":1") && cold.contains("decision 1 — reason 1"));
}
