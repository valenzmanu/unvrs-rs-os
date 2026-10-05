//! Settings schema and validated, comment-preserving configuration writes.
//! Callers enforce captain authority before invoking writes.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};
use toml_edit::{DocumentMut, Item};

#[derive(Clone, Debug, Serialize)]
pub struct Setting {
    pub key: String,
    pub section: String,
    pub label: String,
    pub explanation: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub default: Value,
    pub allowed: Value,
    pub range: Value,
    pub file: Option<String>,
    pub toml_path: Option<Vec<Value>>,
    pub apply: String,
    pub editable: bool,
    pub sensitive: bool,
    pub fields: Value,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub schema: Vec<Setting>,
    pub values: BTreeMap<String, Value>,
    pub effective_source: BTreeMap<String, String>,
    pub live_catalog: Value,
    pub history: Vec<Change>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub change_id: String,
    pub key: String,
    pub before: Value,
    pub after: Value,
    pub by: String,
    pub at: u64,
    pub reverts: Option<String>,
}
// Keep registry fields explicit in the single metadata table.
#[allow(clippy::too_many_arguments)]
fn setting(
    key: &str,
    section: &str,
    label: &str,
    explanation: &str,
    kind: &str,
    default: Value,
    allowed: Value,
    range: Value,
    sensitive: bool,
    fields: Value,
) -> Setting {
    let editable = matches!(section, "routing" | "sources");
    let (file, toml_path) = if let Some(path) = key.strip_prefix("econ.") {
        (
            Some("econ.toml".into()),
            Some(path.split('.').map(|s| json!(s)).collect()),
        )
    } else {
        (None, None)
    };
    Setting {
        key: key.into(),
        section: section.into(),
        label: label.into(),
        explanation: explanation.into(),
        kind: kind.into(),
        default,
        allowed,
        range,
        file,
        toml_path,
        apply: if editable { "live" } else { "restart" }.into(),
        editable,
        sensitive,
        fields,
    }
}

fn candidate_fields() -> Value {
    json!({
        "harness":{"type":"enum","explanation":"Choose the harness that runs this candidate. Example: codex.","allowed":["codex","claude"],"required":true},
        "model":{"type":"string","explanation":"Use an exact catalog model ID or a newest-family selector. Example: newest sonnet.","allowed":null,"required":true},
        "effort":{"type":"enum","explanation":"Pin this candidate's effort instead of the task's usual effort. Light and deep use your class preferences; named efforts require exact harness support. A lower pin requires captain fallback. Example: low pins Sol to low effort.","allowed":["none","minimal","low","medium","high","xhigh","max","light","deep"],"required":false},
        "captain_fallback":{"type":"bool","explanation":"Record the captain's permission to use this candidate below the task's reasoning level. The receipt and Crew card show the model and effort as captain fallback. Example: true permits the research Sol fallback at low effort.","allowed":null,"required":false}
    })
}

