//! Context sources through the public API (context-sources.md §1–§3): registry, map, search,
//! versioned refs, bounded fetch, access scopes, git clones.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use uke::{CTX_GET_BYTES, CTX_SNIPPET, SourceRec, SourceScope, Sources, parse_ref};

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("uke-ctx-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        Self(p.canonicalize().unwrap())
    }
    fn home(&self) -> PathBuf {
        let h = self.0.join("home");
        fs::create_dir_all(&h).unwrap();
        h
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(p: &Path, body: impl AsRef<[u8]>) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, msg: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])[..12].to_string()
}

/// A small company repo: brand notes, media (excluded), a binary, a big file, a secret.
fn repo(t: &Tmp) -> PathBuf {
    let r = t.0.join("acme");
    fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    write(
        &r.join("README.md"),
        "# Acme\n\nFlowers, delivered.\n",
    );
    write(
        &r.join("brand/CONTEXT.md"),
        "# Brand\n\nOur palette is peony pink and moss green.\nVoice: warm, plain.\n",
    );
    write(
        &r.join("brand/voice.md"),
        "# Voice\n\nWrite warm sentences.\n",
    );
    write(
        &r.join("sales/clients.md"),
        "# Clients\n\nAcme buys peony bouquets weekly.\n",
    );
    write(
        &r.join("media/peony-notes.txt"),
        "peony photo shoot notes\n",
    );
    write(&r.join("assets/logo.bin"), b"peony\0\x01\x02binary");
    write(&r.join(".env"), "PEONY_TOKEN=hunter2\n");
    let big: String = (1..=2000)
        .map(|i| format!("line {i:05} of the big ledger, peony stock count\n"))
        .collect();
    write(&r.join("finance/ledger.txt"), big);
    commit(&r, "init");
    r
}

fn rec(id: &str, kind: &str, uri: &str) -> SourceRec {
    SourceRec {
        id: id.into(),
        kind: kind.into(),
        uri: uri.into(),
        purpose: format!("{id} purpose"),
        exclude: vec![],
        sensitivity: "private".into(),
        branch: None,
        refresh: None,
    }
}

fn all(id: &str) -> SourceScope {
    SourceScope {
        id: id.into(),
        prefix: None,
    }
}

fn setup(t: &Tmp) -> (PathBuf, PathBuf, Sources) {
    let home = t.home();
    let r = repo(t);
    let mut s = rec("acme", "path", r.to_str().unwrap());
    s.exclude = vec!["media/**".into()];
    Sources::add(&home, s).unwrap();
    let srcs = Sources::load(&home).unwrap();
    (home, r, srcs)
}

#[test]
fn load_without_and_with_registry() {
    let t = Tmp::new("load");
    let home = t.home();
    let s = Sources::load(&home).unwrap();
    assert_eq!(s.list().len(), 1);
    let m = s.get("memory").unwrap();
    assert_eq!(m.kind, "memory");
    assert_eq!(s.root("memory").unwrap(), home);

    write(
        &home.join("sources.toml"),
        "[[source]]\nid = \"notes\"\nkind = \"path\"\nuri = \"/tmp\"\nexclude = [\"a/**\"]\n",
    );
    let s = Sources::load(&home).unwrap();
    assert_eq!(s.list().len(), 2);
    let n = s.get("notes").unwrap();
    assert_eq!(n.sensitivity, "private");
    assert_eq!(n.exclude, vec!["a/**".to_string()]);
    assert!(s.get("memory").is_some());

    // A registry memory entry replaces the built-in.
    write(
        &home.join("sources.toml"),
        "[[source]]\nid = \"memory\"\nkind = \"memory\"\npurpose = \"mine\"\n",
    );
    let s = Sources::load(&home).unwrap();
    assert_eq!(s.list().len(), 1);
    assert_eq!(s.get("memory").unwrap().purpose, "mine");
}

