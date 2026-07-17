//! Reviewer: dispatches a `claude -p` review of broken findings and parses the decision.
//! Routing, env filter, prompt assembly, text extraction, and decision parsing are the
//! pure layer — fully golden-tested. The dispatch (`claude -p`) is the only non-deterministic
//! seam (Task 5.4).

use dotclaude_support::model::{ReviewAction, ReviewParser};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

/// The broken-findings review request passed to the reviewer.
pub struct ReviewRequest {
    pub task: String,
    pub agent_output: String,
    pub findings: Vec<Value>, // each {id, result, confidence, evidence}
    pub scenario_context: Option<Value>,
    pub files: Vec<String>,
}

impl ReviewRequest {
    /// Construct from a golden `input` JSON object (camelCase keys → snake_case).
    pub fn from_golden(v: &Value) -> Self {
        let task = v["task"].as_str().unwrap_or("").to_string();
        let agent_output = v["agentOutput"].as_str().unwrap_or("").to_string();
        let findings: Vec<Value> = v["findings"].as_array().cloned().unwrap_or_default();
        let scenario_context = match &v["scenarioContext"] {
            Value::Null => None,
            other => Some(other.clone()),
        };
        let files: Vec<String> = v["files"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            task,
            agent_output,
            findings,
            scenario_context,
            files,
        }
    }
}

/// Parsed decision without `reviewer_skill` (attached by `Reviewer::review`).
pub struct DecisionCore {
    pub action: ReviewAction,
    pub feedback: Option<String>,
    pub reasoning: Option<String>,
    pub parser: ReviewParser,
}

// `filter_child_env` (the env allowlist for the `claude -p` child) lives in
// `cascadr` alongside `ClaudeCliDispatch`, which is its only caller.
// Re-exported here for the existing `reviewer::filter_child_env` golden test.
pub use cascadr::filter_child_env;

/// Select the reviewer skill: empty files → generic; any Java test file →
/// test-code-reviewer; else generic. Pure, no I/O.
pub fn pick_reviewer_skill(files: &[String]) -> &'static str {
    if files.is_empty() {
        return "review-deterministic-findings";
    }
    if files
        .iter()
        .any(|f| dotclaude_support::java_test::is_java_test_file(f))
    {
        "test-code-reviewer"
    } else {
        "review-deterministic-findings"
    }
}

/// Unwrap `{result:"…"}` or `{text:"…"}` from `claude -p --output-format json`;
/// returns the raw stdout unchanged if neither key is present.
pub fn extract_text(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        if let Some(s) = v.get("result").and_then(|x| x.as_str()) {
            return s.to_string();
        }
        if let Some(s) = v.get("text").and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    raw.to_string()
}

// Regexes for fenced code blocks and inline JSON objects with an "action" key.
// `[\s\S]` matches any character including newlines (supported by the regex crate).
static FENCED: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"```(?:json)?\s*(\{[\s\S]*?\})\s*```").unwrap());
static INLINE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\{[^{}]*"action"[\s\S]*?\}"#).unwrap());

/// Parse the reviewer's output into a `DecisionCore`. Tries a fenced ```json
/// block, then the first inline `{…"action"…}`. `retry` with empty/whitespace/null
/// feedback normalizes to `accept` (parser retry-without-feedback). No valid
/// candidate → accept (parser failed). Conservative by construction — never
/// returns an unbacked retry.
pub fn parse_decision(text: &str) -> DecisionCore {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(c) = FENCED.captures(text) {
        candidates.push(c[1].to_string());
    }
    if let Some(m) = INLINE.find(text) {
        candidates.push(m.as_str().to_string());
    }

    for cand in &candidates {
        let Ok(parsed) = serde_json::from_str::<Value>(cand) else {
            continue;
        };
        let action = parsed.get("action").and_then(|a| a.as_str());
        if action != Some("accept") && action != Some("retry") {
            continue;
        }
        // Only non-empty string feedback is kept.
        let feedback = if action == Some("retry") {
            parsed
                .get("feedback")
                .and_then(|f| f.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
        } else {
            None
        };

        if action == Some("retry") && feedback.is_none() {
            // JS: `parsed.reasoning || fallback` — empty/whitespace reasoning is
            // falsy, so falls back to the sentinel string.
            let reasoning = parsed
                .get("reasoning")
                .and_then(|r| r.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    "retry-without-feedback: reviewer requested retry but provided no actionable feedback"
                        .to_string()
                });
            return DecisionCore {
                action: ReviewAction::Accept,
                feedback: None,
                reasoning: Some(reasoning),
                parser: ReviewParser::RetryWithoutFeedback,
            };
        }
        // JS: `parsed.reasoning || null` — empty string maps to None.
        let reasoning = parsed
            .get("reasoning")
            .and_then(|r| r.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        return DecisionCore {
            action: if action == Some("retry") {
                ReviewAction::Retry
            } else {
                ReviewAction::Accept
            },
            feedback,
            reasoning,
            parser: ReviewParser::Ok,
        };
    }

    DecisionCore {
        action: ReviewAction::Accept,
        feedback: None,
        reasoning: Some(
            "parser-failure: no valid {action, feedback, reasoning} JSON found in reviewer output"
                .to_string(),
        ),
        parser: ReviewParser::Failed,
    }
}

// ---- Dispatch (Provider) re-exports + test double ----

// The dispatch trait and its live/router implementations live in `cascadr`
// (Phase 2: multi-provider routing). Re-exported here so existing
// `reviewer::{Provider, ClaudeCliDispatch, ...}` import paths keep working.
pub use cascadr::{ClaudeCliDispatch, OpenAiCompat, Provider, ProviderError, Router};

/// Test double: returns a fixed `Ok` or `Err(Unavailable)` regardless of the prompt.
pub struct StubDispatch(Result<String, String>);

impl StubDispatch {
    pub fn ok(s: &str) -> Self {
        Self(Ok(s.to_string()))
    }
    pub fn err(s: &str) -> Self {
        Self(Err(s.to_string()))
    }
}

#[async_trait::async_trait]
impl Provider for StubDispatch {
    async fn dispatch(&self, _prompt: &str) -> Result<String, ProviderError> {
        self.0.clone().map_err(ProviderError::Unavailable)
    }

    fn label(&self) -> &'static str {
        "stub"
    }
}

