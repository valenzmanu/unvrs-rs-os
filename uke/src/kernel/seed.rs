//! `seed import <dir>` (captain's terminal only): the captain's context seed, a folder of
//! plain files, becomes sources, projects and memory in one step.
//!
//! * `sources.toml` — `[[source]]` records exactly as in `<home>/sources.toml`;
//! * `projects.toml` — `[[project]] id, purpose, sources, links`;
//! * `captain.md` — bullets → captain memory, pinned;
//! * `projects/<id>/memory.md` — bullets → that project's memory, aging unless clearly
//!   permanent (no date, no open or tentative wording). A bullet may start with
//!   `[pinned]`, `[aging]` or `[perishable]` to say so.
//!
//! A bullet's trailing parenthetical citation is kept as the note's evidence. Importing
//! inspects before it writes, so re-running updates and never duplicates: sources are
//! replaced by id, projects updated in place, known facts reinforced, and seed notes whose
//! bullet left the seed archived (never deleted).
use super::{Inner, Kernel, crew::valid_project_id, now_ms};
use crate::{Filed, MemStore, SourceRec, Sources, Tier};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct SeedSources {
    #[serde(default)]
    source: Vec<SourceRec>,
}

#[derive(Deserialize)]
struct SeedProject {
    id: String,
    purpose: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    links: Vec<String>,
}

#[derive(Deserialize)]
struct SeedProjects {
    #[serde(default)]
    project: Vec<SeedProject>,
}

/// One memory bullet: the fact, where it came from, and an explicit tier if it said one.
#[derive(Debug, PartialEq)]
pub(crate) struct Bullet {
    pub text: String,
    pub cite: Option<String>,
    pub tier: Option<Tier>,
}

/// Top-level `- ` / `* ` bullets of a markdown file; indented lines continue a bullet.
pub(crate) fn bullets(md: &str) -> Vec<Bullet> {
    let mut raw: Vec<String> = vec![];
    let mut open = false;
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            raw.push(rest.trim().to_owned());
            open = true;
        } else if open && line.starts_with([' ', '\t']) && !line.trim().is_empty() {
            let t = line.trim().trim_start_matches(['-', '*']).trim();
            if let Some(last) = raw.last_mut() {
                last.push(' ');
                last.push_str(t);
            }
        } else {
            open = false;
        }
    }
    let mut prev: Option<String> = None;
    raw.into_iter()
        .filter(|r| !r.is_empty())
        .map(|r| {
            let (tier, body) = explicit_tier(&r);
            let (text, cite) = split_cite(body);
            let cite = cite.map(|c| resolve_same(&c, prev.as_deref()));
            if cite.is_some() {
                prev = cite.clone();
            }
            Bullet { text, cite, tier }
        })
        .filter(|b| !b.text.is_empty())
        .collect()
}

fn explicit_tier(s: &str) -> (Option<Tier>, &str) {
    for (mark, tier) in [
        ("[pinned]", Tier::Pinned),
        ("[aging]", Tier::Aging),
        ("[perishable]", Tier::Perishable),
    ] {
        if let Some(rest) = s.strip_prefix(mark) {
            return (Some(tier), rest.trim_start());
        }
    }
    (None, s)
}

/// `Fact. (cite)` → ("Fact.", "cite"). Only a parenthetical that closes the bullet after a
/// finished sentence is a citation; `… (may have changed).` stays in the fact.
fn split_cite(s: &str) -> (String, Option<String>) {
    let t = s.trim_end();
    if !t.ends_with(')') {
        return (t.to_owned(), None);
    }
    let mut depth = 0i32;
    for (i, c) in t.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    let before = t[..i].trim_end();
                    if before.ends_with(['.', ';', ':', '!', '?']) {
                        let cite = t[i + 1..t.len() - 1].replace('`', "").trim().to_owned();
                        return (before.to_owned(), (!cite.is_empty()).then_some(cite));
                    }
                    return (t.to_owned(), None);
                }
            }
            _ => {}
        }
    }
    (t.to_owned(), None)
}

/// "same file" / "same" point at the previous bullet's citation.
fn resolve_same(cite: &str, prev: Option<&str>) -> String {
    let Some(prev) = prev else {
        return cite.to_owned();
    };
    if cite.contains('/') {
        return cite.to_owned();
    }
    for word in ["same file", "same"] {
        if cite == word {
            return prev.to_owned();
        }
        if let Some(head) = cite.strip_suffix(word) {
            return format!("{head}{prev}");
        }
    }
    cite.to_owned()
}

/// Project facts are aging unless clearly permanent: no date, nothing open or tentative.
fn project_tier(text: &str) -> Tier {
    let t = text.to_lowercase();
    let dated = t.as_bytes().windows(5).any(|w| {
        w[0] == b'2' && w[1] == b'0' && w[2..4].iter().all(u8::is_ascii_digit) && w[4] == b'-'
    });
    const TRANSIENT: &[&str] = &[
        "open",
        "status",
        "staged",
        "not yet",
        "pending",
        "proposal",
        "proposed",
        "unconfirmed",
        "unvalidated",
        "consider",
        "idea:",
        "draft",
        "todo",
        "debt",
        "estimate",
        "until",
        "parked",
        "may have changed",
        "currently",
        "under way",
    ];
    if dated || TRANSIENT.iter().any(|w| t.contains(w)) {
        Tier::Aging
    } else {
        Tier::Pinned
    }
}

