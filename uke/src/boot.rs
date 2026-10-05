//! DrvBoot stub (uke-design §10): pick a nest plan for a new seat.
//!
//! Boot never installs anything. It filters the catalogue by captain-channel policy, asks a
//! [`SkillPicker`] backend (`rules` or `jev`) and returns a capped [`NestPlan`] plus a
//! [`BootRecord`] for Obs. DrvAgent installs the plan; the CLI wires the two through uKe types.
use crate::skill::SkillEntry;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

/// Hard ceiling on a nest plan; config may lower it, never raise it.
pub const NEST_CAP: usize = 3;
/// Jev sees at most this many candidates as one closed Choice.
const SHORTLIST: usize = 12;

/// Where the new PID sits. Only a seat with a captain channel may carry interactive skills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Seat {
    pub rank: u8,
    /// True while DrvIntf has this seat focused (the captain is looking at it).
    pub intf_focused: bool,
}
impl Seat {
    /// `l1` | `l2` | `l2-focused` | `l3`
    pub fn parse(text: &str) -> Result<Self> {
        let (rank, intf_focused) = match text {
            "l1" => (1, true),
            "l2" => (2, false),
            "l2-focused" => (2, true),
            "l3" => (3, false),
            _ => bail!("usage: seat must be l1|l2|l2-focused|l3"),
        };
        Ok(Self { rank, intf_focused })
    }
    /// L1 always; L2 only while Intf-focused; L3 never.
    pub fn has_captain_channel(&self) -> bool {
        self.rank == 1 || (self.rank == 2 && self.intf_focused)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Rules,
    Jev,
}
impl Backend {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "rules" => Ok(Self::Rules),
            "jev" => Ok(Self::Jev),
            _ => bail!("usage: picker must be rules|jev"),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::Jev => "jev",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    Rules,
    Refuse,
}

/// `[boot]` table, e.g. in `.unvrs/boot.toml`. Every field is optional.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BootConfig {
    pub backend: Backend,
    /// Jev Noul floor for "any skill helps at all"; below it the plan is empty.
    pub noul_min: f64,
    /// Jev Choice confidence floor; below it the plan is empty (not a fallback: Jev answered).
    pub confidence_min: f64,
    /// A runner-up joins the plan only with at least this Choice probability.
    pub probability_min: f64,
    pub max_skills: usize,
    pub on_error: OnError,
}
impl Default for BootConfig {
    fn default() -> Self {
        Self {
            backend: Backend::Rules,
            noul_min: 0.5,
            confidence_min: 0.3,
            probability_min: 0.2,
            max_skills: NEST_CAP,
            on_error: OnError::Rules,
        }
    }
}
impl BootConfig {
    /// Missing file means defaults; a present file must parse.
    pub fn load(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            boot: BootConfig,
        }
        if !path.exists() {
            return Ok(Self::default());
        }
        let file: File = toml::from_str(&std::fs::read_to_string(path)?)
            .with_context(|| format!("Invalid boot config {}", path.display()))?;
        Ok(file.boot)
    }
    pub fn cap(&self) -> usize {
        self.max_skills.min(NEST_CAP)
    }
}

/// A picker's answer. `selected` holds candidate ids only, best first.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Pick {
    pub selected: Vec<String>,
    pub noul: Option<f64>,
    pub confidence: Option<f64>,
    /// Per-candidate score (rules) or probability (jev), best first.
    pub scores: Vec<(String, f64)>,
    pub note: String,
}

/// The seam: two adapters (`rules`, `jev`) cross it today.
pub trait SkillPicker {
    fn backend(&self) -> Backend;
    fn pick(&self, brief: &str, candidates: &[SkillEntry], cap: usize) -> Result<Pick>;
}

// ── rules ───────────────────────────────────────────────────────────────────────────────

/// Deterministic picker: intent rules plus lexical overlap between brief and catalogue text.
pub struct RulesPicker;

