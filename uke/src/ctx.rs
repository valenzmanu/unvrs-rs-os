//! Context sources (context-sources.md §1–§3): the registry (`<home>/sources.toml`), the
//! built-in `path`, `git` and `memory` adapters, and the read API (`map`, `search`, `fetch`).
//! Read-only; content is returned exactly as stored and is data, never instruction (D50).
//! Access is decided by the caller (the kernel) and passed in as `SourceScope`s.
use crate::signals::CommandTracking;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub const CTX_MAP_BYTES: usize = 4000;
pub const CTX_GET_BYTES: usize = 12000;
pub const CTX_SNIPPET: usize = 240;

const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
const BIN_PROBE: usize = 8192;
const DEFAULT_K: usize = 8;
const MAX_K: usize = 20;
const DEFAULT_REFRESH_S: u64 = 3600;
const DEFAULT_EXCLUDE: &[&str] = &["**/.git/**", "**/node_modules/**", "**/target/**"];
/// Never returned, whatever the source's sensitivity.
const SECRET_FILES: &[&str] = &[".env", ".env.*", "*.pem", "id_rsa*", "*.key"];
/// The memory source is the UNVRS home; git clones and caches under it are not memory.
const MEMORY_EXCLUDE: &[&str] = &["sources/**", "cache/**"];
const ENTRY_NAMES: &[&str] = &["CONTEXT-MAP.md", "CONTEXT.md", "AGENTS.md", "README.md"];
const MEMORY_PURPOSE: &str = "UNVRS memory: captain and project notes, task results";
const REFRESH_MARK: &str = "unvrs-refreshed";

