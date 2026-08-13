//! A consumer using attestr's public surface WITHOUT adding baseplate (attestr#22).
//!
//! Every path below goes through `attestr::`. That is the whole assertion: a `use` that does
//! not resolve is a compile error, so this file failing to build IS the failure — a consumer
//! who cannot name the type attestr just returned them.
//!
//! ## Why this is a test file and not a doc comment
//!
//! The claim "the public surface is self-contained" is only checkable by compiling something
//! that depends on it being true. Before this, `reviewer::parse_decision` was public,
//! documented in `CONTRACT.md` §Surface, and returned a `DecisionCore` whose `action` field
//! no consumer could name — and everything in the crate still built, because inside the crate
//! `baseplate` is right there.
//!
//! ## What it does not prove
//!
//! `baseplate` is a normal dependency, so it is nameable from this test crate too. Nothing
//! stops a future edit here from reaching for it directly and papering over a missing
//! re-export. The guard is that these paths are all `attestr::`; a reviewer reading a `use
//! baseplate::` line in this file should treat it as the defect, not the fix.

// The one import a consumer needs. `attestr::model` is the module re-export — the completeness
// mechanism — so this line covers types added to baseplate later without an edit here.
use attestr::model;

/// The types on the reviewer path, by the two routes a consumer would actually try:
/// `attestr::model::X` and `attestr::reviewer::X` (the latter being what rustc's E0603 hint
/// pointed at, back when it pointed at nothing that compiled).
#[test]
fn a_consumer_can_name_what_parse_decision_returns() {
    let d = attestr::reviewer::parse_decision("no decision block here at all");

    // Matching on the action is the minimum useful thing to do with a decision, and it is
    // impossible without naming the enum.
    let acted = match d.action {
        attestr::reviewer::ReviewAction::Accept => "accept",
        attestr::reviewer::ReviewAction::Retry => "retry",
    };
    assert_eq!(
        acted, "accept",
        "no valid candidate must still fail open to accept"
    );

    // …and the parser field, via the other path, to prove both resolve to the same type.
    let p: model::ReviewParser = d.parser;
    assert_eq!(p, attestr::reviewer::ReviewParser::Failed);
}

/// The verification types, which cross three modules of the public surface.
#[test]
fn a_consumer_can_construct_and_read_a_verification_result() {
    let r = model::VerificationResult {
        promise_id: "complete-output".to_string(),
        // `method` is a String on the wire, not the `Method` enum — `Method` is what the
        // registry parses into, and the two are deliberately not the same field. Naming both
        // here is the point: a consumer needs the enum for the registry side regardless.
        method: "grep".to_string(),
        result: model::Observation::Kept,
        confidence: model::Confidence::High,
        evidence: "no markers".to_string(),
        timestamp: "2026-08-13T00:00:00Z".to_string(),
    };

    // `compute_run_observation` is public and takes these, so a consumer must be able to
    // build one to call it at all.
    let obs = attestr::trust::compute_run_observation(std::slice::from_ref(&r))
        .expect("a single kept result is a valid run");
    assert_eq!(obs, Some(1.0));

    // The convenience aliases next to the verifiers resolve to the same types.
    let _: attestr::verify::Observation = r.result;
    let _: attestr::verify::Confidence = r.confidence;
    let _: attestr::verify::Method = attestr::verify::Method::Grep;
}

/// Every fallible `TrustStore` signature, bound to its error type by hand.
///
/// This is a compile-time assertion, not a behavioural one: naming
/// `Result<_, attestr::trust::TrustError>` on each `fn` pointer fails to build the moment a
/// signature goes back to leaking `rusqlite::Result`, which is the whole of attestr#7. A
/// consumer must be able to handle a store failure without depending on the crate attestr
/// happens to store trust in — and without taking a breaking change when that crate bumps.
#[test]
fn no_trust_store_method_makes_a_consumer_name_the_storage_crate() {
    use attestr::trust::{TrustError, TrustStore};
    use std::path::Path;

    let _: fn(&Path) -> Result<TrustStore, TrustError> = TrustStore::open;
    let _: fn(&TrustStore, &str) -> Result<Option<f64>, TrustError> = TrustStore::get;
    // Aliased only because clippy calls the inline form too complex; the point is the
    // `TrustError` in the return position, not the arity.
    type UpdateAtomic =
        fn(&mut TrustStore, &str, Option<f64>, f64, &str) -> Result<(f64, f64), TrustError>;
    let _: UpdateAtomic = TrustStore::update_atomic;
    let _: fn(&mut TrustStore, &str, f64, &str) -> Result<f64, TrustError> = TrustStore::set;

    // And the error is usable as one: a consumer logging it needs Display, and a consumer
    // wrapping it in its own error type needs the Error bound.
    let e = TrustError::UnsupportedSchema {
        found: 2,
        supported: attestr::trust::SCHEMA_VERSION,
    };
    let _: &dyn std::error::Error = &e;
    assert!(e.to_string().contains("newer than this build"));
}

/// The field types of a re-exported struct must be nameable too, or the re-export is a
/// half-measure: a consumer holding a `PromiseSpec` and matching on `promise_type` is stopped
/// by a type nobody thought to list. This is the case a hand-written name list would miss,
/// and it is why `attestr::model` re-exports the module rather than a set of names.
#[test]
fn the_field_types_of_a_re_exported_struct_are_reachable() {
    let _: fn(&model::PromiseSpec) -> &model::PromiseType = |s| &s.promise_type;
    let _: fn(&model::PromiseSpec) -> &Option<model::Requires> = |s| &s.requires;
    let _: fn(&model::ReviewDecision) -> &model::ReviewAction = |d| &d.action;
    let _: fn(&model::MethodOutcome) -> &model::Observation = |m| &m.result;
}
