//! 3 trace-backed behavioral verifiers: `verify_read_before_write`,
//! `verify_exploration_breadth`, and `verify_context_acquisition`.
//! Pure over the trace — no I/O, no async.

use baseplate::model::{Confidence, Observation, VerificationResult};
use baseplate::trace::ts_now;
use serde_json::Value;
use std::collections::HashSet;

fn mk(
    id: &str,
    result: Observation,
    conf: Confidence,
    evidence: impl Into<String>,
) -> VerificationResult {
    VerificationResult {
        promise_id: id.to_string(),
        method: "behavioral".to_string(),
        confidence: conf,
        result,
        evidence: evidence.into(),
        timestamp: ts_now(),
    }
}

/// Collect the `file_path` of every `Read` tool event in the trace.
fn extract_reads(trace: &[Value]) -> HashSet<&str> {
    trace
        .iter()
        .filter(|ev| {
            ev.get("ev").and_then(|v| v.as_str()) == Some("tool")
                && ev.get("name").and_then(|v| v.as_str()) == Some("Read")
        })
        .filter_map(|ev| ev.get("file_path").and_then(|v| v.as_str()))
        .collect()
}

/// Suffix-aware path match: handles absolute-vs-relative variance between trace
/// paths and repo-relative `changed_files` from `git diff --name-only`.
fn path_covered_by(changed: &str, covered: &HashSet<&str>) -> bool {
    for &cov in covered {
        if cov == changed {
            return true;
        }
        if cov.ends_with(&format!("/{changed}")) {
            return true;
        }
        if changed.ends_with(&format!("/{cov}")) {
            return true;
        }
    }
    false
}

/// Union of `direct_dependents`, `transitive_dependents`, and `test_files` arrays
/// at the top level of the blast_radius object. Items may be strings or
/// `{file:…}` / `{path:…}` objects. Deduplicates.
fn extract_related_files(blast_radius: &Value) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for key in ["direct_dependents", "transitive_dependents", "test_files"] {
        let Some(arr) = blast_radius.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in arr {
            let path = if let Some(s) = item.as_str() {
                s.to_string()
            } else if let Some(s) = item.get("file").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(s) = item.get("path").and_then(|v| v.as_str()) {
                s.to_string()
            } else {
                continue;
            };
            if seen.insert(path.clone()) {
                files.push(path);
            }
        }
    }
    files
}

/// True if `name` is a cxpak context tool: strips `mcp__<server>__` prefix,
/// then checks for a `cxpak_` prefix.
fn is_cxpak_context_tool(name: &str) -> bool {
    let stripped = if let Some(rest) = name.strip_prefix("mcp__") {
        // Non-greedy strip: find the first `__` after the leading `mcp__`
        // and drop everything up to and including it, mirroring /^mcp__.*?__/.
        rest.find("__").map(|pos| &rest[pos + 2..]).unwrap_or(name)
    } else {
        name
    };
    stripped.starts_with("cxpak_")
}

/// Entry point — runs all three behavioral verifiers over the trace.
pub fn verify_behavioral(
    trace: &[Value],
    changed_files: &[String],
    blast_radius: Option<&Value>,
    docs_currency: Option<&DocsCurrency>,
) -> Vec<VerificationResult> {
    let mut out = vec![
        verify_read_before_write(trace, changed_files),
        verify_exploration_breadth(trace, blast_radius),
        verify_context_acquisition(trace),
    ];
    // The docs-currency check is host policy: it runs only when the host injects
    // its surface/doc file map. No map → the check is absent (not a silent Kept).
    if let Some(cfg) = docs_currency {
        out.push(verify_docs_currency(changed_files, cfg));
    }
    out
}

