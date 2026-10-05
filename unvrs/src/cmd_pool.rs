//! `unvrs pool list|resolve`: inspect skill pools and their catalogue (M3).
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};
use uke as pool;
use uke::{scalar, write_json};

const DEFAULT_CONFIG: &str = ".unvrs/skill-pools.toml";
const FALLBACK_CONFIG: &str = "behaviour-eval/fixtures/skill-pools.toml";

/// `--config <toml>`, else `.unvrs/skill-pools.toml`, else the eval fixture.
fn config(args: &[String]) -> Result<PathBuf> {
    let explicit = match args.iter().position(|a| a == "--config") {
        Some(i) => Some(
            args.get(i + 1)
                .context("usage: unvrs pool list --config <toml>")?,
        ),
        None => None,
    };
    config_path(explicit.map(Path::new))
}

/// Shared with `unvrs boot --pools`.
pub fn config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        ensure!(path.is_file(), "Pools file missing: {}", path.display());
        return Ok(path.to_path_buf());
    }
    for candidate in [DEFAULT_CONFIG, FALLBACK_CONFIG] {
        if Path::new(candidate).is_file() {
            return Ok(PathBuf::from(candidate));
        }
    }
    bail!("usage: no pools file; pass a pools <toml> or write {DEFAULT_CONFIG}")
}

pub fn run(args: &[String]) -> Result<()> {
    ensure!(
        args.iter()
            .all(|a| !a.starts_with("--") || matches!(a.as_str(), "--config" | "--json")),
        "usage: unvrs pool list [--config <toml>] [--json]"
    );
    let json = args.iter().any(|a| a == "--json");
    let path = config(args)?;
    let cache = pool::default_cache_dir()?;
    match args.first().map(String::as_str) {
        Some("list") => {
            let skills = pool::catalog(&path, &cache)?;
            if json {
                let value = serde_json::to_value(&skills)?;
                return write_json(&mut std::io::stdout(), &value);
            }
            println!("config: {}", scalar(&path.display().to_string()));
            println!("cache: {}", scalar(&cache.display().to_string()));
            println!("skills[{}]{{pool,id,needs_captain,summary}}:", skills.len());
            for skill in &skills {
                println!(
                    "  {},{},{},{}",
                    skill.pool,
                    skill.id,
                    skill.needs_captain,
                    scalar(&skill.summary)
                );
            }
            println!(
                "needs_captain: {}",
                skills.iter().filter(|s| s.needs_captain).count()
            );
        }
        Some("resolve") => {
            let file = pool::PoolsFile::load(&path)?;
            let mut rows = Vec::new();
            for config in &file.pools {
                let dir = pool::resolve(config, &cache)?;
                rows.push((config, dir));
            }
            if json {
                let value = serde_json::Value::Array(
                    rows.iter()
                        .map(|(config, dir)| {
                            serde_json::json!({
                                "id": config.id,
                                "kind": config.kind,
                                "ref": config.r#ref,
                                "root": config.root,
                                "dir": dir.display().to_string(),
                            })
                        })
                        .collect(),
                );
                return write_json(&mut std::io::stdout(), &value);
            }
            println!("pools[{}]{{id,kind,ref,dir}}:", rows.len());
            for (config, dir) in &rows {
                println!(
                    "  {},{},{},{}",
                    config.id,
                    format!("{:?}", config.kind).to_lowercase(),
                    config.r#ref.as_deref().unwrap_or("-"),
                    scalar(&dir.display().to_string())
                );
            }
        }
        _ => bail!("usage: unvrs pool list|resolve [--config <toml>] [--json]"),
    }
    Ok(())
}
