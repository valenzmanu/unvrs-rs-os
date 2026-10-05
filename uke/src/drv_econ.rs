//! Deterministic task admission. Harness discovery supplies facts; routing has no I/O.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

pub const DEFAULT_CONFIG: &str = include_str!("econ-default.toml");
// The only existing research profile whose values migration may replace.
const PROVISIONAL_RESEARCH: &str = "[{harness='claude',model='newest sonnet'},{harness='codex',model='newest sol',effort='low',captain_fallback=true}]";
const KINDS: [&str; 6] = [
    "implement",
    "review",
    "validate",
    "research",
    "design",
    "decide",
];
// Order defines upward effort fallback; model support comes from the harness.
use crate::EFFORTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Judge,
    Worker,
    Design,
    Research,
    #[serde(rename = "research_quick")]
    ResearchQuick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reasoning {
    Light,
    Deep,
    Max,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub policy: Policy,
    pub profiles: BTreeMap<Role, Vec<Candidate>>,
    pub catalog: CatalogPolicy,
    #[serde(default)]
    pub reasoning: ReasoningEfforts,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReasoningEfforts {
    pub light: String,
    pub deep: String,
    pub max: String,
}

impl Default for ReasoningEfforts {
    fn default() -> Self {
        Self {
            light: "low".into(),
            deep: "high".into(),
            max: "max".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub default_role: Role,
    pub default_reasoning: Reasoning,
    pub allow_max: bool,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub ranks: Vec<u8>,
    #[serde(default)]
    pub kinds: Vec<String>,
    pub judgment: Option<String>,
    pub thoroughness: Option<String>,
    pub role: Option<Role>,
    pub reasoning: Option<Reasoning>,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub harness: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captain_fallback: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPolicy {
    pub refresh_minutes: u64,
    pub max_age_minutes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFacts {
    pub rank: u8,
    pub kind: Option<String>,
    pub judgment: Option<String>,
    pub thoroughness: Option<String>,
}

impl Default for TaskFacts {
    fn default() -> Self {
        Self {
            rank: 3,
            kind: None,
            judgment: None,
            thoroughness: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Normalized harness evidence. Unknown quota admits with a receipt warning.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Catalog {
    pub observed_at: u64,
    pub signed_in: Option<bool>,
    pub models: Vec<Model>,
    pub quota_available: Option<bool>,
    pub quota_observed_at: u64,
    pub error: Option<String>,
    pub source: String,
    pub quota: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Model {
    pub id: String,
    pub efforts: Vec<String>,
    pub is_default: bool,
    /// A model-specific exhausted bound vetoes an otherwise healthy account.
    pub quota_available: Option<bool>,
    pub quota_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Selection {
    #[serde(default)]
    pub captain_fallback: bool,
    pub harness: String,
    pub selector: String,
    pub model: String,
    pub effort: String,
    pub catalog_age_ms: u64,
    pub quota_age_ms: Option<u64>,
    #[serde(default)]
    pub quota_available: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Skipped {
    pub candidate: Candidate,
    pub reason: String,
    pub catalog_age_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Decision {
    pub inputs: TaskFacts,
    pub overrides: Overrides,
    pub override_by: Option<usize>,
    pub role: Role,
    pub reasoning: Reasoning,
    pub reason: String,
    pub chosen: Option<Selection>,
    pub skipped: Vec<Skipped>,
    pub hold_reason: Option<String>,
}

fn harness_valid(h: &str) -> bool {
    matches!(h, "codex" | "claude")
}
fn level_valid(v: &Option<String>) -> bool {
    v.as_deref().is_none_or(|s| matches!(s, "low" | "high"))
}
fn model_id_valid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn selector_valid(s: &str) -> bool {
    s.strip_prefix("newest ").map_or_else(
        || model_id_valid(s),
        |family| !family.is_empty() && family.chars().all(|c| c.is_ascii_alphanumeric()),
    )
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text).context("invalid econ.toml")?;
        for effort in [
            &config.reasoning.light,
            &config.reasoning.deep,
            &config.reasoning.max,
        ] {
            ensure!(
                EFFORTS.contains(&effort.as_str()),
                "invalid reasoning effort: {effort}"
            );
        }
        ensure!(
            config.catalog.refresh_minutes > 0
                && config.catalog.max_age_minutes >= config.catalog.refresh_minutes
                && config.catalog.max_age_minutes <= u64::MAX / 60_000,
            "catalog ages must be positive, bounded, and max_age >= refresh"
        );
        for role in [Role::Judge, Role::Worker, Role::Design] {
            ensure!(
                config.profiles.contains_key(&role),
                "missing profile for {role:?}"
            );
        }
        for candidates in config.profiles.values() {
            for c in candidates {
                ensure!(
                    harness_valid(&c.harness) && selector_valid(&c.model),
                    "invalid candidate {c:?}"
                );
                ensure!(
                    c.effort
                        .as_deref()
                        .is_none_or(|e| EFFORTS.contains(&e) || matches!(e, "light" | "deep")),
                    "invalid candidate effort for {} {}: use a harness effort or light|deep",
                    c.harness,
                    c.model
                );
            }
        }
        for r in &config.policy.rules {
            ensure!(
                r.role.is_some() || r.reasoning.is_some(),
                "policy rule needs an output"
            );
            ensure!(!r.reason.trim().is_empty(), "policy rule needs a reason");
            ensure!(
                r.ranks.iter().all(|n| (1..=3).contains(n))
                    && r.kinds.iter().all(|s| KINDS.contains(&s.as_str()))
                    && level_valid(&r.judgment)
                    && level_valid(&r.thoroughness),
                "invalid policy rule {r:?}"
            );
        }
        // Reject pins that would lower any reachable task class before saving config.
        if config
            .profiles
            .values()
            .flatten()
            .any(|c| c.effort.is_some() && c.captain_fallback != Some(true))
        {
            for rank in 1..=3 {
                for kind in std::iter::once(None).chain(KINDS.into_iter().map(Some)) {
                    for judgment in [None, Some("low"), Some("high")] {
                        for thoroughness in [None, Some("low"), Some("high")] {
                            let task = TaskFacts {
                                rank,
                                kind: kind.map(str::to_owned),
                                judgment: judgment.map(str::to_owned),
                                thoroughness: thoroughness.map(str::to_owned),
                            };
                            let (role_rule, class_rule) = config.rules_for(&task);
                            let role = role_rule
                                .and_then(|r| r.role)
                                .unwrap_or(config.policy.default_role);
                            let class = class_rule
                                .and_then(|r| r.reasoning)
                                .unwrap_or(config.policy.default_reasoning);
                            for candidate in config.profiles.get(&role).into_iter().flatten() {
                                config.candidate_effort(candidate, class)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(config)
    }

    fn rules_for(&self, task: &TaskFacts) -> (Option<&Rule>, Option<&Rule>) {
        (
            self.policy
                .rules
                .iter()
                .find(|r| r.role.is_some() && r.matches(task)),
            self.policy
                .rules
                .iter()
                .find(|r| r.reasoning.is_some() && r.matches(task)),
        )
    }

    /// Migrate missing defaults under the Settings writer lock, preserving edits and comments.
    pub fn load(home: &Path) -> Result<Self> {
        let _lock = crate::settings::write_lock(home)?;
        let path = home.join("econ.toml");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                crate::settings::atomic(&path, DEFAULT_CONFIG.as_bytes())?;
                return Self::parse(DEFAULT_CONFIG);
            }
            Err(e) => return Err(e).context("cannot read econ.toml"),
        };
        let config = Self::parse(&text)?;
        let mut doc: toml_edit::DocumentMut = text.parse()?;
        let mut changed = false;
        if !doc.contains_key("reasoning") {
            // Append as before so files needing only this migration keep their exact prefix.
            doc = format!(
                "{text}\n\n[reasoning]\n{}",
                toml::to_string(&config.reasoning)?
            )
            .parse()?;
            changed = true;
        }
        let defaults: toml_edit::DocumentMut = DEFAULT_CONFIG.parse()?;
        let provisional: BTreeMap<String, Vec<Candidate>> =
            toml::from_str(&format!("candidates={PROVISIONAL_RESEARCH}"))?;
        if config.profiles.get(&Role::Research) == Some(&provisional["candidates"]) {
            // Add the approved pin in place, retaining comments and array/table syntax.
            match &mut doc["profiles"]["research"] {
                toml_edit::Item::Value(toml_edit::Value::Array(rows)) => {
                    rows.get_mut(0)
                        .unwrap()
                        .as_inline_table_mut()
                        .unwrap()
                        .insert("effort", toml_edit::Value::from("deep"));
                }
                toml_edit::Item::ArrayOfTables(rows) => {
                    rows.get_mut(0)
                        .unwrap()
                        .insert("effort", toml_edit::value("deep"));
                }
                _ => anyhow::bail!("invalid econ.toml: research profile must be a list"),
            }
            changed = true;
        }
        let mut rules = config.policy.rules.clone();
        for (role, name) in [
            (Role::ResearchQuick, "research_quick"),
            (Role::Research, "research"),
        ] {
            if config.profiles.contains_key(&role) {
                continue;
            }
            let has_routing =
                config.policy.default_role == role || rules.iter().any(|r| r.role == Some(role));
            // Existing full-research routing is captain-owned. Quick defaults are new.
            if role == Role::Research && has_routing {
                continue;
            }
            doc["profiles"][name] = defaults["profiles"][name].clone();
            changed = true;
            if has_routing {
                continue;
            }
            let index = rules
                .iter()
                .rposition(|r| {
                    r.role == Some(Role::Judge)
                        || (role == Role::Research && r.role == Some(Role::ResearchQuick))
                })
                .map_or(0, |i| i + 1);
            let rule = defaults["policy"]["rules"]
                .as_array_of_tables()
                .unwrap()
                .iter()
                .find(|r| r["role"].as_str() == Some(name))
                .unwrap()
                .clone();
            match &mut doc["policy"]["rules"] {
                toml_edit::Item::ArrayOfTables(tables) => {
                    let mut rows: Vec<_> = tables.iter().cloned().collect();
                    let mut rule = rule.clone();
                    // Document positions determine serialization order as well as the array order.
                    rule.set_position(
                        rows.get(index.saturating_sub(1))
                            .and_then(toml_edit::Table::position)
                            .unwrap_or(0),
                    );
                    rows.insert(index, rule);
                    *tables = rows.into_iter().collect();
                }
                toml_edit::Item::Value(toml_edit::Value::Array(rows)) => {
                    let mut value = toml_edit::Value::InlineTable(rule.clone().into_inline_table());
                    value
                        .decor_mut()
                        .set_prefix(format!("\n# added {name} at the captain's request\n"));
                    rows.insert_formatted(index, value);
                }
                _ => anyhow::bail!("invalid econ.toml: policy.rules must be a list"),
            }
            rules.insert(index, toml::from_str(&rule.to_string())?);
        }
        if changed {
            let edited = doc.to_string();
            let config = Self::parse(&edited)?;
            crate::settings::atomic(&path, edited.as_bytes())?;
            return Ok(config);
        }
        Ok(config)
    }

    fn preferred(&self, reasoning: Reasoning) -> &str {
        match reasoning {
            Reasoning::Light => &self.reasoning.light,
            Reasoning::Deep => &self.reasoning.deep,
            Reasoning::Max => &self.reasoning.max,
        }
    }

    fn candidate_effort<'a>(
        &'a self,
        candidate: &'a Candidate,
        reasoning: Reasoning,
    ) -> Result<&'a str> {
        let preferred = self.preferred(reasoning);
        let (pinned, class) = match candidate.effort.as_deref() {
            Some("light") => (self.preferred(Reasoning::Light), Some(Reasoning::Light)),
            Some("deep") => (self.preferred(Reasoning::Deep), Some(Reasoning::Deep)),
            Some(effort) => (effort, None),
            None => return Ok(preferred),
        };
        let level = EFFORTS
            .iter()
            .position(|e| *e == pinned)
            .with_context(|| format!("invalid econ.toml: candidate effort {pinned}"))?;
        ensure!(
            level >= EFFORTS.iter().position(|e| *e == preferred).unwrap()
                && class.is_none_or(|class| class >= reasoning)
                || candidate.captain_fallback == Some(true),
            "invalid econ.toml: {} {} pins effort {pinned} below task class {reasoning:?} ({preferred}); lower effort requires captain_fallback=true",
            candidate.harness,
            candidate.model
        );
        Ok(pinned)
    }
}

impl Rule {
    fn matches(&self, task: &TaskFacts) -> bool {
        (self.ranks.is_empty() || self.ranks.contains(&task.rank))
            && (self.kinds.is_empty() || task.kind.as_ref().is_some_and(|k| self.kinds.contains(k)))
            && (self.judgment.is_none() || self.judgment == task.judgment)
            && (self.thoroughness.is_none() || self.thoroughness == task.thoroughness)
    }
}

// Compare numeric version components numerically: 6.10 follows 6.9, 10 follows 9.
// ponytail: newest orders versioned IDs; use harness release metadata if IDs stop versioning.
fn version_key(id: &str) -> Vec<(bool, String)> {
    id.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.bytes().all(|c| c.is_ascii_digit()) {
                let digits = part.trim_start_matches('0');
                (true, format!("{:03}:{digits}", digits.len()))
            } else {
                (false, part.into())
            }
        })
        .collect()
}

fn resolve<'a>(selector: &str, catalog: &'a Catalog) -> Option<&'a Model> {
    if let Some(family) = selector.strip_prefix("newest ") {
        catalog
            .models
            .iter()
            .filter(|m| {
                m.id.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|part| part.eq_ignore_ascii_case(family))
            })
            .max_by_key(|m| (version_key(&m.id), &m.id))
    } else {
        catalog.models.iter().find(|m| m.id == selector)
    }
}

fn effort(model: &Model, preferred: &str, allow_max: bool) -> Option<&'static str> {
    let start = EFFORTS.iter().position(|e| *e == preferred)?;
    EFFORTS[start..]
        .iter()
        .find(|e| (allow_max || **e != "max") && model.efforts.iter().any(|s| s == **e))
        .copied()
}

/// Pure decision from one coherent snapshot. Overrides constrain admission too.
pub fn route(
    config: &Config,
    task: &TaskFacts,
    overrides: &Overrides,
    catalogs: &BTreeMap<String, Catalog>,
    by: usize,
    now: u64,
) -> Result<Decision> {
    ensure!((1..=3).contains(&task.rank), "task rank must be 1, 2 or 3");
    ensure!(
        task.kind.as_deref().is_none_or(|k| KINDS.contains(&k)),
        "invalid --kind"
    );
    ensure!(level_valid(&task.judgment), "--judgment takes low|high");
    ensure!(
        level_valid(&task.thoroughness),
        "--thoroughness takes low|high"
    );
    ensure!(
        overrides.harness.as_deref().is_none_or(harness_valid),
        "--on takes codex|claude"
    );
    ensure!(
        overrides.model.as_deref().is_none_or(model_id_valid),
        "--model takes an exact model id"
    );
    ensure!(
        overrides
            .effort
            .as_deref()
            .is_none_or(|e| EFFORTS.contains(&e)),
        "unsupported --effort"
    );
    let (role_rule, class_rule) = config.rules_for(task);
    let role = role_rule
        .and_then(|r| r.role)
        .unwrap_or(config.policy.default_role);
    let reasoning = class_rule
        .and_then(|r| r.reasoning)
        .unwrap_or(config.policy.default_reasoning);
    let preferred = config.preferred(reasoning);
    let overridden =
        overrides.harness.is_some() || overrides.model.is_some() || overrides.effort.is_some();
    let mut decision = Decision {
        inputs: task.clone(),
        overrides: overrides.clone(),
        override_by: overridden.then_some(by),
        role,
        reasoning,
        reason: format!(
            "{}; {}{}",
            role_rule.map_or("default role", |r| r.reason.as_str()),
            class_rule
                .map(|r| r.reason.clone())
                .unwrap_or_else(|| format!("default {reasoning:?} reasoning").to_lowercase()),
            if overridden {
                format!("; override by PID {by}")
            } else {
                String::new()
            }
        ),
        chosen: None,
        skipped: vec![],
        hold_reason: None,
    };
    if (overrides.effort.as_deref() == Some("max")
        || (overrides.effort.is_none() && (reasoning == Reasoning::Max || preferred == "max")))
        && !config.policy.allow_max
    {
        decision.hold_reason =
            Some("max reasoning requires the captain's explicit policy permission".into());
        return Ok(decision);
    }
    let mut candidates = config.profiles.get(&role).cloned().unwrap_or_default();
    if let Some(harness) = &overrides.harness {
        candidates.retain(|c| &c.harness == harness);
        if candidates.is_empty() && overrides.model.is_none() {
            candidates.push(Candidate {
                harness: harness.clone(),
                model: catalogs
                    .get(harness)
                    .and_then(|c| c.models.iter().find(|m| m.is_default))
                    .map(|m| m.id.clone())
                    .unwrap_or_default(),
                ..Default::default()
            });
        }
    }
    if let Some(model) = &overrides.model {
        // Exact model overrides may name a harness outside this role's profile.
        let mut harnesses: Vec<String> = candidates.iter().map(|c| c.harness.clone()).collect();
        if let Some(harness) = &overrides.harness {
            harnesses = vec![harness.clone()];
        } else {
            harnesses.extend(
                catalogs
                    .keys()
                    .filter(|h| harness_valid(h) && !harnesses.contains(h))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        candidates.clear();
        for harness in harnesses {
            let candidate = Candidate {
                harness,
                model: model.clone(),
                ..Default::default()
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    // Validate every candidate before availability checks; a bad pin is a config error,
    // even if an earlier candidate is ready or this candidate's catalog is missing.
    if overrides.effort.is_none() {
        for candidate in &candidates {
            config.candidate_effort(candidate, reasoning)?;
        }
    }
    let max_age = config.catalog.max_age_minutes * 60_000;
    for candidate in candidates {
        let candidate_preferred = if overrides.effort.is_none() {
            config.candidate_effort(&candidate, reasoning)?
        } else {
            preferred
        };
        let catalog = catalogs.get(&candidate.harness);
        let age = catalog.and_then(|c| now.checked_sub(c.observed_at));
        let selection = (|| -> std::result::Result<Selection, String> {
            let c = catalog.ok_or("harness catalog missing")?;
            if let Some(error) = &c.error {
                return Err(format!("catalog unavailable: {error}"));
            }
            let age = age
                .filter(|a| *a <= max_age && c.observed_at != 0)
                .ok_or("catalog stale or timestamp invalid")?;
            match c.signed_in {
                Some(true) => {}
                Some(false) => return Err("not signed in".into()),
                None => return Err("sign-in state unknown".into()),
            }
            let model = resolve(&candidate.model, c).ok_or("model missing from catalog")?;
            let chosen_effort = if let Some(e) = overrides.effort.as_deref() {
                if !model.efforts.iter().any(|supported| supported == e) {
                    return Err(format!("unsupported effort: {e}"));
                }
                e
            } else if candidate
                .effort
                .as_deref()
                .is_some_and(|e| !matches!(e, "light" | "deep"))
            {
                if candidate_preferred == "max" && !config.policy.allow_max {
                    return Err("max effort requires allow_max=true".into());
                }
                if !model.efforts.iter().any(|e| e == candidate_preferred) {
                    return Err(format!(
                        "unsupported candidate effort: {candidate_preferred}"
                    ));
                }
                candidate_preferred
            } else {
                effort(model, candidate_preferred, config.policy.allow_max)
                    .ok_or_else(|| format!("unsupported reasoning class: {reasoning:?} requires effort >= {candidate_preferred} (max needs allow_max)"))?
            };
            let quota_age = now
                .checked_sub(c.quota_observed_at)
                .filter(|_| c.quota_observed_at != 0);
            let fresh = quota_age.is_some_and(|a| a <= max_age);
            if fresh && c.quota_available == Some(false) {
                return Err("account quota unavailable".into());
            }
            if fresh && model.quota_available == Some(false) {
                return Err("model quota unavailable".into());
            }
            let quota_available = (fresh && model.quota_error.is_none())
                .then_some(c.quota_available)
                .flatten();
            Ok(Selection {
                captain_fallback: candidate.captain_fallback == Some(true)
                    && candidate.effort.is_some()
                    && overrides.effort.is_none(),
                harness: candidate.harness.clone(),
                selector: candidate.model.clone(),
                model: model.id.clone(),
                effort: chosen_effort.into(),
                catalog_age_ms: age,
                quota_age_ms: quota_age,
                quota_available,
            })
        })();
        match selection {
            Ok(chosen) => {
                if chosen.quota_available.is_none() {
                    decision.reason = format!("quota unknown, admitted; {}", decision.reason);
                }
                if chosen.captain_fallback {
                    decision.reason = format!(
                        "captain fallback: {} at {}; {}",
                        chosen.model, chosen.effort, decision.reason
                    );
                }
                decision.chosen = Some(chosen);
                break;
            }
            Err(reason) => decision.skipped.push(Skipped {
                candidate,
                reason,
                catalog_age_ms: age,
            }),
        }
    }
    if decision.chosen.is_none() {
        decision.hold_reason = Some(
            "no candidate satisfies catalog, sign-in, reasoning and quota requirements".into(),
        );
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: u64 = 2_000_000;

    fn catalogs() -> BTreeMap<String, Catalog> {
        [
            ("codex", ["gpt-6.9-sol", "gpt-6.10-sol"]),
            ("claude", ["claude-opus-9", "claude-opus-10"]),
        ]
        .into_iter()
        .map(|(h, ids)| {
            (
                h.into(),
                Catalog {
                    observed_at: NOW - 100,
                    signed_in: Some(true),
                    quota_available: Some(true),
                    quota_observed_at: NOW - 50,
                    models: ids
                        .into_iter()
                        .map(|id| Model {
                            id: id.into(),
                            efforts: if h == "codex" {
                                ["low", "medium", "high", "xhigh"]
                                    .map(str::to_owned)
                                    .to_vec()
                            } else {
                                ["low", "medium", "high", "xhigh", "max"]
                                    .map(str::to_owned)
                                    .to_vec()
                            },
                            is_default: id.ends_with("10-sol") || id.ends_with("10"),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                },
            )
        })
        .collect()
    }

    fn research_catalogs() -> BTreeMap<String, Catalog> {
        let mut catalogs = catalogs();
        for id in ["gpt-6.9-luna", "gpt-6.10-luna"] {
            catalogs.get_mut("codex").unwrap().models.push(Model {
                id: id.into(),
                efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
                ..Default::default()
            });
        }
        for id in ["claude-sonnet-5-4", "claude-sonnet-5-5"] {
            catalogs.get_mut("claude").unwrap().models.push(Model {
                id: id.into(),
                efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
                ..Default::default()
            });
        }
        catalogs
    }

    #[test]
    fn research_routes_sonnet_and_explicit_captain_fallback() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        let task = TaskFacts {
            kind: Some("research".into()),
            ..Default::default()
        };
        for thoroughness in [None, Some("high")] {
            let task = TaskFacts {
                thoroughness: thoroughness.map(str::to_owned),
                ..task.clone()
            };
            let decision = route(
                &config,
                &task,
                &Overrides::default(),
                &research_catalogs(),
                1,
                NOW,
            )
            .unwrap();
            assert_eq!(decision.role, Role::Research);
            let chosen = decision.chosen.unwrap();
            assert_eq!(
                (
                    chosen.harness.as_str(),
                    chosen.selector.as_str(),
                    chosen.model.as_str()
                ),
                ("claude", "newest sonnet", "claude-sonnet-5-5")
            );
            assert_eq!(chosen.effort, "high");
            assert!(!chosen.captain_fallback);
            assert!(!decision.reason.contains("captain fallback"));
        }
        for cause in [
            "missing model",
            "missing harness",
            "account quota",
            "model quota",
        ] {
            let mut catalogs = research_catalogs();
            match cause {
                "missing model" => catalogs
                    .get_mut("claude")
                    .unwrap()
                    .models
                    .retain(|m| !m.id.contains("sonnet")),
                "missing harness" => {
                    catalogs.remove("claude");
                }
                "account quota" => {
                    catalogs.get_mut("claude").unwrap().quota_available = Some(false)
                }
                "model quota" => {
                    catalogs
                        .get_mut("claude")
                        .unwrap()
                        .models
                        .last_mut()
                        .unwrap()
                        .quota_available = Some(false)
                }
                _ => unreachable!(),
            }
            let decision = route(&config, &task, &Overrides::default(), &catalogs, 1, NOW).unwrap();
            assert_eq!(decision.reasoning, Reasoning::Deep, "{cause}");
            let chosen = decision.chosen.as_ref().unwrap();
            assert_eq!(
                (
                    chosen.harness.as_str(),
                    chosen.selector.as_str(),
                    chosen.model.as_str(),
                    chosen.effort.as_str()
                ),
                ("codex", "newest sol", "gpt-6.10-sol", "low"),
                "{cause}"
            );
            assert!(chosen.captain_fallback);
            assert!(
                decision
                    .reason
                    .starts_with("captain fallback: gpt-6.10-sol at low;")
            );
            assert_eq!(decision.skipped.len(), 1);
            let saved = serde_json::to_value(&decision).unwrap();
            assert_eq!(saved["chosen"]["captain_fallback"], true);
            let restored: Decision = serde_json::from_value(saved).unwrap();
            assert!(restored.chosen.unwrap().captain_fallback);
        }
        for task in [
            TaskFacts {
                judgment: Some("high".into()),
                ..task.clone()
            },
            TaskFacts {
                kind: Some("decide".into()),
                ..task.clone()
            },
        ] {
            let decision = route(
                &config,
                &task,
                &Overrides::default(),
                &research_catalogs(),
                1,
                NOW,
            )
            .unwrap();
            assert_eq!(decision.role, Role::Judge);
            let chosen = decision.chosen.unwrap();
            assert_eq!(
                (chosen.model.as_str(), chosen.effort.as_str()),
                ("claude-opus-10", "high")
            );
            assert!(!chosen.captain_fallback);
        }
    }

    #[test]
    fn quick_research_uses_luna_medium_then_sonnet_light() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        let task = TaskFacts {
            kind: Some("research".into()),
            thoroughness: Some("low".into()),
            ..Default::default()
        };
        for cause in [
            "ready",
            "missing model",
            "signed out",
            "account quota",
            "model quota",
            "unsupported effort",
        ] {
            let mut catalogs = research_catalogs();
            let codex = catalogs.get_mut("codex").unwrap();
            match cause {
                "missing model" => codex.models.retain(|m| !m.id.contains("luna")),
                "signed out" => codex.signed_in = Some(false),
                "account quota" => codex.quota_available = Some(false),
                "model quota" => codex.models.last_mut().unwrap().quota_available = Some(false),
                "unsupported effort" => {
                    codex.models.last_mut().unwrap().efforts = vec!["high".into()]
                }
                _ => (),
            }
            let decision = route(&config, &task, &Overrides::default(), &catalogs, 1, NOW).unwrap();
            assert_eq!(
                (decision.role, decision.reasoning),
                (Role::ResearchQuick, Reasoning::Light)
            );
            let chosen = decision.chosen.as_ref().unwrap();
            let expected = if cause == "ready" {
                ("codex", "newest luna", "gpt-6.10-luna", "medium")
            } else {
                ("claude", "newest sonnet", "claude-sonnet-5-5", "low")
            };
            assert_eq!(
                (
                    chosen.harness.as_str(),
                    chosen.selector.as_str(),
                    chosen.model.as_str(),
                    chosen.effort.as_str()
                ),
                expected,
                "{cause}"
            );
            assert!(!chosen.captain_fallback);
            assert!(!decision.reason.contains("captain fallback"));
            assert_eq!(decision.skipped.len(), usize::from(cause != "ready"));
            assert_eq!(
                serde_json::to_value(&decision).unwrap()["role"],
                "research_quick"
            );
        }
        for (kind, judgment) in [("research", Some("high")), ("decide", None)] {
            let decision = route(
                &config,
                &TaskFacts {
                    kind: Some(kind.into()),
                    judgment: judgment.map(str::to_owned),
                    ..task.clone()
                },
                &Overrides::default(),
                &research_catalogs(),
                1,
                NOW,
            )
            .unwrap();
            assert_eq!(decision.role, Role::Judge);
            assert_eq!(decision.chosen.unwrap().model, "claude-opus-10");
        }
        // Light is a class pin: the fallback follows captain effort preferences.
        let mut config = config;
        config.reasoning.light = "medium".into();
        let mut catalogs = research_catalogs();
        catalogs.remove("codex");
        assert_eq!(
            route(&config, &task, &Overrides::default(), &catalogs, 1, NOW)
                .unwrap()
                .chosen
                .unwrap()
                .effort,
            "medium"
        );
    }

    #[test]
    fn candidate_pins_are_validated_and_scoped_to_the_candidate() {
        for pin in ["low", "light", "invented"] {
            let mut config = Config::parse(DEFAULT_CONFIG).unwrap();
            let fallback = &mut config.profiles.get_mut(&Role::Research).unwrap()[1];
            fallback.effort = Some(pin.into());
            fallback.captain_fallback = None;
            let error = Config::parse(&toml::to_string(&config).unwrap())
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(if pin == "invented" {
                    "invalid candidate effort"
                } else {
                    "captain_fallback=true"
                }),
                "{error}"
            );
        }
        let mut config = Config::parse(DEFAULT_CONFIG).unwrap();
        let task = TaskFacts {
            kind: Some("research".into()),
            ..Default::default()
        };
        config.profiles.get_mut(&Role::Research).unwrap()[1].captain_fallback = Some(false);
        assert!(
            route(
                &config,
                &task,
                &Overrides::default(),
                &research_catalogs(),
                1,
                NOW
            )
            .unwrap_err()
            .to_string()
            .contains("captain_fallback=true")
        );
        let mut config = Config::parse(DEFAULT_CONFIG).unwrap();
        let mut catalogs = research_catalogs();
        catalogs.remove("claude");
        config.profiles.get_mut(&Role::Research).unwrap()[1].effort = Some("light".into());
        let decision = route(&config, &task, &Overrides::default(), &catalogs, 1, NOW).unwrap();
        assert_eq!(decision.chosen.unwrap().effort, "low");
        // Explicit task effort overrides replace the profile pin and carry their own receipt.
        let decision = route(
            &config,
            &task,
            &Overrides {
                effort: Some("high".into()),
                ..Default::default()
            },
            &catalogs,
            1,
            NOW,
        )
        .unwrap();
        let chosen = decision.chosen.unwrap();
        assert_eq!(chosen.effort, "high");
        assert!(!chosen.captain_fallback);
        assert!(decision.reason.contains("override by PID 1"));
        assert!(!decision.reason.contains("captain fallback"));
        // A stronger class pin raises only its candidate, even on a light task.
        config
            .policy
            .rules
            .retain(|r| r.role != Some(Role::ResearchQuick));
        config.profiles.get_mut(&Role::Research).unwrap()[0].effort = Some("deep".into());
        let light = TaskFacts {
            thoroughness: Some("low".into()),
            ..task
        };
        let decision = route(
            &config,
            &light,
            &Overrides::default(),
            &research_catalogs(),
            1,
            NOW,
        )
        .unwrap();
        assert_eq!(decision.reasoning, Reasoning::Light);
        assert_eq!(decision.chosen.unwrap().effort, "high");
        // Named pins require exact support; class pins may fall up to harness support.
        let mut catalogs = research_catalogs();
        catalogs
            .get_mut("claude")
            .unwrap()
            .models
            .last_mut()
            .unwrap()
            .efforts = vec!["medium".into(), "high".into()];
        config.profiles.get_mut(&Role::Research).unwrap()[0].effort = Some("light".into());
        let decision = route(&config, &light, &Overrides::default(), &catalogs, 1, NOW).unwrap();
        assert_eq!(decision.chosen.unwrap().effort, "medium");
        config.profiles.get_mut(&Role::Research).unwrap()[0].effort = Some("low".into());
        let decision = route(&config, &light, &Overrides::default(), &catalogs, 1, NOW).unwrap();
        assert_eq!(decision.chosen.unwrap().harness, "codex");
        assert!(
            decision.skipped[0]
                .reason
                .contains("unsupported candidate effort")
        );
    }

    #[test]
    fn research_migration_preserves_edits_comments_and_is_idempotent() {
        let home =
            std::env::temp_dir().join(format!("econ-research-migrate-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let path = home.join("econ.toml");
        let mut doc: toml_edit::DocumentMut = DEFAULT_CONFIG.parse().unwrap();
        doc["profiles"].as_table_mut().unwrap().remove("research");
        doc["profiles"]
            .as_table_mut()
            .unwrap()
            .remove("research_quick");
        doc["policy"]["rules"]
            .as_array_of_tables_mut()
            .unwrap()
            .retain(|r| {
                !matches!(
                    r.get("role").and_then(toml_edit::Item::as_str),
                    Some("research" | "research_quick")
                )
            });
        let old = doc
            .to_string()
            .replace("allow_max = false", "allow_max = true # keep captain edit")
            .replace("deep = \"high\"", "deep = \"xhigh\" # keep effort")
            .replace("newest sol", "gpt-6.9-sol")
            + "\n# keep trailing note\n";
        fs::write(&path, &old).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    Config::load(&home).unwrap();
                });
            }
        });
        let migrated = fs::read_to_string(&path).unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), migrated);
        assert_eq!(
            migrated
                .matches("# added 2026-10-01 at the captain's request")
                .count(),
            1
        );
        for line in old.lines().filter(|line| !line.is_empty()) {
            assert!(migrated.contains(line), "Lost captain text: {line}");
        }
        assert!(config.policy.allow_max);
        assert_eq!(config.reasoning.deep, "xhigh");
        assert_eq!(config.profiles[&Role::Worker][0].model, "gpt-6.9-sol");
        assert_eq!(
            config.profiles[&Role::Research],
            Config::parse(DEFAULT_CONFIG).unwrap().profiles[&Role::Research]
        );
        let research_index = config
            .policy
            .rules
            .iter()
            .position(|r| r.role == Some(Role::Research))
            .unwrap();
        assert!(
            config.policy.rules[..research_index]
                .iter()
                .any(|r| r.judgment.as_deref() == Some("high"))
        );
        assert!(
            config.policy.rules[..research_index]
                .iter()
                .any(|r| r.kinds == ["decide"])
        );
        // An existing research profile is captain-owned, even without a routing rule.
        let mut customized: toml_edit::DocumentMut = old.parse().unwrap();
        customized["profiles"]["research"] = "[{harness='codex', model='newest sol'}]"
            .parse::<toml_edit::Value>()
            .unwrap()
            .into();
        let customized = customized.to_string();
        fs::write(&path, &customized).unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(config.profiles[&Role::Research][0].harness, "codex");
        assert_eq!(config.profiles[&Role::Research][0].model, "newest sol");
        assert!(config.profiles.contains_key(&Role::ResearchQuick));
        assert!(
            !config
                .policy
                .rules
                .iter()
                .any(|r| r.role == Some(Role::Research))
        );
        let migrated = fs::read_to_string(&path).unwrap();
        for line in customized.lines().filter(|line| !line.is_empty()) {
            assert!(migrated.contains(line), "Lost captain text: {line}");
        }
        Config::load(&home).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), migrated);
        // Inline policy/rule/profile tables migrate without rewriting existing values.
        let inline = "# keep inline\npolicy = {default_role='worker', default_reasoning='deep', allow_max=false, rules=[{judgment='high', role='judge', reason='judgment'}, {kinds=['decide'], role='judge', reason='decision'}]} # keep policy\nprofiles = {judge=[{harness='claude', model='newest opus'}], worker=[{harness='codex', model='newest sol'}], design=[{harness='claude', model='newest opus'}]} # keep profiles\ncatalog = {refresh_minutes=5, max_age_minutes=15}\nreasoning = {light='low', deep='high', max='max'} # keep preferences\n";
        fs::write(&path, inline).unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(config.policy.rules[2].role, Some(Role::ResearchQuick));
        assert_eq!(config.policy.rules[3].role, Some(Role::Research));
        let migrated = fs::read_to_string(&path).unwrap();
        for note in [
            "# keep inline",
            "# keep policy",
            "# keep profiles",
            "# keep preferences",
            "# added research at the captain's request",
            "# added research_quick at the captain's request",
        ] {
            assert!(migrated.contains(note), "Lost {note}");
        }
        Config::load(&home).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), migrated);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn provisional_research_upgrade_preserves_captain_profiles_and_rules() {
        let home =
            std::env::temp_dir().join(format!("econ-research-upgrade-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let path = home.join("econ.toml");
        let defaults = Config::parse(DEFAULT_CONFIG).unwrap();
        let mut doc: toml_edit::DocumentMut = DEFAULT_CONFIG.parse().unwrap();
        doc["profiles"]
            .as_table_mut()
            .unwrap()
            .remove("research_quick");
        doc["policy"]["rules"]
            .as_array_of_tables_mut()
            .unwrap()
            .retain(|r| r.get("role").and_then(toml_edit::Item::as_str) != Some("research_quick"));
        doc["profiles"]["research"] = PROVISIONAL_RESEARCH
            .parse::<toml_edit::Value>()
            .unwrap()
            .into();
        doc["profiles"]["research"]
            .as_value_mut()
            .unwrap()
            .decor_mut()
            .set_suffix(" # keep profile note");
        let provisional = doc.to_string() + "\n# keep end note\n";
        for edit in [
            None,
            Some("model"),
            Some("effort"),
            Some("fallback"),
            Some("order"),
            Some("quick"),
            Some("tables"),
        ] {
            let mut doc: toml_edit::DocumentMut = provisional.parse().unwrap();
            let rows = doc["profiles"]["research"].as_array_mut().unwrap();
            let primary = rows.get_mut(0).unwrap().as_inline_table_mut().unwrap();
            match edit {
                Some("model") => {
                    primary.insert("model", toml_edit::Value::from("claude-sonnet-5-4"));
                }
                Some("effort") => {
                    primary.insert("effort", toml_edit::Value::from("high"));
                }
                Some("fallback") => {
                    primary.insert("captain_fallback", toml_edit::Value::from(false));
                }
                Some("order") => {
                    let mut reversed: Vec<_> = rows.iter().cloned().collect();
                    reversed.reverse();
                    rows.clear();
                    for value in reversed {
                        rows.push_formatted(value);
                    }
                }
                Some("quick") => {
                    doc["profiles"]["research_quick"] =
                        "[{harness='codex',model='newest sol',effort='high'}]"
                            .parse::<toml_edit::Value>()
                            .unwrap()
                            .into();
                }
                Some("tables") => {
                    doc = toml::to_string(&Config::parse(&provisional).unwrap())
                        .unwrap()
                        .parse()
                        .unwrap();
                    doc["profiles"]["research"]
                        .as_array_of_tables_mut()
                        .unwrap()
                        .get_mut(0)
                        .unwrap()
                        .decor_mut()
                        .set_prefix("# keep profile note\n");
                    doc.decor_mut().set_suffix("\n# keep end note\n");
                }
                _ => (),
            }
            let before = doc.to_string();
            let expected = Config::parse(&before).unwrap();
            fs::write(&path, &before).unwrap();
            let migrated = Config::load(&home).unwrap();
            assert_eq!(
                migrated.profiles[&Role::Research],
                if matches!(edit, None | Some("quick" | "tables")) {
                    &defaults.profiles[&Role::Research]
                } else {
                    &expected.profiles[&Role::Research]
                }
                .clone(),
                "{edit:?}"
            );
            if edit == Some("quick") {
                assert_eq!(
                    migrated.profiles[&Role::ResearchQuick],
                    expected.profiles[&Role::ResearchQuick]
                );
                assert!(
                    !migrated
                        .policy
                        .rules
                        .iter()
                        .any(|r| r.role == Some(Role::ResearchQuick))
                );
            } else {
                assert_eq!(
                    migrated.profiles[&Role::ResearchQuick],
                    defaults.profiles[&Role::ResearchQuick]
                );
                for thoroughness in [None, Some("low"), Some("high")] {
                    let decision = route(
                        &migrated,
                        &TaskFacts {
                            kind: Some("research".into()),
                            thoroughness: thoroughness.map(str::to_owned),
                            ..Default::default()
                        },
                        &Overrides::default(),
                        &research_catalogs(),
                        1,
                        NOW,
                    )
                    .unwrap();
                    assert_eq!(
                        decision.role,
                        if thoroughness == Some("low") {
                            Role::ResearchQuick
                        } else {
                            Role::Research
                        }
                    );
                }
            }
            let after = fs::read_to_string(&path).unwrap();
            assert!(after.contains("# keep profile note"));
            assert!(after.contains("# keep end note"));
            let old_rules: Vec<_> = expected
                .policy
                .rules
                .iter()
                .map(|r| serde_json::to_value(r).unwrap())
                .collect();
            let retained: Vec<_> = migrated
                .policy
                .rules
                .iter()
                .filter(|r| r.role != Some(Role::ResearchQuick))
                .map(|r| serde_json::to_value(r).unwrap())
                .collect();
            assert_eq!(retained, old_rules);
            Config::load(&home).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), after);
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn policy_table_and_family_resolution() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        for (rank, kind, judgment, thoroughness, role, class, harness, model, effort) in [
            (
                3,
                None,
                None,
                None,
                Role::Worker,
                Reasoning::Deep,
                "codex",
                "gpt-6.10-sol",
                "high",
            ),
            (
                3,
                Some("research"),
                None,
                Some("low"),
                Role::ResearchQuick,
                Reasoning::Light,
                "codex",
                "gpt-6.10-luna",
                "medium",
            ),
            (
                3,
                Some("implement"),
                None,
                Some("high"),
                Role::Worker,
                Reasoning::Deep,
                "codex",
                "gpt-6.10-sol",
                "high",
            ),
            (
                3,
                Some("review"),
                None,
                None,
                Role::Worker,
                Reasoning::Deep,
                "codex",
                "gpt-6.10-sol",
                "high",
            ),
            (
                3,
                Some("validate"),
                None,
                None,
                Role::Worker,
                Reasoning::Deep,
                "codex",
                "gpt-6.10-sol",
                "high",
            ),
            (
                3,
                Some("design"),
                Some("high"),
                None,
                Role::Design,
                Reasoning::Deep,
                "claude",
                "claude-opus-10",
                "high",
            ),
            (
                3,
                None,
                Some("high"),
                None,
                Role::Judge,
                Reasoning::Deep,
                "claude",
                "claude-opus-10",
                "high",
            ),
            (
                3,
                Some("decide"),
                None,
                None,
                Role::Judge,
                Reasoning::Deep,
                "claude",
                "claude-opus-10",
                "high",
            ),
            (
                1,
                Some("implement"),
                None,
                None,
                Role::Judge,
                Reasoning::Deep,
                "claude",
                "claude-opus-10",
                "high",
            ),
            (
                2,
                None,
                None,
                None,
                Role::Judge,
                Reasoning::Deep,
                "claude",
                "claude-opus-10",
                "high",
            ),
        ] {
            let task = TaskFacts {
                rank,
                kind: kind.map(str::to_owned),
                judgment: judgment.map(str::to_owned),
                thoroughness: thoroughness.map(str::to_owned),
            };
            let decision = route(
                &config,
                &task,
                &Overrides::default(),
                &research_catalogs(),
                7,
                NOW,
            )
            .unwrap();
            assert_eq!(
                (decision.role, decision.reasoning),
                (role, class),
                "{task:?}"
            );
            let chosen = decision.chosen.unwrap();
            assert_eq!(
                (
                    chosen.harness.as_str(),
                    chosen.model.as_str(),
                    chosen.effort.as_str()
                ),
                (harness, model, effort),
                "{task:?}"
            );
            assert_eq!(chosen.catalog_age_ms, 100);
            assert_eq!(chosen.quota_age_ms, Some(50));
            assert!(decision.override_by.is_none());
        }
    }

    #[test]
    fn reasoning_preferences_fall_up_per_harness_never_down() {
        for harness in ["codex", "claude"] {
            for (class, preferred, supported, allow_max, expected) in [
                (
                    Reasoning::Deep,
                    "high",
                    "max xhigh high medium low",
                    false,
                    Some("high"),
                ),
                (
                    Reasoning::Deep,
                    "high",
                    "max xhigh medium low",
                    false,
                    Some("xhigh"),
                ),
                (Reasoning::Deep, "high", "medium low", true, None),
                (Reasoning::Deep, "high", "max medium low", false, None),
                (Reasoning::Deep, "high", "max medium low", true, Some("max")),
                (
                    Reasoning::Deep,
                    "medium",
                    "high medium low",
                    false,
                    Some("medium"),
                ),
                (
                    Reasoning::Deep,
                    "xhigh",
                    "max xhigh high",
                    false,
                    Some("xhigh"),
                ),
                (
                    Reasoning::Light,
                    "low",
                    "none minimal medium high",
                    false,
                    Some("medium"),
                ),
                (Reasoning::Light, "low", "none minimal", false, None),
                (Reasoning::Deep, "max", "max high", false, None),
                (Reasoning::Max, "max", "max xhigh high", false, None),
                (Reasoning::Max, "max", "max xhigh high", true, Some("max")),
                (Reasoning::Max, "max", "xhigh high", true, None),
            ] {
                let mut config = Config::parse(DEFAULT_CONFIG).unwrap();
                config.policy.default_reasoning = class;
                config.policy.allow_max = allow_max;
                match class {
                    Reasoning::Light => config.reasoning.light = preferred.into(),
                    Reasoning::Deep => config.reasoning.deep = preferred.into(),
                    Reasoning::Max => config.reasoning.max = preferred.into(),
                }
                let mut evidence = catalogs();
                for model in &mut evidence.get_mut(harness).unwrap().models {
                    model.efforts = supported.split_whitespace().map(str::to_owned).collect();
                }
                let decision = route(
                    &config,
                    &TaskFacts::default(),
                    &Overrides {
                        harness: Some(harness.into()),
                        ..Default::default()
                    },
                    &evidence,
                    7,
                    NOW,
                )
                .unwrap();
                assert_eq!(
                    decision.chosen.as_ref().map(|c| c.effort.as_str()),
                    expected,
                    "{harness} {class:?} {preferred} {supported} allow_max={allow_max}"
                );
                assert_eq!(decision.hold_reason.is_some(), expected.is_none());
            }
        }
    }

    #[test]
    fn missing_reasoning_migration_preserves_edits_and_is_idempotent() {
        let home = std::env::temp_dir().join(format!("econ-migrate-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let path = home.join("econ.toml");
        let old = DEFAULT_CONFIG
            .split("\n[reasoning]")
            .next()
            .unwrap()
            .replace("allow_max = false", "allow_max = true")
            .replace("newest sol", "gpt-6.9-sol")
            + "\n# Captain note mentioning [reasoning] without a table";
        assert_eq!(Config::parse(&old).unwrap().reasoning.deep, "high");
        fs::write(&path, &old).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let config = Config::load(&home).unwrap();
                    assert!(config.policy.allow_max);
                    assert_eq!(config.reasoning.deep, "high");
                    assert_eq!(config.profiles[&Role::Worker][0].model, "gpt-6.9-sol");
                });
            }
        });
        let migrated = fs::read_to_string(&path).unwrap();
        assert!(migrated.starts_with(&old));
        assert_eq!(migrated.matches("\n[reasoning]\n").count(), 1);
        Config::load(&home).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), migrated);

        // Existing partial preferences also remain byte-for-byte captain-owned.
        let edited = old.clone() + "\n[reasoning]\ndeep = \"xhigh\" # captain preference";
        fs::write(&path, &edited).unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(
            (
                config.reasoning.light.as_str(),
                config.reasoning.deep.as_str()
            ),
            ("low", "xhigh")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);

        // A malformed policy is rejected without appending or rewriting anything.
        let invalid = old.replace("allow_max = true", "allow_max = \"invalid\"");
        fs::write(&path, &invalid).unwrap();
        assert!(Config::load(&home).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn overrides_win_without_bypassing_admission() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        for (harness, model, effort, expected) in [
            (
                Some("claude"),
                None,
                None,
                Some(("claude", "claude-opus-10", "high")),
            ),
            (
                None,
                Some("gpt-6.9-sol"),
                None,
                Some(("codex", "gpt-6.9-sol", "high")),
            ),
            (
                None,
                Some("claude-opus-9"),
                Some("low"),
                Some(("claude", "claude-opus-9", "low")),
            ),
            (Some("codex"), Some("claude-opus-9"), None, None),
            (Some("codex"), None, Some("max"), None),
            (
                Some("claude"),
                None,
                Some("xhigh"),
                Some(("claude", "claude-opus-10", "xhigh")),
            ),
            (Some("claude"), None, Some("minimal"), None),
            (
                Some("codex"),
                None,
                Some("xhigh"),
                Some(("codex", "gpt-6.10-sol", "xhigh")),
            ),
            (
                None,
                None,
                Some("low"),
                Some(("codex", "gpt-6.10-sol", "low")),
            ),
        ] {
            let overrides = Overrides {
                harness: harness.map(str::to_owned),
                model: model.map(str::to_owned),
                effort: effort.map(str::to_owned),
            };
            let d = route(
                &config,
                &TaskFacts::default(),
                &overrides,
                &catalogs(),
                42,
                NOW,
            )
            .unwrap();
            assert_eq!(d.override_by, Some(42));
            assert!(d.reason.contains("override by PID 42"));
            assert_eq!(
                d.chosen.as_ref().map(|c| (
                    c.harness.as_str(),
                    c.model.as_str(),
                    c.effort.as_str()
                )),
                expected
            );
            assert_eq!(d.hold_reason.is_some(), expected.is_none());
        }
        // A harness override for a judge uses the harness-reported default, not a guessed ID.
        let task = TaskFacts {
            rank: 1,
            ..Default::default()
        };
        let d = route(
            &config,
            &task,
            &Overrides {
                harness: Some("codex".into()),
                ..Default::default()
            },
            &catalogs(),
            1,
            NOW,
        )
        .unwrap();
        assert_eq!(d.chosen.unwrap().model, "gpt-6.10-sol");
        let mut allowed = config;
        allowed.policy.allow_max = true;
        let d = route(
            &allowed,
            &task,
            &Overrides {
                harness: Some("claude".into()),
                effort: Some("max".into()),
                ..Default::default()
            },
            &catalogs(),
            1,
            NOW,
        )
        .unwrap();
        assert_eq!(d.chosen.unwrap().effort, "max");
    }

    #[test]
    fn unknown_and_stale_quota_admit_with_warning_for_both_harnesses() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        for harness in ["codex", "claude"] {
            for (available, observed_at, model_unknown) in [
                (None, NOW - 50, false),
                (Some(false), 1, false),
                (Some(true), 0, false),
                (Some(false), NOW + 1, false),
                (Some(true), NOW - 50, true),
            ] {
                let mut evidence = catalogs();
                let c = evidence.get_mut(harness).unwrap();
                c.quota_available = available;
                c.quota_observed_at = observed_at;
                if model_unknown {
                    for m in &mut c.models {
                        m.quota_error = Some("window unknown".into());
                    }
                }
                let d = route(
                    &config,
                    &TaskFacts::default(),
                    &Overrides {
                        harness: Some(harness.into()),
                        ..Default::default()
                    },
                    &evidence,
                    7,
                    NOW,
                )
                .unwrap();
                assert!(d.skipped.is_empty());
                assert_eq!(d.chosen.as_ref().unwrap().harness, harness);
                assert_eq!(d.chosen.as_ref().unwrap().quota_available, None);
                assert!(d.reason.contains("quota unknown, admitted"));
                let receipt = serde_json::to_value(d).unwrap();
                assert!(receipt["chosen"]["quota_available"].is_null());
            }
        }
    }

    #[test]
    fn rejection_table_falls_back_without_weakening_then_holds() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        for cause in [
            "missing catalog",
            "missing model",
            "unsupported effort",
            "signed out",
            "unknown sign-in",
            "quota empty",
            "model quota empty",
            "stale catalog",
            "future catalog",
            "probe error",
        ] {
            let mut evidence = catalogs();
            let c = evidence.get_mut("codex").unwrap();
            match cause {
                "missing catalog" => {
                    evidence.remove("codex");
                }
                "missing model" => c.models.clear(),
                "unsupported effort" => {
                    for m in &mut c.models {
                        m.efforts = vec!["max".into()];
                    }
                }
                "signed out" => c.signed_in = Some(false),
                "unknown sign-in" => c.signed_in = None,
                "quota empty" => c.quota_available = Some(false),
                "model quota empty" => {
                    for m in &mut c.models {
                        m.quota_available = Some(false);
                    }
                }
                "stale catalog" => c.observed_at = 1,
                "future catalog" => c.observed_at = NOW + 1,
                "probe error" => c.error = Some("recorded refusal".into()),
                _ => unreachable!(),
            }
            let d = route(
                &config,
                &TaskFacts::default(),
                &Overrides::default(),
                &evidence,
                7,
                NOW,
            )
            .unwrap();
            assert_eq!(d.reasoning, Reasoning::Deep, "{cause}");
            assert_eq!(d.skipped.len(), 1, "{cause}");
            let chosen = d.chosen.unwrap();
            assert_eq!(
                (chosen.harness.as_str(), chosen.effort.as_str()),
                ("claude", "high"),
                "{cause}"
            );
            evidence.remove("claude");
            let d = route(
                &config,
                &TaskFacts::default(),
                &Overrides::default(),
                &evidence,
                7,
                NOW,
            )
            .unwrap();
            assert!(d.chosen.is_none() && d.hold_reason.is_some(), "{cause}");
            assert_eq!(d.skipped.len(), 2, "{cause}");
        }
    }

    #[test]
    fn configuration_is_authoritative_and_input_is_validated() {
        let text = DEFAULT_CONFIG
            .replace(
                "default_reasoning = \"deep\"",
                "default_reasoning = \"light\"",
            )
            .replace("newest sol", "gpt-6.9-sol");
        let config = Config::parse(&text).unwrap();
        let d = route(
            &config,
            &TaskFacts::default(),
            &Overrides::default(),
            &catalogs(),
            7,
            NOW,
        )
        .unwrap();
        assert_eq!(d.chosen.unwrap().model, "gpt-6.9-sol");
        assert!(d.reason.contains("default light reasoning"));
        for bad in [
            DEFAULT_CONFIG.replace("refresh_minutes = 5", "refresh_minutes = 0"),
            DEFAULT_CONFIG.replace("harness = \"codex\"", "harness = \"guess\""),
            DEFAULT_CONFIG.replace("newest sol", "newest "),
            DEFAULT_CONFIG.replace("deep = \"high\"", "deep = \"guess\""),
            DEFAULT_CONFIG.replace("deep = \"high\"", "deep = \"high\"\nunknown = true"),
            DEFAULT_CONFIG.replace("allow_max = false", "allow_max = false\nunknown = true"),
        ] {
            assert!(Config::parse(&bad).is_err());
        }
        for task in [
            TaskFacts {
                rank: 0,
                ..Default::default()
            },
            TaskFacts {
                kind: Some("guess".into()),
                ..Default::default()
            },
            TaskFacts {
                judgment: Some("medium".into()),
                ..Default::default()
            },
            TaskFacts {
                thoroughness: Some("max".into()),
                ..Default::default()
            },
        ] {
            assert!(route(&config, &task, &Overrides::default(), &catalogs(), 7, NOW).is_err());
        }
        assert!(
            route(
                &config,
                &TaskFacts::default(),
                &Overrides {
                    model: Some("newest sol".into()),
                    ..Default::default()
                },
                &catalogs(),
                7,
                NOW
            )
            .is_err()
        );
    }

    #[test]
    fn defaults_are_created_once_and_decisions_round_trip() {
        let home = std::env::temp_dir().join(format!("econ-config-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(
            fs::read_to_string(home.join("econ.toml")).unwrap(),
            DEFAULT_CONFIG
        );
        let edited = DEFAULT_CONFIG.replace(
            "default_reasoning = \"deep\"",
            "default_reasoning = \"light\"",
        );
        fs::write(home.join("econ.toml"), &edited).unwrap();
        assert_eq!(
            Config::load(&home).unwrap().policy.default_reasoning,
            Reasoning::Light
        );
        assert_eq!(fs::read_to_string(home.join("econ.toml")).unwrap(), edited);
        let d = route(
            &config,
            &TaskFacts::default(),
            &Overrides::default(),
            &catalogs(),
            7,
            NOW,
        )
        .unwrap();
        let saved = serde_json::to_string(&d).unwrap();
        let restored: Decision = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.chosen.unwrap().model, "gpt-6.10-sol");
        fs::remove_dir_all(home).unwrap();
    }
}
