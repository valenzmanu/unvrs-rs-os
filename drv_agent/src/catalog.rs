//! Live harness-owned model, authentication and quota evidence; no inference calls.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use uke::{
    TurnRequest,
    drv_econ::{Catalog, Model},
    signals::CommandTracking,
};

fn percent_available<'a>(rows: impl Iterator<Item = &'a Value>, field: &str) -> Option<bool> {
    let mut observed = false;
    let mut unknown = false;
    for row in rows.filter(|v| !v.is_null()) {
        match row[field].as_f64().filter(|n| *n >= 0.0 && n.is_finite()) {
            Some(n) if n >= 100.0 => return Some(false),
            Some(_) => observed = true,
            None => unknown = true,
        }
    }
    (observed && !unknown).then_some(true)
}

fn codex_bound(v: &Value) -> Option<bool> {
    if v["spendControlReached"] == true {
        return Some(false);
    }
    percent_available([&v["primary"], &v["secondary"]].into_iter(), "usedPercent")
}

fn efforts(values: &Value, key: Option<&str>) -> Result<Vec<String>> {
    let Some(values) = values.as_array() else {
        ensure!(values.is_null(), "supported effort list is malformed");
        return Ok(vec![]);
    };
    values
        .iter()
        .map(|v| {
            key.map_or(v, |k| &v[k])
                .as_str()
                .map(str::to_owned)
                .context("effort value missing")
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 120 && !id.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn parse_codex_catalog(
    models: &Value,
    account: &Value,
    quota: &Value,
    cache: Option<&Value>,
    at: u64,
) -> Result<Catalog> {
    ensure!(
        models["nextCursor"].is_null(),
        "incomplete Codex model catalog"
    );
    let data = models["data"]
        .as_array()
        .context("Codex catalog data missing")?;
    let mut normalized = vec![];
    for m in data {
        let id = m["model"]
            .as_str()
            .or_else(|| m["id"].as_str())
            .context("Codex model id missing")?;
        ensure!(valid_id(id), "invalid Codex model id");
        let mut supported = efforts(&m["supportedReasoningEfforts"], Some("reasoningEffort"))?;
        if supported.is_empty() {
            // The cache may fill effort metadata for a live-listed ID, never add membership.
            if let Some(cached) = cache
                .and_then(|c| c["models"].as_array())
                .and_then(|rows| rows.iter().find(|r| r["slug"] == id))
            {
                supported = efforts(&cached["supported_reasoning_levels"], Some("effort"))?;
            }
        }
        let bounds: Vec<_> = quota["rateLimitsByLimitId"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .filter(|v| v["normalModelSlug"] == id)
            .collect();
        let mut model_bound = None;
        let mut quota_error = None;
        for bound in bounds {
            match codex_bound(bound) {
                Some(false) => {
                    model_bound = Some(false);
                    break;
                }
                Some(true) => model_bound = Some(true),
                None => quota_error = Some("applicable Codex limit has no usable window".into()),
            }
        }
        normalized.push(Model {
            id: id.into(),
            efforts: supported,
            is_default: m["isDefault"] == true,
            quota_available: model_bound,
            quota_error,
        });
    }
    let signed_in = if account["account"].is_null() {
        Some(false)
    } else {
        Some(account["account"]["type"] == "chatgpt")
    };
    let row = crate::headless::codex_quota_row(quota, at)?;
    let mut available = codex_bound(&quota["rateLimits"]);
    match quota["ordinaryUsageAllowed"].as_bool() {
        Some(false) => available = Some(false),
        None if available != Some(false) => available = None,
        _ => {}
    }
    // All account-wide bounds apply, even when a model-specific row is healthy.
    for bound in quota["rateLimitsByLimitId"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter(|v| v["normalModelSlug"].is_null())
    {
        match codex_bound(bound) {
            Some(false) => available = Some(false),
            None if available != Some(false) => available = None,
            _ => {}
        }
    }
    Ok(Catalog { observed_at: at, signed_in, models: normalized, quota_available: available,
        quota_observed_at: at, source: "codex app-server model/list + account/read + account/rateLimits/read; optional models_cache effort metadata".into(), quota: row, ..Default::default() })
}

fn control_result(v: &Value) -> Result<&Value> {
    ensure!(
        v["response"]["subtype"] == "success",
        "Claude catalog control request refused"
    );
    v["response"]
        .get("response")
        .context("Claude control response missing")
}

pub fn parse_claude_catalog(
    models: &Value,
    auth: &Value,
    usage: &Value,
    at: u64,
) -> Result<Catalog> {
    let models = control_result(models)?;
    let usage = control_result(usage).unwrap_or(&Value::Null);
    let data = models["models"]
        .as_array()
        .context("Claude selectable models missing")?;
    let limits = &usage["rate_limits"];
    let mut available = percent_available(
        [
            &limits["five_hour"],
            &limits["seven_day"],
            &limits["seven_day_oauth_apps"],
        ]
        .into_iter(),
        "utilization",
    );
    if usage["rate_limits_available"] != true && available != Some(false) {
        available = None;
    }
    let mut normalized: Vec<Model> = vec![];
    for m in data.iter().filter(|m| m["disabled"] != true) {
        // resolvedModel is supplied by Claude itself; aliases are never invented IDs.
        let id = m["resolvedModel"]
            .as_str()
            .context("Claude exact resolved model missing")?;
        ensure!(valid_id(id), "invalid Claude model id");
        let supported = efforts(&m["supportedEffortLevels"], None)?;
        let has_family = |family: &str| {
            id.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|p| p.eq_ignore_ascii_case(family))
        };
        let mut applicable = vec![];
        for family in ["opus", "sonnet"] {
            if has_family(family) && !limits[format!("seven_day_{family}")].is_null() {
                applicable.push(&limits[format!("seven_day_{family}")]);
            }
        }
        for row in limits["model_scoped"].as_array().into_iter().flatten() {
            if row["display_name"].as_str().is_some_and(has_family) {
                applicable.push(row);
            }
        }
        let bound = if applicable.is_empty() {
            None
        } else {
            percent_available(applicable.into_iter(), "utilization")
        };
        let quota_error = (bound.is_none()
            && (limits["model_scoped"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["display_name"].as_str().is_some_and(has_family))
                || ["opus", "sonnet"]
                    .iter()
                    .any(|f| has_family(f) && !limits[format!("seven_day_{f}")].is_null())))
        .then(|| "applicable Claude quota window is unknown".into());
        let is_default = m["value"] == "default";
        if let Some(old) = normalized.iter_mut().find(|old| old.id == id) {
            old.is_default |= is_default;
        } else {
            normalized.push(Model {
                id: id.into(),
                efforts: supported,
                is_default,
                quota_available: bound,
                quota_error,
            });
        }
    }
    let signed_in = auth["loggedIn"]
        .as_bool()
        .map(|logged| logged && auth["authMethod"] == "claude.ai");
    let quota_at = usage["observed_at"].as_u64().unwrap_or(at);
    let source = usage["source"].as_str().unwrap_or("claude SDK get_usage");
    let window = |name: &str| {
        let row = &limits[name];
        json!({"remaining_pct":row["utilization"].as_f64().filter(|n| *n >= 0.0 && n.is_finite()).map(|n| (100.0-n).max(0.0)),
            "resets_at":row["resets_at"]})
    };
    Ok(Catalog {
        observed_at: at,
        signed_in,
        models: normalized,
        quota_available: available,
        quota_observed_at: quota_at,
        source: format!(
            "claude SDK list_models + supportedEffortLevels; claude auth status; {source}"
        ),
        quota: json!({"harness":"claude","account":"default","observed_at":quota_at,"source":source,
            "remaining_pct":window("five_hour")["remaining_pct"],
            "five_hour":window("five_hour"),"weekly":window("seven_day"),
            "subscription_type":usage["subscription_type"],"rate_limits":limits}),
        ..Default::default()
    })
}

/// Adapt the Observatory's saved reading to the existing quota parser.
fn saved_claude_usage(v: &Value, at: u64, max_age: u64) -> Option<Value> {
    let observed_at = v["at_ms"].as_u64().filter(|n| *n != 0)?;
    if at.checked_sub(observed_at)? > max_age {
        return None;
    }
    let mut limits = json!({});
    for name in ["five_hour", "seven_day"] {
        if !v[name].is_null() {
            limits[name] =
                json!({"utilization":v[name]["used_pct"],"resets_at":v[name]["resets_at"]});
        }
    }
    percent_available(
        [&limits["five_hour"], &limits["seven_day"]].into_iter(),
        "utilization",
    )?;
    Some(json!({"response":{"subtype":"success","response":{
        "observed_at":observed_at,"subscription_type":v["plan"],
        "source":v["source"].as_str().unwrap_or("kernel/claude-usage.json"),
        "rate_limits_available":true,"rate_limits":limits}}}))
}

fn auth_json(mut cmd: Command) -> Result<Value> {
    let mut child = cmd
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_owned()?;
    let stdout = child.stdout.take().context("Claude auth stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = vec![];
        let result = stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    let until = Instant::now() + Duration::from_secs(15);
    let bytes = rx
        .recv_timeout(Duration::from_secs(15))
        .context("Claude auth timed out")??;
    ensure!(bytes.len() <= 65536, "Claude auth response too large");
    while child.try_wait()?.is_none() {
        ensure!(Instant::now() < until, "Claude auth exit timed out");
        thread::sleep(Duration::from_millis(10));
    }
    serde_json::from_slice(&bytes).context("Claude auth response invalid")
}

pub fn claude_catalog_raw(home: &Path, env: &[(String, String)]) -> Result<Value> {
    let command = || {
        let mut cmd =
            Command::new(std::env::var("UNVRS_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()));
        cmd.current_dir(home)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .env_remove("CLAUDECODE")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN");
        cmd
    };
    let auth = auth_json(command())?;
    let max_age = uke::drv_econ::Config::load(home)?.catalog.max_age_minutes * 60_000;
    let saved = fs::read(home.join("kernel/claude-usage.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| saved_claude_usage(&v, uke::now_ms(), max_age));
    let mut requests = vec![json!({"subtype":"list_models"})];
    if saved.is_none() {
        requests.push(json!({"subtype":"get_usage","skip_behaviors":true}));
    }
    let responses = crate::claude_control(command(), &requests)?;
    let usage = saved.unwrap_or_else(|| responses[2].clone());
    Ok(json!({"models":responses[1],"usage":usage,"auth":auth}))
}

/// A failed refresh replaces eligibility with an explicit unavailable catalog.
pub fn catalogs(home: &Path) -> BTreeMap<String, Catalog> {
    let _owner = uke::signals::owner_scope(home, 0);
    ["codex", "claude"]
        .into_iter()
        .map(|harness| {
            let result = (|| -> Result<Catalog> {
                crate::harness_ready(harness)?;
                if harness == "codex" {
                    let req = TurnRequest {
                        harness: harness.into(),
                        cwd: home.into(),
                        session: None,
                        prompt: String::new(),
                        env: vec![],
                        model: None,
                        effort: None,
                    };
                    let raw = crate::codex::catalog(&req)?;
                    let cache_path = crate::Homes::from_env()?.codex.join("models_cache.json");
                    let max_age =
                        uke::drv_econ::Config::load(home)?.catalog.max_age_minutes * 60_000;
                    let cache_fresh = fs::metadata(&cache_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|d| u64::try_from(d.as_millis()).ok())
                        .and_then(|t| uke::now_ms().checked_sub(t))
                        .is_some_and(|age| age <= max_age);
                    let cache = cache_fresh
                        .then(|| fs::read(cache_path).ok())
                        .flatten()
                        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                    parse_codex_catalog(
                        &raw["models"],
                        &raw["account"],
                        &raw["quota"],
                        cache.as_ref(),
                        uke::now_ms(),
                    )
                } else {
                    let raw = claude_catalog_raw(home, &[])?;
                    parse_claude_catalog(&raw["models"], &raw["auth"], &raw["usage"], uke::now_ms())
                }
            })();
            (
                harness.into(),
                result.unwrap_or_else(|e| Catalog {
                    observed_at: uke::now_ms(),
                    error: Some(uke::clean(&format!("{e:#}")).chars().take(400).collect()),
                    ..Default::default()
                }),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> Value {
        let text = match name {
            "codex-models" => include_str!("../tests/fixtures/econ/codex-models.json"),
            "codex-account" => include_str!("../tests/fixtures/econ/codex-account.json"),
            "codex-quota" => include_str!("../tests/fixtures/econ/codex-quota.json"),
            "codex-cache" => include_str!("../tests/fixtures/econ/codex-cache.json"),
            "claude-models" => include_str!("../tests/fixtures/econ/claude-models.json"),
            "claude-auth" => include_str!("../tests/fixtures/econ/claude-auth.json"),
            "claude-usage" => include_str!("../tests/fixtures/econ/claude-usage.json"),
            _ => unreachable!(),
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn recorded_codex_catalog_and_cache_require_live_membership() {
        let models = fixture("codex-models")["result"].clone();
        let account = fixture("codex-account")["result"].clone();
        let quota = fixture("codex-quota")["result"].clone();
        let cache = fixture("codex-cache");
        let c = parse_codex_catalog(&models, &account, &quota, Some(&cache), 42).unwrap();
        assert_eq!(c.signed_in, Some(true));
        assert_eq!(c.quota_available, Some(true));
        assert_eq!(
            c.models.iter().find(|m| m.is_default).unwrap().id,
            "gpt-6.1-sol"
        );
        assert!(c.models[0].efforts.contains(&"xhigh".into()));
        assert_eq!(c.quota["weekly"]["remaining_pct"], 92.0);
        assert_eq!(c.quota["ordinary_usage_allowed"], true);
        let mut lacking = models.clone();
        lacking["data"][0]
            .as_object_mut()
            .unwrap()
            .remove("supportedReasoningEfforts");
        assert!(
            parse_codex_catalog(&lacking, &account, &quota, Some(&cache), 42)
                .unwrap()
                .models[0]
                .efforts
                .contains(&"xhigh".into())
        );
        lacking["data"].as_array_mut().unwrap().remove(0);
        assert!(
            !parse_codex_catalog(&lacking, &account, &quota, Some(&cache), 42)
                .unwrap()
                .models
                .iter()
                .any(|m| m.id == "gpt-6.1-sol")
        );
        lacking["nextCursor"] = json!("more");
        assert!(parse_codex_catalog(&lacking, &account, &quota, Some(&cache), 42).is_err());
        for (used, permission, expected) in [
            (100.0, Some(true), Some(false)),
            (100.0, None, Some(false)),
            (101.0, Some(true), Some(false)),
            (-1.0, Some(true), None),
            (8.0, Some(false), Some(false)),
            (8.0, None, None),
        ] {
            let mut q = quota.clone();
            q["rateLimits"]["primary"]["usedPercent"] = json!(used);
            q["ordinaryUsageAllowed"] = json!(permission);
            assert_eq!(
                parse_codex_catalog(&models, &account, &q, None, 42)
                    .unwrap()
                    .quota_available,
                expected
            );
        }
        let mut q = quota.clone();
        q["rateLimits"]["secondary"] = json!({"usedPercent":100,"windowDurationMins":300});
        assert_eq!(
            parse_codex_catalog(&models, &account, &q, None, 42)
                .unwrap()
                .quota_available,
            Some(false)
        );
        let mut a = account;
        a["account"]["type"] = json!("apiKey");
        assert_eq!(
            parse_codex_catalog(&models, &a, &quota, None, 42)
                .unwrap()
                .signed_in,
            Some(false)
        );
    }

    #[test]
    fn saved_claude_windows_keep_age_and_judge_deep_selects_opus() {
        use uke::drv_econ::{Config, DEFAULT_CONFIG, Overrides, TaskFacts, route};
        let at = 2_000_000;
        let max_age = Config::parse(DEFAULT_CONFIG)
            .unwrap()
            .catalog
            .max_age_minutes
            * 60_000;
        let saved = json!({"at_ms":at - 100,"source":"api.anthropic.com/api/oauth/usage (Claude Code login)","plan":"max",
            "five_hour":{"used_pct":21,"resets_at":3_000_000},"seven_day":{"used_pct":21,"resets_at":4_000_000}});
        let models = fixture("claude-models");
        let auth = fixture("claude-auth");
        for (used, timestamp, expected) in [
            (21, at - 100, Some(true)),
            (100, at - 100, Some(false)),
            (100, at - max_age - 1, None),
            (21, at + 1, None),
        ] {
            let mut v = saved.clone();
            v["seven_day"]["used_pct"] = json!(used);
            v["at_ms"] = json!(timestamp);
            // The real SDK's unknown response is the fallback for a stale/missing file.
            let usage = saved_claude_usage(&v, at, max_age).unwrap_or_else(|| json!({
                "response":{"subtype":"success","response":{"rate_limits":null,"remaining_pct":null}}}));
            let c = parse_claude_catalog(&models, &auth, &usage, at).unwrap();
            assert_eq!(c.quota_available, expected);
            if expected.is_some() {
                assert_eq!(c.quota_observed_at, timestamp);
                assert_eq!(c.quota["five_hour"]["remaining_pct"], 79.0);
                assert_eq!(c.quota["weekly"]["remaining_pct"], (100 - used) as f64);
                assert_eq!(c.quota["weekly"]["resets_at"], 4_000_000);
                assert_eq!(c.quota["source"], saved["source"]);
            }
            let d = route(
                &Config::parse(DEFAULT_CONFIG).unwrap(),
                &TaskFacts {
                    rank: 2,
                    ..Default::default()
                },
                &Overrides::default(),
                &BTreeMap::from([("claude".into(), c)]),
                2,
                at,
            )
            .unwrap();
            assert_eq!(d.chosen.is_some(), expected != Some(false));
            if let Some(chosen) = d.chosen {
                assert_eq!(chosen.model, "claude-opus-5-5");
                assert_eq!(chosen.effort, "high");
                assert_eq!(
                    d.reason.contains("quota unknown, admitted"),
                    expected.is_none()
                );
            } else {
                assert!(
                    d.skipped
                        .iter()
                        .all(|s| s.reason == "account quota unavailable")
                );
            }
        }
        let mut invalid = saved;
        invalid["five_hour"]["used_pct"] = json!(-1);
        assert!(saved_claude_usage(&invalid, at, max_age).is_none());
        assert!(saved_claude_usage(&json!({}), at, max_age).is_none());
    }

    #[test]
    fn recorded_claude_catalog_resolves_aliases_and_checks_all_quota_bounds() {
        let models = fixture("claude-models");
        let auth = fixture("claude-auth");
        let usage = fixture("claude-usage");
        let c = parse_claude_catalog(&models, &auth, &usage, 42).unwrap();
        assert_eq!(c.signed_in, Some(true));
        assert_eq!(c.quota_available, Some(true));
        assert_eq!(c.models.len(), 11);
        assert!(
            c.models
                .iter()
                .any(|m| m.id == "claude-opus-5-5" && m.efforts.contains(&"xhigh".into()))
        );
        assert!(!c.models.iter().any(|m| m.id == "opus"));
        for (field, used, expected) in [
            ("five_hour", 100.0, Some(false)),
            ("seven_day", 100.0, Some(false)),
            ("seven_day", -1.0, None),
        ] {
            let mut u = usage.clone();
            u["response"]["response"]["rate_limits"][field]["utilization"] = json!(used);
            assert_eq!(
                parse_claude_catalog(&models, &auth, &u, 42)
                    .unwrap()
                    .quota_available,
                expected
            );
        }
        let mut u = usage.clone();
        u["response"]["response"]["rate_limits"]["model_scoped"][0]["utilization"] = json!(100);
        let c = parse_claude_catalog(&models, &auth, &u, 42).unwrap();
        assert_eq!(c.quota_available, Some(true));
        assert_eq!(
            c.models
                .iter()
                .find(|m| m.id.contains("fable"))
                .unwrap()
                .quota_available,
            Some(false)
        );
        u["response"]["response"]["rate_limits"]["model_scoped"][0]["utilization"] = Value::Null;
        assert!(
            parse_claude_catalog(&models, &auth, &u, 42)
                .unwrap()
                .models
                .iter()
                .find(|m| m.id.contains("fable"))
                .unwrap()
                .quota_error
                .is_some()
        );
        u["response"]["response"]["rate_limits_available"] = json!(false);
        assert_eq!(
            parse_claude_catalog(&models, &auth, &u, 42)
                .unwrap()
                .quota_available,
            None
        );
        let mut a = auth;
        a["loggedIn"] = json!(false);
        assert_eq!(
            parse_claude_catalog(&models, &a, &usage, 42)
                .unwrap()
                .signed_in,
            Some(false)
        );
        let mut m = models;
        m["response"]["response"]["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("resolvedModel");
        assert!(parse_claude_catalog(&m, &a, &usage, 42).is_err());
    }
}