fn private() -> String {
    "private".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceRec {
    pub id: String,
    /// "path" | "git" | "memory"
    pub kind: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub purpose: String,
    /// Globs relative to the source root: "media/**", "*.png".
    #[serde(default)]
    pub exclude: Vec<String>,
    /// public | private | secret
    #[serde(default = "private")]
    pub sensitivity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// git: "1h", "30m", "15s".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
}

/// Which part of a source a caller may read. `prefix` restricts to paths under it
/// (used for the memory source: an L2 may read only `projects/<its id>/`).
#[derive(Clone, Debug)]
pub struct SourceScope {
    pub id: String,
    pub prefix: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CtxHit {
    pub reference: String,
    pub source: String,
    pub path: String,
    pub title: String,
    pub line: usize,
    pub snippet: String,
    pub version: String,
    pub age_s: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CtxAnswer {
    pub reference: String,
    pub source: String,
    pub path: String,
    pub version: String,
    pub text: String,
    pub bytes: usize,
    pub next: Option<String>,
    pub stale: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct RegFile {
    #[serde(default)]
    source: Vec<SourceRec>,
}

#[derive(Default, Deserialize)]
struct LocalCfg {
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    entry: Vec<String>,
}

pub struct Sources {
    home: PathBuf,
    list: Vec<SourceRec>,
}

fn memory_rec() -> SourceRec {
    SourceRec {
        id: "memory".into(),
        kind: "memory".into(),
        uri: String::new(),
        purpose: MEMORY_PURPOSE.into(),
        exclude: Vec::new(),
        sensitivity: private(),
        branch: None,
        refresh: None,
    }
}

fn read_reg(home: &Path) -> Result<Vec<SourceRec>> {
    let p = home.join("sources.toml");
    match fs::read_to_string(&p) {
        Ok(s) => Ok(toml::from_str::<RegFile>(&s)
            .with_context(|| format!("parse {}", p.display()))?
            .source),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

impl Sources {
    /// `<home>/sources.toml`. Missing file = no registered sources. The built-in `memory`
    /// source is always present unless the file defines one with id "memory".
    pub fn load(home: &Path) -> Result<Self> {
        let mut list = read_reg(home)?;
        if !list.iter().any(|s| s.id == "memory") {
            list.push(memory_rec());
        }
        Ok(Self {
            home: home.to_path_buf(),
            list,
        })
    }

    pub fn list(&self) -> &[SourceRec] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&SourceRec> {
        self.list.iter().find(|s| s.id == id)
    }

    /// Adds or replaces a source by id and rewrites sources.toml.
    pub fn add(home: &Path, rec: SourceRec) -> Result<()> {
        let _lock = crate::settings::write_lock(home)?;
        validate(&rec)?;
        let mut list = read_reg(home)?;
        match list.iter_mut().find(|s| s.id == rec.id) {
            Some(slot) => *slot = rec,
            None => list.push(rec),
        }
        let body = toml::to_string(&RegFile { source: list }).context("serialize sources")?;
        fs::create_dir_all(home)?;
        let p = home.join("sources.toml");
        let tmp = home.join(format!(".sources.toml.tmp-{}", std::process::id()));
        fs::write(
            &tmp,
            format!("# UNVRS context sources (docs/design/context-sources.md §1)\n\n{body}"),
        )?;
        fs::rename(&tmp, &p).with_context(|| format!("write {}", p.display()))?;
        Ok(())
    }

    /// Root directory on disk: path → expanded uri; git → `<home>/sources/<id>/` (cloned on
    /// first use, pulled when older than `refresh`); memory → home.
    pub fn root(&self, id: &str) -> Result<PathBuf> {
        let rec = self
            .get(id)
            .ok_or_else(|| anyhow!("unknown source: {id}"))?;
        match rec.kind.as_str() {
            "path" => Ok(expand(&rec.uri)),
            "memory" => Ok(self.home.clone()),
            "git" => self.git_root(rec),
            k => bail!("source {id}: unsupported kind {k:?}"),
        }
    }

    fn git_root(&self, rec: &SourceRec) -> Result<PathBuf> {
        let parent = self.home.join("sources");
        let dir = parent.join(&rec.id);
        let refresh = match &rec.refresh {
            Some(r) => parse_dur(r)?,
            None => DEFAULT_REFRESH_S,
        };
        if !dir.join(".git").exists() {
            if dir.exists() {
                if fs::read_dir(&dir)?.next().is_none() {
                    fs::remove_dir(&dir)?;
                } else {
                    bail!(
                        "source {}: {} exists but is not a git clone",
                        rec.id,
                        dir.display()
                    );
                }
            }
            fs::create_dir_all(&parent)?;
            let tmp = parent.join(format!(
                ".{}.tmp-{}-{}",
                rec.id,
                std::process::id(),
                now_ns()
            ));
            let mut cmd = Command::new("git");
            cmd.args(["clone", "--quiet", "--depth", "50"]);
            if let Some(b) = &rec.branch {
                cmd.args(["--branch", b]);
            }
            cmd.arg(git_url(&rec.uri))
                .arg(&tmp)
                .env("GIT_TERMINAL_PROMPT", "0");
            let out = cmd.output_owned().context("run git clone")?;
            if !out.status.success() {
                let _ = fs::remove_dir_all(&tmp);
                bail!(
                    "source {}: git clone {} failed: {}",
                    rec.id,
                    rec.uri,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            if let Err(e) = fs::rename(&tmp, &dir) {
                let _ = fs::remove_dir_all(&tmp);
                if !dir.join(".git").exists() {
                    return Err(e).with_context(|| format!("install clone {}", dir.display()));
                }
            }
            mark_refreshed(&dir);
        } else if now_s().saturating_sub(last_refresh(&dir)) >= refresh {
            // Offline or diverged: keep answering from the clone we have.
            let _ = run_git(&dir, &["pull", "--ff-only", "--quiet"]);
            mark_refreshed(&dir);
        }
        Ok(dir)
    }

    fn open(&self, id: &str) -> Result<Src> {
        let rec = self
            .get(id)
            .ok_or_else(|| anyhow!("unknown source: {id}"))?;
        let raw = self.root(id)?;
        let root = raw
            .canonicalize()
            .with_context(|| format!("source {id}: root {} not found", raw.display()))?;
        let cfg_path = root.join("unvrs-context.toml");
        let cfg: LocalCfg = match fs::read_to_string(&cfg_path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parse {}", cfg_path.display()))?,
            Err(_) => LocalCfg::default(),
        };
        let mut pats: Vec<&str> = DEFAULT_EXCLUDE.to_vec();
        pats.extend_from_slice(SECRET_FILES);
        if rec.kind == "memory" {
            pats.extend_from_slice(MEMORY_EXCLUDE);
        }
        pats.extend(cfg.exclude.iter().map(String::as_str));
        pats.extend(rec.exclude.iter().map(String::as_str));
        Ok(Src {
            id: rec.id.clone(),
            root,
            excl: pats
                .into_iter()
                .flat_map(braces)
                .map(|p| Glob::new(&p))
                .collect(),
            entry: cfg.entry,
            git: None,
        })
    }

    /// Entry points of `<root>/<path>`, else a generated outline; bounded to CTX_MAP_BYTES.
    pub fn map(&self, scope: &SourceScope, path: Option<&str>) -> Result<CtxAnswer> {
        let scopes = std::slice::from_ref(scope);
        let rel = match path {
            Some(p) => norm_rel(p)?,
            None => match &scope.prefix {
                Some(p) => norm_rel(p)?,
                None => String::new(),
            },
        };
        check_scope(scopes, &scope.id, &rel)?;
        let mut src = self.open(&scope.id)?;
        let dir = src.resolve(&rel)?;
        check_scope(scopes, &scope.id, &src.rel_of(&dir))?;
        if !dir.is_dir() {
            bail!("ctx://{}/{rel} is a file; use get", src.id);
        }
        let mut cands: Vec<String> = src
            .entry
            .iter()
            .filter_map(|e| norm_rel(e).ok())
            .filter(|e| !e.is_empty() && under(e, &rel))
            .collect();
        cands.extend(ENTRY_NAMES.iter().map(|n| join_rel(&rel, n)));
        for cand in cands {
            if check_scope(scopes, &scope.id, &cand).is_err() {
                continue;
            }
            let Ok(full) = src.resolve(&cand) else {
                continue;
            };
            if check_scope(scopes, &scope.id, &src.rel_of(&full)).is_err() || !full.is_file() {
                continue;
            }
            let Some((text, mtime)) = read_text(&full)? else {
                continue;
            };
            let version = src.version(&cand, mtime);
            let mut ans = slice(&src.id, &cand, &version, &text, None, CTX_MAP_BYTES)?;
            if let Some(n) = &ans.next {
                if !ans.text.ends_with('\n') {
                    ans.text.push('\n');
                }
                ans.text.push_str(&format!("(more: {n})"));
                ans.bytes = ans.text.len();
            }
            return Ok(ans);
        }
        let text = src.outline(&rel)?;
        let version = src.head().unwrap_or_default();
        Ok(CtxAnswer {
            reference: make_ref(&src.id, &rel, &version, None),
            source: src.id.clone(),
            path: rel,
            version,
            bytes: text.len(),
            text,
            next: None,
            stale: false,
        })
    }

    /// Lexical search over the scopes; every term must appear in the file (content or path).
    pub fn search(&self, query: &str, scopes: &[SourceScope], k: usize) -> Result<Vec<CtxHit>> {
        let mut terms: Vec<String> = Vec::new();
        for t in query.to_lowercase().split_whitespace() {
            if !terms.iter().any(|x| x == t) {
                terms.push(t.to_string());
            }
        }
        if terms.is_empty() {
            bail!("empty query");
        }
        let k = if k == 0 { DEFAULT_K } else { k.min(MAX_K) };
        let mut srcs: Vec<Src> = Vec::new();
        let mut cands: Vec<Cand> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for scope in scopes {
            let si = match srcs.iter().position(|s| s.id == scope.id) {
                Some(i) => i,
                None => {
                    srcs.push(self.open(&scope.id)?);
                    srcs.len() - 1
                }
            };
            let src = &srcs[si];
            let start = match &scope.prefix {
                Some(p) => norm_rel(p)?,
                None => String::new(),
            };
            if !start.is_empty() && (src.excluded_path(&start) || !src.root.join(&start).is_dir()) {
                continue;
            }
            for f in src.walk_files(&start) {
                if !seen.insert((src.id.clone(), f.rel.clone())) {
                    continue;
                }
                if let Some(c) = score_file(si, &f, &terms) {
                    cands.push(c);
                }
            }
        }
        cands.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| srcs[a.src].id.cmp(&srcs[b.src].id))
                .then_with(|| a.rel.cmp(&b.rel))
        });
        cands.truncate(k);
        let now = now_s();
        let mut hits = Vec::with_capacity(cands.len());
        for c in cands {
            let src = &mut srcs[c.src];
            let version = src.version(&c.rel, c.mtime);
            hits.push(CtxHit {
                reference: make_ref(&src.id, &c.rel, &version, None),
                source: src.id.clone(),
                path: c.rel,
                title: c.title,
                line: c.line,
                snippet: c.snippet,
                version,
                age_s: now.saturating_sub(c.mtime),
            });
        }
        Ok(hits)
    }

    /// Reads a ref, bounded to CTX_GET_BYTES; `lines` overrides the ref's #L range.
    pub fn fetch(
        &self,
        reference: &str,
        lines: Option<(usize, usize)>,
        scopes: &[SourceScope],
    ) -> Result<CtxAnswer> {
        let (sid, path, want, range) = parse_ref(reference)?;
        let rel = norm_rel(&path)?;
        if rel.is_empty() {
            bail!("{reference} names no file; use map");
        }
        check_scope(scopes, &sid, &rel)?;
        let mut src = self.open(&sid)?;
        let full = src.resolve(&rel)?;
        check_scope(scopes, &sid, &src.rel_of(&full))?;
        if full.is_dir() {
            bail!("{reference} is a directory; use map");
        }
        let (text, mtime) =
            read_text(&full)?.ok_or_else(|| anyhow!("{reference} is a binary file"))?;
        let version = src.version(&rel, mtime);
        let mut ans = slice(&sid, &rel, &version, &text, lines.or(range), CTX_GET_BYTES)?;
        ans.stale = want.is_some_and(|v| v != version);
        Ok(ans)
    }
}

// ---- refs ----

/// `ctx://<source>/<path>@<version>#L<a>-<b>` (version and range optional).
/// Returns (source, path, version, line range).
#[allow(clippy::type_complexity)]
pub fn parse_ref(r: &str) -> Result<(String, String, Option<String>, Option<(usize, usize)>)> {
    let rest = r
        .strip_prefix("ctx://")
        .ok_or_else(|| anyhow!("not a ctx ref (want ctx://<source>/<path>): {r}"))?;
    let (rest, range) = match rest.rsplit_once('#') {
        Some((a, frag)) => (a, Some(parse_range(frag, r)?)),
        None => (rest, None),
    };
    let (rest, version) = match rest.rsplit_once('@') {
        Some((a, v)) if !v.is_empty() && !v.contains('/') => (a, Some(v.to_string())),
        _ => (rest, None),
    };
    let (src, path) = rest.split_once('/').unwrap_or((rest, ""));
    if src.is_empty() {
        bail!("ctx ref without a source: {r}");
    }
    Ok((src.to_string(), path.to_string(), version, range))
}

fn parse_range(frag: &str, r: &str) -> Result<(usize, usize)> {
    let bad = || anyhow!("bad line range in {r} (want #L<a>-<b>)");
    let f = frag.strip_prefix('L').ok_or_else(bad)?;
    let (a, b) = match f.split_once('-') {
        Some((a, b)) => (a, b.trim_start_matches('L')),
        None => (f, f),
    };
    let a: usize = a.parse().map_err(|_| bad())?;
    let b: usize = b.parse().map_err(|_| bad())?;
    if a == 0 || b < a {
        return Err(bad());
    }
    Ok((a, b))
}

fn make_ref(source: &str, path: &str, version: &str, range: Option<(usize, usize)>) -> String {
    let mut s = format!("ctx://{source}/{path}");
    if !version.is_empty() {
        s.push('@');
        s.push_str(version);
    }
    if let Some((a, b)) = range {
        s.push_str(&format!("#L{a}-{b}"));
    }
    s
}

/// Bounded line slice of `text`. `range` is 1-based inclusive; without one the whole file.
fn slice(
    sid: &str,
    rel: &str,
    version: &str,
    text: &str,
    range: Option<(usize, usize)>,
    cap: usize,
) -> Result<CtxAnswer> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let n = lines.len();
    let (a, b) = match range {
        Some((a, b)) => {
            if a == 0 || b < a {
                bail!("bad line range {a}-{b}");
            }
            if a > n.max(1) {
                bail!("line {a} is past the end of ctx://{sid}/{rel} ({n} lines)");
            }
            (a, b.min(n))
        }
        None => (1, n),
    };
    let mut out = String::new();
    let mut last = a.saturating_sub(1);
    for (i, line) in lines.iter().enumerate().take(b).skip(a - 1) {
        if out.len() + line.len() > cap {
            if out.is_empty() {
                // A single line larger than the cap: return its head.
                let mut end = cap;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&line[..end]);
                last = i + 1;
            }
            break;
        }
        out.push_str(line);
        last = i + 1;
    }
    let next = (last < b).then(|| make_ref(sid, rel, version, Some((last + 1, b))));
    let shown = if range.is_some() || next.is_some() {
        Some((a, last.max(a)))
    } else {
        None
    };
    Ok(CtxAnswer {
        reference: make_ref(sid, rel, version, shown),
        source: sid.to_string(),
        path: rel.to_string(),
        version: version.to_string(),
        bytes: out.len(),
        text: out,
        next,
        stale: false,
    })
}

// ---- access and paths ----

fn check_scope(scopes: &[SourceScope], id: &str, rel: &str) -> Result<()> {
    let mut known = false;
    for s in scopes.iter().filter(|s| s.id == id) {
        known = true;
        match &s.prefix {
            None => return Ok(()),
            Some(p) => {
                if under(rel, &norm_rel(p)?) {
                    return Ok(());
                }
            }
        }
    }
    if known {
        bail!("Refused: ctx://{id}/{rel} is outside the part of source {id} this caller may read")
    }
    bail!("Refused: source {id} is not in this caller's scope")
}

/// `rel` is `base` or below it ("" is the root).
fn under(rel: &str, base: &str) -> bool {
    base.is_empty()
        || rel == base
        || (rel.len() > base.len() && rel.starts_with(base) && rel.as_bytes()[base.len()] == b'/')
}

fn join_rel(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}/{name}")
    }
}

/// Relative, normalized, never escaping the root.
fn norm_rel(p: &str) -> Result<String> {
    if p.starts_with('/') || p.starts_with('\\') || p.contains('\0') {
        bail!("Refused: absolute path {p:?}; paths are relative to the source root");
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => bail!("Refused: path {p:?} escapes the source root"),
            s => out.push(s),
        }
    }
    Ok(out.join("/"))
}

