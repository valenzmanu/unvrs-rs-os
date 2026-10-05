//! Pluggable skill pools (`git` / `path`) and their catalogue (uke-design §13).
//!
//! A pools file is TOML: repeated `[[pools]]` tables plus an optional
//! `[overlay.<pool>.<skill_id>]` table that overrides inferred metadata.
//!
//! ```toml
//! [[pools]]
//! id = "mattpocock"
//! kind = "git"
//! uri = "https://github.com/mattpocock/skills"
//! ref = "c55ee46073ed923f86ce59a5eb3b6d895095d1b7"
//! root = "skills"
//!
//! [overlay.mattpocock.tdd]
//! needs_captain = false
//! summary = "Test-driven development."
//! ```
use crate::signals::CommandTracking;
use crate::skill::SkillEntry;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

/// Where a pool's skills come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoolKind {
    /// Cloned and pinned by `ref` under the pool cache.
    Git,
    /// A directory, absolute or relative to the pools file.
    Path,
}

/// One pool declaration from `[[pools]]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolConfig {
    pub id: String,
    pub kind: PoolKind,
    /// Clone URL (`git`) or directory (`path`).
    pub uri: String,
    /// Pinned commit sha, tag or branch. Required for `git` pools.
    #[serde(default, rename = "ref")]
    pub r#ref: Option<String>,
    /// Sub-directory of the pool holding skill directories.
    #[serde(default)]
    pub root: String,
    /// Directory of the pools file; `path` uris resolve against it.
    #[serde(default, skip)]
    pub base: PathBuf,
}

/// Captain-supplied metadata that beats inference.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OverlayEntry {
    pub needs_captain: Option<bool>,
    pub summary: Option<String>,
}

/// `overlay.<pool>.<skill_id>`.
pub type Overlay = BTreeMap<String, BTreeMap<String, OverlayEntry>>;

/// A parsed pools file.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PoolsFile {
    #[serde(default)]
    pub pools: Vec<PoolConfig>,
    #[serde(default)]
    pub overlay: Overlay,
}

