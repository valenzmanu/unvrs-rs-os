//! Real subscription requests, one per configured exact model; no kernel starts.
fn main() {
    let routes = drv_hdff::SubscriptionSummaries::default();
    routes.preflight();
    let events = routes.events();
    let failed = events.iter().any(|e| e["error"].is_string());
    for event in events {
        println!("{event}");
    }
    if failed {
        std::process::exit(1);
    }
    if std::env::args().any(|arg| arg == "--fold-fixture") {
        let brief = uke::Brief {
            goal: "Verify a subscription brief".into(),
            open: vec!["[o1] Check the fixture".into()],
            ..Default::default()
        };
        let job = uke::FoldJob {
            pid: 1,
            version: 0,
            reason: "fixture".into(),
            brief,
            turns: vec!["The fixture is not checked yet; keep o1 open.".into()],
            covered: 1,
            drain: false,
            area: None,
        };
        let fold = routes.fold(&job).expect("live subscription fold failed");
        assert!(
            fold.brief.open.iter().any(|item| item.starts_with("[o1]")),
            "open item lost"
        );
        println!("live fold fixture: parsed brief JSON and retained [o1]");
        for event in routes.events() {
            println!("{event}");
        }
    }
}