#[test]
fn add_roundtrip_and_validation() {
    let t = Tmp::new("add");
    let home = t.home();
    let dir = t.0.join("notes");
    fs::create_dir_all(&dir).unwrap();
    let mut a = rec("notes", "path", dir.to_str().unwrap());
    a.exclude = vec!["media/**".into(), "*.png".into()];
    Sources::add(&home, a.clone()).unwrap();
    let mut g = rec("repo-x", "git", "github.com/org/repo");
    g.branch = Some("main".into());
    g.refresh = Some("30m".into());
    Sources::add(&home, g.clone()).unwrap();
    let s = Sources::load(&home).unwrap();
    assert_eq!(s.get("notes"), Some(&a));
    assert_eq!(s.get("repo-x"), Some(&g));

    a.purpose = "replaced".into();
    Sources::add(&home, a.clone()).unwrap();
    let s = Sources::load(&home).unwrap();
    assert_eq!(s.list().len(), 3);
    assert_eq!(s.get("notes").unwrap().purpose, "replaced");
    let raw = fs::read_to_string(home.join("sources.toml")).unwrap();
    assert!(
        raw.contains("[[source]]") && raw.contains("id = \"notes\""),
        "{raw}"
    );

    assert!(Sources::add(&home, rec("Bad_Id", "path", dir.to_str().unwrap())).is_err());
    assert!(Sources::add(&home, rec("x", "ftp", "x")).is_err());
    assert!(Sources::add(&home, rec("x", "path", "")).is_err());
    assert!(Sources::add(&home, rec("x", "git", "")).is_err());
    assert!(Sources::add(&home, rec("x", "path", "/no/such/dir/anywhere")).is_err());
}

#[test]
fn map_entry_file_or_outline() {
    let t = Tmp::new("map");
    let (_home, r, s) = setup(&t);
    let m = s.map(&all("acme"), Some("brand")).unwrap();
    assert_eq!(m.path, "brand/CONTEXT.md");
    assert!(m.text.starts_with("# Brand"));
    assert!(
        m.reference
            .starts_with("ctx://acme/brand/CONTEXT.md@")
    );

    let root = s.map(&all("acme"), None).unwrap();
    assert_eq!(root.path, "README.md");

    // No entry file: an outline two levels deep, honoring excludes.
    let o = s.map(&all("acme"), Some("sales")).unwrap();
    assert!(o.text.contains("clients.md"), "{}", o.text);
    fs::remove_file(r.join("README.md")).unwrap();
    let o = s.map(&all("acme"), None).unwrap();
    assert!(
        o.text.contains("brand/") && o.text.contains("CONTEXT.md"),
        "{}",
        o.text
    );
    assert!(
        !o.text.contains("media") && !o.text.contains(".env"),
        "{}",
        o.text
    );
    assert!(!o.text.contains(".git/"), "{}", o.text);

    // unvrs-context.toml entry points come first.
    write(
        &r.join("unvrs-context.toml"),
        "entry = [\"sales/clients.md\"]\nexclude = [\"finance/**\"]\n",
    );
    let e = s.map(&all("acme"), None).unwrap();
    assert_eq!(e.path, "sales/clients.md");
    let hits = s.search("ledger", &[all("acme")], 8).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn search_finds_ranks_and_skips() {
    let t = Tmp::new("search");
    let (_home, _r, s) = setup(&t);
    let hits = s.search("Peony", &[all("acme")], 20).unwrap();
    let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    assert!(paths.contains(&"brand/CONTEXT.md"), "{paths:?}");
    assert!(paths.contains(&"sales/clients.md"), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with("media/")), "{paths:?}");
    assert!(!paths.contains(&"assets/logo.bin"), "{paths:?}");
    assert!(!paths.contains(&".env"), "{paths:?}");
    let brand = hits.iter().find(|h| h.path == "brand/CONTEXT.md").unwrap();
    assert_eq!(brand.line, 3);
    assert_eq!(brand.title, "Brand");
    assert!(brand.snippet.contains("peony pink"));
    assert!(brand.snippet.chars().count() <= CTX_SNIPPET);
    assert_eq!(brand.source, "acme");

    // Every term must be present.
    let hits = s.search("peony acme", &[all("acme")], 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "sales/clients.md");
    // k default and bound.
    assert!(s.search("peony", &[all("acme")], 0).unwrap().len() <= 8);
    assert!(s.search("", &[all("acme")], 8).is_err());
}

