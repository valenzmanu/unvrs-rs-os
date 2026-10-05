use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uke::{
    ActiveScope, CompactionFold, MemoryActor, MemoryIndex, PidSession, TAIL_BYTES,
    UNIVERSE_PIN_BYTES,
};

struct Universe(PathBuf);

impl Universe {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("uke-memory-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn memory(&self) -> MemoryIndex {
        MemoryIndex::open(&self.0).unwrap()
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Universe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a2_note_edit_and_index_deletion_keep_recall() {
    let universe = Universe::new("a2");
    let mut memory = universe.memory();
    let note = memory
        .remember(
            MemoryActor::Captain,
            1,
            None,
            "original",
            "before",
            false,
            false,
        )
        .unwrap();
    fs::write(
        &note.path,
        fs::read_to_string(&note.path)
            .unwrap()
            .replace("before", "hand-edited-a2"),
    )
    .unwrap();
    fs::remove_file(universe.path().join("memory/index/lexical.json")).unwrap();

    let mut reloaded = universe.memory();
    assert_eq!(reloaded.note(&note.id).unwrap().body, "hand-edited-a2");
    assert!(
        reloaded
            .recall("hand-edited-a2")
            .unwrap()
            .iter()
            .any(|hit| hit.pointer == note.path)
    );
}

#[test]
fn a3_seat_authority_and_pin_budgets() {
    let universe = Universe::new("a3");
    let mut memory = universe.memory();
    let seat = memory
        .remember(
            MemoryActor::Seat(1),
            1,
            None,
            "seat",
            "ordinary write",
            false,
            false,
        )
        .unwrap();
    assert!(!seat.pinned);
    assert!(
        memory
            .remember(MemoryActor::Seat(1), 1, None, "bad", "pin", true, false)
            .is_err()
    );
    assert!(memory.forget(MemoryActor::Seat(1), &seat.id).is_err());
    assert!(memory.delete_note(MemoryActor::Seat(1), &seat.id).is_err());

    let old = memory
        .remember(
            MemoryActor::Captain,
            1,
            Some("universe"),
            "old-pin",
            &"x".repeat(UNIVERSE_PIN_BYTES - 7),
            true,
            false,
        )
        .unwrap();
    let refused = memory
        .remember(
            MemoryActor::Captain,
            1,
            Some("universe"),
            "later",
            "too much",
            true,
            false,
        )
        .unwrap_err();
    assert!(refused.to_string().contains("retire"));
    assert!(refused.to_string().contains("old-pin"));
    assert!(memory.note(&old.id).unwrap().pinned);
}

#[test]
fn a4_scope_pins_follow_the_active_area() {
    let universe = Universe::new("a4");
    let mut memory = universe.memory();
    memory
        .set_scope(
            MemoryActor::Captain,
            1,
            ActiveScope {
                area: Some("alpha".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let global = memory
        .remember(
            MemoryActor::Captain,
            1,
            Some("universe"),
            "global-a4",
            "global",
            true,
            false,
        )
        .unwrap();
    let alpha = memory
        .remember(
            MemoryActor::Captain,
            1,
            Some("area:alpha"),
            "alpha-a4",
            "alpha",
            true,
            false,
        )
        .unwrap();
    let beta = memory
        .remember(
            MemoryActor::Captain,
            1,
            Some("area:beta"),
            "beta-a4",
            "beta",
            true,
            false,
        )
        .unwrap();

    let hot = memory.hot_set(1).unwrap();
    assert!(hot.contains(&global.id) && hot.contains(&alpha.id) && !hot.contains(&beta.id));
    memory
        .set_scope(
            MemoryActor::Captain,
            1,
            ActiveScope {
                area: Some("beta".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let hot = memory.hot_set(1).unwrap();
    assert!(hot.contains(&global.id) && hot.contains(&beta.id) && !hot.contains(&alpha.id));
}

#[test]
fn a5_cross_record_recall_excludes_flight_journals() {
    let universe = Universe::new("a5");
    let mut memory = universe.memory();
    memory
        .remember(
            MemoryActor::Captain,
            1,
            None,
            "note-a5",
            "cross-record-a5",
            false,
            false,
        )
        .unwrap();

    let session_dir = universe.path().join("sessions/pid-2");
    fs::create_dir_all(&session_dir).unwrap();
    let cold = session_dir.join("transcript.jsonl");
    fs::write(&cold, "{\"text\":\"cross-record-a5\"}\n").unwrap();
    let session = PidSession {
        summary: "cross-record-a5".into(),
        cold_transcript: cold,
        ..Default::default()
    };
    fs::write(
        session_dir.join("session.json"),
        serde_json::to_vec(&session).unwrap(),
    )
    .unwrap();
    let mission = universe.path().join("missions/done-a5.md");
    fs::create_dir_all(mission.parent().unwrap()).unwrap();
    fs::write(&mission, "---\nid: msn_done-a5\nstatus: done\nowner_pid: null\nupdated: 0\n---\n\n# Outcome\ncross-record-a5\n\n# Done check\ncross-record-a5\n\n# Scope\nuniverse\n\n# Inputs\n\n# Draft\n\n# Log\n").unwrap();
    fs::write(universe.path().join("flight-journal.md"), "cross-record-a5").unwrap();

    let hits = memory.recall("cross-record-a5").unwrap();
    assert!(hits.iter().any(|hit| hit.kind == "note"));
    assert!(hits.iter().any(|hit| hit.kind == "summary"));
    assert!(hits.iter().any(|hit| hit.kind == "cold"));
    assert!(hits.iter().any(|hit| hit.kind == "mission"));
    assert!(
        !hits
            .iter()
            .any(|hit| hit.pointer.ends_with("flight-journal.md"))
    );
}

#[test]
fn a6_compaction_redacts_summary_note_and_cold_transcript() {
    let universe = Universe::new("a6");
    let secret = "planted-a6-secret";
    fs::write(universe.path().join(".env"), format!("API_KEY={secret}\n")).unwrap();
    let mut memory = universe.memory();
    memory.append_turn(1, "short tail").unwrap();
    assert!(memory.compaction_job(1).unwrap().is_none());
    memory
        .append_turn(1, &format!("{secret} {}", "x".repeat(TAIL_BYTES)))
        .unwrap();
    let job = memory
        .compaction_job(1)
        .unwrap()
        .expect("tail crossed the compaction threshold");
    memory
        .apply_compaction(
            &job,
            CompactionFold {
                summary: format!("summary {secret}"),
                notes: vec![format!("durable {secret}")],
            },
        )
        .unwrap();

    let summary = memory.session(1).unwrap().summary;
    let note = memory
        .notes()
        .unwrap()
        .into_iter()
        .find(|note| note.source == "compaction:pid:1")
        .unwrap();
    let cold = fs::read_to_string(memory.session(1).unwrap().cold_transcript).unwrap();
    for output in [&summary, &note.body, &cold] {
        assert!(!output.contains(secret));
        assert!(output.contains("[REDACTED]"));
    }
    assert!(!note.pinned);
    let session = memory.session(1).unwrap();
    assert!(!session.tail.is_empty());
    assert!(session.tail.iter().map(String::len).sum::<usize>() <= uke::KEEP_TAIL_BYTES);
}

#[test]
fn a8_admission_floor_and_settling() {
    let universe = Universe::new("a8");
    let mut memory = universe.memory();
    let note = memory
        .remember(
            MemoryActor::Captain,
            1,
            Some("area:bridge"),
            "admit-a8",
            "ready for a mission",
            false,
            true,
        )
        .unwrap();
    assert!(
        memory
            .admit_note(MemoryActor::Captain, &note.id, "", "cargo test proves it")
            .is_err()
    );
    assert!(memory.note(&note.id).unwrap().settled.is_none());

    let mission = memory
        .admit_note(
            MemoryActor::Captain,
            &note.id,
            "A concrete bridge outcome",
            "cargo test -p uke --test memory passes",
        )
        .unwrap();
    let contents = fs::read_to_string(&mission).unwrap();
    assert!(mission.starts_with(memory.root().join("missions")));
    assert!(contents.contains(&note.path.display().to_string()));
    assert!(memory.note(&note.id).unwrap().settled.is_some());
}
