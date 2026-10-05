//! DrvAgent: binds a PID to a harness CPU (pi | codex | claude | cursor).
mod catalog;
mod codex;
mod headless;
mod install;
mod plugin;
mod protocol;
mod seat;

mod memory;
pub use memory::{install_memory_skills, write_hot_region};

pub use catalog::{catalogs, claude_catalog_raw, parse_claude_catalog, parse_codex_catalog};
pub use headless::{
    codex_quota, run_summary, run_summary_with_schema, run_turn, run_turn_observed,
};
pub use install::*;
pub use plugin::{
    HOOK_EVENTS, Homes, MARKETPLACE, MCP_SERVER, PLUGIN, PluginReport, backup, claude_cached,
    claude_control, claude_enabled, claude_fresh, claude_init, claude_installed,
    claude_marketplace_known, codex_app_server, codex_cache_dir, codex_config_without_ours,
    codex_enabled, codex_fresh, codex_installed, codex_marketplace_known, hook_command,
    install_plugin, jsonrpc_session, mcp_json, plugin_id, refresh_claude, refresh_codex,
    refresh_hosts, register_claude, register_codex, render_plugin, render_plugin_checked,
    rendered_version, restore, sh_quote, sha_file, sha256_hex, uninstall_plugin, unregister, which,
};
pub use seat::{Profile, profiles, run};

/// Whether the binary a driven turn on `harness` spawns (UNVRS_<H>_BIN, else the harness
/// name on PATH) exists for this process.
pub fn harness_ready(harness: &str) -> anyhow::Result<()> {
    let (var, default) = match harness {
        "claude" => ("UNVRS_CLAUDE_BIN", "claude"),
        "codex" => ("UNVRS_CODEX_BIN", "codex"),
        other => anyhow::bail!("no driven CPU for harness {other}"),
    };
    let bin = std::env::var(var).unwrap_or_else(|_| default.into());
    let path = std::path::Path::new(&bin);
    let found = if path.components().count() > 1 {
        path.is_file()
    } else {
        which(&bin).is_some()
    };
    anyhow::ensure!(found, "{bin} is not installed or not on the kernel's PATH");
    Ok(())
}
