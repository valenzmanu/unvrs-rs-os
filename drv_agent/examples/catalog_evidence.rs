//! Capture read-only harness evidence in a caller-supplied sandbox.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path, process::Command, time::Duration,
};
use uke::signals::CommandTracking;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .context("usage: catalog_evidence <sandbox> [--discover]")?;
    let dir = Path::new(&dir);
    fs::create_dir_all(dir)?;
    let _owner = uke::signals::owner_scope(&dir.join("unvrs-home"), 0);
    if args.next().as_deref() == Some("--discover") {
        let catalogs = drv_agent::catalogs(dir);
        fs::write(
            dir.join("catalog-normalized.json"),
            serde_json::to_vec_pretty(&catalogs)?,
        )?;
        for (h, c) in &catalogs {
            ensure!(c.error.is_none(), "{h} discovery failed: {:?}", c.error);
            ensure!(
                c.signed_in == Some(true) && c.quota_available == Some(true),
                "{h} discovery did not establish sign-in and positive quota"
            );
            println!(
                "{h}: {} listed models, signed in, positive quota",
                c.models.len()
            );
        }
        return Ok(());
    }
    let original = drv_agent::Homes::from_env()?;
    let codex = dir.join("codex-home");
    let claude = dir.join("claude-home");
    fs::create_dir_all(&codex)?;
    fs::create_dir_all(&claude)?;
    fs::copy(original.codex.join("auth.json"), codex.join("auth.json"))?;
    let mut cmd = Command::new(std::env::var("UNVRS_CODEX_BIN").unwrap_or_else(|_| "codex".into()));
    cmd.arg("app-server")
        .env("CODEX_HOME", &codex)
        .current_dir(dir);
    let responses = drv_agent::jsonrpc_session(
        cmd,
        json!({"clientInfo":{"name":"unvrs-catalog-probe", "version":"1"},"capabilities":{"experimentalApi":true}}),
        &[
            ("model/list", json!({"limit":100})),
            ("account/read", json!({"refreshToken":false})),
            ("account/rateLimits/read", json!({})),
        ],
        Duration::from_secs(40),
    )?;
    fs::write(
        dir.join("codex-models.json"),
        serde_json::to_vec_pretty(&responses[0])?,
    )?;
    let mut account = responses[1].clone();
    if account["result"]["account"].is_object() {
        account["result"]["account"]["email"] = json!("redacted");
    }
    fs::write(
        dir.join("codex-account.json"),
        serde_json::to_vec_pretty(&account)?,
    )?;
    fs::write(
        dir.join("codex-quota.json"),
        serde_json::to_vec_pretty(&responses[2])?,
    )?;
    // Existing OAuth access only; do not refresh or alter the macOS keychain.
    let credentials = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output_owned()?;
    ensure!(
        credentials.status.success(),
        "Claude credential read failed"
    );
    let mut credentials: Value = serde_json::from_slice(&credentials.stdout)?;
    ensure!(
        credentials["claudeAiOauth"]["accessToken"].is_string(),
        "Claude existing access token missing"
    );
    credentials["claudeAiOauth"]
        .as_object_mut()
        .context("Claude OAuth credentials missing")?
        .remove("refreshToken");
    let credential_path = claude.join(".credentials.json");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&credential_path)?
        .write_all(&serde_json::to_vec(&credentials)?)?;
    let old_config = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .context("HOME missing")?
        .join(".claude.json");
    if let Ok(config) = fs::read(old_config) {
        let config: Value = serde_json::from_slice(&config)?;
        fs::write(
            claude.join(".claude.json"),
            serde_json::to_vec(
                &json!({"oauthAccount":config["oauthAccount"],"hasCompletedOnboarding":true}),
            )?,
        )?;
    }
    let env = vec![
        ("CLAUDE_CONFIG_DIR".into(), claude.display().to_string()),
        (
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
            claude.display().to_string(),
        ),
    ];
    let raw = drv_agent::claude_catalog_raw(dir, &env)?;
    for (name, value) in [
        ("claude-models", &raw["models"]),
        ("claude-usage", &raw["usage"]),
        ("claude-auth", &raw["auth"]),
    ] {
        fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value)?,
        )?;
    }
    println!(
        "Captured catalog/control evidence without sending task prompts: {}",
        dir.display()
    );
    Ok(())
}