#[derive(Default)]
struct MemCount {
    new: usize,
    reinforced: usize,
    archived: usize,
}

impl Kernel {
    /// `seed import <dir>`; the caller has already been checked to be the captain.
    pub(crate) fn seed(&self, inner: &mut Inner, args: &[String]) -> Result<String> {
        let dir = match args {
            [sub, dir] if sub == "import" => PathBuf::from(dir),
            _ => bail!("usage: seed import <dir>"),
        };
        ensure!(
            dir.is_dir(),
            "No seed folder at {} (expected sources.toml, projects.toml, captain.md, projects/<id>/memory.md)",
            dir.display()
        );
        let home = self.u.root().to_path_buf();
        let mut out = vec![format!("Seed {}:", dir.display())];
        let mut warn: Vec<String> = vec![];

        // 1. Sources: replaced by id.
        let (mut added, mut updated, mut same) = (vec![], vec![], 0);
        if let Some(text) = read_opt(&dir.join("sources.toml"))? {
            let seed: SeedSources = toml::from_str(&text).context("parse seed sources.toml")?;
            let have = Sources::load(&home)?;
            for rec in seed.source {
                let id = rec.id.clone();
                let before = have.get(&id).cloned();
                if before.as_ref() == Some(&rec) {
                    same += 1;
                    continue;
                }
                match Sources::add(&home, rec) {
                    Ok(()) if before.is_some() => updated.push(id),
                    Ok(()) => added.push(id),
                    Err(e) => warn.push(format!("source {id} skipped: {e:#}")),
                }
            }
        }
        out.push(format!(
            "- sources: {} added, {} updated, {same} unchanged{}",
            added.len(),
            updated.len(),
            list(&added, &updated)
        ));

        // 2. Projects: created with their lead seat, or updated in place.
        let registry = Sources::load(&home)?;
        let (mut created, mut changed, mut kept) = (vec![], vec![], 0);
        if let Some(text) = read_opt(&dir.join("projects.toml"))? {
            let seed: SeedProjects = toml::from_str(&text).context("parse seed projects.toml")?;
            for p in seed.project {
                if let Err(e) = valid_project_id(&p.id) {
                    warn.push(format!("project {:?} skipped: {e:#}", p.id));
                    continue;
                }
                let sources: Vec<String> = p
                    .sources
                    .iter()
                    .filter(|s| {
                        let ok = registry.get(s).is_some();
                        if !ok {
                            warn.push(format!("project {}: unknown source {s} left out", p.id));
                        }
                        ok
                    })
                    .cloned()
                    .collect();
                let path = self.u.project_dir(&p.id).join("project.toml");
                match self.project(&p.id) {
                    Some(mut rec) => {
                        if rec.purpose == p.purpose.trim()
                            && rec.sources == sources
                            && rec.links == p.links
                        {
                            kept += 1;
                            continue;
                        }
                        rec.purpose = p.purpose.trim().to_owned();
                        rec.sources = sources.clone();
                        rec.links = p.links.clone();
                        fs::write(&path, toml::to_string(&rec)?)?;
                        self.event(
                            inner,
                            "project.update",
                            json!({"project": p.id, "sources": sources, "links": p.links, "by": "seed"}),
                        );
                        changed.push(p.id);
                    }
                    None => match self.create_project(inner, &p.id, &p.purpose, &sources, "seed") {
                        Ok(_) => {
                            if !p.links.is_empty()
                                && let Some(mut rec) = self.project(&p.id)
                            {
                                rec.links = p.links.clone();
                                fs::write(&path, toml::to_string(&rec)?)?;
                            }
                            created.push(p.id);
                        }
                        Err(e) => warn.push(format!("project {} skipped: {e:#}", p.id)),
                    },
                }
            }
        }
        out.push(format!(
            "- projects: {} created (each with its lead seat), {} updated, {kept} unchanged{}",
            created.len(),
            changed.len(),
            list(&created, &changed)
        ));

        // 3. Captain memory: pinned.
        let now = now_ms();
        if let Some(text) = read_opt(&dir.join("captain.md"))? {
            let c = file_notes(
                &MemStore::captain(&home),
                &bullets(&text),
                "seed:captain.md",
                |_| Tier::Pinned,
                now,
            )?;
            out.push(format!(
                "- captain memory: {} new, {} already known (reinforced), {} archived (left the seed)",
                c.new, c.reinforced, c.archived
            ));
        }

        // 4. Project memory: aging unless clearly permanent.
        let mut per = vec![];
        let mut dirs: Vec<PathBuf> = fs::read_dir(dir.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("memory.md").is_file())
            .collect();
        dirs.sort();
        for d in dirs {
            let id = d
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if self.project(&id).is_none() {
                warn.push(format!("projects/{id}/memory.md skipped: no project {id}"));
                continue;
            }
            let text = fs::read_to_string(d.join("memory.md"))?;
            let c = file_notes(
                &MemStore::project(&home, &id),
                &bullets(&text),
                &format!("seed:projects/{id}/memory.md"),
                project_tier,
                now,
            )?;
            per.push(format!(
                "{id} {} new/{} known/{} archived",
                c.new, c.reinforced, c.archived
            ));
        }
        if !per.is_empty() {
            out.push(format!("- project memory: {}", per.join(", ")));
        }
        for w in &warn {
            out.push(format!("- warning: {w}"));
        }
        self.event(
            inner,
            "seed.import",
            json!({"dir": dir.display().to_string(), "sources_added": added, "sources_updated": updated,
                   "projects_created": created, "projects_updated": changed, "warnings": warn}),
        );
        Ok(out.join("\n"))
    }
}

