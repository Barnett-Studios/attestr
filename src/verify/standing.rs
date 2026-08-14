use crate::verify::patterns::{compile_ci, compile_cs};
use baseplate::model::{Confidence, Method, MethodOutcome, Observation, PromiseSpec};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

/// Standing promise ids that verify against the diff (not the assistant text)
/// when a diff is available.
static DIFF_ELIGIBLE: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "complete-output",
        "no-placeholders",
        "no-dead-code",
        "tests-with-code",
        "error-handling",
        "resource-cleanup",
        "implementation-depth",
        "test-assertion-deserialize",
    ]
    .into_iter()
    .collect()
});

/// Verify one standing promise: diff-eligible promises verify against the diff
/// (medium confidence) when a diff is present, otherwise against the assistant
/// text (low confidence). Returns (result, evidence, confidence).
pub fn verify_standing_promise(
    spec: &PromiseSpec,
    text: &str,
    diff: Option<&str>,
) -> (Observation, String, Confidence) {
    let use_diff = diff.is_some() && DIFF_ELIGIBLE.contains(spec.id.as_str());
    let target = if use_diff { diff.unwrap() } else { text };
    let confidence = if use_diff {
        Confidence::Medium
    } else {
        Confidence::Low
    };
    let Some(method) = spec.method else {
        // Fail open rather than panic — but `Skipped`, not the `Partial` this carried until
        // #29. Nothing ran: there is no verifier for a method that could not be resolved, so
        // `Some(0.5)` was half a pass for a promise that was never executed. The sixth site
        // of the same defect #27 removed from the other five, and the one an operand-shaped
        // enumeration misses, because what is unset here is the *method*.
        //
        // Latent, not live: `registry::load` refuses an unrecognised `method:` string
        // outright (`RegistryError::UnknownStandingMethod`), so no registry can produce this
        // state. A hand-constructed `PromiseSpec` can — `method` is `#[serde(skip)]` — and
        // that is a real path for a published crate.
        return (
            Observation::Skipped,
            "no resolved standing method".to_string(),
            Confidence::Low,
        );
    };
    let ctx = VerifyContext {
        diff,
        tokens_used: 0,
        elapsed_ms: 0,
    };
    let out = verify(method, spec, target, &ctx);
    (out.result, out.evidence, confidence)
}

pub struct VerifyContext<'a> {
    pub diff: Option<&'a str>,
    pub tokens_used: i64,
    pub elapsed_ms: i64,
}

fn outcome(result: Observation, evidence: impl Into<String>) -> MethodOutcome {
    MethodOutcome {
        result,
        evidence: evidence.into(),
    }
}

pub fn verify(
    method: Method,
    spec: &PromiseSpec,
    output: &str,
    ctx: &VerifyContext,
) -> MethodOutcome {
    match method {
        Method::Grep => grep(spec, output),
        Method::GrepAbsent => grep_absent(spec, output),
        Method::ConsecutiveComments => consecutive_comments(spec, output),
        Method::OutputLength => output_length(spec, output),
        Method::FileCheck => file_check(spec, output),
        Method::OutputContains => output_contains(spec, output),
        Method::OutputStructure => output_structure(spec, output),
        Method::TokenMetric => token_metric(spec, ctx),
        Method::Timing => timing(spec, ctx),
        Method::TestAssertionPatterns => test_assertion_patterns(spec, output, ctx),
    }
}

/// The pattern the spec supplied, or the reason there is nothing to scan for.
///
/// `unwrap_or("")` here was not a default, it was a vacuous scan: the empty regex matches at
/// every position of every input, so `grep` answered `Broken` and `grep_absent` answered
/// `Kept` — on all output, including no output, with no input able to flip either. A verdict
/// that cannot go the other way is not a measurement of the agent. The scan did not run, so
/// it reports `Skipped` (#27).
fn configured_pattern(spec: &PromiseSpec) -> Result<&str, MethodOutcome> {
    match spec.pattern.as_deref() {
        Some(p) if !p.is_empty() => Ok(p),
        _ => Err(outcome(
            Observation::Skipped,
            "no pattern configured — nothing to scan for, refusing to fall back to the empty \
             pattern",
        )),
    }
}

