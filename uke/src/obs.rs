//! Crew and bounded bus-tail view state (folded from DrvObs in 0.8) and the
//! append-only JSONL journal for Boot picks and install results.
use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::{Result, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Appends `{"at": <unix seconds>, "kind": kind, ...fields}` as one line; creates parent dirs.
pub fn append_journal(path: &Path, kind: &str, fields: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut event = serde_json::Map::new();
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    event.insert("at".into(), at.into());
    event.insert("kind".into(), kind.into());
    if let Some(fields) = fields.as_object() {
        event.extend(fields.clone());
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", Value::Object(event))
}

use std::collections::VecDeque;

/// What Obs needs to read from a crew seat; the seat record itself stays with its owner.
pub trait CrewSeat {
    fn state(&self) -> &str;
    fn mission(&self) -> &str;
}

/// A handoff record the crew view can follow to its receiving PID.
pub trait HandoffTarget {
    fn target_pid(&self) -> Option<usize>;
}

pub struct Obs<A, H> {
    pub crew: Vec<A>,
    pub tail: VecDeque<String>,
    pub handoffs: Vec<H>,
}
impl<A, H> Default for Obs<A, H> {
    fn default() -> Self {
        Self {
            crew: Vec::new(),
            tail: VecDeque::new(),
            handoffs: Vec::new(),
        }
    }
}
impl<A: CrewSeat, H: HandoffTarget> Obs<A, H> {
    pub fn focus(&self) -> &str {
        self.handoffs
            .iter()
            .rev()
            .filter_map(|s| s.target_pid().and_then(|pid| self.crew.get(pid - 1)))
            .chain(self.crew.iter())
            .find(|a| a.state() == "working")
            .map(|a| a.mission())
            .unwrap_or("YOUR INTENT")
    }
    pub fn working(&self) -> usize {
        self.crew.iter().filter(|a| a.state() == "working").count()
    }
}
pub fn glyph(state: &str) -> &str {
    match state {
        "working" => "◆",
        "idle" => "○",
        "blocked" => "▲",
        "offline" => "✕",
        _ => "◌",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn appends_one_line_per_event() {
        let path = std::env::temp_dir().join(format!("unvrs-journal-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        for n in 0..2 {
            super::append_journal(&path, "boot.pick", &serde_json::json!({"n": n})).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"kind\":\"boot.pick\""));
    }
}
