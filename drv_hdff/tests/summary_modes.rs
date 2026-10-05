//! Readiness and recovery remain text; only brief folds request a JSON schema.
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn only_brief_folds_use_the_output_schema() {
    let dir = std::env::temp_dir().join(format!("unvrs-summary-modes-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("codex");
    let log = dir.join("modes");
    fs::write(&bin, r#"#!/usr/bin/env python3
import json,os,sys
if sys.argv[1:3]==['login','status']:
 print('Logged in using ChatGPT',file=sys.stderr);sys.exit(0)
prompt=sys.stdin.read()
fold=prompt.startswith('You maintain the brief')
assert ('--output-schema' in sys.argv)==fold
if fold:
 schema=json.load(open(sys.argv[sys.argv.index('--output-schema')+1]))
 assert schema['properties']['brief']['additionalProperties'] is False
 assert 'open' in schema['properties']['brief']['required']
 text=json.dumps({'brief':{'goal':'fixture','open':['[o1] retained']},'notes':[]})
elif 'UNVRS_SUMMARY_READY' in prompt: text='UNVRS_SUMMARY_READY'
else: text='UNVRS-PROGRESS: retained'
with open(os.environ['UNVRS_FAKE_MODE_LOG'],'a') as log: log.write(('fold' if fold else 'text')+'\n')
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'intermediate progress'}}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':text}}))
"#).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    unsafe {
        std::env::set_var("UNVRS_CODEX_BIN", &bin);
        std::env::set_var("UNVRS_FAKE_MODE_LOG", &log);
    }
    let routes = drv_hdff::SubscriptionSummaries::new("codex=gpt-5.6-luna");
    routes.preflight();
    assert!(routes.events()[0]["error"].is_null());
    assert_eq!(
        routes.summarize("Return UNVRS-PROGRESS").unwrap(),
        "UNVRS-PROGRESS: retained"
    );
    let fold = routes
        .fold(&uke::FoldJob {
            pid: 1,
            version: 0,
            reason: "fixture".into(),
            brief: Default::default(),
            turns: vec!["retain o1".into()],
            covered: 1,
            drain: false,
            area: None,
        })
        .unwrap();
    assert_eq!(fold.brief.open, vec!["[o1] retained"]);
    assert_eq!(fs::read_to_string(log).unwrap(), "text\ntext\nfold\n");
    fs::remove_dir_all(dir).unwrap();
}