fn grep(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let pattern = match configured_pattern(spec) {
        Ok(p) => p,
        Err(skipped) => return skipped,
    };
    let re = match compile_ci(pattern) {
        Ok(r) => r,
        Err(e) => return outcome(Observation::Partial, format!("Invalid regex pattern: {e}")),
    };
    let matches: Vec<&str> = re.find_iter(output).map(|m| m.as_str()).collect();
    if !matches.is_empty() {
        let first3 = matches
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        outcome(
            Observation::Broken,
            format!("Found {} match(es): {first3}", matches.len()),
        )
    } else {
        outcome(Observation::Kept, "No matches found")
    }
}

fn grep_absent(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let pattern = match configured_pattern(spec) {
        Ok(p) => p,
        Err(skipped) => return skipped,
    };
    let re = match compile_ci(pattern) {
        Ok(r) => r,
        Err(e) => return outcome(Observation::Partial, format!("Invalid regex pattern: {e}")),
    };
    let matches: Vec<&str> = re.find_iter(output).map(|m| m.as_str()).collect();
    if !matches.is_empty() {
        let first3 = matches
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        outcome(
            Observation::Kept,
            format!("Required pattern found: {first3}"),
        )
    } else {
        outcome(Observation::Broken, "Required pattern not found in output")
    }
}

static COMMENT_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*//").unwrap());
fn consecutive_comments(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let min = spec.min_lines.unwrap_or(3);
    let (mut cur, mut max) = (0i64, 0i64);
    for line in output.split('\n') {
        if COMMENT_LINE.is_match(line) {
            cur += 1;
            max = max.max(cur);
        } else {
            cur = 0;
        }
    }
    if max >= min {
        outcome(
            Observation::Broken,
            format!("Found {max} consecutive comment lines (threshold: {min})"),
        )
    } else {
        outcome(
            Observation::Kept,
            format!("Max consecutive comments: {max} (threshold: {min})"),
        )
    }
}

fn output_length(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let min = spec.min_chars.unwrap_or(500);
    // JS String.length counts UTF-16 code units; encode_utf16().count() gives the same
    // value for astral-plane characters (e.g. 😀 = 1 Unicode scalar but 2 UTF-16 units).
    let len = output.encode_utf16().count() as i64;
    if len >= min {
        outcome(
            Observation::Kept,
            format!("Output length {len} chars (min: {min})"),
        )
    } else {
        outcome(
            Observation::Broken,
            format!("Output length {len} chars below minimum {min}"),
        )
    }
}

/// The `check` string the spec supplied, or the reason there is nothing to check.
///
/// The three readers of this field split two ways once it is present — `output_contains`
/// looks for it verbatim, `file_check`/`output_structure` ask whether they recognise it — but
/// they had the same hole under it. `unwrap_or_default()` turned an unconfigured promise into
/// a verdict: `"anything".contains("")` is true, so `output_contains` answered
/// `Kept — Found "" in output` for every input, and the other two answered `Partial` (worth
/// 0.5, so still a nudge) with an empty name in the evidence. A promise nobody configured is
/// not half an answer and not a pass — it is no answer (#27).
fn configured_check(spec: &PromiseSpec) -> Result<&str, MethodOutcome> {
    match spec.check.as_deref() {
        Some(c) if !c.is_empty() => Ok(c),
        _ => Err(outcome(
            Observation::Skipped,
            "no check string configured — nothing to look for, refusing to fall back to the \
             empty string",
        )),
    }
}

static TEST_FILE_REF: Lazy<Regex> =
    Lazy::new(|| compile_ci(r"\.(test|spec)\.(js|ts|jsx|tsx)|tests?/").unwrap());
