//! Scripted app-server: validate pins before task input, resumed sessions, lifecycle,
//! denied native delegation, errors, and bounded process cleanup without model calls.
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Instant};
use uke::TurnRequest;
const SERVER: &str = r#"#!/usr/bin/env python3
import json,os,sys,time,subprocess,signal
mode=os.environ.get('CASE','ok')
assert 'agents.enabled=false' in sys.argv
assert 'features.multi_agent=false' in sys.argv and 'features.multi_agent_v2=false' in sys.argv
child=None; turns=0

def emit(x): print(json.dumps(x),flush=True)
def large_note(mib):
 # Stream an exact-sized JSON notification without allocating the payload in the fixture.
 prefix='{"method":"item/commandExecution/outputDelta","params":{"delta":"'
 suffix='"}}\n'
 sys.stdout.write(prefix)
 left=mib*1024*1024-len(prefix)-len(suffix)
 while left:
  n=min(left,8192);sys.stdout.write('x'*n);left-=n
 sys.stdout.write(suffix);sys.stdout.flush()
def note(method,turn='t1',**p): emit({'method':method,'params':dict(threadId='s1',turnId=turn,**p)})
def start_child():
 global child
 child=subprocess.Popen([sys.executable,'-c',"import time;time.sleep(3);open('late-marker','w').write('bad')"],start_new_session=True)
 assert os.getpgid(child.pid)!=os.getpgrp()
 open('separate-group','w').write(str(child.pid))
 note('item/started',item={'id':'cmd1','processId':'p1','type':'commandExecution','status':'inProgress'})
def end_child():
 global child
 if child:
  os.killpg(child.pid,signal.SIGKILL);child.wait();child=None
  # The original command belongs to t1, even when a newer model turn is active.
  note('item/completed',item={'id':'cmd1','type':'commandExecution','status':'completed'})
try:
 for line in sys.stdin:
  v=json.loads(line);method=v.get('method');params=v.get('params',{});id=v.get('id')
  if mode.startswith('silent-'):
   with open('protocol-log','a') as log: log.write(method+'\n')
  if method=='initialize':
   assert params['capabilities']['experimentalApi'] is True
   emit({'id':id,'result':{}})
  elif method=='config/read': emit({'id':id,'result':{'config':{'agents':{'enabled':mode=='native-enabled'},'features':{'multi_agent':False,'multi_agent_v2':mode=='native-feature-enabled'}}}})
  elif method in ('thread/start','thread/resume'):
   assert params['config']['agents.enabled'] is False
   assert params['config']['features.multi_agent'] is False and params['config']['features.multi_agent_v2'] is False
   assert params['config']['model_reasoning_effort']=='high'
   assert params['model'] in ['gpt-6-astra','gpt-6.1-sol']
   if method=='thread/resume': assert params['threadId']=='old-session'
   if mode=='reject': emit({'id':id,'error':{'message':'model rejected'}});continue
   emit({'id':id,'result':{'thread':{'id':'s1'},'cwd':'/' if mode=='cwd' else os.getcwd(),'model':'wrong' if mode=='model' else params['model'],'reasoningEffort':'low' if mode=='effort' else 'high'}})
  elif method=='thread/backgroundTerminals/list':
   if mode=='oversized-lines' and turns:large_note(80)
   emit({'id':id,'result':{'data':[{'processId':'p1','itemId':'cmd1'}] if child else [],'nextCursor':None}})
  elif method=='thread/backgroundTerminals/terminate':
   assert params['processId']=='p1'
   if mode!='cleanup-refusal':end_child() # Terminal notification interleaves before RPC reply.
   emit({'id':id,'result':{'terminated':mode!='cleanup-refusal'}})
  elif method=='turn/interrupt':emit({'id':id,'result':{}})
  elif method=='large/read':
   large_note(80)
   emit({'id':id,'result':{'text':'x'*(2*1024*1024)}})
  elif method=='turn/start':
   turns+=1;turn='t'+str(turns)
   open('prompt-seen','w').write(params['input'][0]['text'])
   assert params['threadId']=='s1' and params['model'] in ['gpt-6-astra','gpt-6.1-sol'] and params['effort']=='high'
   if mode.startswith('silent-'):
    open('server-pid','w').write(str(os.getpid()))
    if mode=='silent-wedged':
     while True: time.sleep(1)
    continue
   emit({'id':id,'result':{'turn':{'id':turn}}})
   note('turn/started',turn=turn)
   if mode=='oversized-lines':large_note(80)
   if mode in ('settle','cap','cleanup-refusal'):
    if turns==1:start_child()
    if mode=='settle' and turns==2:
     assert 'polling' in params['input'][0]['text'];end_child()
    text='UNVRS-RESULT: settled' if mode=='settle' and turns==2 else 'UNVRS-RESULT: premature'
    note('item/completed',turn=turn,item={'type':'agentMessage','text':text})
   elif mode in ('reroute','spawn'):
    start_child()
    if mode=='reroute':note('model/rerouted',fromModel=params['model'],toModel='wrong')
    else:note('item/started',item={'type':'collabAgentToolCall','tool':'spawnAgent'})
   elif mode=='failure':note('error',error={'message':'429 usage limit'},willRetry=False)
   else:
    note('item/started',item={'id':'cmd1','type':'commandExecution','status':'inProgress','command':'curl -H Authorization:Bearer:opaque-secret example.com'})
    note('item/completed',item={'id':'cmd1','type':'commandExecution','status':'completed','command':'curl -H Authorization:Bearer:opaque-secret example.com'})
    note('item/completed',item={'type':'agentMessage','text':'x'*(2*1024*1024) if mode=='large-line' else 'UNVRS-RESULT: done'})
   emit({'method':'turn/completed','params':{'threadId':'s1','turn':{'id':turn,'status':'failed' if mode=='failure' else 'completed','error':{'message':'429 usage limit'} if mode=='failure' else None}}})
 if mode=='oversized-lines':large_note(80)
 if mode=='silent-late-command':
  note('item/started',item={'id':'late','type':'commandExecution','status':'inProgress'})
