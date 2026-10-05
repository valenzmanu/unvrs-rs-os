//! Flight session snapshot: quit/reopen in the same root restores the latest flight state.
//! The crew itself lives in the kernel; this file keeps the console's view and preferences.
use anyhow::Result;
use drv_hdff::HandoffSummary;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct Seat {
    pub parent: usize,
    pub rank: u8,
    pub name: String,
    pub mission: String,
    pub state: String,
    pub model: String,
    pub harness: String,
    pub effort: String,
    pub transcript: String,
    /// Kept for the file format; the kernel owns mailboxes now.
    #[serde(default)]
    pub queue: Vec<serde_json::Value>,
    pub unread: bool,
    pub needs_captain: bool,
    pub turn_active: bool,
}
#[derive(Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub saved_at: u64,
    pub clock: u64,
    pub selected: usize,
    pub input: String,
    pub mission_input: bool,
    pub motion: bool,
    pub expanded: bool,
    pub telemetry: bool,
    pub logs: bool,
    pub log_filter: Option<String>,
    pub log_query: String,
    /// DrvBoot picker backend (`rules` | `jev`); absent in pre-0.5 sessions.
    #[serde(default)]
    pub picker: String,
    /// Mission file the cockpit had admitted and selected.
    #[serde(default)]
    pub mission: Option<PathBuf>,
    pub crew: Vec<Seat>,
    pub tail: Vec<String>,
    pub handoffs: Vec<HandoffSummary>,
}

pub fn path(demo: bool) -> PathBuf {
    Path::new(".unvrs").join(if demo {
        "session-demo.json"
    } else {
        "session.json"
    })
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
/// Missing file → None. Unreadable or other-version file is set aside, never deleted.
pub fn load(path: &Path) -> Option<Session> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice::<Session>(&bytes) {
        Ok(session) if session.version == VERSION && !session.crew.is_empty() => Some(session),
        _ => {
            archive(path, "invalid");
            None
        }
    }
}
/// Private (0600) temp file then rename, so a crash never leaves a torn snapshot.
pub fn save(path: &Path, session: &mut Session) -> Result<()> {
    session.version = VERSION;
    session.saved_at = now();
    let tmp = path.with_extension("json.tmp");
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    serde_json::to_writer(&mut file, session)?;
    file.flush()?;
    fs::rename(tmp, path)?;
    Ok(())
}
pub fn archive(path: &Path, label: &str) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let to = path.with_file_name(format!("{stem}-{label}-{}.json", now()));
    fs::rename(path, &to).ok()?;
    // ponytail: keep the five newest set-aside sessions per label; timestamps sort by name.
    let prefix = format!("{stem}-{label}-");
    let mut old = fs::read_dir(path.parent()?)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| {
            p.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    old.sort_by_key(|p| {
        p.file_stem()
            .and_then(|name| name.to_str())
            .and_then(|name| name.rsplit('-').next()?.parse::<u64>().ok())
    });
    for stale in old.iter().rev().skip(5) {
        let _ = fs::remove_file(stale);
    }
    Some(to)
}
