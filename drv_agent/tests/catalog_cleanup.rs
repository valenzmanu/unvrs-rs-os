//! Real catalog control/cleanup path with a local protocol child; no model calls.
use std::{fs, os::unix::fs::PermissionsExt};
use uke::signals::{LedgerStore, owner_scope};

#[test]
fn catalog_eof_cleanup_creates_no_refusals() {
    let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/catalog-cleanup-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let _scope = owner_scope(&home, 0);
    let fixture = home.join("claude");
    fs::write(&fixture, r#"#!/bin/sh
if [ "$1" = auth ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai"}'
    exit 0
fi
for id in 0 1 2; do
    IFS= read -r request || exit 1
    printf '{"type":"control_response","response":{"subtype":"success","request_id":"unvrs-catalog-%s","response":{}}}\n' "$id"
done
while IFS= read -r request; do :; done
"#).unwrap();
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
    // One test in this binary; this override cannot race another test.
    unsafe {
        std::env::set_var("UNVRS_CLAUDE_BIN", &fixture);
    }
    let store = LedgerStore::new(&home);
    for round in 0..30 {
        let raw = drv_agent::claude_catalog_raw(&home, &[]).unwrap();
        assert_eq!(raw["auth"]["loggedIn"], true);
        let entries = store.snapshot().unwrap();
        assert!(entries.entries().is_empty());
        eprintln!(
            "catalog round={round} refusals={}",
            store.refusals().unwrap().len()
        );
    }
    fs::create_dir_all(home.join("kernel")).unwrap();
    fs::write(
        home.join("kernel/claude-usage.json"),
        serde_json::json!({
            "at_ms":uke::now_ms(), "source":"saved quota test", "plan":"max",
            "five_hour":{"used_pct":21,"resets_at":2_000_000},
            "seven_day":{"used_pct":21,"resets_at":3_000_000}
        })
        .to_string(),
    )
    .unwrap();
    let raw = drv_agent::claude_catalog_raw(&home, &[]).unwrap();
    assert_eq!(
        raw["usage"]["response"]["response"]["source"],
        "saved quota test"
    );
    assert_eq!(
        raw["usage"]["response"]["response"]["rate_limits"]["five_hour"]["utilization"],
        21
    );
    let refusals = store.refusals().unwrap();
    fs::remove_dir_all(home).unwrap();
    assert!(
        refusals.is_empty(),
        "normal catalog EOF cleanup produced {refusals:?}"
    );
}