finally:
 if child:os.killpg(child.pid,signal.SIGKILL);child.wait()

"#;
fn req(dir: &Path, model: &str, case: &str) -> TurnRequest {
    TurnRequest {
        harness: "codex".into(),
        cwd: dir.into(),
        session: None,
        prompt: "task must not arrive before pins match".into(),
        model: Some(model.into()),
        effort: Some("high".into()),
        env: vec![
            ("CASE".into(), case.into()),
            ("UNVRS_HOME".into(), dir.display().to_string()),
            ("UNVRS_DRIVEN_PID".into(), "149".into()),
        ],
    }
}
#[test]
fn codex_acceptance_precedes_prompt_and_managed_lifecycle_survives() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../target/unvrs-codex-protocol-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("codex");
    fs::write(&bin, SERVER).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    unsafe {
        std::env::set_var("UNVRS_CODEX_BIN", &bin);
    }
    for model in ["gpt-6-astra", "gpt-6.1-sol"] {
        let events = std::cell::RefCell::new(vec![]);
        let mut r = req(&dir, model, "ok");
        if model == "gpt-6.1-sol" {
            r.session = Some("old-session".into());
        }
        let out = drv_agent::run_turn_observed(&r, &|_| {}, &|v| {
            if let Some(kind) = v["kind"].as_str() {
                events.borrow_mut().push(kind.to_owned());
            }
        })
        .unwrap();
        assert_eq!(out.model.as_deref(), Some(model));
        assert_eq!(out.effort.as_deref(), Some("high"));
        assert_eq!(out.session.as_deref(), Some("s1"));
        assert_eq!(out.text, "UNVRS-RESULT: done");
        assert_eq!(out.tools, vec!["shell"]);
        assert_eq!(
            *events.borrow(),
            vec![
                "accepted",
                "turn.started",
                "tool.started",
                "tool.completed",
                "progress",
                "turn.completed"
            ]
        );
        assert!(
            out.effort_evidence
                .unwrap()
                .contains("harness configuration")
        );
        fs::remove_file(dir.join("prompt-seen")).unwrap();
        for case in [
            "model",
            "effort",
            "reject",
            "native-enabled",
            "native-feature-enabled",
            "cwd",
        ] {
            let err = drv_agent::run_turn(&req(&dir, model, case), &|_| {}).unwrap_err();
            assert!(
                !dir.join("prompt-seen").exists(),
                "prompt leaked for {case}: {err}"
            );
        }
    }
    for case in ["large-line", "oversized-lines"] {
        let events = std::cell::RefCell::new(vec![]);
        let out = drv_agent::run_turn_observed(&req(&dir, "gpt-6.1-sol", case), &|_| {}, &|v| {
            if !v.is_null() {
                events.borrow_mut().push(v.clone());
            }
        })
        .unwrap();
        assert!(out.error.is_none());
        assert_eq!(out.session.as_deref(), Some("s1"));
        if case == "large-line" {
            assert_eq!(out.text.len(), 2 * 1024 * 1024);
            assert!(out.text.bytes().all(|b| b == b'x'));
        } else {
            assert_eq!(out.text, "UNVRS-RESULT: done");
            let events = events.borrow();
            let drops: Vec<_> = events
                .iter()
                .filter(|v| v["kind"] == "protocol.dropped")
                .collect();
            assert_eq!(drops.len(), 4); // Turn, two cleanup-list interleavings, and stdin-EOF shutdown.
            assert!(drops.iter().all(|v| v["bytes"] == 80 * 1024 * 1024));
            assert!(!serde_json::to_string(&drops).unwrap().contains("xxxxx"));
        }
    }
    let mut cmd = std::process::Command::new(&bin);
    cmd.args([
        "agents.enabled=false",
        "features.multi_agent=false",
        "features.multi_agent_v2=false",
    ]);
    cmd.current_dir(&dir).env("CASE", "plugin-lines");
    let answers = drv_agent::jsonrpc_session(
        cmd,
        serde_json::json!({"capabilities":{"experimentalApi":true}}),
        &[("large/read", serde_json::json!({}))],
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(
        answers[0]["result"]["text"].as_str().unwrap().len(),
        2 * 1024 * 1024
    );
    for (case, expected) in [
        ("spawn", "native delegation restriction violated"),
        ("reroute", "model mismatch"),
    ] {
        let err = drv_agent::run_turn(&req(&dir, "gpt-6.1-sol", case), &|_| {}).unwrap_err();
        assert!(err.to_string().contains(expected), "{err}");
    }
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(!dir.join("late-marker").exists());
    let out = drv_agent::run_turn(&req(&dir, "gpt-6.1-sol", "failure"), &|_| {}).unwrap();
    assert!(out.quota);
    assert!(out.error.unwrap().contains("429 usage limit"));

    for (case, expected) in [
        ("settle", ""),
        ("cap", "exceeded two completion turns"),
        ("cleanup-refusal", "cleanup unconfirmed"),
    ] {
        let events = std::cell::RefCell::new(vec![]);
        let out = drv_agent::run_turn_observed(&req(&dir, "gpt-6.1-sol", case), &|_| {}, &|v| {
            if !v.is_null() {
                events.borrow_mut().push(v.clone());
            }
        });
        if case == "settle" {
            assert_eq!(out.unwrap().text, "UNVRS-RESULT: settled");
        } else {
            assert!(format!("{:#}", out.unwrap_err()).contains(expected));
        }
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|v| v["kind"] == "turn.settling")
                .count(),
            if case == "settle" { 1 } else { 2 }
        );
        assert!(dir.join("separate-group").exists());
        assert!(
            !serde_json::to_string(&*events.borrow())
                .unwrap()
                .contains("opaque-secret")
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(3200));
    assert!(!dir.join("late-marker").exists());

    unsafe {
        std::env::set_var("UNVRS_TURN_IDLE_SECS", "2");
        std::env::set_var("UNVRS_TURN_MAX_SECS", "2");
    }
    for (case, expected) in [
        ("silent-exit", "turn timed out"),
        ("silent-late-command", "cleanup unconfirmed"),
        ("silent-wedged", "cleanup unconfirmed"),
    ] {
        let start = Instant::now();
        let events = std::cell::RefCell::new(vec![]);
        let err = drv_agent::run_turn_observed(&req(&dir, "gpt-6.1-sol", case), &|_| {}, &|v| {
            if !v.is_null() {
                events.borrow_mut().push(v.clone());
            }
        })
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains(expected), "{case}: {message}");
        if case == "silent-exit" {
            assert!(!message.contains("cleanup unconfirmed"), "{message}");
        }
        assert!(
            start.elapsed().as_secs() < 25,
            "{case} exceeded cleanup bound"
        );
        let pid: i32 = fs::read_to_string(dir.join("server-pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            uke::signals::process_exists(i64::from(pid))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ESRCH)
        );
        let log = fs::read_to_string(dir.join("protocol-log")).unwrap();
        assert!(!log.contains("turn/interrupt"), "{case}: {log}");
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|v| v["kind"] == "cleanup.verified")
                .count(),
            if case == "silent-exit" { 1 } else { 0 }
        );
        if case == "silent-exit" {
            assert_eq!(
                log.split_once("turn/start\n")
                    .unwrap()
                    .1
                    .matches("thread/backgroundTerminals/list")
                    .count(),
                2
            );
        }
        fs::remove_file(dir.join("protocol-log")).unwrap();
    }
    unsafe {
        std::env::remove_var("UNVRS_TURN_IDLE_SECS");
        std::env::remove_var("UNVRS_TURN_MAX_SECS");
    }
    fs::remove_dir_all(dir).unwrap();
}