fn expand(uri: &str) -> PathBuf {
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if uri == "~" {
        home()
    } else if let Some(r) = uri.strip_prefix("~/") {
        home().join(r)
    } else {
        PathBuf::from(uri)
    }
}

fn git_url(uri: &str) -> String {
    if uri.contains("://") || uri.starts_with("git@") {
        return uri.to_string();
    }
    if uri.starts_with('/') || uri.starts_with('~') || uri.starts_with('.') {
        return expand(uri).to_string_lossy().into_owned();
    }
    let host = uri.split('/').next().unwrap_or("");
    if host.contains('.') && !Path::new(uri).exists() {
        return format!("https://{uri}");
    }
    uri.to_string()
}

pub(crate) fn validate(rec: &SourceRec) -> Result<()> {
    let id_ok = !rec.id.is_empty()
        && rec.id.len() <= 40
        && rec
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !id_ok {
        bail!("source id {:?} must be [a-z0-9-], 1-40 chars", rec.id);
    }
    if !matches!(rec.sensitivity.as_str(), "public" | "private" | "secret") {
        bail!(
            "source {}: sensitivity {:?} must be public, private or secret",
            rec.id,
            rec.sensitivity
        );
    }
    match rec.kind.as_str() {
        "memory" => {}
        "path" | "git" if rec.uri.trim().is_empty() => {
            bail!("source {}: kind {} needs a uri", rec.id, rec.kind)
        }
        "path" => {
            let p = expand(&rec.uri);
            if !p.exists() {
                bail!("source {}: path {} does not exist", rec.id, p.display());
            }
        }
        "git" => {
            if let Some(r) = &rec.refresh {
                parse_dur(r)?;
            }
        }
        k => bail!("source {}: unknown kind {k:?} (path, git, memory)", rec.id),
    }
    Ok(())
}

