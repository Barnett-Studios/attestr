//! `attestr verify` — attestr's self-contained behavioral verifier as a one-shot CLI (ADR-0054).
//!
//! Reads a JSON request on stdin, runs the behavioral verifiers (read-before-write,
//! exploration-breadth, context-acquisition, and — when a docs map is supplied —
//! docs-currency), and writes an ADR-0052 response envelope on stdout. Fully
//! self-contained: no network, no cxpak, no filesystem — safe under `docker run
//! --network none`.
//!
//! Out of scope for this one-shot CLI, by nature (documented, not a gap): the *structural* pillar
//! is cxpak-backed (`verify::structural::verify_all` needs a live `CxpakClient`), so it cannot run
//! under `--network none`; and the *standing-promise* pillar needs the resolved promise registry
//! (a host concern). Both stay in-process / host-wired — a harness wanting the full three-pillar
//! verify links the crate. This image gives any harness the behavioral pillar via `docker run`.

use std::io::Read;

use attestr::verify::behavioral::{verify_behavioral, DocsCurrency};
use serde::Deserialize;
use serde_json::{json, Value};

const USAGE: &str = "usage: attestr verify\n  reads a JSON verify request on stdin, writes an \
     ADR-0052 response envelope on stdout.\n  request: {trace, changed_files, blast_radius?, \
     docs_currency?}";

#[derive(Deserialize)]
struct DocsCurrencyInput {
    #[serde(default)]
    surface_paths: Vec<String>,
    #[serde(default)]
    doc_paths: Vec<String>,
}

/// The verify request read from stdin. Every field is optional so a caller can supply only
/// what it has; the verifiers fail open on absent inputs.
#[derive(Deserialize)]
struct VerifyRequest {
    /// The turn's tool-call trace (opaque JSON events; the verifiers read Read/tool events).
    #[serde(default)]
    trace: Vec<Value>,
    #[serde(default)]
    changed_files: Vec<String>,
    /// Optional cxpak blast-radius object, if the caller pre-fetched one.
    #[serde(default)]
    blast_radius: Option<Value>,
    /// Optional host docs-currency policy (surface files → required doc files).
    #[serde(default)]
    docs_currency: Option<DocsCurrencyInput>,
}

/// Run the behavioral verifiers over a request and return an ADR-0052 `ok` envelope.
fn run_verify(input: &str) -> Result<String, String> {
    let req: VerifyRequest =
        serde_json::from_str(input).map_err(|e| format!("invalid verify request JSON: {e}"))?;
    let docs = req.docs_currency.map(|d| DocsCurrency {
        surface_paths: d.surface_paths,
        doc_paths: d.doc_paths,
    });
    let findings = verify_behavioral(
        &req.trace,
        &req.changed_files,
        req.blast_radius.as_ref(),
        docs.as_ref(),
    );
    let envelope = json!({
        "schema_version": "1",
        "status": "ok",
        "body": { "findings": findings },
    });
    serde_json::to_string(&envelope).map_err(|e| format!("serialize failure: {e}"))
}

/// An `error`-status envelope — the ADR-0052 sentinel. A consumer treats `status != "ok"` as an
/// infrastructure failure and falls back to its in-process path rather than trusting a result.
fn error_envelope(message: &str) -> String {
    json!({
        "schema_version": "1",
        "status": "error",
        "body": { "message": message },
    })
    .to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("verify") => {
            let mut input = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut input) {
                println!("{}", error_envelope(&format!("failed to read stdin: {e}")));
                std::process::exit(1);
            }
            match run_verify(&input) {
                Ok(out) => println!("{out}"),
                Err(e) => {
                    // Report the failure as a status=error envelope AND a non-zero exit, so the
                    // consumer's fallback fires (fail-open) instead of trusting an empty result.
                    println!("{}", error_envelope(&e));
                    std::process::exit(1);
                }
            }
        }
        // A conventional, exit-0 help so `attestr --help` (e.g. the Homebrew formula's smoke test)
        // succeeds; a missing/unknown subcommand is a misuse → usage on stderr, exit 2.
        Some("--help") | Some("-h") | Some("help") => println!("{USAGE}"),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_emits_ok_envelope_with_behavioral_findings() {
        // A file changed with an empty trace: the behavioral pillar must run and the
        // read-before-write verifier must appear, all wrapped in an `ok` envelope.
        let input = r#"{"trace":[],"changed_files":["src/foo.rs"]}"#;
        let out = run_verify(input).expect("verify should succeed on valid input");
        let v: Value = serde_json::from_str(&out).expect("output is JSON");
        assert_eq!(v["schema_version"], "1");
        assert_eq!(v["status"], "ok");
        let findings = v["body"]["findings"].as_array().expect("findings array");
        assert!(
            findings
                .iter()
                .any(|f| f["promise_id"] == "read-before-write"),
            "expected a read-before-write finding, got: {out}"
        );
    }

    #[test]
    fn invalid_json_is_a_hard_error_not_a_false_clean_pass() {
        // The sentinel path: bad input must be a hard error (→ status=error envelope + non-zero
        // exit in main), never a silently-empty ok result.
        let err = run_verify("not json").expect_err("invalid JSON must error");
        assert!(err.contains("invalid verify request"), "got: {err}");
    }
}