impl PoolsFile {
    /// Parse `path`; every pool remembers the file's directory as its base.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Pools file unreadable: {}", path.display()))?;
        let mut file: Self = toml::from_str(&text)
            .with_context(|| format!("Pools file invalid TOML: {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut ids = BTreeSet::new();
        for pool in &mut file.pools {
            ensure!(!pool.id.is_empty(), "Pool id must not be empty");
            ensure!(
                ids.insert(pool.id.clone()),
                "Duplicate pool id: {}",
                pool.id
            );
            pool.base = base.clone();
        }
        ensure!(!file.pools.is_empty(), "Pools file declares no pools");
        Ok(file)
    }
}

/// `$UNVRS_POOL_CACHE`, else `~/.cache/unvrs/pools`.
pub fn default_cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("UNVRS_POOL_CACHE") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("HOME unset; set UNVRS_POOL_CACHE")?;
    Ok(PathBuf::from(home).join(".cache/unvrs/pools"))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    let out = cmd.args(args).output_owned().context("git not available")?;
    ensure!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Materialise a pool on disk. `git` pools clone into
/// `<cache_dir>/<id>-<ref>` and `checkout --detach <ref>`; a cache already
/// parked on a full-sha ref touches the network not at all.
pub fn resolve(pool: &PoolConfig, cache_dir: &Path) -> Result<PathBuf> {
    match pool.kind {
        PoolKind::Path => {
            let uri = Path::new(&pool.uri);
            let dir = if uri.is_absolute() {
                uri.to_path_buf()
            } else {
                pool.base.join(uri)
            };
            ensure!(
                dir.is_dir(),
                "Pool {} path missing: {}",
                pool.id,
                dir.display()
            );
            Ok(dir)
        }
        PoolKind::Git => {
            let r#ref = pool
                .r#ref
                .as_deref()
                .filter(|r| !r.is_empty())
                .with_context(|| format!("Pool {} is git: pinned ref required", pool.id))?;
            let dir = cache_dir.join(format!("{}-{}", sanitize(&pool.id), sanitize(r#ref)));
            if dir.join(".git").is_dir() {
                let head = git(Some(&dir), &["rev-parse", "HEAD"])?;
                if is_sha(r#ref) && head == r#ref {
                    return Ok(dir);
                }
                git(Some(&dir), &["fetch", "--quiet", "origin"])?;
            } else {
                std::fs::create_dir_all(cache_dir)?;
                git(
                    None,
                    &["clone", "--quiet", &pool.uri, &dir.to_string_lossy()],
                )?;
            }
            git(Some(&dir), &["checkout", "--quiet", "--detach", r#ref])
                .or_else(
                    |e| match git(Some(&dir), &["fetch", "--quiet", "origin", r#ref]) {
                        Ok(_) => git(
                            Some(&dir),
                            &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
                        ),
                        Err(_) => Err(e),
                    },
                )
                .with_context(|| format!("Pool {} cannot check out {}", pool.id, r#ref))?;
            if is_sha(r#ref) {
                let head = git(Some(&dir), &["rev-parse", "HEAD"])?;
                ensure!(
                    head == r#ref,
                    "Pool {} HEAD {head} is not the pinned {}",
                    pool.id,
                    r#ref
                );
            }
            Ok(dir)
        }
    }
}

/// Squash to one line and cap at ~300 chars.
fn one_line(s: &str) -> String {
    let squashed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if squashed.chars().count() <= 300 {
        return squashed;
    }
    squashed.chars().take(299).collect::<String>() + "…"
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        return value[1..value.len() - 1].replace("''", "'");
    }
    if !(bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"') {
        return value.to_string();
    }
    let mut out = String::new();
    let mut chars = value[1..value.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// Frontmatter as flat `key -> scalar`, plus the body after it.
///
/// Hand-rolled: top-level `key: value` pairs only, with `>`/`|` folded blocks
/// and single/double quoted scalars. Nested maps (`metadata:`) are skipped.
fn frontmatter(text: &str) -> (BTreeMap<String, String>, String) {
    let mut keys = BTreeMap::new();
    let rest = match text.strip_prefix("---\n") {
        Some(rest) => rest,
        None => return (keys, text.to_string()),
    };
    let mut lines = rest.lines().peekable();
    let mut consumed = 4;
    let mut closed = false;
    while let Some(line) = lines.next() {
        consumed += line.len() + 1;
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.starts_with(char::is_whitespace) || key.contains(' ') {
            continue;
        }
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if value == ">" || value == "|" || value == ">-" || value == "|-" {
            let mut block: Vec<String> = Vec::new();
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() || next.starts_with(char::is_whitespace) {
                    block.push(next.trim().to_string());
                    consumed += next.len() + 1;
                    lines.next();
                } else {
                    break;
                }
            }
            keys.insert(key, block.join("\n"));
        } else if !value.is_empty() {
            keys.insert(key, unquote(value));
        }
    }
    let body = if closed { &rest[consumed - 4..] } else { rest };
    (keys, body.to_string())
}

/// Phrases that mark a skill as an interview with the human.
const CAPTAIN_PHRASES: [&str; 10] = [
    "grill",
    "interview",
    "ask the user questions",
    "ask you questions",
    "one question at a time",
    "questionnaire",
    "walks a human",
    "walk a human",
    "teach the user",
    "interactive wizard",
];

/// Does this skill only make sense with a captain on the channel?
///
/// Upstream (mattpocock/skills) splits skills on *who may invoke them*:
/// user-invoked skills carry `disable-model-invocation: true` in frontmatter
/// (mirrored as `policy.allow_implicit_invocation: false` in
/// `agents/openai.yaml`) and are reachable only when the human types them;
/// everything else is model-invocable. So:
///
/// 1. frontmatter flag: `disable-model-invocation: true`, `user-invocable: true`
///    or `allow-implicit-invocation: false` → needs a captain;
/// 2. otherwise a narrow wording heuristic over description + body
///    ([`CAPTAIN_PHRASES`]: "grill", "interview", "ask the user questions", …)
///    catches interview-shaped skills that forgot the flag (e.g. `grilling`);
/// 3. otherwise false. An `[overlay.<pool>.<id>]` entry beats all of this.
fn needs_captain(keys: &BTreeMap<String, String>, description: &str, body: &str) -> bool {
    let flag = |key: &str| keys.get(key).map(|v| v.trim().to_ascii_lowercase());
    if flag("disable-model-invocation").as_deref() == Some("true")
        || flag("user-invocable").as_deref() == Some("true")
        || flag("allow-implicit-invocation").as_deref() == Some("false")
    {
        return true;
    }
    let haystack = format!(
        "{description}\n{}",
        body.chars().take(4000).collect::<String>()
    )
    .to_ascii_lowercase();
    CAPTAIN_PHRASES.iter().any(|p| haystack.contains(p))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("Pool root unreadable: {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.') || n == "node_modules" || n == "target")
        })
        .collect();
    entries.sort();
    for entry in entries {
        if entry.join("SKILL.md").is_file() {
            out.push(entry);
        } else {
            walk(&entry, out)?;
        }
    }
    Ok(())
}

fn entry(pool: &str, dir: &Path) -> Result<SkillEntry> {
    let path = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Skill unreadable: {}", path.display()))?;
    let (keys, body) = frontmatter(&text);
    let id = keys
        .get("name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| dir.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .with_context(|| format!("Skill has no id: {}", path.display()))?;
    let description = keys.get("description").cloned().unwrap_or_default();
    Ok(SkillEntry {
        pool: pool.to_string(),
        needs_captain: needs_captain(&keys, &description, &body),
        summary: one_line(&description),
        id,
        dir: dir.to_path_buf(),
    })
}

/// Catalogue every pool in `config_path`, sorted by `(pool, id)`.
///
/// Duplicate ids inside one pool keep the first directory in sorted order.
pub fn catalog(config_path: &Path, cache_dir: &Path) -> Result<Vec<SkillEntry>> {
    let file = PoolsFile::load(config_path)?;
    let mut out: Vec<SkillEntry> = Vec::new();
    for pool in &file.pools {
        let dir = resolve(pool, cache_dir)?;
        let root = dir.join(pool.root.trim_start_matches('/'));
        let mut dirs = Vec::new();
        walk(&root, &mut dirs)?;
        let mut seen = BTreeSet::new();
        for skill_dir in dirs {
            let mut entry = entry(&pool.id, &skill_dir)?;
            if !seen.insert(entry.id.clone()) {
                continue;
            }
            if let Some(over) = file.overlay.get(&pool.id).and_then(|p| p.get(&entry.id)) {
                if let Some(needs) = over.needs_captain {
                    entry.needs_captain = needs;
                }
                if let Some(summary) = &over.summary {
                    entry.summary = one_line(summary);
                }
            }
            out.push(entry);
        }
    }
    ensure!(
        !out.is_empty(),
        "Pools yielded no skills: {}",
        config_path.display()
    );
    out.sort_by(|a, b| (&a.pool, &a.id).cmp(&(&b.pool, &b.id)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(dir: &Path, name: &str, frontmatter: &str, body: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(
            dir.join(name).join("SKILL.md"),
            format!("---\n{frontmatter}---\n{body}\n"),
        )
        .unwrap();
    }

    /// A temp `path` pool: three skills, one nested, one duplicate id.
    fn fixture(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("uke-pool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let pool = base.join("pool");
        skill(
            &pool,
            "tdd-lite",
            "name: tdd-lite\ndescription: >\n  Test-driven development.\n  Red, green, refactor.\n",
            "# TDD\nWrite the test first.\n",
        );
        skill(
            &pool,
            "grill-captain",
            "name: grill-captain\ndescription: \"A relentless interview: sharpen the plan.\"\ndisable-model-invocation: true\n",
            "# Grill\n",
        );
        std::fs::create_dir_all(pool.join("nested")).unwrap();
        skill(
            &pool.join("nested"),
            "bug-hunt",
            "name: bug-hunt\ndescription: Diagnose a failing test.\nmetadata:\n  credits: none\n",
            "# Hunt\n",
        );
        // Duplicate id, later in sort order: dropped.
        skill(
            &pool.join("zzz"),
            "copy",
            "name: tdd-lite\ndescription: Shadow copy.\n",
            "# Dupe\n",
        );
        std::fs::write(
            base.join("pools.toml"),
            "[[pools]]\nid = \"local\"\nkind = \"path\"\nuri = \"pool\"\nroot = \"\"\n\n[overlay.local.\"bug-hunt\"]\nsummary = \"Overlaid summary.\"\nneeds_captain = true\n",
        )
        .unwrap();
        base
    }

    #[test]
    fn catalogs_a_path_pool() {
        let base = fixture("catalog");
        let skills = catalog(&base.join("pools.toml"), &base.join("cache")).unwrap();
        assert_eq!(
            skills.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["bug-hunt", "grill-captain", "tdd-lite"]
        );
        // Folded description squashed to one line.
        assert_eq!(
            skills[2].summary,
            "Test-driven development. Red, green, refactor."
        );
        assert!(!skills[2].needs_captain);
        // Flag wins; quoted scalar unquoted.
        assert!(skills[1].needs_captain);
        assert_eq!(
            skills[1].summary,
            "A relentless interview: sharpen the plan."
        );
        // Overlay beats inference.
        assert!(skills[0].needs_captain);
        assert_eq!(skills[0].summary, "Overlaid summary.");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn infers_captain_from_wording_and_rejects_unpinned_git() {
        let keys = BTreeMap::from([("name".into(), "grilling".into())]);
        assert!(needs_captain(
            &keys,
            "Grill the user relentlessly about a plan.",
            ""
        ));
        assert!(!needs_captain(
            &keys,
            "Test-driven development. Use when the user wants tests first.",
            "Write a failing test."
        ));
        let pool = PoolConfig {
            id: "up".into(),
            kind: PoolKind::Git,
            uri: "https://example.invalid/x".into(),
            r#ref: None,
            root: String::new(),
            base: PathBuf::new(),
        };
        assert!(
            resolve(&pool, Path::new("/nonexistent"))
                .unwrap_err()
                .to_string()
                .contains("pinned ref required")
        );
    }

    #[test]
    fn caps_long_summaries() {
        let long = "word ".repeat(200);
        assert_eq!(one_line(&long).chars().count(), 300);
    }
}