fn parse_dur(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last() {
        Some((i, 's')) => (&s[..i], 1),
        Some((i, 'm')) => (&s[..i], 60),
        Some((i, 'h')) => (&s[..i], 3600),
        Some((i, 'd')) => (&s[..i], 86400),
        _ => (s, 1),
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow!("bad refresh {s:?} (want 15s, 30m, 1h, 1d)"))?;
    n.checked_mul(mult)
        .ok_or_else(|| anyhow!("refresh {s:?} is too large"))
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn mtime_s(md: &fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mark_refreshed(dir: &Path) {
    let _ = fs::write(dir.join(".git").join(REFRESH_MARK), now_s().to_string());
}

fn last_refresh(dir: &Path) -> u64 {
    fs::read_to_string(dir.join(".git").join(REFRESH_MARK))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output_owned()
        .context("run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Text and mtime of a file, or None when it is binary (NUL in the first 8 KiB).
fn read_text(p: &Path) -> Result<Option<(String, u64)>> {
    let md = fs::metadata(p).with_context(|| format!("stat {}", p.display()))?;
    let bytes = fs::read(p).with_context(|| format!("read {}", p.display()))?;
    if bytes[..bytes.len().min(BIN_PROBE)].contains(&0) {
        return Ok(None);
    }
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Ok(Some((text, mtime_s(&md))))
}

// ---- globs ----

/// `a{b,c}d` → `abd`, `acd` (every group, left to right; no nesting). A pattern without a
/// closed group is returned as is.
fn braces(p: &str) -> Vec<String> {
    let Some(open) = p.find('{') else {
        return vec![p.to_owned()];
    };
    let Some(len) = p[open..].find('}') else {
        return vec![p.to_owned()];
    };
    let (head, inner, tail) = (&p[..open], &p[open + 1..open + len], &p[open + len + 1..]);
    inner
        .split(',')
        .flat_map(|alt| braces(&format!("{head}{alt}{tail}")))
        .collect()
}

/// `**` any number of segments, `*` within a segment, `?` one char, `{a,b}` alternatives.
/// Matched against the path relative to the root; a pattern without `/` also matches the
/// basename.
struct Glob {
    segs: Vec<Vec<char>>,
    base: bool,
}

impl Glob {
    fn new(p: &str) -> Self {
        let mut p = p.trim().trim_start_matches("./").trim_start_matches('/');
        let owned;
        if p.ends_with('/') {
            owned = format!("{p}**");
            p = &owned;
        }
        Glob {
            base: !p.contains('/'),
            segs: p
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().collect())
                .collect(),
        }
    }

    fn matches(&self, rel: &str) -> bool {
        let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segs_match(&self.segs, &parts) {
            return true;
        }
        self.base && self.segs.len() == 1 && parts.last().is_some_and(|b| wild(&self.segs[0], b))
    }
}

fn segs_match(p: &[Vec<char>], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(h) if h.len() == 2 && h[0] == '*' && h[1] == '*' => {
            (0..=s.len()).any(|i| segs_match(&p[1..], &s[i..]))
        }
        Some(h) => !s.is_empty() && wild(h, s[0]) && segs_match(&p[1..], &s[1..]),
    }
}

fn wild(p: &[char], s: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ---- one source, opened for one call ----

struct GitState {
    head12: String,
    /// Source root relative to the work tree top ("" when the root is the top).
    sub: String,
    tracked: HashSet<String>,
    dirty: HashSet<String>,
}

struct Src {
    id: String,
    /// Canonical.
    root: PathBuf,
    excl: Vec<Glob>,
    entry: Vec<String>,
    /// Computed at most once per call.
    git: Option<Option<GitState>>,
}

struct FileEnt {
    rel: String,
    path: PathBuf,
    len: u64,
    mtime: u64,
}

struct Cand {
    src: usize,
    rel: String,
    score: u64,
    title: String,
    line: usize,
    snippet: String,
    mtime: u64,
}

impl Src {
    fn excluded(&self, rel: &str) -> bool {
        self.excl.iter().any(|g| g.matches(rel))
    }

    /// The path or any of its ancestors is excluded.
    fn excluded_path(&self, rel: &str) -> bool {
        let mut acc = String::new();
        for seg in rel.split('/').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            if self.excluded(&acc) {
                return true;
            }
        }
        false
    }

    fn rel_of(&self, full: &Path) -> String {
        full.strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    }

    /// Canonical path of `rel`, inside the root and not excluded (by name or by target).
    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let full = self
            .root
            .join(rel)
            .canonicalize()
            .with_context(|| format!("not found: ctx://{}/{rel}", self.id))?;
        if !full.starts_with(&self.root) {
            bail!(
                "Refused: ctx://{}/{rel} resolves outside the source root",
                self.id
            );
        }
        if self.excluded_path(rel) || self.excluded_path(&self.rel_of(&full)) {
            bail!("Refused: ctx://{}/{rel} is excluded", self.id);
        }
        Ok(full)
    }

    fn git(&mut self) -> Option<&GitState> {
        if self.git.is_none() {
            self.git = Some(load_git(&self.root));
        }
        self.git.as_ref().and_then(|g| g.as_ref())
    }

    fn head(&mut self) -> Option<String> {
        self.git().map(|g| g.head12.clone())
    }

    fn version(&mut self, rel: &str, mtime: u64) -> String {
        match self.git() {
            Some(g) => {
                let key = join_rel(&g.sub, rel);
                if g.tracked.contains(&key) && !g.dirty.contains(&key) {
                    g.head12.clone()
                } else {
                    format!("{}+m{mtime}", g.head12)
                }
            }
            None => format!("m{mtime}"),
        }
    }

    /// Regular files under `start`, honoring excludes; symlinks are not followed.
    fn walk_files(&self, start: &str) -> Vec<FileEnt> {
        let mut out = Vec::new();
        let mut stack = vec![start.to_string()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(self.root.join(&dir)) else {
                continue;
            };
            for ent in rd.flatten() {
                let Ok(name) = ent.file_name().into_string() else {
                    continue;
                };
                let rel = join_rel(&dir, &name);
                let Ok(ft) = ent.file_type() else { continue };
                if ft.is_symlink() || self.excluded(&rel) {
                    continue;
                }
                if ft.is_dir() {
                    stack.push(rel);
                } else if ft.is_file() {
                    let Ok(md) = ent.metadata() else { continue };
                    out.push(FileEnt {
                        rel,
                        path: ent.path(),
                        len: md.len(),
                        mtime: mtime_s(&md),
                    });
                    if out.len() >= MAX_FILES {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Sorted (name, is_dir) children of `rel`, honoring excludes, no symlinks.
    fn children(&self, rel: &str) -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = match fs::read_dir(self.root.join(rel)) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let ft = e.file_type().ok()?;
                    if ft.is_symlink() || self.excluded(&join_rel(rel, &name)) {
                        return None;
                    }
                    Some((name, ft.is_dir()))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Dirs and files two levels deep, bounded to CTX_MAP_BYTES.
    fn outline(&self, rel: &str) -> Result<String> {
        let mut lines = vec![format!(
            "{}/ (no entry file; outline of ctx://{}/{rel})",
            if rel.is_empty() { "." } else { rel },
            self.id
        )];
        for (name, is_dir) in self.children(rel) {
            if is_dir {
                lines.push(format!("  {name}/"));
                for (sub, sub_dir) in self.children(&join_rel(rel, &name)) {
                    lines.push(format!("    {sub}{}", if sub_dir { "/" } else { "" }));
                }
            } else {
                lines.push(format!("  {name}"));
            }
        }
        let mut out = String::new();
        let budget = CTX_MAP_BYTES - 80;
        for (i, l) in lines.iter().enumerate() {
            if out.len() + l.len() + 1 > budget {
                out.push_str(&format!(
                    "(more: {} entries not shown; map a subdirectory)\n",
                    lines.len() - i
                ));
                break;
            }
            out.push_str(l);
            out.push('\n');
        }
        Ok(out)
    }
}

fn load_git(root: &Path) -> Option<GitState> {
    let out = run_git(root, &["rev-parse", "--show-toplevel", "HEAD"]).ok()?;
    let mut it = out.lines();
    let top = PathBuf::from(it.next()?).canonicalize().ok()?;
    let head = it.next()?.trim();
    if head.len() < 12 {
        return None;
    }
    let sub = root
        .strip_prefix(&top)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let tracked = run_git(&top, &["ls-files", "-z"])
        .ok()?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let status = run_git(
        &top,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )
    .ok()?;
    let mut dirty = HashSet::new();
    let mut parts = status.split('\0');
    while let Some(e) = parts.next() {
        if e.len() < 4 {
            continue;
        }
        let code = &e[..2];
        dirty.insert(e[3..].to_string());
        if code.starts_with('R') || code.starts_with('C') {
            parts.next();
        }
    }
    Some(GitState {
        head12: head[..12].to_string(),
        sub,
        tracked,
        dirty,
    })
}

/// Reads and scores one file; None when it does not match every term (or is skipped).
fn score_file(src: usize, f: &FileEnt, terms: &[String]) -> Option<Cand> {
    if f.len > MAX_FILE {
        return None;
    }
    let bytes = fs::read(&f.path).ok()?;
    if bytes[..bytes.len().min(BIN_PROBE)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let low = text.to_lowercase();
    let rel_low = f.rel.to_lowercase();
    let mut score = 0u64;
    let mut counts = Vec::with_capacity(terms.len());
    for t in terms {
        let n = low.matches(t.as_str()).count();
        let in_path = rel_low.contains(t.as_str());
        if n == 0 && !in_path {
            return None;
        }
        score += (n as u64).min(50);
        if in_path {
            score += 10;
        }
        counts.push(n);
    }
    let name = f.rel.rsplit('/').next().unwrap_or(&f.rel);
    let title = text
        .lines()
        .take(60)
        .find_map(|l| l.trim().strip_prefix("# ").map(|h| h.trim().to_string()))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| name.to_string());
    let title_low = title.to_lowercase();
    score += 5 * terms
        .iter()
        .filter(|t| title_low.contains(t.as_str()))
        .count() as u64;
    // Best line: most distinct terms, then most hits.
    let (mut best, mut best_key) = (0usize, (0usize, 0usize));
    if counts.iter().any(|&n| n > 0) {
        for (i, line) in text.lines().enumerate() {
            let l = line.to_lowercase();
            let mut distinct = 0;
            let mut hits = 0;
            for t in terms {
                let n = l.matches(t.as_str()).count();
                if n > 0 {
                    distinct += 1;
                    hits += n;
                }
            }
            if (distinct, hits) > best_key {
                best_key = (distinct, hits);
                best = i;
            }
        }
    } else {
        best = text.lines().position(|l| !l.trim().is_empty()).unwrap_or(0);
    }
    let line = text.lines().nth(best).unwrap_or("");
    Some(Cand {
        src,
        rel: f.rel.clone(),
        score,
        title,
        line: best + 1,
        snippet: snippet(line, terms),
        mtime: f.mtime,
    })
}

fn snippet(line: &str, terms: &[String]) -> String {
    let line = line.trim();
    if line.chars().count() <= CTX_SNIPPET {
        return line.to_string();
    }
    let low = line.to_lowercase();
    let at = terms
        .iter()
        .filter_map(|t| low.find(t.as_str()))
        .min()
        .map(|b| low[..b].chars().count())
        .unwrap_or(0);
    let start = at.saturating_sub(CTX_SNIPPET / 3);
    line.chars().skip(start).take(CTX_SNIPPET).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brace_globs_expand() {
        assert_eq!(braces("**/*.{png,jpg}"), vec!["**/*.png", "**/*.jpg"]);
        assert_eq!(braces("{a,b}/{c,d}").len(), 4);
        assert_eq!(braces("media/**"), vec!["media/**"]);
        let g: Vec<Glob> = braces("**/*.{png,pdf}")
            .iter()
            .map(|p| Glob::new(p))
            .collect();
        assert!(g.iter().any(|g| g.matches("brand/logo.png")));
        assert!(g.iter().any(|g| g.matches("x/y/z.pdf")));
        assert!(!g.iter().any(|g| g.matches("brand/voice.md")));
    }
}