/// (brief triggers, skill-id fragments boosted when any trigger appears)
const INTENTS: &[(&[&str], &[&str])] = &[
    (
        &[
            "tdd",
            "test-first",
            "test first",
            "red-green",
            "failing test first",
        ],
        &["tdd", "test"],
    ),
    (
        &[
            "bug",
            "regression",
            "diagnose",
            "root cause",
            "crash",
            "broken",
        ],
        &["bug", "debug", "diagnos", "triage"],
    ),
    (
        &["grill", "interview me", "question me", "challenge my plan"],
        &["grill", "interview"],
    ),
    (&["refactor", "architecture"], &["refactor", "architecture"]),
    (&["prd", "requirements doc"], &["prd"]),
];
const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "then", "must", "should",
    "are", "not", "use", "using", "used", "user", "all", "any", "has", "have", "its", "new", "one",
    "out", "can", "will", "about", "code", "file", "files", "skill", "agent", "mission", "done",
    "check", "scope", "outcome", "prompt", "pointers", "none", "only", "before", "after", "want",
    "wants", "make", "change", "changes", "work", "readme",
];
/// An intent hit scores this much; lexical overlap alone must clear [`RULES_MIN`].
const INTENT_SCORE: f64 = 5.0;
const RULES_MIN: f64 = 5.0;

fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 3 && !STOP.contains(t))
        .map(|t| t.trim_end_matches('s').to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn rules_score(brief_lower: &str, brief_tokens: &[String], skill: &SkillEntry) -> f64 {
    let id = skill.id.to_ascii_lowercase();
    let mut score = 0.0;
    for (triggers, fragments) in INTENTS {
        if triggers.iter().any(|t| brief_lower.contains(t))
            && fragments.iter().any(|f| id.contains(f))
        {
            score += INTENT_SCORE;
        }
    }
    let id_tokens = tokens(&id);
    let summary_tokens = tokens(&skill.summary);
    for token in brief_tokens {
        if id_tokens.contains(token) {
            score += 2.0;
        } else if summary_tokens.contains(token) {
            score += 0.5;
        }
    }
    score
}

/// Every candidate scored, best first; ties break on id so output is stable.
fn rank(brief: &str, candidates: &[SkillEntry]) -> Vec<(String, f64)> {
    let lower = brief.to_ascii_lowercase();
    let brief_tokens = tokens(brief);
    let mut scores: Vec<(String, f64)> = candidates
        .iter()
        .map(|s| (s.id.clone(), rules_score(&lower, &brief_tokens, s)))
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scores
}

impl SkillPicker for RulesPicker {
    fn backend(&self) -> Backend {
        Backend::Rules
    }
    fn pick(&self, brief: &str, candidates: &[SkillEntry], cap: usize) -> Result<Pick> {
        let mut scores = rank(brief, candidates);
        let selected = scores
            .iter()
            .filter(|(_, score)| *score >= RULES_MIN)
            .take(cap)
            .map(|(id, _)| id.clone())
            .collect();
        scores.truncate(SHORTLIST);
        Ok(Pick {
            selected,
            scores,
            note: format!("rules: score >= {RULES_MIN}"),
            ..Pick::default()
        })
    }
}

// ── jev ─────────────────────────────────────────────────────────────────────────────────

/// One System One round trip. The HTTP adapter lives with the CLI; tests use a fake.
pub trait JevTransport {
    fn ask(&self, body: &Value) -> Result<Value>;
}

/// TypeSafe `POST /v1/systemone`. The key is held in memory only and never logged.
pub struct JevHttp {
    key: String,
    url: String,
}
impl JevHttp {
    pub const KEY_ENV: &'static str = "TYPESAFE_API_KEY";
    /// `None` when the key is blank or absent from both the environment and any `.env`
    /// above `start`: callers fall back per `on_error`.
    pub fn discover(start: &Path) -> Option<Self> {
        let key = crate::dotenv::lookup(start, Self::KEY_ENV)?
            .trim()
            .to_string();
        (!key.is_empty()).then(|| Self {
            key,
            url: std::env::var("TYPESAFE_API_URL")
                .unwrap_or_else(|_| "https://api.typesafe.ai/v1/systemone".into()),
        })
    }
}
impl JevTransport for JevHttp {
    fn ask(&self, body: &Value) -> Result<Value> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        let mut response = agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.key))
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("Jev request failed: {e}"))?;
        response
            .body_mut()
            .read_json::<Value>()
            .context("Jev response was not JSON")
    }
}

/// Noul ("does any skill help?") plus one closed Choice over a shortlist, in a single call.
pub struct JevPicker<T> {
    pub transport: T,
    pub noul_min: f64,
    pub confidence_min: f64,
    pub probability_min: f64,
}
const NONE: &str = "none";