fn file_check(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let check = match configured_check(spec) {
        Ok(c) => c.to_lowercase(),
        Err(skipped) => return skipped,
    };
    if check.contains("test") {
        if TEST_FILE_REF.is_match(output) {
            outcome(Observation::Kept, "Test file reference found in output")
        } else {
            outcome(
                Observation::Broken,
                "No test file reference found in output",
            )
        }
    } else {
        outcome(
            Observation::Partial,
            format!("Unrecognized file_check: {check}"),
        )
    }
}

fn output_contains(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let check = match configured_check(spec) {
        Ok(c) => c,
        Err(skipped) => return skipped,
    };
    if output.contains(check) {
        outcome(Observation::Kept, format!("Found \"{check}\" in output"))
    } else {
        outcome(
            Observation::Broken,
            format!("\"{check}\" not found in output"),
        )
    }
}

static CONSTRAINT_RE: Lazy<Regex> =
    Lazy::new(|| compile_ci(r"constraint|limitation|caveat|note:").unwrap());
fn output_structure(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let check = match configured_check(spec) {
        Ok(c) => c.to_lowercase(),
        Err(skipped) => return skipped,
    };
    if check.contains("constraint") {
        if CONSTRAINT_RE.is_match(output) {
            outcome(Observation::Kept, "Constraints section detected")
        } else {
            outcome(Observation::Broken, "No constraints section found")
        }
    } else {
        outcome(
            Observation::Partial,
            format!("Cannot verify structure: {check}"),
        )
    }
}

fn token_metric(spec: &PromiseSpec, ctx: &VerifyContext) -> MethodOutcome {
    let count = ctx.tokens_used;
    let threshold = spec.max_tokens.unwrap_or(50000);
    if count <= threshold {
        outcome(
            Observation::Kept,
            format!("{count} tokens (limit: {threshold})"),
        )
    } else {
        outcome(
            Observation::Broken,
            format!("{count} tokens exceeds limit of {threshold}"),
        )
    }
}

fn timing(spec: &PromiseSpec, ctx: &VerifyContext) -> MethodOutcome {
    let elapsed = ctx.elapsed_ms;
    let max = spec.max_ms.unwrap_or(300000);
    if elapsed <= max {
        outcome(Observation::Kept, format!("{elapsed}ms (limit: {max}ms)"))
    } else {
        outcome(
            Observation::Broken,
            format!("{elapsed}ms exceeds limit of {max}ms"),
        )
    }
}

