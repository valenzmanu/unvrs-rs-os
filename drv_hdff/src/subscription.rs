//! One ordered subscription route list for seat folds and worker recovery.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Mutex};
use uke::{BriefFold, FoldJob, TurnRequest};

pub const DEFAULT_SUMMARY_ROUTES: &str = "codex=gpt-5.6-luna,claude=claude-sonnet-5-5";

#[derive(Clone, Debug)]
struct Route {
    harness: String,
    model: String,
    ready: bool,
}

pub struct SubscriptionSummaries {
    routes: Mutex<Vec<Route>>,
    events: Mutex<Vec<Value>>,
    config_error: Option<String>,
}

impl Default for SubscriptionSummaries {
    fn default() -> Self {
        Self::new(
            &std::env::var("UNVRS_SUMMARY_ROUTES")
                .unwrap_or_else(|_| DEFAULT_SUMMARY_ROUTES.into()),
        )
    }
}
impl SubscriptionSummaries {
    pub fn new(config: &str) -> Self {
        match parse_routes(config) {
            Ok(routes) => Self {
                routes: Mutex::new(routes),
                events: Mutex::new(vec![]),
                config_error: None,
            },
            Err(e) => Self {
                routes: Mutex::new(vec![]),
                events: Mutex::new(vec![]),
                config_error: Some(format!("{e:#}")),
            },
        }
    }
    fn record(&self, route: &Route, phase: &str, error: Option<String>) {
        self.events.lock().unwrap().push(json!({"job": format!("summary:{}={}",route.harness,route.model), "harness":route.harness,"model":route.model,"phase":phase,"error":error}));
    }
    pub fn events(&self) -> Vec<Value> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
    pub fn preflight(&self) {
        self.check_with(&execute);
    }
    fn check_with(&self, run: &impl Fn(&Route, &str) -> Result<String>) {
        if let Some(error) = &self.config_error {
            self.events
                .lock()
                .unwrap()
                .push(json!({"job":"summary:configuration","phase":"startup","error":error}));
        }
        let routes = self.routes.lock().unwrap().clone();
        for (i, route) in routes.iter().enumerate() {
            let result = run(
                route,
                "Reply with exactly UNVRS_SUMMARY_READY. Use no tools.",
            )
            .and_then(|text| {
                ensure!(
                    text.trim() == "UNVRS_SUMMARY_READY",
                    "Summary startup returned unexpected response: {text}"
                );
                Ok(())
            });
            self.routes.lock().unwrap()[i].ready = result.is_ok();
            self.record(route, "startup", result.err().map(|e| format!("{e:#}")));
        }
    }
    pub fn summarize(&self, prompt: &str) -> Result<String> {
        self.try_with(prompt, &execute, Ok)
    }
    fn try_with<T>(
        &self,
        prompt: &str,
        run: &impl Fn(&Route, &str) -> Result<String>,
        parse: impl Fn(String) -> Result<T>,
    ) -> Result<T> {
        if let Some(error) = &self.config_error {
            anyhow::bail!("{error}");
        }
        let routes = self.routes.lock().unwrap().clone();
        let mut errors = vec![];
        for (i, route) in routes.iter().enumerate().filter(|(_, r)| r.ready) {
            match run(route, prompt).and_then(&parse) {
                Ok(value) => {
                    self.record(route, "request", None);
                    return Ok(value);
                }
                Err(e) => {
                    let error = format!("{e:#}");
                    self.record(route, "request", Some(error.clone()));
                    self.routes.lock().unwrap()[i].ready = false;
                    errors.push(format!("{}={}: {error}", route.harness, route.model));
                }
            }
        }
        anyhow::bail!(
            "No ready subscription summary route; failed routes stay disabled until restart. {}",
            errors.join("\n")
        )
    }
    pub fn fold(&self, job: &FoldJob) -> Result<BriefFold> {
        let prompt = format!(
            "{}\n{}",
            super::compaction::BRIEF_SYSTEM,
            json!({"previous":job.brief,"turns":job.turns,"rejected":job.reason.strip_prefix("retry: ")})
        );
        self.try_with(
            &prompt,
            &|route, prompt| execute_with_schema(route, prompt, Some(&brief_schema())),
            |text| {
                let text = text
                    .trim()
                    .trim_start_matches("```json")
                    .trim_start_matches("```")
                    .trim_end_matches("```")
                    .trim();
                serde_json::from_str(text).context("Summary returned invalid brief JSON")
            },
        )
    }
}
fn parse_routes(config: &str) -> Result<Vec<Route>> {
    let mut routes = vec![];
    for entry in config.split(',') {
        let (harness, model) = entry
            .trim()
            .split_once('=')
            .context("Summary route must be harness=exact-model")?;
        ensure!(
            matches!(harness, "codex" | "claude"),
            "No subscription summary driver for {harness}"
        );
        ensure!(
            !model.is_empty()
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)),
            "Invalid exact summary model {model:?}"
        );
        routes.push(Route {
            harness: harness.into(),
            model: model.into(),
            ready: false,
        });
    }
    Ok(routes)
}
fn execute(route: &Route, prompt: &str) -> Result<String> {
    execute_with_schema(route, prompt, None)
}

