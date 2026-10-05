use drv_agent::{install_memory_skills, native_root, write_hot_region};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uke::{GENERATED_END, GENERATED_START, MemoryIndex};

struct Universe(PathBuf);

impl Universe {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "drv-agent-memory-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
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

/// Harness memory is the harness's own cache (memory-layer.md §10): writing the
/// seat-local hot region keeps the seat's own lines, replaces only the generated
/// block, files no note, and never touches the shared harness stores.
#[test]
fn a7_hot_region_never_imports_or_touches_harness_memory() {
    let universe = Universe::new("a7");
    let mut memory = MemoryIndex::open(universe.path()).unwrap();
    let harness = universe.path().join("sessions/pid-1/harness-memory.md");
    fs::create_dir_all(harness.parent().unwrap()).unwrap();
    fs::write(
        &harness,
        format!("outside-line-a7\n{GENERATED_START}\nold-generated-a7\n{GENERATED_END}\n"),
    )
    .unwrap();
    let home_like = universe.path().join("home-like");
    let claude = home_like.join(".claude/projects/x/memory/MEMORY.md");
    let codex = home_like.join(".codex/memories/a.md");
    for f in [&claude, &codex] {
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, "harness-owned line").unwrap();
    }
    let hot = write_hot_region(universe.path(), 1, "inside-generated-a7").unwrap();
    let rewritten = fs::read_to_string(&hot).unwrap();
    assert!(rewritten.contains("outside-line-a7") && rewritten.contains("inside-generated-a7"));
    assert!(!rewritten.contains("old-generated-a7"));
    assert!(
        memory
            .recall("harness-owned")
            .unwrap()
            .iter()
            .all(|h| h.kind != "note"),
        "nothing from a harness store becomes a note"
    );
    assert_eq!(fs::read_to_string(&claude).unwrap(), "harness-owned line");
    assert_eq!(fs::read_to_string(&codex).unwrap(), "harness-owned line");
}

#[test]
fn a9_all_profiles_install_remember_and_recall_skills() {
    let universe = Universe::new("a9");
    for profile in ["pi", "codex", "claude", "cursor"] {
        install_memory_skills(profile, universe.path()).unwrap();
        let root = native_root(profile, universe.path()).unwrap();
        let remember = root.join("remember/SKILL.md");
        let recall = root.join("recall/SKILL.md");
        assert!(remember.is_file(), "{}", remember.display());
        assert!(recall.is_file(), "{}", recall.display());
        assert!(
            fs::read_to_string(recall)
                .unwrap()
                .contains("Before answering that you do not know or do not remember")
        );
    }
}