/// Coverage check: every file in `changed_files` must appear in at least one
/// Read event (suffix-aware). When a cxpak context tool was invoked but no
/// authoritative file list is available the result is `kept` at medium confidence
/// (M3 downgrade).
fn verify_read_before_write(trace: &[Value], changed_files: &[String]) -> VerificationResult {
    let id = "read-before-write";

    if changed_files.is_empty() {
        return mk(id, Observation::Kept, Confidence::High, "no files changed");
    }

    let reads = extract_reads(trace);

    let unread: Vec<&str> = changed_files
        .iter()
        .filter(|f| !path_covered_by(f, &reads))
        .map(String::as_str)
        .collect();

    if unread.is_empty() {
        // All modified files are covered by direct Reads (no cxpak file list here).
        let sources = if reads.is_empty() {
            "context".to_string()
        } else {
            format!("{} read(s)", reads.len())
        };
        return mk(
            id,
            Observation::Kept,
            Confidence::High,
            format!(
                "{} modified file(s), all covered by {}",
                changed_files.len(),
                sources
            ),
        );
    }

    // Coverage gap. If any cxpak context tool was called but no file list was
    // provided, accept at medium confidence (cxpak directive followed).
    let cxpak_called = trace.iter().any(|ev| {
        ev.get("ev").and_then(|v| v.as_str()) == Some("tool")
            && is_cxpak_context_tool(ev.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });

    if cxpak_called {
        return mk(
            id,
            Observation::Kept,
            Confidence::Medium,
            format!(
                "context acquired via cxpak (structured); file-level coverage of {} modified file(s) unverified — pass cxpakReadFiles to tighten",
                changed_files.len()
            ),
        );
    }

    // Broken: list uncovered files (at most 5, with a count of any remaining).
    let detail = unread[..unread.len().min(5)].join(", ");
    let suffix = if unread.len() > 5 {
        format!(" (+{} more)", unread.len() - 5)
    } else {
        String::new()
    };
    let evidence_prefix = if reads.is_empty() {
        format!("{} file(s) modified, 0 reads in trace", changed_files.len())
    } else {
        format!("{} file(s) modified without reading", unread.len())
    };
    mk(
        id,
        Observation::Broken,
        Confidence::High,
        format!("{}: {}{}", evidence_prefix, detail, suffix),
    )
}

/// Related files = union of `direct_dependents + transitive_dependents + test_files`
/// at the top level of `blast_radius`. Reads the explored subset and computes
/// ratio; >= 0.5 → kept (low confidence).
fn verify_exploration_breadth(trace: &[Value], blast_radius: Option<&Value>) -> VerificationResult {
    let id = "exploration-breadth";

    // Treat JSON null the same as absent (JS: `if (!blastRadiusResponse) return partial`).
    let br = match blast_radius.filter(|v| !v.is_null()) {
        None => {
            return mk(
                id,
                Observation::Partial,
                Confidence::Low,
                "cxpak_blast_radius unavailable",
            )
        }
        Some(v) => v,
    };

    let reads = extract_reads(trace);
    let related = extract_related_files(br);

    if related.is_empty() {
        return mk(
            id,
            Observation::Kept,
            Confidence::Low,
            "no architecturally-related files identified",
        );
    }

    let explored_count = related
        .iter()
        .filter(|dep| {
            for &read_path in &reads {
                if read_path == dep.as_str() {
                    return true;
                }
                if read_path.ends_with(&format!("/{dep}")) {
                    return true;
                }
                if dep.ends_with(&format!("/{read_path}")) {
                    return true;
                }
            }
            false
        })
        .count();

    let ratio = explored_count as f64 / related.len() as f64;
    // `round()` rounds half away from zero; for integer multiples of 1/N the
    // result matches half-to-even, which is all this ratio produces.
    let pct = (ratio * 100.0).round() as u64;

    let obs = if ratio >= 0.5 {
        Observation::Kept
    } else {
        Observation::Broken
    };
    mk(
        id,
        obs,
        Confidence::Low,
        format!(
            "{}/{} related files explored ({}%)",
            explored_count,
            related.len(),
            pct
        ),
    )
}

/// Detect a context-acquisition cxpak call in the trace via case-insensitive
/// substring match on the event name. Confidence: low (from registry).
///
/// Matches both surfaces across the cxpak 2.3.0→3.0.0 transition: the pre-3.0.0
/// tool names `cxpak_auto_context` / `cxpak_overview` (also the direct-call
/// deprecated aliases in 3.0.0), and the 3.0.0 intent-tool `cxpak_context` that
/// hosts both as `op` values (`context` from auto_context, `overview` from
/// overview — cxpak MIGRATION-3.0). The `cxpak_context` substring also catches
/// `cxpak_context_for_task` (still a context op) and `cxpak_context_diff` (a
/// review op under `cxpak_review`, a genuine but harmless false-positive):
/// acceptable slack for a low-confidence, observation-only telemetry promise.
fn verify_context_acquisition(trace: &[Value]) -> VerificationResult {
    let id = "context-acquisition";
    let acquired = trace.iter().any(|ev| {
        // JS: `t.tool || t.name || t.action || ''` — tool first, then name, then action.
        let name = ev
            .get("tool")
            .and_then(|v| v.as_str())
            .or_else(|| ev.get("name").and_then(|v| v.as_str()))
            .or_else(|| ev.get("action").and_then(|v| v.as_str()))
            .unwrap_or("");
        let lower = name.to_lowercase();
        lower.contains("cxpak_auto_context")
            || lower.contains("cxpak_overview")
            || lower.contains("cxpak_context")
    });
    mk(
        id,
        if acquired {
            Observation::Kept
        } else {
            Observation::Broken
        },
        Confidence::Low,
        if acquired {
            "cxpak context tool called"
        } else {
            "No cxpak_auto_context/cxpak_overview/cxpak_context call detected"
        },
    )
}

/// Injected policy for the `docs-currency` check. `surface_paths` are the
/// changed-file paths that count as a "documented public surface" (a change
/// there is expected to touch a doc); `doc_paths` are the canonical docs that
/// satisfy that expectation. The library ships **no** defaults — a host supplies
/// its own file map, encoding its own repository layout, or omits it (pass
/// `None` to `verify_behavioral`) to skip the check entirely.
///
/// Matching (see `path_matches`): an entry ending in `/` is a directory prefix
/// (matches any file under it); every other entry is a full repo-relative file
/// path matched exactly — so a `README.md` entry matches only the root README,
/// never `docs/adrs/README.md`.
pub struct DocsCurrency {
    pub surface_paths: Vec<String>,
    pub doc_paths: Vec<String>,
}

/// Match a changed-file path against a surface/doc entry precisely. An entry
/// ending in `/` is a directory prefix (matches files under it); every other
/// entry is a full repo-relative file path matched exactly. This is deliberately
/// NOT a substring test — `contains("README.md")` matched every README in the
/// repo (`docs/adrs/README.md`, `agents/README.md`, …), silently satisfying the
/// promise when the canonical root README was never touched.
fn path_matches(f: &str, entry: &str) -> bool {
    if entry.ends_with('/') {
        f.starts_with(entry)
    } else {
        f == entry
    }
}

/// Behavioral, observation-only telemetry: when a turn's `changed_files`
/// touches a documented public surface, a canonical doc should have been
/// touched too. Never blocks — `Broken` is Confidence::Low telemetry only,
/// same tier as `read-before-write`/`exploration-breadth`/`context-acquisition`.
/// An empty `changed_files` list fails open to `Kept` (mirroring
/// `verify_read_before_write`'s own empty-list branch) rather than treating
/// "unknown" as evidence of a broken promise.
fn verify_docs_currency(changed_files: &[String], cfg: &DocsCurrency) -> VerificationResult {
    let id = "docs-currency";

    if changed_files.is_empty() {
        return mk(id, Observation::Kept, Confidence::Low, "no files changed");
    }

    let surface_touched: Vec<&String> = changed_files
        .iter()
        .filter(|f| cfg.surface_paths.iter().any(|p| path_matches(f, p)))
        .collect();

    if surface_touched.is_empty() {
        return mk(
            id,
            Observation::Kept,
            Confidence::Low,
            "no documented-surface file changed",
        );
    }

    let doc_touched = changed_files
        .iter()
        .any(|f| cfg.doc_paths.iter().any(|p| path_matches(f, p)));

    if doc_touched {
        mk(
            id,
            Observation::Kept,
            Confidence::Low,
            format!(
                "{} surface file(s) changed alongside a canonical doc update",
                surface_touched.len()
            ),
        )
    } else {
        let detail = surface_touched
            .iter()
            .take(5)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        mk(
            id,
            Observation::Broken,
            Confidence::Low,
            format!(
                "{} surface file(s) changed with no canonical doc update: {}",
                surface_touched.len(),
                detail
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_before_write_medium_when_cxpak_context_called_but_file_uncovered() {
        // Changed file not covered by a Read, but a cxpak context tool is in the
        // trace → JS M3 downgrade: Kept at medium confidence, file-level coverage
        // unverified. Trigger condition: unread.len() > 0 && cxpak_called == true.
        let trace = vec![
            json!({ "ev": "tool", "name": "cxpak_auto_context", "file_path": null, "args_summary": "task=implement feature", "tokens": 200 }),
            json!({ "ev": "tool", "name": "Write", "file_path": "src/new.js", "args_summary": "Write src/new.js", "tokens": 80 }),
        ];
        let changed = vec!["src/new.js".to_string()];
        let results = verify_behavioral(&trace, &changed, None, None);
        let rbw = results
            .iter()
            .find(|r| r.promise_id == "read-before-write")
            .unwrap();
        assert_eq!(rbw.result, Observation::Kept);
        assert_eq!(rbw.confidence, Confidence::Medium);
        assert_eq!(
            rbw.evidence,
            "context acquired via cxpak (structured); file-level coverage of 1 modified file(s) unverified — pass cxpakReadFiles to tighten"
        );
    }

    #[test]
    fn is_cxpak_context_tool_recognizes_prefix_not_in_old_allowlist() {
        // O9: the `CXPAK_CONTEXT_TOOLS` allowlist was unreachable dead code — every
        // entry starts with `cxpak_`, so the `starts_with` branch always short-circuits
        // first. Prove the real logic is the prefix rule, not the (removed) allowlist,
        // by using a `cxpak_`-prefixed name that was never in the old 15-element list.
        assert!(super::is_cxpak_context_tool(
            "cxpak_totally_novel_tool_name"
        ));
    }

    #[test]
    fn context_acquisition_kept_for_cxpak_3_0_intent_tool() {
        // cxpak 3.0.0: the agent sees only the 5 intent-tools in tools/list, so
        // it calls `cxpak_context` (op context/overview) rather than the old
        // `cxpak_auto_context`. context-acquisition must still register Kept.
        let trace = vec![
            json!({ "ev": "tool", "name": "cxpak_context", "args_summary": "op=context task=x" }),
        ];
        let results = verify_behavioral(&trace, &[], None, None);
        let ca = results
            .iter()
            .find(|r| r.promise_id == "context-acquisition")
            .unwrap();
        assert_eq!(ca.result, Observation::Kept);
    }

    #[test]
    fn exploration_breadth_broken_when_ratio_below_threshold() {
        // 0 reads, 3 related files → ratio 0.0 < 0.5 → Broken.
        let trace: Vec<Value> = vec![];
        let blast = json!({ "direct_dependents": ["a.js", "b.js", "c.js"], "transitive_dependents": [], "test_files": [] });
        let results = verify_behavioral(&trace, &[], Some(&blast), None);
        let eb = results
            .iter()
            .find(|r| r.promise_id == "exploration-breadth")
            .unwrap();
        assert_eq!(eb.result, Observation::Broken);
        assert_eq!(eb.confidence, Confidence::Low);
        assert_eq!(eb.evidence, "0/3 related files explored (0%)");
    }

    /// A representative host file map: a CLI directory prefix and an entry-point
    /// file are surfaces; only the root README is canonical.
    fn docs_cfg() -> DocsCurrency {
        DocsCurrency {
            surface_paths: vec!["src/cli/".to_string(), "src/main.rs".to_string()],
            doc_paths: vec!["README.md".to_string()],
        }
    }

    #[test]
    fn docs_currency_kept_when_no_files_changed() {
        let cfg = docs_cfg();
        let results = verify_behavioral(&[], &[], None, Some(&cfg));
        let dc = results
            .iter()
            .find(|r| r.promise_id == "docs-currency")
            .unwrap();
        assert_eq!(dc.result, Observation::Kept);
        assert_eq!(dc.confidence, Confidence::Low);
    }

    #[test]
    fn docs_currency_absent_when_no_config_injected() {
        // No host file map → the check does not run at all (not a silent Kept).
        let results = verify_behavioral(&[], &["src/main.rs".to_string()], None, None);
        assert!(results.iter().all(|r| r.promise_id != "docs-currency"));
    }

    #[test]
    fn docs_currency_kept_when_surface_and_doc_both_touched() {
        let cfg = docs_cfg();
        let changed = vec!["src/main.rs".to_string(), "README.md".to_string()];
        let results = verify_behavioral(&[], &changed, None, Some(&cfg));
        let dc = results
            .iter()
            .find(|r| r.promise_id == "docs-currency")
            .unwrap();
        assert_eq!(dc.result, Observation::Kept);
    }

    #[test]
    fn docs_currency_broken_when_surface_touched_without_doc() {
        let cfg = docs_cfg();
        let changed = vec!["src/cli/run.rs".to_string()];
        let results = verify_behavioral(&[], &changed, None, Some(&cfg));
        let dc = results
            .iter()
            .find(|r| r.promise_id == "docs-currency")
            .unwrap();
        assert_eq!(dc.result, Observation::Broken);
        assert_eq!(dc.confidence, Confidence::Low);
    }

    #[test]
    fn docs_currency_broken_when_only_a_non_canonical_readme_touched() {
        // A surface change plus an UNRELATED README (docs/adrs/README.md) must not
        // satisfy the promise — only the canonical root README.md counts. Guards the
        // substring→exact-path fix (contains("README.md") matched any README).
        let cfg = docs_cfg();
        let changed = vec![
            "src/cli/run.rs".to_string(),
            "docs/adrs/README.md".to_string(),
        ];
        let results = verify_behavioral(&[], &changed, None, Some(&cfg));
        let dc = results
            .iter()
            .find(|r| r.promise_id == "docs-currency")
            .unwrap();
        assert_eq!(dc.result, Observation::Broken);
    }
}