#[test]
fn refs_parse_and_roundtrip() {
    let (s, p, v, r) = parse_ref("ctx://acme/brand/CONTEXT.md@abc123def456#L10-40").unwrap();
    assert_eq!(
        (s.as_str(), p.as_str(), v.as_deref(), r),
        (
            "acme",
            "brand/CONTEXT.md",
            Some("abc123def456"),
            Some((10, 40))
        )
    );
    let (_, p, v, r) = parse_ref("ctx://memory/projects/x/notes.md").unwrap();
    assert_eq!((p.as_str(), v, r), ("projects/x/notes.md", None, None));
    let (_, _, v, r) = parse_ref("ctx://a/b.md@m17#L5").unwrap();
    assert_eq!((v.as_deref(), r), (Some("m17"), Some((5, 5))));
    assert!(parse_ref("http://a/b").is_err());
    assert!(parse_ref("ctx://a/b#L9-2").is_err());

    // A hit's ref fetches the same file at the same version.
    let t = Tmp::new("refs");
    let (_home, _r, srcs) = setup(&t);
    let hit = &srcs.search("moss", &[all("acme")], 8).unwrap()[0];
    let (sid, path, ver, _) = parse_ref(&hit.reference).unwrap();
    assert_eq!(
        (sid.as_str(), path.as_str()),
        ("acme", "brand/CONTEXT.md")
    );
    assert_eq!(ver.as_deref(), Some(hit.version.as_str()));
    let a = srcs
        .fetch(&hit.reference, None, &[all("acme")])
        .unwrap();
    assert!(!a.stale);
    assert_eq!(a.reference, hit.reference);
    assert_eq!(
        a.text,
        "# Brand\n\nOur palette is peony pink and moss green.\nVoice: warm, plain.\n"
    );
}

#[test]
fn fetch_bounded_with_next_and_ranges() {
    let t = Tmp::new("fetch");
    let (_home, _r, s) = setup(&t);
    let sc = [all("acme")];
    let a = s
        .fetch("ctx://acme/finance/ledger.txt", None, &sc)
        .unwrap();
    assert!(a.bytes <= CTX_GET_BYTES && a.bytes == a.text.len());
    let next = a.next.clone().expect("next");
    let (_, _, _, range) = parse_ref(&next).unwrap();
    let (from, to) = range.unwrap();
    assert_eq!(to, 2000);
    assert!(a.text.ends_with(&format!(
        "line {:05} of the big ledger, peony stock count\n",
        from - 1
    )));
    let b = s.fetch(&next, None, &sc).unwrap();
    assert!(b.text.starts_with(&format!("line {from:05} ")));

    let r = s
        .fetch("ctx://acme/finance/ledger.txt#L10-12", None, &sc)
        .unwrap();
    assert_eq!(r.text.lines().count(), 3);
    assert!(r.text.starts_with("line 00010 "));
    assert!(r.next.is_none());
    let o = s
        .fetch(
            "ctx://acme/finance/ledger.txt#L10-12",
            Some((20, 20)),
            &sc,
        )
        .unwrap();
    assert!(o.text.starts_with("line 00020 ") && o.text.lines().count() == 1);

    assert!(
        s.fetch("ctx://acme/assets/logo.bin", None, &sc)
            .is_err()
    );
    let e = s.fetch("ctx://acme/.env", None, &sc).unwrap_err();
    assert!(e.to_string().starts_with("Refused:"), "{e}");
    let e = s
        .fetch("ctx://acme/media/peony-notes.txt", None, &sc)
        .unwrap_err();
    assert!(e.to_string().starts_with("Refused:"), "{e}");
}

#[test]
fn versions_stale_and_dirty() {
    let t = Tmp::new("versions");
    let (_home, r, s) = setup(&t);
    let sc = [all("acme")];
    let head = git(&r, &["rev-parse", "HEAD"])[..12].to_string();
    let a = s
        .fetch("ctx://acme/brand/voice.md", None, &sc)
        .unwrap();
    assert_eq!(a.version, head);
    let old = a.reference.clone();

    // Dirty file: <commit12>+m<mtime>.
    write(
        &r.join("brand/voice.md"),
        "# Voice\n\nWrite warmer sentences.\n",
    );
    let d = s.fetch(&old, None, &sc).unwrap();
    assert!(d.version.starts_with(&format!("{head}+m")), "{}", d.version);
    assert!(d.stale);
    // Untracked file in a git tree.
    write(&r.join("brand/new.md"), "fresh\n");
    let u = s.fetch("ctx://acme/brand/new.md", None, &sc).unwrap();
    assert!(u.version.contains("+m"), "{}", u.version);

    // New commit: the old ref is stale and the answer carries the current version.
    let new_head = commit(&r, "warmer");
    let c = s.fetch(&old, None, &sc).unwrap();
    assert!(c.stale);
    assert_eq!(c.version, new_head);
    assert!(c.text.contains("warmer"));
    assert!(!s.fetch(&c.reference, None, &sc).unwrap().stale);

    // Outside git: m<mtime>.
    let plain = t.0.join("plain");
    write(&plain.join("notes.md"), "# Notes\nhello\n");
    Sources::add(&t.home(), rec("plain", "path", plain.to_str().unwrap())).unwrap();
    let s = Sources::load(&t.home()).unwrap();
    let p = s
        .fetch("ctx://plain/notes.md", None, &[all("plain")])
        .unwrap();
    assert!(
        p.version.starts_with('m') && p.version[1..].parse::<u64>().is_ok(),
        "{}",
        p.version
    );
}

