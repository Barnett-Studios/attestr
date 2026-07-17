//! 7 cxpak-backed structural verifiers + concurrent orchestration.
//! `None` response from any tool → `Observation::Skipped`, never an error (spec §5.6).

use dotclaude_support::cxpak::{
    Architecture, CallGraph, CxpakClient, DeadCode, DeadSymbol, Health, Predict, SecuritySurface,
    Verify,
};
use dotclaude_support::model::{Confidence, Observation, VerificationResult};
use dotclaude_support::trace::ts_now;
use serde_json::{json, Value};

fn mk_result(
    id: &str,
    method: &str,
    obs: Observation,
    conf: Confidence,
    evidence: impl Into<String>,
) -> VerificationResult {
    VerificationResult {
        promise_id: id.to_string(),
        method: method.to_string(),
        confidence: conf,
        result: obs,
        evidence: evidence.into(),
        timestamp: ts_now(),
    }
}

fn skipped(id: &str, method: &str) -> VerificationResult {
    mk_result(
        id,
        method,
        Observation::Skipped,
        Confidence::Low,
        "cxpak returned no data",
    )
}

fn parse<T: for<'de> serde::Deserialize<'de>>(v: Option<Value>) -> Option<T> {
    v.and_then(|v| serde_json::from_value(v).ok())
}

/// Call a cxpak capability, preferring the cxpak 3.0.0 intent-tool + `op` form and
/// falling back to the pre-3.0.0 tool name. This keeps structural verification
/// working across BOTH cxpak surfaces with NO dependence on the deprecated-alias
/// grace window (cxpak MIGRATION-3.0): on cxpak ≥3.0.0 the intent-tool answers
/// directly (the supported surface); on ≤2.3.0 the intent-tool is absent → `None`
/// → the legacy native tool answers. Crucially, when cxpak eventually retires the
/// 26 legacy aliases, this path is already on the intent-tool surface — it does not
/// silently degrade every structural verifier to `Skipped`. The extra call on
/// ≤2.3.0 (intent-tool miss then legacy) is one wasted round-trip on the old path
/// only; the 7 capabilities still run concurrently via the `tokio::join!` below.
async fn call_capability(
    client: &dyn CxpakClient,
    intent_tool: &str,
    op: &str,
    legacy: &str,
    args: Value,
) -> Option<Value> {
    let mut intent_args = args.clone();
    if let Value::Object(map) = &mut intent_args {
        map.insert("op".to_string(), Value::String(op.to_string()));
    }
    if let Some(v) = client.call(intent_tool, intent_args).await {
        return Some(v);
    }
    client.call(legacy, args).await
}

/// Fire 7 cxpak capabilities concurrently, then run the 7 structural verifiers.
/// Returns exactly 7 results keyed by promise_id. A `None` response from any
/// capability yields `Observation::Skipped` (spec §5.6); the turn never errors.
/// Each call prefers the cxpak 3.0.0 intent-tool and falls back to the pre-3.0.0
/// name — see `call_capability` (removes the deprecated-alias time-bomb).
pub async fn verify_all(
    client: &dyn CxpakClient,
    changed_files: &[String],
    diff: Option<&str>,
) -> Vec<VerificationResult> {
    // No files changed: all 7 verifiers short-circuit to Kept without calling cxpak.
    // This fires when `git diff --name-only HEAD` is empty.
    if changed_files.is_empty() {
        return vec![
            mk_result(
                "import-validity",
                "cxpak_call_graph",
                Observation::Kept,
                Confidence::High,
                "no files to check",
            ),
            mk_result(
                "function-length",
                "cxpak_health",
                Observation::Kept,
                Confidence::Medium,
                "no files to check",
            ),
            mk_result(
                "duplication",
                "cxpak_dead_code",
                Observation::Kept,
                Confidence::Medium,
                "no files to check",
            ),
            mk_result(
                "architectural-boundary",
                "cxpak_architecture",
                Observation::Kept,
                Confidence::Medium,
                "no files to check",
            ),
            mk_result(
                "convention-compliance",
                "cxpak_verify",
                Observation::Kept,
                Confidence::Medium,
                "no files to check",
            ),
            mk_result(
                "change-impact",
                "cxpak_predict",
                Observation::Kept,
                Confidence::Medium,
                "no files to check",
            ),
            mk_result(
                "security-surface",
                "cxpak_security_surface",
                Observation::Kept,
                Confidence::High,
                "no files to check",
            ),
        ];
    }

    let predict_args = json!({ "files": changed_files });
    let call_graph_args = json!({ "files": changed_files });
    let (call_graph, health, verify_r, dead_code, predict, security, architecture) = tokio::join!(
        call_capability(
            client,
            "cxpak_graph",
            "call_graph",
            "cxpak_call_graph",
            call_graph_args
        ),
        call_capability(client, "cxpak_insight", "health", "cxpak_health", json!({})),
        call_capability(client, "cxpak_review", "verify", "cxpak_verify", json!({})),
        call_capability(
            client,
            "cxpak_graph",
            "dead_code",
            "cxpak_dead_code",
            json!({})
        ),
        call_capability(
            client,
            "cxpak_graph",
            "predict",
            "cxpak_predict",
            predict_args
        ),
        call_capability(
            client,
            "cxpak_insight",
            "security_surface",
            "cxpak_security_surface",
            json!({})
        ),
        call_capability(
            client,
            "cxpak_insight",
            "architecture",
            "cxpak_architecture",
            json!({})
        ),
    );

    vec![
        verify_import_validity(parse::<CallGraph>(call_graph), changed_files, diff),
        verify_function_length(parse::<Health>(health.clone())),
        verify_duplication(
            parse::<DeadCode>(dead_code),
            parse::<Health>(health),
            changed_files,
        ),
        verify_architectural_boundary(parse::<Architecture>(architecture)),
        verify_convention_compliance(parse::<Verify>(verify_r), changed_files),
        verify_change_impact(parse::<Predict>(predict)),
        verify_security_surface(parse::<SecuritySurface>(security), changed_files),
    ]
}