impl<T: JevTransport> SkillPicker for JevPicker<T> {
    fn backend(&self) -> Backend {
        Backend::Jev
    }
    fn pick(&self, brief: &str, candidates: &[SkillEntry], cap: usize) -> Result<Pick> {
        if candidates.is_empty() {
            return Ok(Pick {
                note: "jev: no candidates".into(),
                ..Pick::default()
            });
        }
        // Closed set: a rules-ranked shortlist keeps the Choice small on large pools.
        let shortlist: Vec<&SkillEntry> = rank(brief, candidates)
            .iter()
            .take(SHORTLIST)
            .filter_map(|(id, _)| candidates.iter().find(|s| &s.id == id))
            .collect();
        let mut criteria = BTreeMap::new();
        for skill in &shortlist {
            ensure!(skill.id != NONE, "Skill id `none` is reserved");
            criteria.insert(skill.id.clone(), skill.summary.clone());
        }
        criteria.insert(
            NONE.into(),
            "No listed skill fits; the agent should work with its default setup".into(),
        );
        let body = json!({
            "model": "jev-latest",
            "state": brief,
            "questions": {
                "any_skill": {
                    "type": "noul",
                    "instructions": "A specialised agent skill (a packaged method such as test-driven development, bug diagnosis, or interviewing the requester) would materially help an AI coding agent complete this mission, compared with just doing the work directly"
                },
                "skill": {
                    "type": "choice",
                    "instructions": "Which skill best helps an AI coding agent complete this mission",
                    "criteria": criteria,
                }
            }
        });
        let answer = self.transport.ask(&body)?;
        let answers = &answer["answers"];
        let noul = answers["any_skill"]["noul"]
            .as_f64()
            .context("Jev answer lacks any_skill.noul")?;
        let choice = &answers["skill"];
        let confidence = choice["confidence"]
            .as_f64()
            .context("Jev answer lacks skill.confidence")?;
        let top = choice["choice"]
            .as_str()
            .context("Jev answer lacks skill.choice")?;
        // Never invent ids: anything outside the shortlist is dropped here.
        let mut scores: Vec<(String, f64)> = choice["probabilities"]
            .as_object()
            .context("Jev answer lacks skill.probabilities")?
            .iter()
            .filter(|(id, _)| shortlist.iter().any(|s| &s.id == *id))
            .map(|(id, p)| (id.clone(), p.as_f64().unwrap_or(0.0)))
            .collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let (selected, note) = if noul < self.noul_min {
            (
                vec![],
                format!("jev: noul {noul:.2} < {:.2}", self.noul_min),
            )
        } else if top == NONE {
            (vec![], "jev: chose none".to_string())
        } else if confidence < self.confidence_min {
            (
                vec![],
                format!(
                    "jev: confidence {confidence:.2} < {:.2}",
                    self.confidence_min
                ),
            )
        } else {
            let picked: Vec<String> = scores
                .iter()
                .filter(|(id, p)| id == top || *p >= self.probability_min)
                .take(cap)
                .map(|(id, _)| id.clone())
                .collect();
            (
                picked,
                format!(
                    "jev: model {}",
                    answer["model"].as_str().unwrap_or("unknown")
                ),
            )
        };
        Ok(Pick {
            selected,
            noul: Some(noul),
            confidence: Some(confidence),
            scores,
            note,
        })
    }
}

// ── nest ────────────────────────────────────────────────────────────────────────────────

/// `[(pool, id), …]` resolved to catalogue entries, at most [`NEST_CAP`].
#[derive(Clone, Debug, Default, Serialize)]
pub struct NestPlan {
    pub skills: Vec<SkillEntry>,
}

/// Everything Obs records about one Boot decision.
#[derive(Clone, Debug, Serialize)]
pub struct BootRecord {
    pub seat: Seat,
    pub captain_channel: bool,
    pub backend_requested: Backend,
    pub backend_used: Backend,
    /// Why the requested backend was not used (missing key, transport error, …).
    pub fallback: Option<String>,
    pub catalog: usize,
    /// Candidates offered to the picker after policy and hints.
    pub candidates: usize,
    /// `needs_captain` ids removed because this seat has no captain channel.
    pub stripped: Vec<String>,
    pub hints: Vec<String>,
    pub noul: Option<f64>,
    pub confidence: Option<f64>,
    pub scores: Vec<(String, f64)>,
    /// `pool/id`, best first.
    pub selected: Vec<String>,
    pub note: String,
}

