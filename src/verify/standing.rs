use crate::verify::patterns::{compile_ci, compile_cs};
use dotclaude_support::model::{Confidence, Method, MethodOutcome, Observation, PromiseSpec};
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
        // Non-standing / unresolved method should never reach here (callers gate
        // on promise_type == Standing), but fail open rather than panic.
        return (
            Observation::Partial,
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

fn grep(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let re = match compile_ci(spec.pattern.as_deref().unwrap_or("")) {
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
    let re = match compile_ci(spec.pattern.as_deref().unwrap_or("")) {
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

static TEST_FILE_REF: Lazy<Regex> =
    Lazy::new(|| compile_ci(r"\.(test|spec)\.(js|ts|jsx|tsx)|tests?/").unwrap());
fn file_check(spec: &PromiseSpec, output: &str) -> MethodOutcome {
    let check = spec.check.clone().unwrap_or_default().to_lowercase();
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
    let check = spec.check.clone().unwrap_or_default();
    if output.contains(&check) {
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
    let check = spec.check.clone().unwrap_or_default().to_lowercase();
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
        _ => return outcome(Observation::Partial,
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
        return outcome(Observation::Partial, "no forbidden_patterns configured");
    }
    let diff = ctx.diff.unwrap_or(output);
    if diff.is_empty() {
        return outcome(Observation::Kept, "no diff content to scan");
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
    use dotclaude_support::model::{PromiseSpec, PromiseType};

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

    use dotclaude_support::java_test::JAVA_TEST_FILE_PATTERN as JAVA_TFP;
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

    /// Load the REAL `promise/registry.yaml` and exercise its actual
    /// `complete-output`/`no-placeholders` patterns end-to-end (not the local
    /// duplicated consts above) — proves the shipped YAML, not just a copy of
    /// it, is word-anchored.
    #[test]
    fn registry_yaml_complete_output_and_no_placeholders_are_word_anchored() {
        let (registry_path, _) = dotclaude_support::registry::default_paths();
        let registry = dotclaude_support::registry::load(&registry_path, None)
            .expect("registry.yaml must load for this test");

        let complete_output = registry
            .promises
            .get("complete-output")
            .expect("complete-output must exist in registry.yaml");
        let no_placeholders = registry
            .promises
            .get("no-placeholders")
            .expect("no-placeholders must exist in registry.yaml");

        let marker_broken = grep(complete_output, "// TODO: fix this later");
        assert_eq!(marker_broken.result, Observation::Broken);
        let identifier_kept = grep(complete_output, "struct TodoItem;");
        assert_eq!(
            identifier_kept.result,
            Observation::Kept,
            "registry.yaml complete-output pattern must not match TodoItem; evidence: {}",
            identifier_kept.evidence
        );

        let stub_broken = grep(no_placeholders, "this is a stub impl for now");
        assert_eq!(stub_broken.result, Observation::Broken);
        let stub_dispatch_kept = grep(no_placeholders, "StubDispatch::new()");
        assert_eq!(
            stub_dispatch_kept.result,
            Observation::Kept,
            "registry.yaml no-placeholders pattern must not match StubDispatch; evidence: {}",
            stub_dispatch_kept.evidence
        );
    }

    #[test]
    fn anti_widening_guard_missing_tfp_returns_partial() {
        let spec = tap_spec(None, FORBIDDEN);
        let ctx = ctx_empty();
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(got.result, Observation::Partial);
        assert!(got.evidence.contains("no test_file_pattern provided"));
    }

    #[test]
    fn anti_widening_guard_empty_tfp_returns_partial() {
        let spec = tap_spec(Some(""), FORBIDDEN);
        let ctx = ctx_empty();
        let got = verify(Method::TestAssertionPatterns, &spec, "", &ctx.borrow());
        assert_eq!(got.result, Observation::Partial);
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
}