fn brief_schema() -> Value {
    let mut properties = serde_json::Map::new();
    for name in ["goal", "now", "narrative"] {
        properties.insert(name.into(), json!({"type":"string"}));
    }
    for name in [
        "next",
        "decisions",
        "open",
        "done",
        "cancelled",
        "gotchas",
        "artifacts",
    ] {
        properties.insert(
            name.into(),
            json!({"type":"array", "items":{"type":"string"}}),
        );
    }
    properties.insert("mission".into(), json!({"type":["string","null"]}));
    let required = properties.keys().cloned().collect::<Vec<_>>();
    json!({"type":"object", "additionalProperties":false, "required":["brief","notes"],
        "properties":{"brief":{"type":"object", "additionalProperties":false, "properties":properties, "required":required},
        "notes":{"type":"array", "items":{"type":"string"}}}})
}

fn execute_with_schema(route: &Route, prompt: &str, schema: Option<&Value>) -> Result<String> {
    // A fresh directory prevents repository instructions from becoming summary instructions.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "unvrs-summary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir)?;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let temp = Temp(dir);
    let schema_path = if let Some(schema) = schema.filter(|_| route.harness == "codex") {
        let path = temp.0.join("brief-schema.json");
        std::fs::write(&path, serde_json::to_vec(schema)?)?;
        Some(path)
    } else {
        None
    };
    drv_agent::run_summary_with_schema(
        &TurnRequest {
            harness: route.harness.clone(),
            cwd: temp.0.clone(),
            session: None,
            prompt: prompt.into(),
            env: vec![],
            model: Some(route.model.clone()),
            effort: None,
        },
        schema_path.as_deref(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brief_schema_and_malformed_json_fallback_keep_positive_receipt() {
        let schema = brief_schema();
        let mut sample_fold = BriefFold::default();
        sample_fold.brief.mission = Some("mission".into());
        sample_fold.brief.cancelled = vec!["[o1] cancelled".into()];
        let sample = serde_json::to_value(sample_fold).unwrap();
        for key in schema["properties"]["brief"]["required"]
            .as_array()
            .unwrap()
        {
            assert!(sample["brief"].get(key.as_str().unwrap()).is_some());
        }
        let routes = SubscriptionSummaries::new(DEFAULT_SUMMARY_ROUTES);
        routes.check_with(&|_, _| Ok("UNVRS_SUMMARY_READY".into()));
        routes.events();
        let answer: BriefFold = routes
            .try_with(
                "fold",
                &|route, _| {
                    if route.harness == "codex" {
                        Ok("{malformed".into())
                    } else {
                        Ok(serde_json::to_string(&BriefFold::default()).unwrap())
                    }
                },
                |text| serde_json::from_str(&text).context("Summary returned invalid brief JSON"),
            )
            .unwrap();
        assert_eq!(answer.brief.open, Vec::<String>::new());
        let events = routes.events();
        assert_eq!(events[0]["harness"], "codex");
        assert!(
            events[0]["error"]
                .as_str()
                .unwrap()
                .contains("invalid brief JSON")
        );
        assert_eq!(events[1]["harness"], "claude");
        assert_eq!(events[1]["phase"], "request");
        assert!(events[1]["error"].is_null());
    }

    #[test]
    fn startup_rejects_missing_models_and_runtime_fallback_retains_reason() {
        let fallback = SubscriptionSummaries::new(DEFAULT_SUMMARY_ROUTES);
        fallback.check_with(&|_, _| Ok("UNVRS_SUMMARY_READY".into()));
        fallback.events();
        assert_eq!(
            fallback
                .try_with(
                    "data",
                    &|r, _| if r.harness == "codex" {
                        anyhow::bail!("exact quota reason")
                    } else {
                        Ok("fallback brief".into())
                    },
                    Ok
                )
                .unwrap(),
            "fallback brief"
        );
        let events = fallback.events();
        assert_eq!(events[0]["error"], "exact quota reason");
        assert!(events[1]["error"].is_null());
        let s = SubscriptionSummaries::new(DEFAULT_SUMMARY_ROUTES);
        s.check_with(&|r, _| {
            if r.harness == "codex" {
                anyhow::bail!("Model gpt-5.6-luna not supported")
            } else {
                Ok("UNVRS_SUMMARY_READY".into())
            }
        });
        assert_eq!(s.events()[0]["error"], "Model gpt-5.6-luna not supported");
        let answer = s
            .try_with(
                "data",
                &|r, _| {
                    assert_eq!(r.harness, "claude");
                    Ok("brief".into())
                },
                Ok,
            )
            .unwrap();
        assert_eq!(answer, "brief");
        assert!(SubscriptionSummaries::new("codex").config_error.is_some());
        assert!(SubscriptionSummaries::new("api=x").config_error.is_some());
        s.try_with(
            "data",
            &|_, _| anyhow::bail!("quota unavailable"),
            Ok::<_, anyhow::Error>,
        )
        .unwrap_err();
        assert!(s.events().iter().any(|e| e["error"] == "quota unavailable"));
        assert!(
            s.try_with(
                "data",
                &|_, _| panic!("disabled route retried"),
                Ok::<_, anyhow::Error>
            )
            .is_err()
        );
    }
}