/// Slash-anchored path-suffix match, shared by every verifier that attributes
/// a cxpak-reported issue back to `changed_files`: exact equality, or one path
/// ends with `/` + the other. A bare (non-anchored) `ends_with` false-matches
/// `"FooUtils.js".ends_with("Utils.js")`; requiring a `/` boundary before the
/// shared suffix rules that out while still matching `"pkg/src/utils.js"`
/// against `"src/utils.js"`. Mirrors `behavioral.rs`'s `path_covered_by`.
fn path_suffix_matches(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// Identifiers a RELATIVE import brings into scope, harvested from the diff's ADDED
/// lines. cxpak 3.1.0 reports an `unresolved` callee by its SYMBOL name (`ghost`,
/// `missing_fn`) — never a path — so the old `is_file_import(callee_name)` path check
/// matched nothing and import-validity never fired. Instead, a call cxpak leaves
/// unresolved whose callee was imported *relatively* in this diff is a genuine "won't
/// run" defect (the local module/symbol does not exist); an unresolved external/stdlib
/// call (`print`, `os.system` — cxpak indexes the repo, not site-packages) is NOT a
/// defect and must never block. Precision-biased: only clearly-relative imports
/// (Python `from .`, JS/TS `from './'|'../'|'/'`); an unhandled import form under-flags
/// (safe — a missed defect) rather than over-flags (harmful — blocks legit code).
fn relative_import_symbols(diff: Option<&str>) -> std::collections::HashSet<String> {
    let mut syms = std::collections::HashSet::new();
    let Some(diff) = diff else {
        return syms;
    };
    for raw in diff.lines() {
        // Added lines only; strip the leading '+', skip the '+++' file header.
        let Some(line) = raw.strip_prefix('+') else {
            continue;
        };
        if line.starts_with("++") {
            continue;
        }
        let line = line.trim();

        // Python: `from .mod import a, b as c`  /  `from . import x`
        if let Some(rest) = line.strip_prefix("from ") {
            if rest.starts_with('.') {
                if let Some((_, names)) = rest.split_once(" import ") {
                    for part in names.split(',') {
                        add_import_binding(&mut syms, part);
                    }
                }
            }
            continue;
        }

        // JS/TS: `import <bindings> from '<specifier>'` with a relative specifier.
        if let Some(rest) = line.strip_prefix("import ") {
            if let Some(spec) = js_import_specifier(rest) {
                if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
                    let bindings = rest.split_once(" from ").map(|(b, _)| b).unwrap_or("");
                    for part in bindings.split([',', '{', '}']) {
                        add_import_binding(&mut syms, part);
                    }
                }
            }
        }
    }
    syms
}

/// Insert the usable identifier from one import fragment: the alias if present
/// (`x as y` → `y`, the name used at the call site that cxpak reports), else the
/// name. Drops `*`, empty fragments, and anything not a plain identifier.
fn add_import_binding(syms: &mut std::collections::HashSet<String>, fragment: &str) {
    let f = fragment
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    let ident = f.rsplit(" as ").next().unwrap_or(f).trim();
    if !ident.is_empty()
        && ident != "*"
        && ident
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    {
        syms.insert(ident.to_string());
    }
}

/// The quoted module specifier of a JS/TS import statement body
/// (`{ a } from './x';` → `./x`), or None if the line has no `from '...'` clause.
fn js_import_specifier(rest: &str) -> Option<&str> {
    let (_, after) = rest.split_once(" from ")?;
    let after = after.trim_start();
    let quote = after.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body = &after[1..];
    let end = body.find(quote)?;
    Some(&body[..end])
}