#[test]
fn access_scopes_and_escapes() {
    let t = Tmp::new("access");
    let (home, r, s) = setup(&t);
    let refused = |e: anyhow::Error| assert!(e.to_string().starts_with("Refused:"), "{e}");

    refused(
        s.fetch("ctx://acme/README.md", None, &[all("memory")])
            .unwrap_err(),
    );
    assert!(s.map(&all("other"), None).is_err());
    let other = s.search("peony", &[all("memory")], 8).unwrap();
    assert!(other.iter().all(|h| h.source == "memory"), "{other:?}");
    refused(
        s.fetch("ctx://acme/../outside.md", None, &[all("acme")])
            .unwrap_err(),
    );
    refused(s.map(&all("acme"), Some("../")).unwrap_err());
    refused(s.map(&all("acme"), Some("/etc")).unwrap_err());

    // Symlink pointing outside the root.
    write(&t.0.join("outside.md"), "secret peony plan\n");
    std::os::unix::fs::symlink(t.0.join("outside.md"), r.join("link.md")).unwrap();
    refused(
        s.fetch("ctx://acme/link.md", None, &[all("acme")])
            .unwrap_err(),
    );
    let hits = s.search("secret", &[all("acme")], 8).unwrap();
    assert!(hits.is_empty(), "{hits:?}");

    // Memory with a prefix: an L2 reads only its project.
    write(
        &home.join("projects/alpha/notes.md"),
        "# Alpha\nalpha orchid plan\n",
    );
    write(
        &home.join("projects/beta/notes.md"),
        "# Beta\nbeta orchid plan\n",
    );
    let alpha = SourceScope {
        id: "memory".into(),
        prefix: Some("projects/alpha/".into()),
    };
    let sc = [alpha.clone()];
    let hits = s.search("orchid", &sc, 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "projects/alpha/notes.md");
    assert!(
        s.fetch("ctx://memory/projects/alpha/notes.md", None, &sc)
            .is_ok()
    );
    refused(
        s.fetch("ctx://memory/projects/beta/notes.md", None, &sc)
            .unwrap_err(),
    );
    refused(
        s.fetch("ctx://memory/projects/alpha/../beta/notes.md", None, &sc)
            .unwrap_err(),
    );
    refused(s.map(&alpha, Some("projects")).unwrap_err());
    let m = s.map(&alpha, None).unwrap();
    assert!(m.text.contains("notes.md"), "{}", m.text);
    // The memory source never searches its git clones or sources.toml secrets by path.
    let all_mem = s.search("orchid", &[all("memory")], 8).unwrap();
    assert_eq!(all_mem.len(), 2);
}

#[test]
fn git_source_clones_into_home() {
    let t = Tmp::new("git");
    let home = t.home();
    let r = repo(&t);
    let head = git(&r, &["rev-parse", "HEAD"])[..12].to_string();
    let mut g = rec("ib", "git", &format!("file://{}", r.display()));
    g.refresh = Some("0s".into());
    Sources::add(&home, g).unwrap();
    let s = Sources::load(&home).unwrap();
    let root = s.root("ib").unwrap();
    assert_eq!(root, home.join("sources/ib"));
    assert!(root.join("brand/CONTEXT.md").is_file());
    let a = s
        .fetch("ctx://ib/brand/CONTEXT.md", None, &[all("ib")])
        .unwrap();
    assert_eq!(a.version, head);

    // A new upstream commit is pulled once the refresh interval has passed.
    write(&r.join("brand/CONTEXT.md"), "# Brand\n\nNow lilac.\n");
    let new_head = commit(&r, "lilac");
    let b = s.fetch(&a.reference, None, &[all("ib")]).unwrap();
    assert!(b.stale);
    assert_eq!(b.version, new_head);
    assert!(b.text.contains("lilac"));

    // A plain local path uri works too.
    Sources::add(&home, rec("ib2", "git", r.to_str().unwrap())).unwrap();
    let s = Sources::load(&home).unwrap();
    assert!(s.root("ib2").unwrap().join("README.md").is_file());
    // Git clones are not part of the memory source.
    let hits = s.search("lilac", &[all("memory")], 8).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}