/// One metadata table; wildcard source/project rows expand against the current files.
fn table() -> Vec<Setting> {
    let default_econ = crate::drv_econ::Config::parse(crate::drv_econ::DEFAULT_CONFIG)
        .expect("valid default econ configuration");
    vec![
        setting(
            "safety.settings_write_auth",
            "safety",
            "Settings write protection",
            "The browser must send a page CSRF value from the exact Observatory origin; the local authorization token stays server-side. Example: a request from another website is refused.",
            "string",
            json!("same-origin + page CSRF + server-side local token"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "econ.policy.default_role",
            "routing",
            "Default model profile",
            "Choose a model profile when no role rule matches. Example: worker tries the worker candidates.",
            "enum",
            json!("worker"),
            json!(["judge", "worker", "design", "research", "research_quick"]),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "econ.policy.default_reasoning",
            "routing",
            "Default reasoning",
            "Choose reasoning when no reasoning rule matches. Example: deep requests the deep effort preference.",
            "enum",
            json!("deep"),
            json!(["light", "deep", "max"]),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "econ.policy.allow_max",
            "routing",
            "Allow maximum reasoning",
            "Allow maximum reasoning and explicit maximum effort overrides. Example: true permits max; this can increase spending.",
            "bool",
            json!(false),
            json!(null),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "econ.policy.rules",
            "routing",
            "Routing rules",
            "The first matching rule for each axis wins; list order matters. Example: ranks [1, 2] selects judge for seats.",
            "ordered-list",
            json!([{"ranks":[1,2],"role":"judge","reason":"seat judgment"},{"kinds":["design"],"role":"design","reason":"design task"},{"judgment":"high","role":"judge","reason":"high judgment"},{"kinds":["decide"],"role":"judge","reason":"decision task"},{"kinds":["research"],"thoroughness":"low","role":"research_quick","reasoning":"light","reason":"quick research task"},{"kinds":["research"],"role":"research","reason":"research task"},{"thoroughness":"low","reasoning":"light","reason":"low thoroughness"}]),
            json!(null),
            json!(null),
            true,
            json!({"ranks":{"type":"list","explanation":"Match these crew levels; an empty list matches any level. Example: [1, 2] matches the captain and leads.","allowed":[1,2,3],"required":false},"kinds":{"type":"list","explanation":"Match these task kinds; an empty list matches any kind. Example: [\"design\"] matches design work.","allowed":["implement","review","validate","research","design","decide"],"required":false},"judgment":{"type":"enum","explanation":"Match the requested judgment level. Example: high selects tasks needing more judgment.","allowed":["low","high"],"required":false},"thoroughness":{"type":"enum","explanation":"Match the requested thoroughness. Example: low selects a lighter reasoning rule.","allowed":["low","high"],"required":false},"role":{"type":"enum","explanation":"Choose the model profile for matching tasks. Example: judge tries the judge candidates.","allowed":["judge","worker","design","research","research_quick"],"required":false},"reasoning":{"type":"enum","explanation":"Choose reasoning independently from the model profile. Example: deep uses the deep effort preference.","allowed":["light","deep","max"],"required":false},"reason":{"type":"string","explanation":"Explain why this rule exists in route receipts. Example: seat judgment.","allowed":null,"required":true}}),
        ),
        setting(
            "econ.catalog.refresh_minutes",
            "routing",
            "Catalog refresh interval",
            "Refresh harness model and quota evidence this often. Example: 5 checks every five minutes.",
            "int",
            json!(5),
            json!(null),
            json!({"min":1,"max":307445734561825u64}),
            false,
            json!(null),
        ),
        setting(
            "econ.catalog.max_age_minutes",
            "routing",
            "Oldest accepted catalog",
            "Hold tasks when required catalog evidence is older than this limit. Example: 15 rejects evidence older than fifteen minutes.",
            "int",
            json!(15),
            json!(null),
            json!({"min":1,"max":307445734561825u64}),
            true,
            json!(null),
        ),
        setting(
            "econ.profiles.judge",
            "routing",
            "Judge model choices",
            "Try these candidates in order until a ready model supports the required effort. Example: try codex newest sol, then claude newest opus.",
            "ordered-list",
            json!([{"harness":"claude","model":"newest opus"},{"harness":"claude","model":"newest fable"}]),
            json!(null),
            json!(null),
            false,
            candidate_fields(),
        ),
        setting(
            "econ.profiles.worker",
            "routing",
            "Worker model choices",
            "Try these candidates in order until a ready model supports the required effort. Example: try codex newest sol, then claude newest opus.",
            "ordered-list",
            json!([{"harness":"codex","model":"newest sol"},{"harness":"claude","model":"newest opus"}]),
            json!(null),
            json!(null),
            false,
            candidate_fields(),
        ),
        setting(
            "econ.profiles.design",
            "routing",
            "Design model choices",
            "Try these candidates in order until a ready model supports the required effort. Example: try codex newest sol, then claude newest opus.",
            "ordered-list",
            json!([{"harness":"claude","model":"newest opus"}]),
            json!(null),
            json!(null),
            false,
            candidate_fields(),
        ),
        setting(
            "econ.profiles.research_quick",
            "routing",
            "Quick research model choices",
            "Try these candidates for low-thoroughness research. Example: newest Codex Luna at medium, then newest Claude Sonnet at light.",
            "ordered-list",
            json!(default_econ.profiles[&crate::drv_econ::Role::ResearchQuick]),
            json!(null),
            json!(null),
            true,
            candidate_fields(),
        ),
        setting(
            "econ.profiles.research",
            "routing",
            "Research model choices",
            "Try these research candidates in order. Unpinned candidates use the task's reasoning level; a lower pin needs captain fallback. Example: a candidate pinned to low records the captain's fallback choice.",
            "ordered-list",
            json!(default_econ.profiles[&crate::drv_econ::Role::Research]),
            json!(null),
            json!(null),
            true,
            candidate_fields(),
        ),
        setting(
            "econ.reasoning.light",
            "routing",
            "Light reasoning effort",
            "Set preferred effort; a harness may move up to its nearest supported effort. Example: high requests high or stronger reasoning.",
            "enum",
            json!("low"),
            json!(["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "econ.reasoning.deep",
            "routing",
            "Deep reasoning effort",
            "Set preferred effort; a harness may move up to its nearest supported effort. Example: high requests high or stronger reasoning.",
            "enum",
            json!("high"),
            json!(["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "econ.reasoning.max",
            "routing",
            "Max reasoning effort",
            "Set preferred effort; a harness may move up to its nearest supported effort. Example: high requests high or stronger reasoning.",
            "enum",
            json!("max"),
            json!(["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "sources.*.id",
            "sources",
            "Source name",
            "Name this context source so tasks can refer to it. Example: handbook.",
            "string",
            json!(""),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "sources.*.kind",
            "sources",
            "Source kind",
            "Choose a local folder, Git repository or UNVRS memory. Example: path reads a local folder.",
            "enum",
            json!("path"),
            json!(["path", "git", "memory"]),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "sources.*.uri",
            "sources",
            "Source location",
            "Point to the folder or Git URL to read. Example: /Users/me/handbook.",
            "path",
            json!(""),
            json!(null),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "sources.*.purpose",
            "sources",
            "What this source contains",
            "Describe when this source is useful. Example: team operating instructions.",
            "string",
            json!(""),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "sources.*.exclude",
            "sources",
            "Excluded paths",
            "Keep matching paths out of context results. Example: private/** hides files in private.",
            "list",
            json!([]),
            json!(null),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "sources.*.sensitivity",
            "sources",
            "Source privacy",
            "Set the source disclosure level. Example: private treats its contents as private context.",
            "enum",
            json!("private"),
            json!(["public", "private", "secret"]),
            json!(null),
            true,
            json!(null),
        ),
        setting(
            "sources.*.branch",
            "sources",
            "Git branch",
            "Select a branch for a Git source; null uses its default branch. Example: main.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "sources.*.refresh",
            "sources",
            "Git refresh interval",
            "Set how often a Git source pulls updates; null uses one hour. Example: 30m refreshes every half hour.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.id",
            "projects",
            "Project name",
            "Name the project. Example: unvrs-rs.",
            "string",
            json!(""),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.purpose",
            "projects",
            "Project purpose",
            "Describe the work this project owns. Example: maintain the kernel.",
            "string",
            json!(""),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.sources",
            "projects",
            "Project context sources",
            "List context sources granted to this project. Example: [\"handbook\"].",
            "list",
            json!([]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.repos",
            "projects",
            "Project repositories",
            "List repositories associated with this project. Example: [\"/Users/me/kernel\"].",
            "list",
            json!([]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.links",
            "projects",
            "Project links",
            "List reference links for this project. Example: [\"https://example.com/spec\"].",
            "list",
            json!([]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.created",
            "projects",
            "Project creation time",
            "Record when the project was created, in Unix milliseconds. Example: 1790890000000.",
            "int",
            json!(0),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "projects.*.created_by",
            "projects",
            "Project creator",
            "Record who created this project. Example: captain.",
            "string",
            json!(""),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.watchdog_grace_ms",
            "safety",
            "Watchdog grace",
            "Wait this many extra milliseconds for periodic work before reporting a fault. Example: 120000.",
            "int",
            json!(120000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.watchdog_failures",
            "safety",
            "Failures before alert",
            "Alert after this many consecutive failed completions; startup faults alert immediately. Example: 3.",
            "int",
            json!(3),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.driver_silence_ms",
            "safety",
            "Driver watchdog silence",
            "Alert when a managed driver produces no lifecycle event within this budget. Example: 120000.",
            "int",
            json!(120000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.max_stalls",
            "safety",
            "Stalled attempts before ending",
            "End a worker lineage after this many stalled attempts. Example: 2.",
            "int",
            json!(2),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.stall_turns",
            "safety",
            "Turns without progress",
            "Stop an attempt after this many consecutive turns change neither the brief nor the worktree HEAD. Example: 3.",
            "int",
            json!(3),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.max_continuations",
            "safety",
            "Worker continuation ceiling",
            "Limit automatic continuations of a worker lineage; null means unbounded. Example: null.",
            "int",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.turn_idle_secs",
            "safety",
            "Turn silence limit",
            "End a harness turn after this many seconds without output. Example: 1200.",
            "int",
            json!(1200),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.turn_max_secs",
            "safety",
            "Turn time limit",
            "End a harness turn after this many total seconds, even while it produces output. Example: 14400.",
            "int",
            json!(14400),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.fold_budget_ms",
            "kernel",
            "Memory fold watchdog budget",
            "Allow this many milliseconds for a worker memory fold to finish. Example: 150000.",
            "int",
            json!(150000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.summary_startup_ms",
            "kernel",
            "Summary startup watchdog budget",
            "Allow this many milliseconds for summary-harness preflight. Example: 150000.",
            "int",
            json!(150000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.quota_interval_secs",
            "kernel",
            "Quota polling interval",
            "Read harness quota on this cadence, in seconds. Example: 600.",
            "int",
            json!(600),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.memory_interval_secs",
            "kernel",
            "Memory sweep interval",
            "Run memory maintenance on this cadence, in seconds. Example: 3600.",
            "int",
            json!(3600),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.maintenance_interval_ms",
            "kernel",
            "Maintenance interval",
            "Check worker and seat state on this cadence, in milliseconds. Example: 500.",
            "int",
            json!(500),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.idle_secs",
            "kernel",
            "Kernel idle exit",
            "Exit an idle kernel after this many seconds; launchd mode disables idle exit. Example: 1800.",
            "int",
            json!(1800),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_port",
            "kernel",
            "Observatory port",
            "Serve the Observatory on this loopback port; off or 0 disables it. Example: \"7576\".",
            "string",
            json!("7576"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.notify",
            "kernel",
            "Desktop notifications",
            "Show a desktop notification when the captain needs to act; UNVRS_NOTIFY=0 disables it. Example: true.",
            "bool",
            json!(true),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.quota_enabled",
            "kernel",
            "Quota polling enabled",
            "Poll harness quota unless UNVRS_QUOTA is exactly 0. Example: true.",
            "bool",
            json!(true),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.open_apps",
            "kernel",
            "Open captain apps",
            "Open the captain app unless UNVRS_NO_OPEN is present. Example: true.",
            "bool",
            json!(true),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.checkpoint_secs",
            "kernel",
            "Checkpoint command budget",
            "Give each checkpoint Git command this many seconds to finish. Example: 600.",
            "int",
            json!(600),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.checkpoint_commands",
            "kernel",
            "Checkpoint Git commands",
            "Allow this many command budgets in the checkpoint watchdog. Example: 7.",
            "int",
            json!(7),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.checkpoint_grace_ms",
            "kernel",
            "Checkpoint extra grace",
            "Add this many milliseconds of watchdog grace to checkpoint commands. Example: 5000.",
            "int",
            json!(5000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.seat_retry_base_ms",
            "kernel",
            "First seat retry delay",
            "Start retrying a failed detached seat after this delay, in milliseconds. Example: 30000.",
            "int",
            json!(30000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.seat_retry_max_ms",
            "kernel",
            "Longest seat retry delay",
            "Cap exponential detached-seat retry backoff at this many milliseconds. Example: 900000.",
            "int",
            json!(900000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.thread_idle_unknown_ms",
            "kernel",
            "Unknown thread idle age",
            "Treat a thread with unknown activity as idle after this many milliseconds. Example: 1800000.",
            "int",
            json!(1800000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.recent_events",
            "kernel",
            "Recent events kept",
            "Keep this many recent events in the in-memory kernel tail. Example: 400.",
            "int",
            json!(400),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.quota_budget_ms",
            "kernel",
            "Quota watchdog budget",
            "Allow this many milliseconds for one quota refresh to finish. Example: 60000.",
            "int",
            json!(60000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.memory_budget_ms",
            "kernel",
            "Memory watchdog budget",
            "Allow this many milliseconds for a memory sweep to finish. Example: 120000.",
            "int",
            json!(120000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.deploy_drain_secs",
            "kernel",
            "Deployment drain deadline",
            "Wait this many seconds for work to drain by default; --drain-secs overrides one deployment. Example: 900.",
            "int",
            json!(900),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.deploy_poll_secs",
            "kernel",
            "Deployment polling interval",
            "Check draining workers every this many seconds. Example: 2.",
            "int",
            json!(2),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.socket_read_secs",
            "kernel",
            "Socket read timeout",
            "Wait this many seconds for a Unix socket request to arrive. Example: 10.",
            "int",
            json!(10),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.kernel_start_secs",
            "kernel",
            "Kernel startup deadline",
            "Wait this many seconds for the daemon to start in normal CLI calls. Example: 8.",
            "int",
            json!(8),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.request_secs",
            "kernel",
            "CLI request timeout",
            "Wait this many seconds for ordinary kernel status and control replies. Example: 5.",
            "int",
            json!(5),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.kernel_watchdog_ms",
            "kernel",
            "Watchdog sweep interval",
            "Check job watchdog deadlines this often, in milliseconds. Example: 500.",
            "int",
            json!(500),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.catalog_watch_secs",
            "kernel",
            "Catalog scheduler polling",
            "Check whether catalog refresh is due every this many seconds. Example: 1.",
            "int",
            json!(1),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hook_watch_secs",
            "kernel",
            "Hook watch lifetime",
            "Keep a hook watcher alive for this many seconds before it exits. Example: 86400.",
            "int",
            json!(86400),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.summary_secs",
            "kernel",
            "Summary turn timeout",
            "Give a subscription summary harness turn this many seconds. Example: 60.",
            "int",
            json!(60),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.recovery_summary_bytes",
            "kernel",
            "Recovery summary tail",
            "Include at most this many bytes of worker output in a recovery summary. Example: 12000.",
            "int",
            json!(12000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.compaction_secs",
            "kernel",
            "Compaction timeout",
            "Give the compaction worker this many seconds before terminating it. Example: 90.",
            "int",
            json!(90),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_fold_secs",
            "kernel",
            "Brief fold timeout",
            "Give the brief-fold worker this many seconds before terminating it. Example: 120.",
            "int",
            json!(120),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.acp_start_secs",
            "kernel",
            "ACP startup deadline",
            "Wait this many seconds for an ACP adapter to become ready. Example: 45.",
            "int",
            json!(45),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.catalog_discovery_secs",
            "kernel",
            "Catalog discovery timeout",
            "Give each harness catalog discovery this many seconds. Example: 15.",
            "int",
            json!(15),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.codex_cleanup_secs",
            "kernel",
            "Codex cleanup deadline",
            "Wait at most this many seconds for a started Codex task to clean up. Example: 15.",
            "int",
            json!(15),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.codex_cancel_secs",
            "kernel",
            "Codex cancel deadline",
            "Wait this many seconds after requesting cancellation before escalating. Example: 2.",
            "int",
            json!(2),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_bytes",
            "kernel",
            "Hot context budget",
            "Cap generated hot context at this many bytes. Example: 9000.",
            "int",
            json!(crate::HOT_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_brief_bytes",
            "kernel",
            "Hot brief budget",
            "Reserve at most this many bytes for the brief in hot context. Example: 2400.",
            "int",
            json!(2400),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_memory_bytes",
            "kernel",
            "Hot memory budget",
            "Reserve at most this many bytes for memory in hot context. Example: 2200.",
            "int",
            json!(2200),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_calls_bytes",
            "kernel",
            "Hot calls budget",
            "Reserve at most this many bytes for calls in hot context. Example: 1200.",
            "int",
            json!(1200),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_wakes_bytes",
            "kernel",
            "Hot wakes budget",
            "Reserve at most this many bytes for queued wakes in hot context. Example: 1600.",
            "int",
            json!(1600),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_projects_bytes",
            "kernel",
            "Hot projects budget",
            "Reserve at most this many bytes for project summaries in hot context. Example: 1200.",
            "int",
            json!(1200),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_sources_bytes",
            "kernel",
            "Hot sources budget",
            "Reserve at most this many bytes for source summaries in hot context. Example: 900.",
            "int",
            json!(900),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.hot_work_bytes",
            "kernel",
            "Hot work budget",
            "Reserve at most this many bytes for work summaries in hot context. Example: 700.",
            "int",
            json!(700),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_map_bytes",
            "kernel",
            "Context map page size",
            "Return at most this many bytes per context map page. Example: 4000.",
            "int",
            json!(crate::CTX_MAP_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_get_bytes",
            "kernel",
            "Context fetch page size",
            "Return at most this many bytes per context fetch page. Example: 12000.",
            "int",
            json!(crate::CTX_GET_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_snippet_bytes",
            "kernel",
            "Context search snippet",
            "Return at most this many bytes for a context search snippet. Example: 240.",
            "int",
            json!(crate::CTX_SNIPPET),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_max_file_bytes",
            "kernel",
            "Largest context file",
            "Skip context files larger than this many bytes. Example: 2097152.",
            "int",
            json!(2097152),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_max_files",
            "kernel",
            "Context scan file limit",
            "Scan at most this many files in one source. Example: 20000.",
            "int",
            json!(20000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_binary_probe_bytes",
            "kernel",
            "Context binary probe",
            "Inspect this many initial bytes to decide whether a file is binary. Example: 8192.",
            "int",
            json!(8192),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_default_results",
            "kernel",
            "Default search results",
            "Return this many context search hits when no count is supplied. Example: 8.",
            "int",
            json!(8),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_max_results",
            "kernel",
            "Maximum search results",
            "Return at most this many context search hits. Example: 20.",
            "int",
            json!(20),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_git_refresh_secs",
            "kernel",
            "Default Git source refresh",
            "Pull Git source updates after this many seconds when refresh is absent. Example: 3600.",
            "int",
            json!(3600),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.ctx_default_exclude",
            "safety",
            "Default context exclusions",
            "Always exclude these generated folders from context scanning. Example: [\"**/.git/**\", \"**/node_modules/**\", \"**/target/**\"].",
            "list",
            json!(["**/.git/**", "**/node_modules/**", "**/target/**"]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.ctx_secret_files",
            "safety",
            "Secret context files",
            "Never return files matching these secret-file patterns. Example: [\".env\", \".env.*\", \"*.pem\", \"id_rsa*\", \"*.key\"].",
            "list",
            json!([".env", ".env.*", "*.pem", "id_rsa*", "*.key"]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "safety.ctx_memory_exclude",
            "safety",
            "Memory context exclusions",
            "Keep source clones and caches out of the built-in memory source. Example: [\"sources/**\", \"cache/**\"].",
            "list",
            json!(["sources/**", "cache/**"]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.ctx_entry_names",
            "kernel",
            "Context entry files",
            "Look for these entry files when mapping a source. Example: [\"CONTEXT-MAP.md\", \"CONTEXT.md\", \"AGENTS.md\", \"README.md\"].",
            "list",
            json!(["CONTEXT-MAP.md", "CONTEXT.md", "AGENTS.md", "README.md"]),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.boot_nest_cap",
            "kernel",
            "Skill nesting limit",
            "Expand skill nesting up to this many levels. Example: 3.",
            "int",
            json!(crate::NEST_CAP),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.boot_shortlist",
            "kernel",
            "Skill shortlist size",
            "Keep at most this many candidate skills before final selection. Example: 12.",
            "int",
            json!(12),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.boot_intent_score",
            "kernel",
            "Intent matching score",
            "Use this score for recognized task-intent matches. Example: 5.",
            "int",
            json!(5),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.boot_rules_min",
            "kernel",
            "Minimum skill rule score",
            "Require this score for a skill rule match. Example: 5.",
            "int",
            json!(5),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.boot_api_secs",
            "kernel",
            "Skill API deadline",
            "Give the skill-selection API this many seconds. Example: 20.",
            "int",
            json!(20),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_decisions",
            "kernel",
            "Brief decisions kept",
            "Keep at most this many decisions in a bounded brief. Example: 8.",
            "int",
            json!(crate::BRIEF_DECISIONS),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_done",
            "kernel",
            "Brief completed items kept",
            "Keep at most this many completed items in a bounded brief. Example: 12.",
            "int",
            json!(crate::BRIEF_DONE),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_next",
            "kernel",
            "Brief next steps kept",
            "Keep at most this many next steps in a bounded brief. Example: 3.",
            "int",
            json!(crate::BRIEF_NEXT),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_list",
            "kernel",
            "Brief list limit",
            "Keep at most this many items in a general brief list. Example: 12.",
            "int",
            json!(crate::BRIEF_LIST),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_item_bytes",
            "kernel",
            "Brief item size",
            "Limit each brief item to this many bytes. Example: 400.",
            "int",
            json!(crate::BRIEF_ITEM_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.brief_narrative_bytes",
            "kernel",
            "Brief narrative size",
            "Limit the brief narrative to this many bytes. Example: 1200.",
            "int",
            json!(crate::BRIEF_NARRATIVE_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.memory_aging_days",
            "kernel",
            "Memory aging interval",
            "Age memory notes after this many days. Example: 30.",
            "int",
            json!(crate::knowledge::AGING_DAYS),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.memory_perishable_days",
            "kernel",
            "Perishable memory age",
            "Expire perishable memory after this many days. Example: 7.",
            "int",
            json!(crate::knowledge::PERISHABLE_DAYS),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.memory_note_bytes",
            "kernel",
            "Memory note size",
            "Limit a stored memory note to this many bytes. Example: 2000.",
            "int",
            json!(crate::knowledge::NOTE_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.universe_pin_bytes",
            "kernel",
            "Global pinned memory budget",
            "Limit global pinned memory to this many bytes. Example: 4096.",
            "int",
            json!(crate::UNIVERSE_PIN_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.scope_pin_bytes",
            "kernel",
            "Project pinned memory budget",
            "Limit project or seat pinned memory to this many bytes. Example: 12288.",
            "int",
            json!(crate::SCOPE_PIN_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.tail_bytes",
            "kernel",
            "Memory turn tail budget",
            "Fold turn tails after this many bytes. Example: 16384.",
            "int",
            json!(crate::TAIL_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.keep_tail_bytes",
            "kernel",
            "Memory tail retained",
            "Keep this many bytes of turn tail after folding. Example: 4096.",
            "int",
            json!(crate::KEEP_TAIL_BYTES),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_tick_ms",
            "kernel",
            "Observatory refresh interval",
            "Send connected Observatory pages fresh snapshots on this cadence. Example: 1000.",
            "int",
            json!(1000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_usage_ttl_ms",
            "kernel",
            "Claude usage refresh",
            "Cache Claude usage for this many milliseconds. Example: 180000.",
            "int",
            json!(180000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_usage_stale_ms",
            "kernel",
            "Claude usage stale age",
            "Mark Claude usage stale after this many milliseconds. Example: 1800000.",
            "int",
            json!(1800000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_disk_ttl_ms",
            "kernel",
            "Disk probe refresh",
            "Cache disk readings for this many milliseconds. Example: 10000.",
            "int",
            json!(10000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_state_ttl_ms",
            "kernel",
            "State probe refresh",
            "Cache kernel state readings for this many milliseconds. Example: 2000.",
            "int",
            json!(2000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_journal_ttl_ms",
            "kernel",
            "Journal probe refresh",
            "Cache journal readings for this many milliseconds. Example: 2000.",
            "int",
            json!(2000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_procs_ttl_ms",
            "kernel",
            "Process probe refresh",
            "Cache process readings for this many milliseconds. Example: 10000.",
            "int",
            json!(10000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_ws_ttl_ms",
            "kernel",
            "Worktree probe refresh",
            "Cache worktree readings for this many milliseconds. Example: 60000.",
            "int",
            json!(60000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_output_ttl_ms",
            "kernel",
            "Output probe refresh",
            "Cache worker output readings for this many milliseconds. Example: 2000.",
            "int",
            json!(2000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_size_ttl_ms",
            "kernel",
            "Worktree size refresh",
            "Cache worktree sizes for this many milliseconds. Example: 900000.",
            "int",
            json!(900000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_size_retry_ms",
            "kernel",
            "Worktree size retry",
            "Retry a failed worktree size scan after this many milliseconds. Example: 60000.",
            "int",
            json!(60000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_upstream_ms",
            "kernel",
            "Observatory upstream deadline",
            "Limit an upstream Observatory request to this many milliseconds. Example: 1500.",
            "int",
            json!(1500),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.observatory_size_parallel",
            "kernel",
            "Parallel size probes",
            "Run at most this many worktree size probes in parallel. Example: 4.",
            "int",
            json!(4),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.transcript_tail_bytes",
            "kernel",
            "Transcript tail probe",
            "Read at most this many bytes of a thread transcript tail. Example: 262144.",
            "int",
            json!(262144),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.output_tail_bytes",
            "kernel",
            "Worker output tail probe",
            "Read at most this many bytes of worker output per probe. Example: 1048576.",
            "int",
            json!(1048576),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.task_text_bytes",
            "kernel",
            "Task text limit",
            "Accept task text up to this many bytes. Example: 8000.",
            "int",
            json!(8000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.handoff_bytes",
            "kernel",
            "Handoff package limit",
            "Accept handoff packages up to this many bytes. Example: 32000.",
            "int",
            json!(32000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "kernel.call_text_bytes",
            "kernel",
            "Call text limit",
            "Accept captain call text up to this many bytes. Example: 2000.",
            "int",
            json!(2000),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.HOME",
            "overrides",
            "User home",
            "Choose the base folder for default UNVRS, harness and LaunchAgent paths. Example: /Users/me.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.PATH",
            "overrides",
            "Executable search path",
            "Choose folders searched for installed harnesses and command links. Example: /usr/local/bin:/usr/bin.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.SHELL",
            "overrides",
            "Setup shell",
            "Choose which default startup file setup uses. Example: /bin/zsh selects .zshrc.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.CODEX_HOME",
            "overrides",
            "Codex configuration folder",
            "Choose the Codex configuration and plugin folder. Example: /Users/me/.codex.",
            "string",
            json!("~/.codex"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.CLAUDE_CONFIG_DIR",
            "overrides",
            "Claude configuration folder",
            "Choose the Claude configuration folder used for plugins and usage readings. Example: /Users/me/.claude.",
            "string",
            json!("~/.claude"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.CODEX_THREAD_ID",
            "overrides",
            "Codex thread identity",
            "Supply the harness thread hint to socket commands and hooks. Example: a Codex thread UUID.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.CLAUDE_CODE_SESSION_ID",
            "overrides",
            "Claude session identity",
            "Supply the harness session hint to socket commands and hooks. Example: a Claude session UUID.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.CLAUDECODE",
            "overrides",
            "Claude process marker",
            "Mark a model-owned environment so setup refuses captain steps. Example: 1.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.TYPESAFE_API_URL",
            "overrides",
            "Skill API endpoint",
            "Choose the optional skill-selection API endpoint. Example: https://api.typesafe.ai/v1/systemone.",
            "string",
            json!("https://api.typesafe.ai/v1/systemone"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.TYPESAFE_API_KEY",
            "overrides",
            "Skill API credential",
            "Authenticate the optional skill-selection API; its value is always redacted. Example: a credential set in the environment.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_HOME",
            "overrides",
            "UNVRS home",
            "Choose where the kernel stores configuration, projects and state. Example: ~/.unvrs.",
            "string",
            json!("~/.unvrs"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_DAEMON",
            "overrides",
            "Daemon mode",
            "Use launchd to keep the kernel alive instead of exiting when idle. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_KERNEL_IDLE_SECS",
            "overrides",
            "Kernel idle override",
            "Override seconds before an idle kernel exits; invalid numbers use 1800. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_OBSERVATORY_PORT",
            "overrides",
            "Observatory port override",
            "Choose a local port; off or 0 disables the server. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_NOTIFY",
            "overrides",
            "Notifications override",
            "Set exactly 0 to disable desktop notifications. Example: 0.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_NO_OPEN",
            "overrides",
            "App opening override",
            "Disable automatic opening of captain apps when this variable is present. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_QUOTA",
            "overrides",
            "Quota polling override",
            "Set exactly 0 to disable the quota polling driver. Example: 0.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_MAX_CONTINUATIONS",
            "overrides",
            "Continuation ceiling override",
            "Set an optional worker continuation ceiling; unset, none or unlimited means unbounded. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_STALL_TURNS",
            "overrides",
            "Stalled turns override",
            "Set a positive number of no-progress turns; invalid or zero uses 3. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CHECKPOINT_SECS",
            "overrides",
            "Checkpoint budget override",
            "Set a positive Git command budget in seconds; invalid or zero uses 600. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_TURN_IDLE_SECS",
            "overrides",
            "Turn silence override",
            "Set a positive output-silence limit in seconds; invalid or zero uses 1200. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_TURN_MAX_SECS",
            "overrides",
            "Turn lifetime override",
            "Set a positive total turn limit in seconds; invalid or zero uses 14400. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_L3_CODEX_MODEL",
            "overrides",
            "Codex worker model override",
            "This exact model pin bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_L3_CLAUDE_MODEL",
            "overrides",
            "Claude worker model override",
            "This exact model pin bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_SEAT_CODEX_MODEL",
            "overrides",
            "Codex seat model override",
            "This exact model pin bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_SEAT_CLAUDE_MODEL",
            "overrides",
            "Claude seat model override",
            "This exact model pin bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CODEX_BIN",
            "overrides",
            "Codex executable",
            "Choose the Codex harness executable for discovery and headless turns. Example: codex.",
            "string",
            json!("codex"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CLAUDE_BIN",
            "overrides",
            "Claude executable",
            "Choose the Claude harness executable for discovery and headless turns. Example: claude.",
            "string",
            json!("claude"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_SUMMARY_ROUTES",
            "overrides",
            "Summary harness choices",
            "Choose ordered subscription harness/model pairs for memory summaries. Example: codex=gpt-5.6-luna,claude=claude-sonnet-5-5.",
            "string",
            json!("codex=gpt-5.6-luna,claude=claude-sonnet-5-5"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_COMPACTION_CPU",
            "overrides",
            "Compaction executable",
            "Choose the executable that compacts turn tails. Example: pi.",
            "string",
            json!("pi"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_COMPACTION_MODEL",
            "overrides",
            "Compaction model",
            "Choose the model used for turn-tail compaction. Example: gpt-5.6-luna.",
            "string",
            json!("gpt-5.6-luna"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_POOL_CACHE",
            "overrides",
            "Skill pool cache",
            "Choose where fetched skill pools are cached. Example: ~/.cache/unvrs/pools.",
            "string",
            json!("~/.cache/unvrs/pools"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_REPO",
            "overrides",
            "Deployment repository",
            "Choose the repository used by deployment unless --repo is supplied. Example: ~/github/unvrs-rs.",
            "string",
            json!("~/github/unvrs-rs"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_LAUNCHAGENTS_DIR",
            "overrides",
            "LaunchAgent directory",
            "Choose where installation writes the kernel LaunchAgent. Example: ~/Library/LaunchAgents.",
            "string",
            json!("~/Library/LaunchAgents"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_LAUNCHD_LABEL",
            "overrides",
            "LaunchAgent label",
            "Choose the launchd service label; an empty value uses the default. Example: dev.unvrs.kernel.",
            "string",
            json!("dev.unvrs.kernel"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_T3_STATE",
            "overrides",
            "T3 state database",
            "Choose the T3 database used to detect live threads. Example: ~/.t3/userdata/state.sqlite.",
            "string",
            json!("~/.t3/userdata/state.sqlite"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_DRIVEN_PID",
            "overrides",
            "Driven worker identity",
            "Mark a process as an owned driven worker; this affects caller classification and deploy refusal. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_PID",
            "overrides",
            "ACP owner identity",
            "Supply the fallback owning PID to an ACP adapter. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_SOCKET",
            "overrides",
            "ACP socket",
            "Choose the kernel socket for an ACP seat connection. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_TOKEN",
            "overrides",
            "ACP credential",
            "Authenticate an ACP seat connection; its value is always redacted. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_ROLE",
            "overrides",
            "ACP seat role",
            "Select the role announced by an ACP seat. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_HARNESS",
            "overrides",
            "ACP harness",
            "Choose the ACP harness profile. Example: pi.",
            "string",
            json!("pi"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_ACP",
            "overrides",
            "Default ACP executable",
            "Choose the default ACP adapter executable. Example: pi-acp.",
            "string",
            json!("pi-acp"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CODEX_ACP",
            "overrides",
            "Codex ACP executable",
            "Choose the Codex ACP adapter executable. Example: codex-acp.",
            "string",
            json!("codex-acp"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CLAUDE_ACP",
            "overrides",
            "Claude ACP executable",
            "Choose the Claude ACP adapter executable. Example: claude-agent-acp.",
            "string",
            json!("claude-agent-acp"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CURSOR_ACP",
            "overrides",
            "Cursor ACP executable",
            "Choose the Cursor ACP adapter executable. Example: agent.",
            "string",
            json!("agent"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_MODEL",
            "overrides",
            "Default ACP model",
            "Choose a model for the default ACP profile; this bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CODEX_MODEL",
            "overrides",
            "Codex ACP model",
            "Choose a model for the Codex ACP profile; this bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CLAUDE_MODEL",
            "overrides",
            "Claude ACP model",
            "Choose a model for the Claude ACP profile; this bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_CURSOR_MODEL",
            "overrides",
            "Cursor ACP model",
            "Choose a model for the Cursor ACP profile; this bypasses your routing rules. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_LINK_DIR",
            "overrides",
            "Setup command directory",
            "Choose where setup links the unvrs command. Example: ~/.local/bin.",
            "string",
            json!("~/.local/bin"),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_SHELL_RC",
            "overrides",
            "Setup shell startup file",
            "Choose which shell startup file setup edits. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_SEED",
            "overrides",
            "Setup seed directory",
            "Choose a seed directory containing sources, projects and captain notes. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_BUILD_COMMIT",
            "overrides",
            "Build commit",
            "Record the compile-time commit embedded in the running kernel; runtime changes do not change it. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
        setting(
            "overrides.UNVRS_BUILD_REF",
            "overrides",
            "Build branch",
            "Record the compile-time branch embedded in the running kernel; runtime changes do not change it. Example: /path/or/value.",
            "string",
            json!(null),
            json!(null),
            json!(null),
            false,
            json!(null),
        ),
    ]
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
    }
}
fn json_toml(text: &str) -> Result<Value> {
    Ok(serde_json::to_value(
        toml::from_str::<toml::Value>(text).context("Invalid configuration file")?,
    )?)
}
fn at<'a>(mut value: &'a Value, path: &[Value]) -> Option<&'a Value> {
    for part in path {
        value = if let Some(index) = part.as_u64() {
            value.get(index as usize)?
        } else {
            value.get(part.as_str()?)?
        };
    }
    Some(value)
}
fn secret_env(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "API_KEY"]
        .iter()
        .any(|word| name.contains(word))
}
fn env_effective(setting: &Setting, env: &BTreeMap<String, String>) -> (Value, &'static str) {
    let numeric = match setting.key.as_str() {
        "kernel.idle_secs" => Some(("UNVRS_KERNEL_IDLE_SECS", false, false)),
        "kernel.checkpoint_secs" => Some(("UNVRS_CHECKPOINT_SECS", true, true)),
        "safety.stall_turns" => Some(("UNVRS_STALL_TURNS", true, true)),
        "safety.turn_idle_secs" => Some(("UNVRS_TURN_IDLE_SECS", true, true)),
        "safety.turn_max_secs" => Some(("UNVRS_TURN_MAX_SECS", true, true)),
        _ => None,
    };
    if setting.key == "kernel.idle_secs" && env.get("UNVRS_DAEMON").is_some_and(|v| v == "launchd")
    {
        return (Value::Null, "env");
    }
    if let Some((name, positive, trim)) = numeric
        && let Some(v) = env
            .get(name)
            .and_then(|v| if trim { v.trim() } else { v }.parse::<u64>().ok())
            .filter(|n| {
                (!positive || *n > 0) && (name != "UNVRS_STALL_TURNS" || *n <= u32::MAX.into())
            })
    {
        return (json!(v), "env");
    }
    match setting.key.as_str() {
        "safety.max_continuations" => {
            if let Some(v) = env.get("UNVRS_MAX_CONTINUATIONS") {
                if let Ok(n) = v.trim().parse::<u32>() {
                    return (json!(n), "env");
                }
                if matches!(v.trim(), "none" | "unlimited" | "") {
                    return (Value::Null, "env");
                }
            }
        }
        "kernel.observatory_port" => {
            if let Some(v) = env.get("UNVRS_OBSERVATORY_PORT") {
                return (json!(v), "env");
            }
        }
        "kernel.notify" | "kernel.quota_enabled" => {
            let name = if setting.key == "kernel.notify" {
                "UNVRS_NOTIFY"
            } else {
                "UNVRS_QUOTA"
            };
            if let Some(v) = env.get(name) {
                return (json!(v != "0"), "env");
            }
        }
        "kernel.open_apps" if env.contains_key("UNVRS_NO_OPEN") => return (json!(false), "env"),
        _ => {}
    }
    if let Some(name) = setting.key.strip_prefix("overrides.") {
        // Build identity is compiled into the binary, never taken from its runtime env.
        if matches!(name, "UNVRS_BUILD_COMMIT" | "UNVRS_BUILD_REF") {
            let value = if name == "UNVRS_BUILD_COMMIT" {
                crate::BUILD_COMMIT
            } else {
                crate::BUILD_REF
            };
            return (json!(value), "default");
        }
        if let Some(value) = env.get(name) {
            return (
                json!(if secret_env(name) {
                    "[redacted]"
                } else {
                    value
                }),
                "env",
            );
        }
    }
    (setting.default.clone(), "default")
}

/// Read without creating/migrating configuration files. Catalog facts are supplied by econ.
pub fn get(home: &Path, live_catalog: Value) -> Result<Snapshot> {
    get_with_env(
        home,
        live_catalog,
        &std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect(),
    )
}
fn get_with_env(
    home: &Path,
    live_catalog: Value,
    env: &BTreeMap<String, String>,
) -> Result<Snapshot> {
    let econ_text = read_optional(&home.join("econ.toml"))?;
    let econ = crate::drv_econ::Config::parse(
        econ_text
            .as_deref()
            .unwrap_or(crate::drv_econ::DEFAULT_CONFIG),
    )?;
    let econ_values = serde_json::to_value(econ)?;
    let econ_file = econ_text
        .as_deref()
        .map(json_toml)
        .transpose()?
        .unwrap_or(Value::Null);
    let sources_text = read_optional(&home.join("sources.toml"))?;
    let sources_file = sources_text
        .as_deref()
        .map(json_toml)
        .transpose()?
        .unwrap_or(Value::Null);
    let sources = crate::Sources::load(home)?;
    let templates = table();
    let mut snapshot = Snapshot {
        schema: vec![],
        values: BTreeMap::new(),
        effective_source: BTreeMap::new(),
        live_catalog,
        history: history(home)?,
    };
    let mut insert = |setting: Setting, value: Value, source: &str| {
        snapshot.values.insert(setting.key.clone(), value);
        snapshot
            .effective_source
            .insert(setting.key.clone(), source.into());
        snapshot.schema.push(setting);
    };
    for template in templates.iter().filter(|s| !s.key.contains('*')) {
        let mut setting = template.clone();
        let (value, source) = if setting.section == "routing" {
            let path = setting.toml_path.as_ref().expect("routing path");
            (
                at(&econ_values, path)
                    .cloned()
                    .unwrap_or(setting.default.clone()),
                if at(&econ_file, path).is_some() {
                    "file"
                } else {
                    "default"
                },
            )
        } else {
            env_effective(&setting, env)
        };
        if matches!(setting.section.as_str(), "kernel" | "safety") {
            setting.apply = "restart".into();
        }
        insert(setting, value, source);
    }
    for (index, rec) in sources.list().iter().enumerate() {
        let effective = serde_json::to_value(rec)?;
        for template in templates.iter().filter(|s| s.section == "sources") {
            let field = template.key.rsplit('.').next().expect("field");
            let mut setting = template.clone();
            setting.key = template.key.replace('*', &rec.id);
            setting.file = Some("sources.toml".into());
            let path = vec![json!("source"), json!(index), json!(field)];
            setting.toml_path = Some(path.clone());
            insert(
                setting,
                effective.get(field).cloned().unwrap_or(Value::Null),
                if at(&sources_file, &path).is_some() {
                    "file"
                } else {
                    "default"
                },
            );
        }
    }
    let mut project_dirs = match fs::read_dir(home.join("projects")) {
        Ok(entries) => entries.collect::<std::io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e.into()),
    };
    project_dirs.sort_by_key(|entry| entry.file_name());
    for dir in project_dirs {
        let Some(text) = read_optional(&dir.path().join("project.toml"))? else {
            continue;
        };
        let id = dir.file_name().to_string_lossy().into_owned();
        let values = json_toml(&text)?;
        let fields = values
            .as_object()
            .context("Project configuration must be a table")?;
        let known = templates.iter().filter(|s| s.section == "projects");
        let mut names: BTreeSet<String> = known
            .clone()
            .map(|s| s.key.rsplit('.').next().unwrap().into())
            .collect();
        names.extend(fields.keys().cloned());
        for name in names {
            let template = known.clone().find(|s| s.key.ends_with(&format!(".{name}")));
            let value = fields
                .get(&name)
                .cloned()
                .unwrap_or_else(|| template.map_or(Value::Null, |s| s.default.clone()));
            let mut setting = template.cloned().unwrap_or_else(|| {
                setting(
                    &format!("projects.*.{name}"),
                    "projects",
                    &name,
                    &format!(
                        "Additional field stored in project.toml. Example: {}.",
                        value
                    ),
                    json_kind(&value),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    false,
                    Value::Null,
                )
            });
            setting.key = format!("projects.{id}.{name}");
            setting.file = Some(format!("projects/{id}/project.toml"));
            setting.toml_path = Some(vec![json!(name)]);
            insert(
                setting,
                value,
                if fields.contains_key(&name) {
                    "file"
                } else {
                    "default"
                },
            );
        }
    }
    for name in env.keys().filter(|name| name.starts_with("UNVRS_")) {
        let key = format!("overrides.{name}");
        if templates.iter().any(|s| s.key == key) {
            continue;
        }
        let explanation = if name.ends_with("_MODEL")
            && (name.starts_with("UNVRS_L3_") || name.starts_with("UNVRS_SEAT_"))
        {
            "This model override bypasses your routing rules. Example: an exact model ID pins the harness model."
        } else {
            "This environment variable is present in the kernel process; this build has no registered reader for it. Example: remove it from the launch environment to clear it."
        };
        let setting = setting(
            &key,
            "overrides",
            name,
            explanation,
            "string",
            Value::Null,
            Value::Null,
            Value::Null,
            false,
            Value::Null,
        );
        let (value, source) = env_effective(&setting, env);
        insert(setting, value, source);
    }
    Ok(snapshot)
}
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "bool",
        Value::Number(_) => "int",
        Value::Array(_) => "list",
        Value::Object(_) => "table",
        _ => "string",
    }
}

fn validate_value(
    kind: &str,
    value: &Value,
    allowed: &Value,
    range: &Value,
    nullable: bool,
) -> Result<()> {
    if nullable && value.is_null() {
        return Ok(());
    }
    let valid_type = match kind {
        "enum" | "string" | "path" => value.is_string(),
        "bool" => value.is_boolean(),
        "int" => value.as_i64().is_some() || value.as_u64().is_some(),
        "list" | "ordered-list" => value.is_array(),
        "table" => value.is_object(),
        _ => false,
    };
    ensure!(valid_type, "Expected {kind}");
    if let Some(choices) = allowed.as_array() {
        if value.is_array() {
            ensure!(
                value
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|v| choices.contains(v)),
                "Choose list items from {allowed}"
            );
        } else {
            ensure!(choices.contains(value), "Choose one of {allowed}");
        }
    }
    if range.is_object() {
        let n = value.as_u64().context("Expected a positive integer")?;
        ensure!(
            n >= range["min"].as_u64().unwrap_or(0)
                && n <= range["max"].as_u64().unwrap_or(u64::MAX),
            "Use an integer between {} and {}",
            range["min"],
            range["max"]
        );
    }
    Ok(())
}
fn validate_setting(setting: &Setting, value: &Value, confirm: bool) -> Result<()> {
    ensure!(setting.editable, "{} is read-only", setting.label);
    ensure!(
        !setting.sensitive || confirm,
        "{} needs confirmation; send confirm=true",
        setting.label
    );
    validate_value(
        &setting.kind,
        value,
        &setting.allowed,
        &setting.range,
        setting.default.is_null(),
    )?;
    if let Some(fields) = setting.fields.as_object() {
        for row in value.as_array().context("Expected a list of rows")? {
            let row = row.as_object().context("Each row must be an object")?;
            for name in row.keys() {
                ensure!(fields.contains_key(name), "Unknown row field: {name}");
            }
            for (name, spec) in fields {
                let required = spec["required"].as_bool().unwrap_or(false);
                if let Some(v) = row.get(name) {
                    validate_value(
                        spec["type"].as_str().unwrap_or("string"),
                        v,
                        &spec["allowed"],
                        &Value::Null,
                        !required,
                    )?;
                } else {
                    ensure!(!required, "Each row needs {name}");
                }
            }
        }
    } else if setting.kind == "list" {
        ensure!(
            value.as_array().unwrap().iter().all(Value::is_string),
            "Each list item must be text"
        );
    }
    Ok(())
}

/// Serialize configuration writers for one home, including legacy config migrations.
/// ponytail: one file lock per home; use per-file locks if configuration write throughput matters.
pub(crate) fn write_lock(home: &Path) -> Result<File> {
    fs::create_dir_all(home.join("kernel/settings"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("kernel/settings/write.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0,
        "Cannot lock settings: {}",
        std::io::Error::last_os_error()
    );
    Ok(lock)
}
/// Local authorization credential. Never include this value in API responses or errors.
pub(crate) fn local_token(home: &Path) -> Result<String> {
    let _lock = write_lock(home)?;
    let path = home.join("kernel/settings.token");
    if !path.exists() {
        let mut bytes = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        atomic(&path, token.as_bytes())?;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .context("Cannot open the local settings credential")?;
    ensure!(
        file.metadata()?.is_file(),
        "Local settings credential must be a regular file"
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut token = String::new();
    file.take(65).read_to_string(&mut token)?;
    ensure!(
        token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        "Local settings credential is invalid; settings writes are unavailable"
    );
    Ok(token)
}
pub(crate) fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Settings file needs a parent folder")?;
    fs::create_dir_all(parent)?;
    let temp = path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id(),
        crate::now_ms()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        if let Ok(meta) = fs::metadata(path) {
            file.set_permissions(fs::Permissions::from_mode(
                meta.permissions().mode() & 0o777,
            ))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("Cannot save {}", path.display()))
}

pub fn history(home: &Path) -> Result<Vec<Change>> {
    let text = read_optional(&home.join("kernel/settings/history.jsonl"))?.unwrap_or_default();
    let mut changes = vec![];
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)
            .context("Settings history is damaged; restore it from backup")?;
        if value["kind"] == "config.changed" {
            changes.push(serde_json::from_value(value).context("Invalid settings history record")?);
        }
    }
    changes.reverse();
    Ok(changes)
}

fn value_from_json(value: &Value) -> Result<toml_edit::Value> {
    Ok(match value {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => n.as_i64().context("Integer is too large for TOML")?.into(),
        Value::Array(values) => {
            let mut array = toml_edit::Array::new();
            for value in values {
                array.push(value_from_json(value)?);
            }
            array.into()
        }
        Value::Object(values) => {
            let mut table = toml_edit::InlineTable::new();
            for (key, value) in values {
                if !value.is_null() {
                    table.insert(key, value_from_json(value)?);
                }
            }
            table.into()
        }
        Value::Null => bail!("TOML does not support a null list item"),
    })
}
fn update_value(old: &mut toml_edit::Value, value: &Value) -> Result<()> {
    if let (Some(array), Some(values)) = (old.as_array_mut(), value.as_array()) {
        while array.len() > values.len() {
            array.remove(array.len() - 1);
        }
        for (index, value) in values.iter().enumerate() {
            if let Some(old) = array.get_mut(index) {
                update_value(old, value)?;
            } else {
                array.push(value_from_json(value)?);
            }
        }
        return Ok(());
    }
    if let (Some(table), Some(values)) = (old.as_inline_table_mut(), value.as_object()) {
        let remove: Vec<_> = table
            .iter()
            .filter(|(key, _)| values.get(*key).is_none_or(Value::is_null))
            .map(|(key, _)| key.to_owned())
            .collect();
        for key in remove {
            table.remove(&key);
        }
        for (key, value) in values.iter().filter(|(_, v)| !v.is_null()) {
            if let Some(old) = table.get_mut(key) {
                update_value(old, value)?;
            } else {
                table.insert(key, value_from_json(value)?);
            }
        }
        return Ok(());
    }
    let decor = old.decor().clone();
    *old = value_from_json(value)?;
    *old.decor_mut() = decor;
    Ok(())
}
fn update_table(table: &mut toml_edit::Table, value: &Value) -> Result<()> {
    let values = value.as_object().context("Expected a table")?;
    let remove: Vec<_> = table
        .iter()
        .filter(|(key, _)| values.get(*key).is_none_or(Value::is_null))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in remove {
        table.remove(&key);
    }
    for (key, value) in values.iter().filter(|(_, v)| !v.is_null()) {
        update_item(table.entry(key).or_insert(Item::None), value)?;
    }
    Ok(())
}
fn update_item(item: &mut Item, value: &Value) -> Result<()> {
    match item {
        Item::Table(table) if value.is_object() => update_table(table, value),
        Item::ArrayOfTables(tables) if value.is_array() => {
            let values = value.as_array().unwrap();
            while tables.len() > values.len() {
                tables.remove(tables.len() - 1);
            }
            for (index, value) in values.iter().enumerate() {
                if index == tables.len() {
                    tables.push(toml_edit::Table::new());
                }
                update_table(tables.get_mut(index).unwrap(), value)?;
            }
            Ok(())
        }
        Item::Value(old) => update_value(old, value),
        _ => {
            *item = Item::Value(value_from_json(value)?);
            Ok(())
        }
    }
}
fn edit_path(item: &mut Item, path: &[Value], value: &Value) -> Result<()> {
    if let Item::Value(old) = item {
        return edit_value_path(old, path, value);
    }
    let (part, rest) = path.split_first().context("Settings path is empty")?;
    if let Some(index) = part.as_u64() {
        let index = usize::try_from(index)?;
        let tables = item
            .as_array_of_tables_mut()
            .context("Expected a source table list")?;
        edit_table_path(
            tables.get_mut(index).context("Source no longer exists")?,
            rest,
            value,
        )
    } else {
        edit_table_path(
            item.as_table_mut()
                .context("Expected a configuration table")?,
            path,
            value,
        )
    }
}

fn edit_value_path(old: &mut toml_edit::Value, path: &[Value], value: &Value) -> Result<()> {
    let (part, rest) = path.split_first().context("Settings path is empty")?;
    if let Some(index) = part.as_u64() {
        let item = old
            .as_array_mut()
            .context("Expected a list")?
            .get_mut(usize::try_from(index)?)
            .context("Source no longer exists")?;
        if rest.is_empty() {
            update_value(item, value)
        } else {
            edit_value_path(item, rest, value)
        }
    } else {
        let key = part.as_str().context("Invalid settings path")?;
        let table = old
            .as_inline_table_mut()
            .context("Expected an inline table")?;
        if rest.is_empty() {
            if value.is_null() {
                table.remove(key);
            } else if let Some(old) = table.get_mut(key) {
                update_value(old, value)?;
            } else {
                table.insert(key, value_from_json(value)?);
            }
            Ok(())
        } else {
            edit_value_path(
                table
                    .get_mut(key)
                    .context("Configuration table is missing")?,
                rest,
                value,
            )
        }
    }
}

fn edit_table_path(table: &mut toml_edit::Table, path: &[Value], value: &Value) -> Result<()> {
    let (part, rest) = path.split_first().context("Settings path is empty")?;
    let key = part.as_str().context("Invalid settings path")?;
    if rest.is_empty() {
        if value.is_null() {
            table.remove(key);
        } else {
            update_item(table.entry(key).or_insert(Item::None), value)?;
        }
        Ok(())
    } else {
        if !table.contains_key(key) {
            table.insert(key, Item::Table(toml_edit::Table::new()));
        }
        edit_path(table.get_mut(key).unwrap(), rest, value)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    #[serde(default)]
    source: Vec<crate::SourceRec>,
}
fn validate_sources(text: &str) -> Result<()> {
    let file: SourceFile = toml::from_str(text).context("Invalid sources.toml")?;
    let mut ids = BTreeSet::new();
    for rec in file.source {
        crate::ctx::validate(&rec)?;
        ensure!(
            ids.insert(rec.id.clone()),
            "Source names must be unique: {}",
            rec.id
        );
    }
    Ok(())
}

/// Validates and records one change. The kernel also mirrors the returned receipt to its journal.
pub fn set(home: &Path, key: &str, value: Value, confirm: bool, by: &str) -> Result<Change> {
    let _lock = write_lock(home)?;
    write(home, key, value, confirm, by, None)
}
pub fn revert(home: &Path, change_id: &str, confirm: bool, by: &str) -> Result<Change> {
    let _lock = write_lock(home)?;
    let change = history(home)?
        .into_iter()
        .find(|c| c.change_id == change_id)
        .context("Unknown settings change")?;
    let mut key = change.key.clone();
    if key.starts_with("sources.") && key.ends_with(".id") {
        key = format!(
            "sources.{}.id",
            change
                .after
                .as_str()
                .context("Invalid source rename receipt")?
        );
    }
    let current = get(home, Value::Null)?;
    ensure!(
        current.values.get(&key) == Some(&change.after),
        "Conflict: this setting changed again; review its history before reverting"
    );
    write(
        home,
        &key,
        change.before,
        confirm,
        by,
        Some(change_id.into()),
    )
}
fn write(
    home: &Path,
    key: &str,
    value: Value,
    confirm: bool,
    by: &str,
    reverts: Option<String>,
) -> Result<Change> {
    let snapshot = get(home, Value::Null)?;
    let setting = snapshot
        .schema
        .iter()
        .find(|s| s.key == key)
        .with_context(|| format!("Unknown setting: {key}"))?;
    validate_setting(setting, &value, confirm)?;
    let before = snapshot.values[key].clone();
    ensure!(before != value, "{} already has that value", setting.label);
    let relative = setting
        .file
        .as_ref()
        .context("Setting has no configuration file")?;
    ensure!(
        matches!(relative.as_str(), "econ.toml" | "sources.toml"),
        "This configuration file is read-only"
    );
    let path = home.join(relative);
    let original = read_optional(&path)?;
    let mut doc: DocumentMut = original
        .as_deref()
        .unwrap_or(if relative == "econ.toml" {
            crate::drv_econ::DEFAULT_CONFIG
        } else {
            ""
        })
        .parse()
        .context("Cannot edit configuration")?;
    let toml_path = setting
        .toml_path
        .as_ref()
        .context("Setting has no TOML path")?;
    if relative == "sources.toml" {
        // Materialize the implicit memory source before editing one of its fields.
        let index = toml_path[1].as_u64().context("Invalid source index")? as usize;
        if !doc.contains_key("source") {
            doc["source"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        let count = match &doc["source"] {
            Item::ArrayOfTables(tables) => tables.len(),
            Item::Value(toml_edit::Value::Array(array)) => array.len(),
            _ => bail!("Sources must be a list of tables"),
        };
        if index == count {
            let source = crate::Sources::load(home)?
                .list()
                .get(index)
                .cloned()
                .context("Source no longer exists")?;
            let value = serde_json::to_value(source)?;
            match &mut doc["source"] {
                Item::ArrayOfTables(tables) => {
                    let mut table = toml_edit::Table::new();
                    update_table(&mut table, &value)?;
                    tables.push(table);
                }
                Item::Value(toml_edit::Value::Array(array)) => array.push(value_from_json(&value)?),
                _ => unreachable!(),
            }
        }
    }
    edit_path(doc.as_item_mut(), toml_path, &value)?;
    let edited = doc.to_string();
    if relative == "econ.toml" {
        crate::drv_econ::Config::parse(&edited)?;
    } else {
        validate_sources(&edited)?;
    }
    let at = crate::now_ms();
    let change = Change {
        change_id: format!("settings-{at}-{}", snapshot.history.len() + 1),
        key: key.into(),
        before,
        after: value,
        by: by.into(),
        at,
        reverts,
    };
    let backup_dir = home.join("kernel/settings/backups");
    fs::create_dir_all(&backup_dir)?;
    let backup = backup_dir.join(format!("{}-{relative}", change.change_id));
    // Keep a durable backup even when saving the receipt or replacing the config fails.
    atomic(&backup, original.as_deref().unwrap_or("").as_bytes())?;
    let history_path = home.join("kernel/settings/history.jsonl");
    let old_history = read_optional(&history_path)?.unwrap_or_default();
    let mut receipt = serde_json::to_value(&change)?;
    receipt["kind"] = json!("config.changed");
    // ponytail: rewrite the small settings history atomically; use a transactional store if frequent edits make it large.
    let new_history = format!("{old_history}{}\n", serde_json::to_string(&receipt)?);
    let saved = atomic(&path, edited.as_bytes())
        .and_then(|()| atomic(&history_path, new_history.as_bytes()));
    if let Err(error) = saved {
        let restore = |path: &Path, original: Option<&str>| -> Result<()> {
            if read_optional(path)?.as_deref() == original {
                return Ok(());
            }
            if let Some(text) = original {
                atomic(path, text.as_bytes())
            } else {
                fs::remove_file(path).map_err(Into::into)
            }
        };
        restore(&path, original.as_deref())
            .and_then(|()| {
                restore(
                    &history_path,
                    if old_history.is_empty() {
                        None
                    } else {
                        Some(&old_history)
                    },
                )
            })
            .with_context(|| {
                format!(
                    "Settings save failed ({error:#}) and rollback failed; restore {}",
                    backup.display()
                )
            })?;
        return Err(error).context("Settings were restored because the change could not be saved");
    }
    Ok(change)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Home(std::path::PathBuf);
    impl Home {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "unvrs-settings-{}-{}-{}",
                std::process::id(),
                crate::now_ms(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn snapshot(home: &Home) -> Snapshot {
        get_with_env(&home.0, json!({"codex":{"models":[]}}), &BTreeMap::new()).unwrap()
    }
    fn covered(value: &Value, path: &str, schema: &[Setting]) {
        if let Some(setting) = schema.iter().find(|s| s.key == path) {
            if let Some(rows) = value.as_array() {
                for row in rows {
                    if let Some(fields) = row.as_object() {
                        for key in fields.keys() {
                            assert!(
                                setting.fields.get(key).is_some(),
                                "No metadata for {path}.{key}"
                            );
                            assert!(
                                setting.fields[key]["explanation"]
                                    .as_str()
                                    .unwrap()
                                    .contains("Example:")
                            );
                        }
                    }
                }
            }
            assert!(
                setting.explanation.contains("Example:"),
                "No explanation for {path}"
            );
            return;
        }
        if let Some(fields) = value.as_object() {
            for (key, value) in fields {
                covered(value, &format!("{path}.{key}"), schema);
            }
        } else {
            panic!("No registry entry for {path}");
        }
    }
    #[test]
    fn cli_requests_are_typed_and_flags_are_strict() {
        let parse =
            |args: &[&str]| command(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(parse(&[]).unwrap(), json!({"op":"settings.get"}));
        assert_eq!(parse(&["get"]).unwrap(), json!({"op":"settings.get"}));
        assert_eq!(
            parse(&["history"]).unwrap(),
            json!({"op":"settings.history"})
        );
        assert_eq!(
            parse(&["set", "econ.policy.allow_max", "true", "--confirm"]).unwrap(),
            json!({"op":"settings.set","key":"econ.policy.allow_max","value":true,"confirm":true})
        );
        assert_eq!(
            parse(&["set", "econ.reasoning.deep", r#""medium""#]).unwrap()["value"],
            "medium"
        );
        assert_eq!(
            parse(&["revert", "change-1", "--confirm"]).unwrap(),
            json!({"op":"settings.revert","change_id":"change-1","confirm":true})
        );
        for args in [
            vec!["get", "--confirm"],
            vec!["set", "key", "high"],
            vec!["set", "key", "1", "--unknown"],
            vec!["revert"],
            vec!["history", "extra"],
        ] {
            assert!(parse(&args).is_err(), "Accepted {args:?}");
        }
    }
    #[test]
    fn complete_registry_describes_serialized_econ_sources_and_projects() {
        let home = Home::new();
        fs::create_dir_all(home.0.join("projects/demo")).unwrap();
        fs::write(
            home.0.join("projects/demo/project.toml"),
            "id = 'demo'\npurpose = 'Check settings'\n[extra]\nflag = true\n",
        )
        .unwrap();
        let snapshot = snapshot(&home);
        covered(
            &serde_json::to_value(
                crate::drv_econ::Config::parse(crate::drv_econ::DEFAULT_CONFIG).unwrap(),
            )
            .unwrap(),
            "econ",
            &snapshot.schema,
        );
        let source =
            serde_json::to_value(&crate::Sources::load(&home.0).unwrap().list()[0]).unwrap();
        covered(&source, "sources.memory", &snapshot.schema);
        for field in [
            "id",
            "purpose",
            "sources",
            "repos",
            "links",
            "created",
            "created_by",
            "extra",
        ] {
            assert!(
                snapshot
                    .schema
                    .iter()
                    .any(|s| s.key == format!("projects.demo.{field}")
                        && s.explanation.contains("Example:")
                        && !s.editable)
            );
        }
        assert_eq!(snapshot.schema.len(), snapshot.values.len());
        assert_eq!(snapshot.values.len(), snapshot.effective_source.len());
        assert_eq!(snapshot.live_catalog["codex"]["models"], json!([]));
        assert!(
            !home.0.join("econ.toml").exists(),
            "GET must not create config"
        );
        assert!(
            !home.0.join("kernel").exists(),
            "GET must not create credentials or history"
        );
    }
    #[test]
    fn validation_refuses_wrong_types_safety_changes_and_invalid_whole_files() {
        let home = Home::new();
        for (key, value, confirm) in [
            ("unknown", json!(true), false),
            ("kernel.quota_interval_secs", json!(1), true),
            ("econ.policy.allow_max", json!(true), false),
            ("econ.policy.allow_max", json!("true"), true),
            ("econ.reasoning.deep", json!("invented"), true),
            ("econ.catalog.refresh_minutes", json!(0), false),
            ("econ.catalog.refresh_minutes", json!(16), false),
            ("econ.catalog.max_age_minutes", json!(-1), true),
            ("econ.policy.rules", json!([{"role":"judge"}]), true),
            (
                "econ.profiles.worker",
                json!([{"harness":"unknown","model":"newest sol"}]),
                false,
            ),
            ("sources.memory.exclude", json!([10]), true),
            ("sources.memory.sensitivity", json!("unknown"), true),
        ] {
            assert!(
                set(&home.0, key, value, confirm, "captain").is_err(),
                "Accepted {key}"
            );
        }
        assert!(!home.0.join("econ.toml").exists());
        assert!(history(&home.0).unwrap().is_empty());
        let change = set(
            &home.0,
            "econ.policy.allow_max",
            json!(true),
            true,
            "captain",
        )
        .unwrap();
        assert_eq!(change.before, false);
        assert_eq!(snapshot(&home).values["econ.policy.allow_max"], true);
        let parsed = crate::drv_econ::Config::load(&home.0).unwrap();
        assert!(
            parsed.policy.allow_max,
            "Existing readers must reload settings"
        );
    }
    #[test]
    fn comments_order_backups_and_revert_survive_real_toml_edits() {
        let home = Home::new();
        let original = crate::drv_econ::DEFAULT_CONFIG.replace(
            "deep = \"high\"",
            "# Captain preference\ndeep = \"high\" # keep this note",
        );
        fs::write(home.0.join("econ.toml"), &original).unwrap();
        let change = set(
            &home.0,
            "econ.reasoning.deep",
            json!("medium"),
            true,
            "captain",
        )
        .unwrap();
        let edited = fs::read_to_string(home.0.join("econ.toml")).unwrap();
        assert_eq!(
            edited,
            original.replace("deep = \"high\"", "deep = \"medium\"")
        );
        assert_eq!(
            fs::read_to_string(home.0.join(format!(
                "kernel/settings/backups/{}-econ.toml",
                change.change_id
            )))
            .unwrap(),
            original
        );
        set(
            &home.0,
            "econ.catalog.refresh_minutes",
            json!(6),
            false,
            "captain",
        )
        .unwrap();
        let undo = revert(&home.0, &change.change_id, true, "captain").unwrap();
        assert_eq!(undo.reverts.as_deref(), Some(change.change_id.as_str()));
        let current = snapshot(&home);
        assert_eq!(current.values["econ.reasoning.deep"], "high");
        assert_eq!(current.values["econ.catalog.refresh_minutes"], 6);
        assert_eq!(current.history.len(), 3);
        assert_eq!(current.history[0].change_id, undo.change_id);
        assert!(
            revert(&home.0, &change.change_id, true, "captain")
                .unwrap_err()
                .to_string()
                .starts_with("Conflict:")
        );
        let journal = fs::read_to_string(home.0.join("kernel/settings/history.jsonl")).unwrap();
        assert!(
            journal.lines().all(
                |line| serde_json::from_str::<Value>(line).unwrap()["kind"] == "config.changed"
            )
        );
    }
    #[test]
    fn failed_history_save_restores_the_config_and_keeps_the_backup() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        } // Root bypasses directory permissions.
        let home = Home::new();
        set(
            &home.0,
            "econ.catalog.refresh_minutes",
            json!(6),
            false,
            "captain",
        )
        .unwrap();
        let config = fs::read_to_string(home.0.join("econ.toml")).unwrap();
        let history_before = history(&home.0).unwrap();
        let directory = home.0.join("kernel/settings");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
        let result = set(
            &home.0,
            "econ.reasoning.deep",
            json!("medium"),
            true,
            "captain",
        );
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().to_string().contains("restored"));
        assert_eq!(
            fs::read_to_string(home.0.join("econ.toml")).unwrap(),
            config
        );
        assert_eq!(history(&home.0).unwrap().len(), history_before.len());
        assert_eq!(fs::read_dir(directory.join("backups")).unwrap().count(), 2);
    }
    #[test]
    fn research_candidate_settings_are_editable_and_refuse_unmarked_lower_pins() {
        let home = Home::new();
        fs::write(home.0.join("econ.toml"), crate::drv_econ::DEFAULT_CONFIG).unwrap();
        let initial = snapshot(&home);
        let setting = initial
            .schema
            .iter()
            .find(|s| s.key == "econ.profiles.research")
            .unwrap();
        assert!(setting.editable);
        let defaults = crate::drv_econ::Config::parse(crate::drv_econ::DEFAULT_CONFIG).unwrap();
        assert_eq!(
            setting.default,
            json!(defaults.profiles[&crate::drv_econ::Role::Research])
        );
        assert_eq!(setting.fields["effort"]["type"], "enum");
        assert_eq!(setting.fields["captain_fallback"]["type"], "bool");
        let original = fs::read_to_string(home.0.join("econ.toml")).unwrap();
        let mut candidates = initial.values["econ.profiles.research"].clone();
        candidates[1]["captain_fallback"] = json!(false);
        let error = set(
            &home.0,
            "econ.profiles.research",
            candidates,
            true,
            "captain",
        )
        .unwrap_err();
        assert!(error.to_string().contains("captain_fallback=true"));
        assert_eq!(
            fs::read_to_string(home.0.join("econ.toml")).unwrap(),
            original
        );
        assert!(history(&home.0).unwrap().is_empty());
        let mut candidates = initial.values["econ.profiles.research"].clone();
        candidates[1]["effort"] = json!("light");
        let change = set(
            &home.0,
            "econ.profiles.research",
            candidates.clone(),
            true,
            "captain",
        )
        .unwrap();
        assert_eq!(snapshot(&home).values["econ.profiles.research"], candidates);
        revert(&home.0, &change.change_id, true, "captain").unwrap();
        assert_eq!(
            snapshot(&home).values["econ.profiles.research"],
            initial.values["econ.profiles.research"]
        );
        // An explicit false must survive a save/revert receipt when the pin is safe.
        candidates[1]["effort"] = json!("deep");
        candidates[1]["captain_fallback"] = json!(false);
        let change = set(
            &home.0,
            "econ.profiles.research",
            candidates.clone(),
            true,
            "captain",
        )
        .unwrap();
        assert_eq!(snapshot(&home).values["econ.profiles.research"], candidates);
        revert(&home.0, &change.change_id, true, "captain").unwrap();
        let quick = initial
            .schema
            .iter()
            .find(|s| s.key == "econ.profiles.research_quick")
            .unwrap();
        assert!(quick.editable);
        assert_eq!(
            quick.default,
            json!(defaults.profiles[&crate::drv_econ::Role::ResearchQuick])
        );
        let mut candidates = initial.values["econ.profiles.research_quick"].clone();
        assert_eq!(candidates[0]["effort"], "medium");
        assert_eq!(candidates[1]["effort"], "light");
        candidates[0]["effort"] = json!("high");
        let change = set(
            &home.0,
            "econ.profiles.research_quick",
            candidates.clone(),
            true,
            "captain",
        )
        .unwrap();
        assert_eq!(
            snapshot(&home).values["econ.profiles.research_quick"],
            candidates
        );
        assert_eq!(
            snapshot(&home).values["econ.profiles.research"],
            initial.values["econ.profiles.research"]
        );
        revert(&home.0, &change.change_id, true, "captain").unwrap();
        assert_eq!(
            snapshot(&home).values["econ.profiles.research_quick"],
            initial.values["econ.profiles.research_quick"]
        );
    }

    #[test]
    fn ordered_rule_and_candidate_edits_keep_comments_on_unchanged_rows() {
        let home = Home::new();
        let text = crate::drv_econ::DEFAULT_CONFIG.replace(
            "role = \"judge\"\nreason = \"seat judgment\"",
            "role = \"judge\" # chosen profile\n# Explain this rule\nreason = \"seat judgment\"",
        );
        fs::write(home.0.join("econ.toml"), text).unwrap();
        let mut rules = snapshot(&home).values["econ.policy.rules"].clone();
        rules[0]["reason"] = json!("captain and leads");
        set(&home.0, "econ.policy.rules", rules, true, "captain").unwrap();
        let text = fs::read_to_string(home.0.join("econ.toml")).unwrap();
        assert!(text.contains("role = \"judge\" # chosen profile\n# Explain this rule\nreason = \"captain and leads\""),"{text}");
        set(
            &home.0,
            "econ.profiles.worker",
            json!([{"harness":"claude","model":"newest opus"}]),
            false,
            "captain",
        )
        .unwrap();
        assert_eq!(
            snapshot(&home).values["econ.profiles.worker"],
            json!([{"harness":"claude","model":"newest opus"}])
        );
    }
    #[test]
    fn source_validation_implicit_memory_edits_and_source_rename_revert() {
        let home = Home::new();
        let change = set(
            &home.0,
            "sources.memory.purpose",
            json!("Captain notes"),
            false,
            "captain",
        )
        .unwrap();
        assert_eq!(
            snapshot(&home).values["sources.memory.purpose"],
            "Captain notes"
        );
        revert(&home.0, &change.change_id, false, "captain").unwrap();
        assert_eq!(
            snapshot(&home).values["sources.memory.purpose"],
            change.before
        );
        let original = "# Keep source order\n[[source]]\nid = 'alpha' # identifier\nkind = 'git'\nuri = 'https://example.com/repo'\npurpose = 'References'\nsensitivity = 'private'\nrefresh = '1h'\n\n[[source]]\nid = 'beta'\nkind = 'memory'\n";
        fs::write(home.0.join("sources.toml"), original).unwrap();
        assert!(set(&home.0, "sources.alpha.id", json!("beta"), true, "captain").is_err());
        assert!(
            set(
                &home.0,
                "sources.alpha.refresh",
                json!("18446744073709551615d"),
                false,
                "captain"
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(home.0.join("sources.toml")).unwrap(),
            original
        );
        let rename = set(&home.0, "sources.alpha.id", json!("gamma"), true, "captain").unwrap();
        assert!(snapshot(&home).values.contains_key("sources.gamma.uri"));
        revert(&home.0, &rename.change_id, true, "captain").unwrap();
        assert!(snapshot(&home).values.contains_key("sources.alpha.uri"));
        assert!(
            fs::read_to_string(home.0.join("sources.toml"))
                .unwrap()
                .contains("# identifier")
        );
    }
    #[test]
    fn known_environment_readers_have_registry_entries() {
        let sources = [
            include_str!("kernel.rs"),
            include_str!("pool.rs"),
            include_str!("kernel/crew.rs"),
            include_str!("kernel/recover.rs"),
            include_str!("kernel/driven.rs"),
            include_str!("kernel/econ.rs"),
            include_str!("kernel/snapshot.rs"),
            include_str!("../../unvrs/src/cmd_kernel.rs"),
            include_str!("../../unvrs/src/cmd_deploy.rs"),
            include_str!("../../unvrs/src/cmd_setup.rs"),
            include_str!("../../drv_agent/src/headless.rs"),
            include_str!("../../drv_agent/src/seat.rs"),
            include_str!("../../drv_agent/src/plugin.rs"),
            include_str!("../../drv_agent/src/catalog.rs"),
            include_str!("../../drv_agent/src/codex.rs"),
            include_str!("../../drv_hdff/src/subscription.rs"),
            include_str!("../../drv_hdff/src/compaction.rs"),
        ];
        let registry = table();
        let keys: BTreeSet<_> = registry.iter().map(|s| s.key.as_str()).collect();
        for source in sources {
            let source = source.split("#[cfg(test)]").next().unwrap();
            for prefix in [
                "var(\"",
                "var_os(\"",
                "env_num(\"",
                "secs(\"",
                "env_path(\"",
                "env!(\"",
            ] {
                for rest in source.split(prefix).skip(1) {
                    let name = rest.split('"').next().unwrap();
                    if name.starts_with("UNVRS_") {
                        assert!(
                            keys.contains(format!("overrides.{name}").as_str()),
                            "Missing env metadata for {name}"
                        );
                    }
                }
            }
        }
        for setting in registry {
            assert!(
                setting.explanation.contains("Example:"),
                "Missing example for {}",
                setting.key
            );
            assert!(matches!(
                setting.section.as_str(),
                "routing" | "sources" | "projects" | "safety" | "kernel" | "overrides"
            ));
            assert_eq!(
                setting.editable,
                matches!(setting.section.as_str(), "routing" | "sources")
            );
        }
    }
    #[test]
    fn env_detection_reports_actual_fallbacks_pins_and_redacts_credentials() {
        let home = Home::new();
        let env = BTreeMap::from([
            ("UNVRS_L3_CODEX_MODEL".into(), "pinned-model".into()),
            ("UNVRS_SEAT_FUTURE_MODEL".into(), "another-model".into()),
            ("UNVRS_TOKEN".into(), "opaque-secret".into()),
            ("UNVRS_UNKNOWN_API_KEY".into(), "another-secret".into()),
            ("UNVRS_NOTIFY".into(), "0".into()),
            ("UNVRS_TURN_IDLE_SECS".into(), "0".into()),
            ("UNVRS_STALL_TURNS".into(), " 4 ".into()),
            ("UNVRS_MAX_CONTINUATIONS".into(), "0".into()),
            ("UNVRS_KERNEL_IDLE_SECS".into(), "invalid".into()),
        ]);
        let state = get_with_env(&home.0, Value::Null, &env).unwrap();
        assert_eq!(state.values["kernel.notify"], false);
        assert_eq!(state.values["safety.stall_turns"], 4);
        assert_eq!(state.values["safety.max_continuations"], 0);
        assert_eq!(state.values["safety.turn_idle_secs"], 1200);
        assert_eq!(state.effective_source["safety.turn_idle_secs"], "default");
        assert_eq!(state.values["kernel.idle_secs"], 1800);
        assert_eq!(
            state.effective_source["overrides.UNVRS_L3_CODEX_MODEL"],
            "env"
        );
        assert!(
            state
                .schema
                .iter()
                .find(|s| s.key == "overrides.UNVRS_SEAT_FUTURE_MODEL")
                .unwrap()
                .explanation
                .contains("bypasses your routing rules")
        );
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(!serialized.contains("opaque-secret"));
        assert!(!serialized.contains("another-secret"));
        let env = BTreeMap::from([("UNVRS_DAEMON".into(), "launchd".into())]);
        assert_eq!(
            get_with_env(&home.0, Value::Null, &env).unwrap().values["kernel.idle_secs"],
            Value::Null
        );
    }
}

#[cfg(test)]
mod storage_edge_tests {
    use super::*;
    #[test]
    fn inline_source_and_econ_tables_keep_their_format() {
        let mut source: DocumentMut = "# source note\nsource = [{id = 'memory', kind = 'memory', purpose = 'Old'}] # trailing note\n".parse().unwrap();
        edit_path(
            source.as_item_mut(),
            &[json!("source"), json!(0), json!("purpose")],
            &json!("New"),
        )
        .unwrap();
        assert!(source.to_string().contains("# source note"));
        assert!(
            source
                .to_string()
                .contains("purpose = \"New\"}] # trailing note")
        );
        let mut econ: DocumentMut =
            "reasoning = {light = 'low', deep = 'high', max = 'max'} # keep\n"
                .parse()
                .unwrap();
        edit_path(
            econ.as_item_mut(),
            &[json!("reasoning"), json!("deep")],
            &json!("medium"),
        )
        .unwrap();
        assert!(
            econ.to_string()
                .contains("deep = \"medium\", max = 'max'} # keep")
        );
    }
}

pub const USAGE: &str = "usage: unvrs settings [get | set <key> <json-value> [--confirm] | history | revert <change-id> [--confirm]]";

/// Parse the CLI and ctl spellings into the same typed kernel request.
pub fn command(args: &[String]) -> Result<Value> {
    let (args, confirm) = if args.last().is_some_and(|a| a == "--confirm") {
        (&args[..args.len() - 1], true)
    } else {
        (args, false)
    };
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    Ok(match args.as_slice() {
        [] | ["get"] if !confirm => json!({"op":"settings.get"}),
        ["history"] if !confirm => json!({"op":"settings.history"}),
        ["set", key, value] => {
            json!({"op":"settings.set", "key":key, "value":serde_json::from_str::<Value>(value).context("Use a JSON value, such as true, 5, or \"high\" in quotes")?, "confirm":confirm})
        }
        ["revert", change_id] => {
            json!({"op":"settings.revert", "change_id":change_id, "confirm":confirm})
        }
        _ => bail!("{USAGE}"),
    })
}