/// `cxpak_call_graph`: unresolved edges that are file imports AND whose
/// `caller_file` is one of `changed_files` → Broken; else Kept. Turn-scoped:
/// an unresolved import in a file the turn never touched must not fire
/// Broken (O2 — was previously repo-wide, false-Broken+High on every turn).
/// The recording has 21 bare method names (join, map, etc.) → all filtered as
/// builtin refs, leaving 0 real unresolved file imports → Kept (golden §4.2).
fn verify_import_validity(
    cg: Option<CallGraph>,
    changed_files: &[String],
    diff: Option<&str>,
) -> VerificationResult {
    let id = "import-validity";
    let Some(cg) = cg else {
        return skipped(id, "cxpak_call_graph");
    };
    let local_imports = relative_import_symbols(diff);
    let all_unresolved = &cg.unresolved;
    let file_imports: Vec<&Value> = all_unresolved
        .iter()
        .filter(|u| {
            let name = u.get("callee_name").and_then(|v| v.as_str()).unwrap_or("");
            // Only an unresolved callee that THIS diff imported relatively is a real
            // broken import; bare externals/builtins (not relatively imported here)
            // are cxpak-can't-see-it, not defects.
            if name.is_empty() || !local_imports.contains(name) {
                return false;
            }
            let caller_file = u.get("caller_file").and_then(|v| v.as_str()).unwrap_or("");
            changed_files
                .iter()
                .any(|f| path_suffix_matches(caller_file, f))
        })
        .collect();
    let filtered_count = all_unresolved.len() - file_imports.len();

    if file_imports.is_empty() {
        let filter_note = if filtered_count > 0 {
            format!(", {} builtin refs filtered", filtered_count)
        } else {
            String::new()
        };
        mk_result(
            id,
            "cxpak_call_graph",
            Observation::Kept,
            Confidence::High,
            format!(
                "cxpak_call_graph: {} edges, 0 unresolved imports{}",
                cg.edges.len(),
                filter_note
            ),
        )
    } else {
        let detail: Vec<String> = file_imports
            .iter()
            .take(5)
            .map(|u| {
                let caller = u
                    .get("caller_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let callee = u
                    .get("callee_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("{}→{}", caller, callee)
            })
            .collect();
        let suffix = if file_imports.len() > 5 {
            format!(" (+{} more)", file_imports.len() - 5)
        } else {
            String::new()
        };
        let filter_note = if filtered_count > 0 {
            format!(" [{} builtin refs filtered]", filtered_count)
        } else {
            String::new()
        };
        mk_result(
            id,
            "cxpak_call_graph",
            Observation::Broken,
            Confidence::High,
            format!(
                "{} unresolved import(s): {}{}{}",
                file_imports.len(),
                detail.join("; "),
                suffix,
                filter_note
            ),
        )
    }
}

/// `cxpak_health.conventions` >= 7.0 → Kept (medium); else Broken.
fn verify_function_length(h: Option<Health>) -> VerificationResult {
    let id = "function-length";
    let Some(h) = h else {
        return skipped(id, "cxpak_health");
    };
    let conventions = h.conventions;
    if conventions >= 7.0 {
        mk_result(
            id,
            "cxpak_health",
            Observation::Kept,
            Confidence::Medium,
            format!("cxpak_health conventions: {:.1}/10", conventions),
        )
    } else {
        mk_result(
            id,
            "cxpak_health",
            Observation::Broken,
            Confidence::Medium,
            format!(
                "cxpak_health conventions: {:.1}/10 (below 7.0 threshold)",
                conventions
            ),
        )
    }
}

fn dead_symbol_in_modified(s: &DeadSymbol, changed_files: &[String]) -> bool {
    changed_files
        .iter()
        .any(|f| path_suffix_matches(s.file.as_str(), f.as_str()))
}

/// `cxpak_dead_code`: dead symbols in modified files → Broken; else Kept (high).
/// Fallback to `cxpak_health.dead_code`/`.composite` when dead_code tool absent.
fn verify_duplication(
    dc: Option<DeadCode>,
    h: Option<Health>,
    changed_files: &[String],
) -> VerificationResult {
    let id = "duplication";
    if let Some(dc) = dc {
        let all_symbols = &dc.dead_symbols;
        let matched: Vec<&DeadSymbol> = all_symbols
            .iter()
            .filter(|s| dead_symbol_in_modified(s, changed_files))
            .collect();
        if matched.is_empty() {
            return mk_result(
                id,
                "cxpak_dead_code",
                Observation::Kept,
                Confidence::High,
                format!(
                    "cxpak_dead_code: {} dead symbols in modified files ({} total, {} scanned)",
                    matched.len(),
                    all_symbols.len(),
                    dc.total_scanned,
                ),
            );
        }
        let detail: Vec<String> = matched
            .iter()
            .take(5)
            .map(|s| format!("{}:{}", s.file, s.name))
            .collect();
        let suffix = if matched.len() > 5 {
            format!(" (+{} more)", matched.len() - 5)
        } else {
            String::new()
        };
        return mk_result(
            id,
            "cxpak_dead_code",
            Observation::Broken,
            Confidence::High,
            format!(
                "{} dead symbol(s) in modified files: {}{}",
                matched.len(),
                detail.join("; "),
                suffix
            ),
        );
    }
    let Some(h) = h else {
        return skipped(id, "cxpak_dead_code");
    };
    if let Some(dead_code) = h.dead_code {
        if dead_code >= 7.0 {
            return mk_result(
                id,
                "cxpak_health",
                Observation::Kept,
                Confidence::Medium,
                format!("cxpak_health dead_code: {:.1}/10", dead_code),
            );
        }
        return mk_result(
            id,
            "cxpak_health",
            Observation::Broken,
            Confidence::Medium,
            format!(
                "cxpak_health dead_code: {:.1}/10 (below 7.0 threshold)",
                dead_code
            ),
        );
    }
    let composite = h.composite.unwrap_or(0.0);
    if composite >= 7.0 {
        mk_result(
            id,
            "cxpak_health",
            Observation::Kept,
            Confidence::Medium,
            format!(
                "cxpak_health composite: {:.1}/10 (dead_code unavailable)",
                composite
            ),
        )
    } else {
        mk_result(
            id,
            "cxpak_health",
            Observation::Broken,
            Confidence::Medium,
            format!(
                "cxpak_health composite: {:.1}/10 (below 7.0, dead_code unavailable)",
                composite
            ),
        )
    }
}

/// `cxpak_architecture`: per-module boundary_violations/god_files arrays + top-level
/// circular_deps. All empty → Kept; any non-empty → Broken (medium).
fn verify_architectural_boundary(a: Option<Architecture>) -> VerificationResult {
    let id = "architectural-boundary";
    let Some(a) = a else {
        return skipped(id, "cxpak_architecture");
    };
    let modules = &a.modules;
    let mut violations: Vec<String> = Vec::new();
    for module in modules {
        if !module.boundary_violations.is_empty() {
            violations.push(format!(
                "{} boundary violations",
                module.boundary_violations.len()
            ));
        }
        if !module.god_files.is_empty() {
            violations.push(format!("{} god files", module.god_files.len()));
        }
    }
    if !a.circular_deps.is_empty() {
        violations.push(format!("{} circular dependencies", a.circular_deps.len()));
    }
    if violations.is_empty() {
        mk_result(
            id,
            "cxpak_architecture",
            Observation::Kept,
            Confidence::Medium,
            format!(
                "cxpak_architecture: {} modules, no boundary violations",
                modules.len()
            ),
        )
    } else {
        mk_result(
            id,
            "cxpak_architecture",
            Observation::Broken,
            Confidence::Medium,
            violations.join("; "),
        )
    }
}

/// `cxpak_verify`: violations empty → Kept (medium).
/// File count: `files_checked` when > 0, else `changed_files.len()` (JS fallback).
fn verify_convention_compliance(v: Option<Verify>, changed_files: &[String]) -> VerificationResult {
    let id = "convention-compliance";
    let Some(v) = v else {
        return skipped(id, "cxpak_verify");
    };
    let violations = &v.violations;
    if violations.is_empty() {
        let checked = if v.files_checked > 0 {
            v.files_checked as usize
        } else {
            changed_files.len()
        };
        return mk_result(
            id,
            "cxpak_verify",
            Observation::Kept,
            Confidence::Medium,
            format!(
                "cxpak_verify: {} files, {} convention violations",
                checked,
                violations.len()
            ),
        );
    }
    // loc = file (with optional :line), or "unknown"; desc = rule || message || "violation".
    let detail: Vec<String> = violations
        .iter()
        .take(5)
        .map(|vi| {
            let loc = if vi.file.is_empty() {
                "unknown".to_string()
            } else {
                match vi.line {
                    Some(l) => format!("{}:{}", vi.file, l),
                    None => vi.file.clone(),
                }
            };
            let desc = if !vi.rule.is_empty() {
                vi.rule.as_str()
            } else if !vi.message.is_empty() {
                vi.message.as_str()
            } else {
                "violation"
            };
            format!("{} {}", loc, desc)
        })
        .collect();
    let suffix = if violations.len() > 5 {
        format!(" (+{} more)", violations.len() - 5)
    } else {
        String::new()
    };
    mk_result(
        id,
        "cxpak_verify",
        Observation::Broken,
        Confidence::Medium,
        format!(
            "{} convention violation(s): {}{}",
            violations.len(),
            detail.join("; "),
            suffix
        ),
    )
}

/// `cxpak_predict`: risk_score < 0.5 → Kept; >= 0.5 with >=2 test_predictions → Kept;
/// else Broken (medium). JS faithfully preserved: risk_score defaults to 0, affected_files
/// always 0 (structural_impact.affected_files not captured in Predict DTO).
/// Distinct impacted files across cxpak 3.0.0 predict's per-file impact lists,
/// deduplicated by the item's `path`. Used only on the 3.0.0 shape.
fn predict_affected_files(p: &Predict) -> usize {
    let mut seen = std::collections::HashSet::new();
    for list in [
        &p.structural_impact,
        &p.call_impact,
        &p.historical_impact,
        &p.test_impact,
    ] {
        for item in list {
            if let Some(path) = item.get("path").and_then(|v| v.as_str()) {
                seen.insert(path.to_string());
            }
        }
    }
    seen.len()
}

fn verify_change_impact(p: Option<Predict>) -> VerificationResult {
    let id = "change-impact";
    let Some(p) = p else {
        return skipped(id, "cxpak_predict");
    };
    // cxpak 3.0.0 (ADR-0174) dropped `risk_score` and returns per-file impact lists
    // plus `confidence_summary`. Detect that shape by `confidence_summary` (its
    // `risk_score` would otherwise default to 0.0 and silently pass every turn).
    // No risk threshold survives the restructuring, so report the real
    // affected-file count and fail open to Kept at Low confidence — informational
    // under 3.0.0, not a gating signal. The 2.3.0 `risk_score` path below is
    // unchanged.
    if p.confidence_summary.is_some() {
        return mk_result(
            id,
            "cxpak_predict",
            Observation::Kept,
            Confidence::Low,
            format!(
                "cxpak predict (3.0.0): {} impacted file(s) across structural/call/historical/test impact",
                predict_affected_files(&p)
            ),
        );
    }
    // JS: affectedFiles = (predictResponse.structural_impact?.affected_files) || 0.
    // Predict DTO does not capture structural_impact → 0. Bug-for-bug parity (§9 flag).
    let affected_files: u64 = 0;
    if p.risk_score < 0.5 {
        return mk_result(
            id,
            "cxpak_predict",
            Observation::Kept,
            Confidence::Medium,
            format!(
                "cxpak_predict: risk_score={:.2}, {} affected files",
                p.risk_score, affected_files
            ),
        );
    }
    if p.test_predictions.len() >= 2 {
        return mk_result(
            id,
            "cxpak_predict",
            Observation::Kept,
            Confidence::Medium,
            format!(
                "cxpak_predict: risk_score={:.2}, {} affected, {} test_predictions",
                p.risk_score,
                affected_files,
                p.test_predictions.len()
            ),
        );
    }
    mk_result(
        id,
        "cxpak_predict",
        Observation::Broken,
        Confidence::Medium,
        format!(
            "cxpak_predict: risk_score={:.2}, {} affected files, {} test prediction(s) — high impact, low test coverage",
            p.risk_score,
            affected_files,
            p.test_predictions.len()
        ),
    )
}

fn issue_in_modified_files(issue: &Value, changed_files: &[String]) -> bool {
    let file = issue.get("file").and_then(|v| v.as_str()).unwrap_or("");
    changed_files.iter().any(|f| path_suffix_matches(file, f))
}

/// `cxpak_security_surface`: secret_patterns + sql_injection_surface + unprotected_endpoints
/// filtered to modified files. Any → Broken; else Kept (high).
/// Uses real cxpak 2.3.0 field names (§9 fix); golden parity preserved (0 issues either way).
fn verify_security_surface(
    s: Option<SecuritySurface>,
    changed_files: &[String],
) -> VerificationResult {
    let id = "security-surface";
    let Some(s) = s else {
        return skipped(id, "cxpak_security_surface");
    };
    let secrets: Vec<&Value> = s
        .secret_patterns
        .iter()
        .filter(|i| issue_in_modified_files(i, changed_files))
        .collect();
    let sql: Vec<&Value> = s
        .sql_injection_surface
        .iter()
        .filter(|i| issue_in_modified_files(i, changed_files))
        .collect();
    let unprotected: Vec<&Value> = s
        .unprotected_endpoints
        .iter()
        .filter(|i| issue_in_modified_files(i, changed_files))
        .collect();

    let mut issues: Vec<String> = Vec::new();
    if !secrets.is_empty() {
        let detail: Vec<String> = secrets
            .iter()
            .take(3)
            .map(|item| {
                let file = item
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let line = item
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .map(|l| format!(":{}", l))
                    .unwrap_or_default();
                let kind = item
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("secret");
                format!("{}{} {}", file, line, kind)
            })
            .collect();
        issues.push(format!(
            "{} secret(s): {}",
            secrets.len(),
            detail.join(", ")
        ));
    }
    if !sql.is_empty() {
        let detail: Vec<String> = sql
            .iter()
            .take(3)
            .map(|item| {
                let file = item
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let line = item
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .map(|l| format!(":{}", l))
                    .unwrap_or_default();
                format!("{}{}", file, line)
            })
            .collect();
        issues.push(format!(
            "{} sql_injection risk(s): {}",
            sql.len(),
            detail.join(", ")
        ));
    }
    if !unprotected.is_empty() {
        issues.push(format!("{} unprotected endpoint(s)", unprotected.len()));
    }

    if issues.is_empty() {
        mk_result(
            id,
            "cxpak_security_surface",
            Observation::Kept,
            Confidence::High,
            "cxpak_security_surface: 0 issues in modified files",
        )
    } else {
        mk_result(
            id,
            "cxpak_security_surface",
            Observation::Broken,
            Confidence::High,
            issues.join("; "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{relative_import_symbols, verify_all};
    use dotclaude_support::cxpak::RecordedCxpakClient;
    use dotclaude_support::model::{Confidence, Observation};
    use serde_json::json;
    use std::collections::HashMap;

    fn one_tool(name: &str, value: serde_json::Value) -> RecordedCxpakClient {
        let mut map = HashMap::new();
        map.insert(name.to_string(), value);
        RecordedCxpakClient::new(map)
    }

    /// import-validity must be scoped to `changed_files`: an unresolved import
    /// whose `caller_file` is NOT one of the changed files must not fire Broken
    /// (O2 — repo-wide false-Broken+High on every turn regardless of scope).
    #[tokio::test]
    async fn import_validity_ignores_unresolved_import_in_unchanged_file() {
        // Even a relatively-imported unresolved symbol must NOT fire when its
        // caller_file is not among the changed files (turn-scoped).
        let client = one_tool(
            "cxpak_call_graph",
            json!({
                "edges": [],
                "unresolved": [{"callee_name": "legacy_missing", "caller_file": "src/legacy/untouched.js", "caller_symbol": "foo"}]
            }),
        );
        let diff = "+import { legacy_missing } from './gone.js';\n";
        let results = verify_all(&client, &["src/foo.js".into()], Some(diff)).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "import-validity")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Kept,
            "unresolved import in an unchanged file must not fire Broken; evidence: {}",
            r.evidence
        );
    }

    /// import-validity fires Broken when the unresolved callee is relatively imported
    /// in the diff AND its `caller_file` IS one of the changed files.
    #[tokio::test]
    async fn import_validity_broken_on_unresolved_import_in_changed_file() {
        let client = one_tool(
            "cxpak_call_graph",
            json!({
                "edges": [],
                "unresolved": [{"callee_name": "missing", "caller_file": "src/foo.js", "caller_symbol": "foo"}]
            }),
        );
        let diff = "+import { missing } from './missing.js';\n";
        let results = verify_all(&client, &["src/foo.js".into()], Some(diff)).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "import-validity")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Broken,
            "unresolved relative import in a changed file must fire Broken; evidence: {}",
            r.evidence
        );
    }

    /// Time-bomb guard (cxpak MIGRATION-3.0): once cxpak retires the 26 legacy
    /// tool-name aliases, only the 5 intent-tools remain. Record ONLY the intent-tool
    /// (`cxpak_graph`, with no legacy `cxpak_call_graph` recording) and assert
    /// import-validity still resolves via the fallback's intent-tool arm rather than
    /// silently degrading every structural verifier to Skipped.
    #[tokio::test]
    async fn structural_resolves_via_intent_tool_when_legacy_alias_absent() {
        let client = one_tool("cxpak_graph", json!({"edges": [{}], "unresolved": []}));
        let results = verify_all(&client, &["src/foo.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "import-validity")
            .unwrap();
        assert_ne!(
            r.result,
            Observation::Skipped,
            "import-validity must resolve via the cxpak_graph intent-tool with no legacy alias present; evidence: {}",
            r.evidence
        );
        assert_eq!(r.result, Observation::Kept);
    }

    /// change-impact under the cxpak 3.0.0 predict shape (no risk_score, per-file
    /// impact lists + confidence_summary). Must report the real impacted-file count
    /// rather than silently passing on a defaulted risk_score=0.0.
    #[tokio::test]
    async fn change_impact_handles_cxpak_3_0_predict_shape() {
        let client = one_tool(
            "cxpak_graph",
            json!({
                "changed_files": ["a.rs"],
                "structural_impact": [],
                "historical_impact": [{"path": "x.rs", "score": 0.3}, {"path": "y.rs", "score": 0.2}],
                "call_impact": [{"path": "x.rs", "score": 0.1}],
                "test_impact": [],
                "confidence_summary": "medium"
            }),
        );
        let results = verify_all(&client, &["a.rs".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "change-impact")
            .unwrap();
        assert_eq!(r.result, Observation::Kept);
        assert_eq!(r.confidence, Confidence::Low);
        assert!(
            r.evidence.contains("2 impacted file(s)"),
            "expected 2 distinct impacted files (x.rs, y.rs); evidence: {}",
            r.evidence
        );
    }

    /// function-length: conventions >= 7 → Kept (inverse of the golden Broken at 5.0).
    #[tokio::test]
    async fn function_length_kept_above_threshold() {
        let client = one_tool("cxpak_health", json!({"conventions": 8.0}));
        let results = verify_all(&client, &["src/foo.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "function-length")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Kept,
            "expected Kept at conventions=8.0; evidence: {}",
            r.evidence
        );
        assert!(
            r.evidence.contains("8.0/10"),
            "evidence should include score: {}",
            r.evidence
        );
    }

    #[test]
    fn relative_import_symbols_selects_relative_only_alias_resolved() {
        let d = "+from .helpers import missing_fn, other as aliased\n\
                 +from . import mod\n\
                 +import os\n\
                 +from external.pkg import thing\n\
                 +import { ghost, foo as bar } from './missing.js';\n\
                 +import def_export from '../x';\n\
                 +import * as ns from './n';\n\
                 +import { ext } from 'react';\n";
        let s = relative_import_symbols(Some(d));
        for want in [
            "missing_fn",
            "aliased",
            "mod",
            "ghost",
            "bar",
            "def_export",
            "ns",
        ] {
            assert!(s.contains(want), "want {want} in {s:?}");
        }
        // Non-relative imports (stdlib / external pkg / bare pkg) must be excluded —
        // this is the false-positive the whole discriminator exists to prevent.
        for no in ["os", "thing", "ext", "foo", "other", "*"] {
            assert!(!s.contains(no), "should exclude {no}: {s:?}");
        }
    }

    /// import-validity: an unresolved EXTERNAL call (`os.system`) not relatively
    /// imported → Kept. Regression guard against the dead_code-class false positive.
    #[tokio::test]
    async fn import_validity_kept_on_unresolved_external_call() {
        let client = one_tool(
            "cxpak_graph",
            json!({"edges": [], "unresolved": [
                {"callee_name": "system", "caller_file": "main.py", "caller_symbol": "run"}]}),
        );
        let diff = "+import os\n+def run(cmd):\n+    os.system(cmd)\n";
        let results = verify_all(&client, &["main.py".into()], Some(diff)).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "import-validity")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Kept,
            "external call must not block; evidence: {}",
            r.evidence
        );
    }

    /// import-validity: no diff → cannot confirm a relative import → Kept (safe).
    #[tokio::test]
    async fn import_validity_kept_when_diff_absent() {
        let client = one_tool(
            "cxpak_graph",
            json!({"edges": [], "unresolved": [
                {"callee_name": "missing_fn", "caller_file": "main.py", "caller_symbol": "a"}]}),
        );
        let results = verify_all(&client, &["main.py".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "import-validity")
            .unwrap();
        assert_eq!(r.result, Observation::Kept, "evidence: {}", r.evidence);
    }

    /// duplication: dead_symbols matching a changed file → Broken.
    #[tokio::test]
    async fn duplication_broken_on_dead_symbol_in_modified_file() {
        let client = one_tool(
            "cxpak_dead_code",
            json!({
                "dead_symbols": [{"file": "src/index.js", "name": "unusedExport"}],
                "total_scanned": 5
            }),
        );
        let results = verify_all(&client, &["src/index.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "duplication")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Broken,
            "expected Broken; evidence: {}",
            r.evidence
        );
        assert!(
            r.evidence.contains("unusedExport"),
            "evidence should name the symbol: {}",
            r.evidence
        );
    }

    /// convention-compliance: non-empty violations → Broken.
    #[tokio::test]
    async fn convention_compliance_broken_on_violations() {
        let client = one_tool(
            "cxpak_verify",
            json!({
                "violations": [{"file": "src/foo.js", "rule": "no-unused-vars", "message": "x is unused"}],
                "files_checked": 1
            }),
        );
        let results = verify_all(&client, &["src/foo.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "convention-compliance")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Broken,
            "expected Broken; evidence: {}",
            r.evidence
        );
    }

    /// architectural-boundary: a module with non-empty boundary_violations array → Broken.
    /// Confirms `Vec::len() > 0` (not a numeric coerce) is used for the non-empty check.
    #[tokio::test]
    async fn architectural_boundary_broken_on_module_violations() {
        let client = one_tool(
            "cxpak_architecture",
            json!({
                "circular_deps": [],
                "modules": [
                    {"boundary_violations": [{"from": "src/a.js", "to": "src/b.js"}], "god_files": []}
                ]
            }),
        );
        let results = verify_all(&client, &["src/a.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "architectural-boundary")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Broken,
            "expected Broken; evidence: {}",
            r.evidence
        );
        assert!(
            r.evidence.contains("boundary violation"),
            "evidence should mention boundary violations: {}",
            r.evidence
        );
    }

    /// duplication: a dead symbol in an UNRELATED file whose bare name merely
    /// shares a suffix with a changed file (`FooUtils.js` vs changed `Utils.js`,
    /// no path-separator boundary between `Foo` and `Utils`) must NOT match
    /// (O3 — suffix match with no path-separator boundary).
    #[tokio::test]
    async fn duplication_kept_when_dead_symbol_file_shares_bare_suffix_only() {
        let client = one_tool(
            "cxpak_dead_code",
            json!({
                "dead_symbols": [{"file": "FooUtils.js", "name": "unusedExport"}],
                "total_scanned": 5
            }),
        );
        let results = verify_all(&client, &["Utils.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "duplication")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Kept,
            "FooUtils.js must not match changed Utils.js on bare suffix; evidence: {}",
            r.evidence
        );
    }

    /// duplication: `src/fooutils.js` vs changed `src/utils.js` must not match
    /// (literal bug-report scenario, path-prefixed form).
    #[tokio::test]
    async fn duplication_kept_when_dead_symbol_file_is_path_prefixed_non_boundary_suffix() {
        let client = one_tool(
            "cxpak_dead_code",
            json!({
                "dead_symbols": [{"file": "src/fooutils.js", "name": "unusedExport"}],
                "total_scanned": 5
            }),
        );
        let results = verify_all(&client, &["src/utils.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "duplication")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Kept,
            "src/fooutils.js must not match changed src/utils.js; evidence: {}",
            r.evidence
        );
    }

    /// duplication: a dead symbol whose file path is `pkg/src/utils.js` (real
    /// path-boundary suffix of changed `src/utils.js`) must still match → Broken.
    #[tokio::test]
    async fn duplication_broken_on_dead_symbol_matching_path_boundary_suffix() {
        let client = one_tool(
            "cxpak_dead_code",
            json!({
                "dead_symbols": [{"file": "pkg/src/utils.js", "name": "unusedExport"}],
                "total_scanned": 5
            }),
        );
        let results = verify_all(&client, &["src/utils.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "duplication")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Broken,
            "pkg/src/utils.js must match changed src/utils.js on a real path boundary; evidence: {}",
            r.evidence
        );
    }

    /// security-surface: a secret in an UNRELATED file whose bare name merely
    /// shares a suffix with a changed file (`FooUtils.js` vs changed `Utils.js`)
    /// must NOT match (O3, `issue_in_modified_files`).
    #[tokio::test]
    async fn security_surface_kept_when_issue_file_shares_bare_suffix_only() {
        let client = one_tool(
            "cxpak_security_surface",
            json!({
                "secret_patterns": [{"file": "FooUtils.js", "type": "api_key", "line": 42}],
                "sql_injection_surface": [],
                "unprotected_endpoints": []
            }),
        );
        let results = verify_all(&client, &["Utils.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "security-surface")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Kept,
            "FooUtils.js must not match changed Utils.js on bare suffix; evidence: {}",
            r.evidence
        );
    }

    /// security-surface: secret_patterns entry matching a changed file → Broken.
    #[tokio::test]
    async fn security_surface_broken_on_secret_in_modified_file() {
        let client = one_tool(
            "cxpak_security_surface",
            json!({
                "secret_patterns": [{"file": "src/index.js", "type": "api_key", "line": 42}],
                "sql_injection_surface": [],
                "unprotected_endpoints": []
            }),
        );
        let results = verify_all(&client, &["src/index.js".into()], None).await;
        let r = results
            .iter()
            .find(|r| r.promise_id == "security-surface")
            .unwrap();
        assert_eq!(
            r.result,
            Observation::Broken,
            "expected Broken; evidence: {}",
            r.evidence
        );
        assert!(
            r.evidence.contains("secret"),
            "evidence should mention secrets: {}",
            r.evidence
        );
    }

    /// Empty changed_files → all 7 Kept "no files to check"; cxpak MUST NOT be called.
    /// Mirrors JS verifyAll:511-526. Proves the short-circuit fires even with a
    /// non-empty RecordedCxpakClient that would produce non-Kept results if called.
    #[tokio::test]
    async fn empty_changed_files_returns_all_kept_without_calling_cxpak() {
        // Client has recordings that would produce Broken results if consulted.
        let mut map = HashMap::new();
        map.insert("cxpak_health".to_string(), json!({"conventions": 2.0})); // would → Broken function-length
        map.insert(
            "cxpak_call_graph".to_string(),
            json!({"edges": [], "unresolved": [{"callee_name": "./bad.js"}]}), // would → Broken import-validity
        );
        let client = RecordedCxpakClient::new(map);

        let results = verify_all(&client, &[], None).await;

        assert_eq!(results.len(), 7, "must return exactly 7 results");
        for r in &results {
            assert_eq!(
                r.result,
                Observation::Kept,
                "{} should be Kept on empty files; got {:?} evidence={}",
                r.promise_id,
                r.result,
                r.evidence
            );
            assert_eq!(
                r.evidence, "no files to check",
                "{} evidence mismatch",
                r.promise_id
            );
        }
        // Verify per-verifier confidences match JS (verifyAll:514-521).
        let conf_of = |id: &str| {
            results
                .iter()
                .find(|r| r.promise_id == id)
                .map(|r| r.confidence)
                .unwrap()
        };
        assert_eq!(
            conf_of("import-validity"),
            dotclaude_support::model::Confidence::High
        );
        assert_eq!(
            conf_of("function-length"),
            dotclaude_support::model::Confidence::Medium
        );
        assert_eq!(
            conf_of("duplication"),
            dotclaude_support::model::Confidence::Medium
        );
        assert_eq!(
            conf_of("architectural-boundary"),
            dotclaude_support::model::Confidence::Medium
        );
        assert_eq!(
            conf_of("convention-compliance"),
            dotclaude_support::model::Confidence::Medium
        );
        assert_eq!(
            conf_of("change-impact"),
            dotclaude_support::model::Confidence::Medium
        );
        assert_eq!(
            conf_of("security-surface"),
            dotclaude_support::model::Confidence::High
        );
    }
}