/// Build the nest plan. `jev` is `None` when no transport is available (missing key).
///
/// Errors only when the requested backend fails and `on_error = refuse`.
pub fn nest<T: JevTransport>(
    brief: &str,
    hints: &[String],
    catalog: &[SkillEntry],
    seat: Seat,
    config: &BootConfig,
    jev: Option<T>,
) -> Result<(NestPlan, BootRecord)> {
    let captain_channel = seat.has_captain_channel();
    let mut stripped = Vec::new();
    let candidates: Vec<SkillEntry> = catalog
        .iter()
        .filter(|s| hints.is_empty() || hints.contains(&s.id))
        .filter(|s| {
            let keep = captain_channel || !s.needs_captain;
            if !keep {
                stripped.push(s.id.clone());
            }
            keep
        })
        .cloned()
        .collect();
    let cap = config.cap();
    let rules = |reason: Option<String>| -> Result<(Pick, Backend, Option<String>)> {
        Ok((
            RulesPicker.pick(brief, &candidates, cap)?,
            Backend::Rules,
            reason,
        ))
    };
    let (pick, backend_used, fallback) = match config.backend {
        Backend::Rules => rules(None)?,
        Backend::Jev => {
            let attempt = match jev {
                Some(transport) => JevPicker {
                    transport,
                    noul_min: config.noul_min,
                    confidence_min: config.confidence_min,
                    probability_min: config.probability_min,
                }
                .pick(brief, &candidates, cap),
                None => Err(anyhow::anyhow!("{} is not set", JevHttp::KEY_ENV)),
            };
            match (attempt, config.on_error) {
                (Ok(pick), _) => (pick, Backend::Jev, None),
                (Err(e), OnError::Rules) => rules(Some(format!("{e:#}")))?,
                (Err(e), OnError::Refuse) => return Err(e.context("Boot refused: picker failed")),
            }
        }
    };
    // Defence in depth: whatever a backend says, only policy-clean candidates, capped.
    let skills: Vec<SkillEntry> = pick
        .selected
        .iter()
        .filter_map(|id| candidates.iter().find(|s| &s.id == id))
        .take(cap)
        .cloned()
        .collect();
    let record = BootRecord {
        seat,
        captain_channel,
        backend_requested: config.backend,
        backend_used,
        fallback,
        catalog: catalog.len(),
        candidates: candidates.len(),
        stripped,
        hints: hints.to_vec(),
        noul: pick.noul,
        confidence: pick.confidence,
        scores: pick.scores,
        selected: skills
            .iter()
            .map(|s| format!("{}/{}", s.pool, s.id))
            .collect(),
        note: pick.note,
    };
    Ok((NestPlan { skills }, record))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str, summary: &str, needs_captain: bool) -> SkillEntry {
        SkillEntry {
            pool: "t".into(),
            id: id.into(),
            summary: summary.into(),
            needs_captain,
            dir: id.into(),
        }
    }
    fn catalog() -> Vec<SkillEntry> {
        vec![
            skill(
                "tdd",
                "Test-driven development with a red-green-refactor loop",
                false,
            ),
            skill(
                "diagnose-bug",
                "Find the root cause of a bug before fixing it",
                false,
            ),
            skill(
                "grill-me",
                "Interview the user relentlessly about a plan",
                true,
            ),
            skill(
                "write-a-prd",
                "Create a product requirements document",
                false,
            ),
        ]
    }
    struct Fake(Value);
    impl JevTransport for Fake {
        fn ask(&self, _: &Value) -> Result<Value> {
            Ok(self.0.clone())
        }
    }
    struct Down;
    impl JevTransport for Down {
        fn ask(&self, _: &Value) -> Result<Value> {
            bail!("offline")
        }
    }
    fn jev_config() -> BootConfig {
        BootConfig {
            backend: Backend::Jev,
            ..BootConfig::default()
        }
    }
    fn answer(noul: f64, choice: &str, confidence: f64, probabilities: Value) -> Value {
        json!({"model":"jev-test","answers":{"any_skill":{"type":"noul","noul":noul},
            "skill":{"type":"choice","choice":choice,"confidence":confidence,"probabilities":probabilities}}})
    }

    #[test]
    fn captain_channel_is_l1_or_focused_l2() {
        for (seat, expected) in [
            ("l1", true),
            ("l2", false),
            ("l2-focused", true),
            ("l3", false),
        ] {
            assert_eq!(Seat::parse(seat).unwrap().has_captain_channel(), expected);
        }
        assert!(Seat::parse("l4").is_err());
    }

    #[test]
    fn rules_pick_intent_and_leave_trivial_work_bare() {
        let none: Option<Down> = None;
        let config = BootConfig::default();
        let seat = Seat::parse("l2").unwrap();
        let (plan, record) = nest(
            "Implement the parser test-first (TDD): write a failing test first",
            &[],
            &catalog(),
            seat,
            &config,
            none,
        )
        .unwrap();
        assert_eq!(plan.skills[0].id, "tdd");
        assert!(plan.skills.len() <= NEST_CAP);
        assert_eq!(record.stripped, ["grill-me"]);
        let none: Option<Down> = None;
        let (plan, _) = nest(
            "Rename one heading in the README",
            &[],
            &catalog(),
            seat,
            &config,
            none,
        )
        .unwrap();
        assert!(plan.skills.is_empty());
    }

    #[test]
    fn interactive_skill_needs_a_captain_channel() {
        let brief = "Grill me: interview me about my plan before anything is built";
        let pick = |seat: &str| {
            let none: Option<Down> = None;
            nest(
                brief,
                &[],
                &catalog(),
                Seat::parse(seat).unwrap(),
                &BootConfig::default(),
                none,
            )
            .unwrap()
            .0
            .skills
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>()
        };
        assert!(pick("l1").contains(&"grill-me".to_string()));
        assert!(pick("l2-focused").contains(&"grill-me".to_string()));
        assert!(!pick("l2").contains(&"grill-me".to_string()));
        assert!(!pick("l3").contains(&"grill-me".to_string()));
    }

    #[test]
    fn jev_gates_and_never_invents_ids() {
        let seat = Seat::parse("l2").unwrap();
        let run = |value: Value| {
            nest(
                "fix the bug",
                &[],
                &catalog(),
                seat,
                &jev_config(),
                Some(Fake(value)),
            )
            .unwrap()
        };
        let (plan, record) = run(answer(
            0.9,
            "diagnose-bug",
            0.8,
            json!({"diagnose-bug":0.7,"tdd":0.25,"invented":0.9,"grill-me":0.9,"none":0.0}),
        ));
        let ids: Vec<_> = plan.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["diagnose-bug", "tdd"]);
        assert_eq!(record.backend_used, Backend::Jev);
        assert_eq!(record.noul, Some(0.9));
        assert!(
            run(answer(0.1, "tdd", 0.9, json!({"tdd":1.0})))
                .0
                .skills
                .is_empty()
        );
        assert!(
            run(answer(0.9, "tdd", 0.05, json!({"tdd":1.0})))
                .0
                .skills
                .is_empty()
        );
        assert!(
            run(answer(0.9, "none", 0.9, json!({"none":1.0})))
                .0
                .skills
                .is_empty()
        );
        assert!(
            run(answer(0.9, "invented", 0.9, json!({"invented":1.0})))
                .0
                .skills
                .is_empty()
        );
    }

    #[test]
    fn jev_failure_falls_back_to_rules_or_refuses() {
        let seat = Seat::parse("l2").unwrap();
        let brief = "diagnose the crash bug";
        let (plan, record) = nest(brief, &[], &catalog(), seat, &jev_config(), Some(Down)).unwrap();
        assert_eq!(record.backend_used, Backend::Rules);
        assert!(record.fallback.as_deref().unwrap().contains("offline"));
        assert_eq!(plan.skills[0].id, "diagnose-bug");
        let none: Option<Down> = None;
        let (_, record) = nest(brief, &[], &catalog(), seat, &jev_config(), none).unwrap();
        assert!(record.fallback.unwrap().contains("TYPESAFE_API_KEY"));
        let refuse = BootConfig {
            on_error: OnError::Refuse,
            ..jev_config()
        };
        assert!(nest(brief, &[], &catalog(), seat, &refuse, Some(Down)).is_err());
    }

    #[test]
    fn hints_intersect_and_cap_holds() {
        let none: Option<Down> = None;
        let config = BootConfig {
            max_skills: 9,
            ..BootConfig::default()
        };
        assert_eq!(config.cap(), NEST_CAP);
        let (plan, record) = nest(
            "tdd bug diagnose prd test-first",
            &["write-a-prd".into()],
            &catalog(),
            Seat::parse("l1").unwrap(),
            &config,
            none,
        )
        .unwrap();
        assert_eq!(record.candidates, 1);
        assert!(plan.skills.iter().all(|s| s.id == "write-a-prd"));
    }
}