/// Skill/agent bodies for prompt assembly (injectable for tests).
pub struct SkillBodies {
    pub generic: String,
    pub qa: Option<String>,
}

impl SkillBodies {
    /// Load from framework paths (production). Missing `test-code-reviewer` → `qa = None`
    /// (silent generic fallback). Missing generic → hard error.
    pub fn load() -> Result<Self, String> {
        let generic = std::fs::read_to_string(dotclaude_support::paths::review_skill_path())
            .map_err(|e| format!("cannot load review skill: {e}"))?;
        let qa = std::fs::read_to_string(dotclaude_support::paths::test_code_reviewer_path()).ok();
        Ok(Self { generic, qa })
    }
}

/// Reviewer orchestrator. Lifetime `'a` is the dispatch reference — cheap to construct.
/// `dispatch` is a single `Provider` surface: either an injected test double, or a
/// `Router` walking the configured provider order (the engine builds the `Router`;
/// this type stays agnostic to how many hops sit behind it).
pub struct Reviewer<'a> {
    dispatch: &'a dyn Provider,
    skills: SkillBodies,
}

impl<'a> Reviewer<'a> {
    pub fn with_skills(dispatch: &'a dyn Provider, skills: SkillBodies) -> Self {
        Self { dispatch, skills }
    }

    /// Review broken findings. Empty findings → accept without dispatch. Else build
    /// prompt, dispatch (Err → accept/dispatch-error), extract text, parse, attach skill.
    /// Fail-open: every dispatch or parse failure yields a conservative accept.
    pub async fn review(&self, req: &ReviewRequest) -> dotclaude_support::model::ReviewDecision {
        let reviewer_skill = pick_reviewer_skill(&req.files).to_string();
        if req.findings.is_empty() {
            return dotclaude_support::model::ReviewDecision {
                action: dotclaude_support::model::ReviewAction::Accept,
                feedback: None,
                reasoning: Some("no findings to review".to_string()),
                parser: dotclaude_support::model::ReviewParser::Ok,
                reviewer_skill,
            };
        }
        // qa body forwarded only on the test-code-reviewer route; build_prompt
        // ignores it on the generic route but we keep the selection explicit.
        let qa = if reviewer_skill == "test-code-reviewer" {
            self.skills.qa.as_deref()
        } else {
            None
        };
        let prompt = build_prompt(req, &self.skills.generic, qa);
        match self.dispatch.dispatch(&prompt).await {
            Err(e) => dotclaude_support::model::ReviewDecision {
                action: dotclaude_support::model::ReviewAction::Accept,
                feedback: None,
                reasoning: Some(format!("reviewer-dispatch-error: {e}")),
                parser: dotclaude_support::model::ReviewParser::DispatchError,
                reviewer_skill,
            },
            Ok(raw) => {
                let text = extract_text(&raw);
                let core = parse_decision(&text);
                dotclaude_support::model::ReviewDecision {
                    action: core.action,
                    feedback: core.feedback,
                    reasoning: core.reasoning,
                    parser: core.parser,
                    reviewer_skill,
                }
            }
        }
    }
}

/// Build the prompt string the reviewer receives. `qa_skill = Some(body)` when the
/// test-code-reviewer route is selected AND the qa skill file loaded; `None` means
/// either the generic route or the qa file was missing.
pub fn build_prompt(req: &ReviewRequest, generic_skill: &str, qa_skill: Option<&str>) -> String {
    let route = pick_reviewer_skill(&req.files);
    let (skill_body, reviewer_kind) = if route == "test-code-reviewer" {
        match qa_skill {
            Some(qa) => (
                format!("{qa}\n\n---\n\n## Output contract for this dispatch\n\n{generic_skill}"),
                "test-code-reviewer".to_string(),
            ),
            None => (
                generic_skill.to_string(),
                "review-deterministic-findings (fallback: test-code-reviewer agent not found)"
                    .to_string(),
            ),
        }
    } else {
        (
            generic_skill.to_string(),
            "review-deterministic-findings".to_string(),
        )
    };

    // `changed_files` is always an array, even when empty (an absent files list
    // maps to an empty Vec, not null). Emit `[]` not null for present-but-empty files.
    let payload = json!({
        "task": req.task,
        "agent_output": req.agent_output,
        "findings": req.findings,
        "scenario_context": req.scenario_context.clone().unwrap_or(Value::Null),
        "reviewer_kind": reviewer_kind,
        "changed_files": req.files,
    });
    let payload_json = serde_json::to_string_pretty(&payload).unwrap();
    format!("{skill_body}\n\n---\n\n## Payload to review\n\n```json\n{payload_json}\n```\n\nEmit the JSON object now.")
}
