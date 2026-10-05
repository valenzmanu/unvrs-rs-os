//! uKe core. Wire helpers shared by every driver live here; missions, pools and the
//! DrvBoot stub are modules of this crate until a second adapter earns them a seam.
mod boot;
mod brief;
mod dotenv;
pub mod drv_econ;
mod mission;
mod pool;
pub mod settings;
pub mod signals;
mod skill;

use anyhow::Result;
use serde_json::Value;
use std::io::Write;

pub fn write_json(w: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *w, value)?;
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
/// Splits a command line with simple quoting ("…", '…'), for the MCP tool and ctl.
pub fn shell_words(s: &str) -> anyhow::Result<Vec<String>> {
    kernel::words(s)
}
pub fn scalar(s: &str) -> String {
    serde_json::to_string(&clean(s)).expect("string serialization")
}

mod ctx;
mod memory;
pub use ctx::*;
mod knowledge;
mod obs;
pub use knowledge::{Filed, Note as MemNote, Store as MemStore, Tier};
pub use memory::*;
pub use obs::*;

mod kernel;
pub use kernel::{
    Away, BUILD_COMMIT, BUILD_REF, Captain, Driven, Drivers, EFFORTS, HOT_BYTES, Hold,
    KERNEL_VERSION, MAPP, Mail, Mapp, ObservatoryStart, PidRec, SettingsFn, SilentMapp, SnapshotFn,
    State, ThreadRec, TurnRequest, TurnResult, Universe, Wake, ensure_running, git_checkpoint,
    now_ms, parse_captain, procinfo, request as kernel_request, running as kernel_running,
    serve as serve_kernel,
};

pub use boot::*;
pub use brief::*;
pub use dotenv::{lookup as dotenv_lookup, parse as parse_dotenv};
pub use mission::*;
pub use pool::*;
pub use skill::*;