static FILE_HDR: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\+\+\+\s+b?/?(.+?)(\s|$)").unwrap());
fn test_assertion_patterns(spec: &PromiseSpec, output: &str, ctx: &VerifyContext) -> MethodOutcome {
    let tfp = match spec.test_file_pattern.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return outcome(Observation::Skipped,
            "no test_file_pattern provided \u{2014} scope is undefined, refusing to fall back to .java$ (would widen to production code)"),
    };
    let file_re = match compile_cs(tfp) {
        Ok(r) => r,
        Err(_) => {
            return outcome(
                Observation::Partial,
                format!("Invalid test_file_pattern: {tfp}"),
            )
        }
    };
    let forbidden: Vec<(String, Regex)> = spec
        .forbidden_patterns
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| compile_cs(&p).ok().map(|re| (p, re)))
        .collect();
    if forbidden.is_empty() {
        return outcome(Observation::Skipped, "no forbidden_patterns configured");
    }
    let diff = ctx.diff.unwrap_or(output);
    if diff.is_empty() {
        // A pattern scan over nothing cannot find a pattern, so `Kept` here was a clean
        // bill from a check that could not have failed. The line above already draws this
        // distinction — `Partial` for "no patterns configured" — and this branch is the
        // same shape with the other operand empty (attestr#27).
        return outcome(Observation::Skipped, "no diff content to scan");
    }

    let mut findings: Vec<(String, String)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut is_test_file = false;
    'outer: for line in diff.split('\n') {
        if line.starts_with("+++ ") {
            current_file = FILE_HDR.captures(line).map(|c| c[1].to_string());
            is_test_file = current_file
                .as_deref()
                .map(|f| file_re.is_match(f))
                .unwrap_or(false);
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("diff ") || line.starts_with("@@") {
            continue;
        }
        if !is_test_file || !line.starts_with('+') {
            continue;
        }
        let content = &line[1..];
        for (src, re) in &forbidden {
            if re.is_match(content) {
                findings.push((current_file.clone().unwrap_or_default(), src.clone()));
                if findings.len() >= 5 {
                    break 'outer;
                }
            }
        }
    }

    if findings.is_empty() {
        outcome(
            Observation::Kept,
            "no forbidden test-assertion patterns in diff",
        )
    } else {
        let detail = findings
            .iter()
            .map(|(f, p)| format!("{f}: {p}"))
            .collect::<Vec<_>>()
            .join("; ");
        outcome(
            Observation::Broken,
            format!("forbidden in test file(s): {detail}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baseplate::model::{PromiseSpec, PromiseType};

    fn make_spec(method: Method) -> PromiseSpec {
        PromiseSpec {
            id: "test".into(),
            promise_type: PromiseType::Standing,
            enabled: true,
            method_raw: format!("{method:?}").to_lowercase(),
            confidence: None,
            requires: None,
            description: None,
            pattern: None,
            min_lines: None,
            min_chars: None,
            check: None,
            max_tokens: None,
            max_ms: None,
            test_file_pattern: None,
            forbidden_patterns: None,
            threshold: None,
            tool_pattern: None,
            method: Some(method),
        }
    }

    use baseplate::java_test::JAVA_TEST_FILE_PATTERN as JAVA_TFP;
    const FORBIDDEN: &[&str] = &[r"\bJsonNode\b", r"\.getField\(", r"\.fields\(\)"];

    fn tap_spec(tfp: Option<&str>, forbidden: &[&str]) -> PromiseSpec {
        let mut s = make_spec(Method::TestAssertionPatterns);
        s.test_file_pattern = tfp.map(str::to_string);
        s.forbidden_patterns = Some(forbidden.iter().map(|&p| p.to_string()).collect());
        s
    }

    fn ctx_empty() -> ContextOwned {
        ContextOwned {
            diff: None,
            tokens_used: 0,
            elapsed_ms: 0,
        }
    }

    struct ContextOwned {
        diff: Option<String>,
        tokens_used: i64,
        elapsed_ms: i64,
    }
    impl ContextOwned {
        fn borrow(&self) -> VerifyContext<'_> {
            VerifyContext {
                diff: self.diff.as_deref(),
                tokens_used: self.tokens_used,
                elapsed_ms: self.elapsed_ms,
            }
        }
    }

    // O5/O6: registry.yaml `complete-output`/`no-placeholders` patterns must be
    // word-anchored so they match real markers/words but not substrings of
    // identifiers. `grep` (Method::Grep) compiles patterns case-insensitively
    // (compile_ci), so an unanchored `stub`/`TODO` also matches `StubDispatch`/
    // `TodoItem` — a false Broken on ordinary identifiers.
    const COMPLETE_OUTPUT_PATTERN: &str = r"\bTODO\b|\bFIXME\b|\bHACK\b";
    const NO_PLACEHOLDERS_PATTERN: &str =
        r"\bplaceholder\b|not yet implemented|\bstub\b|dummy implementation";

    fn grep_spec(pattern: &str) -> PromiseSpec {
        let mut s = make_spec(Method::Grep);
        s.pattern = Some(pattern.to_string());
        s
    }

    #[test]
    fn complete_output_broken_on_real_todo_marker() {
        let spec = grep_spec(COMPLETE_OUTPUT_PATTERN);
        let got = grep(&spec, "// TODO: fix this later");
        assert_eq!(got.result, Observation::Broken);
    }

    #[test]
    fn complete_output_kept_on_todo_as_identifier_substring() {
        let spec = grep_spec(COMPLETE_OUTPUT_PATTERN);
        let got = grep(&spec, "struct TodoItem;");
        assert_eq!(
            got.result,
            Observation::Kept,
            "TodoItem must not match \\bTODO\\b under case-insensitive grep; evidence: {}",
            got.evidence
        );
    }

    #[test]
    fn no_placeholders_broken_on_real_stub_word() {
        let spec = grep_spec(NO_PLACEHOLDERS_PATTERN);
        let got = grep(&spec, "this is a stub impl for now");
        assert_eq!(got.result, Observation::Broken);
    }

    #[test]
    fn no_placeholders_kept_on_stub_as_identifier_substring() {
        let spec = grep_spec(NO_PLACEHOLDERS_PATTERN);
        let got = grep(&spec, "StubDispatch::new()");
        assert_eq!(
            got.result,
            Observation::Kept,
            "StubDispatch must not match \\bstub\\b under case-insensitive grep; evidence: {}",
            got.evidence
        );
    }

    #[test]
    fn no_placeholders_kept_on_hackathon_identifier_substring_via_complete_output() {
        // Hackathon must not match \bHACK\b (complete-output pattern).
        let spec = grep_spec(COMPLETE_OUTPUT_PATTERN);
        let got = grep(&spec, "the Hackathon project");
        assert_eq!(
            got.result,
            Observation::Kept,
            "Hackathon must not match \\bHACK\\b under case-insensitive grep; evidence: {}",
            got.evidence
        );
    }

    #[test]
    fn no_placeholders_broken_on_not_yet_implemented_phrase() {
        let spec = grep_spec(NO_PLACEHOLDERS_PATTERN);
        let got = grep(&spec, "this feature is not yet implemented");
        assert_eq!(got.result, Observation::Broken);
    }

    // These two were `Partial` until #27 round 2. `Partial` is `Some(0.5)`, so a promise
    // nobody configured still moved the EMA — down for a trusted agent, up for an untrusted
    // one. An operand the spec never supplied is not half an answer, it is no answer.
    #[test]
    fn anti_widening_guard_missing_tfp_is_no_signal() {
        let spec = tap_spec(None, FORBIDDEN);
        let ctx = ctx_empty();
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(got.result, Observation::Skipped);
        assert!(got.evidence.contains("no test_file_pattern provided"));
    }

    #[test]
    fn anti_widening_guard_empty_tfp_is_no_signal() {
        let spec = tap_spec(Some(""), FORBIDDEN);
        let ctx = ctx_empty();
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(got.result, Observation::Skipped);
    }

    #[test]
    fn no_forbidden_patterns_is_no_signal() {
        let spec = tap_spec(Some(JAVA_TFP), &[]);
        let ctx = ContextOwned {
            diff: Some(
                "--- a/src/test/FooTest.java\n+++ b/src/test/FooTest.java\n@@ -1 +1 @@\n+JsonNode n;"
                    .to_string(),
            ),
            tokens_used: 0,
            elapsed_ms: 0,
        };
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(
            got.result,
            Observation::Skipped,
            "a scan with nothing forbidden to scan FOR cannot fail, even over a diff that \
             would have failed it; evidence: {}",
            got.evidence
        );
    }

    /// attestr#27: a pattern scan over nothing cannot find a pattern, so `Kept` here was a
    /// clean bill from a check that could not have failed. The two branches above this one
    /// already refused for a missing operand — this file distinguished can't-conclude from
    /// concluded-clean everywhere except with the other operand empty.
    #[test]
    fn an_empty_diff_is_no_signal_not_a_pass() {
        let spec = tap_spec(Some(JAVA_TFP), FORBIDDEN);
        let ctx = ctx_empty();
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(
            got.result,
            Observation::Skipped,
            "evidence: {}",
            got.evidence
        );
        assert!(got.evidence.contains("no diff content to scan"));
    }

    /// The control for the skip above: a non-empty diff still reaches a real verdict, in
    /// both directions. Without it, a branch wired to `Skipped` — or a scanner that stopped
    /// scanning — satisfies the assertion above and reports nothing forever.
    #[test]
    fn a_non_empty_diff_still_reaches_a_verdict() {
        let spec = tap_spec(Some(JAVA_TFP), FORBIDDEN);
        let offending = "--- a/src/test/FooTest.java\n+++ b/src/test/FooTest.java\n@@ -1 +1 @@\n+JsonNode n = parse(x);";
        let ctx = ContextOwned {
            diff: Some(offending.to_string()),
            tokens_used: 0,
            elapsed_ms: 0,
        };
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(
            got.result,
            Observation::Broken,
            "a forbidden pattern in a test file is the defect this method exists to find; \
             evidence: {}",
            got.evidence
        );

        let clean = "--- a/src/test/FooTest.java\n+++ b/src/test/FooTest.java\n@@ -1 +1 @@\n+assertThat(r).as(FooDto.class);";
        let ctx = ContextOwned {
            diff: Some(clean.to_string()),
            tokens_used: 0,
            elapsed_ms: 0,
        };
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(
            got.result,
            Observation::Kept,
            "a scanned diff with nothing forbidden in it IS a pass — that is the case the \
             empty-diff branch was borrowing its answer from; evidence: {}",
            got.evidence
        );
    }

    #[test]
    fn prod_file_diff_not_flagged() {
        let diff = "--- a/src/main/Foo.java\n+++ b/src/main/Foo.java\n@@ -1 +1 @@\n+JsonNode n = parse(x);";
        let spec = tap_spec(Some(JAVA_TFP), FORBIDDEN);
        let ctx = ContextOwned {
            diff: Some(diff.to_string()),
            tokens_used: 0,
            elapsed_ms: 0,
        };
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(got.result, Observation::Kept);
        assert_eq!(got.evidence, "no forbidden test-assertion patterns in diff");
    }

    #[test]
    fn test_file_diff_is_flagged() {
        let diff = "--- a/src/test/FooTest.java\n+++ b/src/test/FooTest.java\n@@ -1 +1,2 @@\n+JsonNode n = mapper.readTree(body);\n context line";
        let spec = tap_spec(Some(JAVA_TFP), FORBIDDEN);
        let ctx = ContextOwned {
            diff: Some(diff.to_string()),
            tokens_used: 0,
            elapsed_ms: 0,
        };
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(got.result, Observation::Broken);
        assert!(got.evidence.contains("src/test/FooTest.java"));
        assert!(got.evidence.contains(r"\bJsonNode\b"));
    }

    // #27 round 2. The property under test is NOT "no method answers `Kept` when it cannot
    // fail" — that phrasing is what let this hide, because it enumerates one direction. It is
    // **a verdict must be falsifiable**: for every `Kept`/`Broken` a method can emit, some
    // reachable input has to produce the other one. `grep` breaks it in the `Broken`
    // direction and no scan of `Kept` sites would ever have found it.
    //
    // Measured on `a81d5a2`, spec operand absent, over two inputs (real text and ""):
    //
    //   Grep         -> Broken / "Found 23 match(es): , , "     both inputs
    //   GrepAbsent   -> Kept   / "Required pattern found: , , " both inputs
    //   OutputContains -> Kept / "Found \"\" in output"          both inputs
    //
    // The empty regex matches at every position, `"x".contains("")` is true. Three verdicts,
    // none of them about the agent.
    /// #29. The registry cannot produce `method: None` — `registry::load` refuses an
    /// unrecognised `method:` string with `UnknownStandingMethod` — so this state arrives
    /// only from a hand-constructed spec, and nothing but this test can reach it.
    #[test]
    fn a_promise_with_no_resolved_method_is_no_signal() {
        let mut spec = make_spec(Method::Grep);
        spec.pattern = Some(r"\bTODO\b".to_string());
        spec.method = None;
        let (result, evidence, _) = verify_standing_promise(&spec, "// TODO: fix", None);
        assert_eq!(
            result,
            Observation::Skipped,
            "no verifier ran, so this is not half a pass; evidence: {evidence}"
        );

        // The control, and it is the whole reason the assertion above is not vacuous: the
        // SAME spec with its method resolved must still reach a real verdict — here `Broken`,
        // on input that a `grep` for `\bTODO\b` genuinely fails.
        spec.method = Some(Method::Grep);
        let (result, evidence, _) = verify_standing_promise(&spec, "// TODO: fix", None);
        assert_eq!(
            result,
            Observation::Broken,
            "control: a resolved method must still verify; evidence: {evidence}"
        );
    }

    #[test]
    fn an_unconfigured_operand_is_no_signal_not_a_verdict() {
        let ctx = ctx_empty();
        // Every method that reads `pattern` or `check`. `file_check` and `output_structure`
        // did not emit a false verdict — they answered `Partial`, which is `Some(0.5)` and so
        // still moved the score for a promise nobody configured. Same hole, smaller hole.
        for method in [
            Method::Grep,
            Method::GrepAbsent,
            Method::OutputContains,
            Method::FileCheck,
            Method::OutputStructure,
        ] {
            // Both spellings of "not supplied". `Some("")` reaches the identical vacuous
            // scan, so guarding only on `None` would leave the defect one YAML edit away.
            for spec in [make_spec(method), {
                let mut s = make_spec(method);
                s.pattern = Some(String::new());
                s.check = Some(String::new());
                s
            }] {
                for output in ["some real agent output", ""] {
                    let got = verify(method, &spec, output, &ctx.borrow());
                    assert_eq!(
                        got.result,
                        Observation::Skipped,
                        "{method:?} with no operand over {output:?} must report no signal, \
                         got {:?} / {}",
                        got.result,
                        got.evidence
                    );
                }
            }
        }
    }

    // The control. Without it, three methods hardwired to `Skipped` would satisfy every
    // assertion above while verifying nothing at all — and each method has to reach BOTH
    // verdicts, because reaching only one is the defect this pair of tests is about.
    #[test]
    fn a_configured_operand_still_reaches_both_verdicts() {
        let ctx = ctx_empty();
        let cases: &[(Method, &str, &str, &str)] = &[
            // (method, operand, input that must be Kept, input that must be Broken)
            (Method::Grep, r"\bTODO\b", "clean output", "// TODO: later"),
            (
                Method::GrepAbsent,
                r"\bhandled\b",
                "the error is handled",
                "no such word here",
            ),
            (
                Method::OutputContains,
                "constraint",
                "one constraint applies",
                "nothing of the sort",
            ),
            (
                Method::FileCheck,
                "test",
                "wrote src/foo.test.ts",
                "wrote src/foo.ts",
            ),
            (
                Method::OutputStructure,
                "constraint",
                "Note: one caveat applies",
                "nothing of the sort",
            ),
        ];
        for &(method, operand, kept_in, broken_in) in cases {
            let mut spec = make_spec(method);
            spec.pattern = Some(operand.to_string());
            spec.check = Some(operand.to_string());
            for (input, want) in [
                (kept_in, Observation::Kept),
                (broken_in, Observation::Broken),
            ] {
                let got = verify(method, &spec, input, &ctx.borrow());
                assert_eq!(
                    got.result, want,
                    "{method:?} with operand {operand:?} over {input:?} must be {want:?}, \
                     got {:?} / {}",
                    got.result, got.evidence
                );
            }
        }
    }
}