/// Files every bullet (inspect, then update or write) and archives this seed file's
/// earlier notes whose bullet is gone.
fn file_notes(
    store: &MemStore,
    items: &[Bullet],
    origin: &str,
    default_tier: impl Fn(&str) -> Tier,
    now: u64,
) -> Result<MemCount> {
    let mut c = MemCount::default();
    let mut touched = BTreeSet::new();
    for b in items {
        let tier = b.tier.unwrap_or_else(|| default_tier(&b.text));
        let evidence = b.cite.clone().unwrap_or_else(|| origin.to_owned());
        let filed = store.remember(&b.text, tier, origin, Some(&evidence), None, now)?;
        match &filed {
            Filed::New(_) => c.new += 1,
            Filed::Reinforced(_) => c.reinforced += 1,
        }
        touched.insert(filed.note().id.clone());
    }
    for n in store.notes()? {
        if n.source == origin && n.status == "live" && !touched.contains(&n.id) {
            store.forget(&n.id)?;
            c.archived += 1;
        }
    }
    Ok(c)
}

fn read_opt(p: &Path) -> Result<Option<String>> {
    match fs::read_to_string(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

fn list(a: &[String], b: &[String]) -> String {
    let all: Vec<&str> = a.iter().chain(b).map(String::as_str).collect();
    if all.is_empty() {
        String::new()
    } else {
        format!(" ({})", all.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullets_split_citations() {
        let md = "# Seed\n\nIntro line.\n\n## A\n\n- Lives near the coast; quotes USD. (`~/x/a.md`, `~/x/b.md`)\n\
- Weekly review Sunday (may have changed). (legacy, same file)\n- No cite here (code, no remote).\n\
- [aging] Wraps\n  onto two lines. (same)\n";
        let b = bullets(md);
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].text, "Lives near the coast; quotes USD.");
        assert_eq!(b[0].cite.as_deref(), Some("~/x/a.md, ~/x/b.md"));
        assert_eq!(b[1].text, "Weekly review Sunday (may have changed).");
        assert_eq!(b[1].cite.as_deref(), Some("legacy, ~/x/a.md, ~/x/b.md"));
        assert_eq!(b[2].text, "No cite here (code, no remote).");
        assert_eq!(b[2].cite, None);
        assert_eq!(b[3].text, "Wraps onto two lines.");
        assert_eq!(b[3].tier, Some(Tier::Aging));
    }

    #[test]
    fn project_tiers() {
        assert_eq!(
            project_tier("Handle @captain. Goal: credibility."),
            Tier::Pinned
        );
        assert_eq!(project_tier("Open: north star, audience."), Tier::Aging);
        assert_eq!(project_tier("Repo moved on 2026-09-26."), Tier::Aging);
    }

    #[test]
    fn refiling_never_duplicates() {
        let dir = std::env::temp_dir().join(format!("unvrs-seed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let s = MemStore::captain(&dir);
        let one =
            bullets("- Alpha fact about boats. (`a.md`)\n- Beta fact about trains. (`b.md`)\n");
        let c = file_notes(&s, &one, "seed:captain.md", |_| Tier::Pinned, 1).unwrap();
        assert_eq!((c.new, c.reinforced, c.archived), (2, 0, 0));
        let c = file_notes(&s, &one, "seed:captain.md", |_| Tier::Pinned, 2).unwrap();
        assert_eq!((c.new, c.reinforced, c.archived), (0, 2, 0));
        let two = bullets("- Alpha fact about boats. (`a.md`)\n- Gamma fact about planes.\n");
        let c = file_notes(&s, &two, "seed:captain.md", |_| Tier::Pinned, 3).unwrap();
        assert_eq!((c.new, c.reinforced, c.archived), (1, 1, 1));
        let live: Vec<_> = s
            .notes()
            .unwrap()
            .into_iter()
            .filter(|n| n.status == "live")
            .collect();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].evidence, vec!["a.md".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }
}
