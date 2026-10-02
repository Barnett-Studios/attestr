//! The README's `## Use` block is included here so `cargo test --doc` compiles it
//! (attestr#38 — the block drifted from the API for two releases because nothing did).
#![doc = include_str!("../README.md")]

pub mod reviewer;
pub mod trust;
pub mod verify;

/// The shared value types this crate's public surface is written in — re-exported from
/// [`baseplate`](https://crates.io/crates/baseplate) so a consumer does not need a second,
/// undeclared dependency to use attestr's own return values (attestr#22).
///
/// ## Why the MODULE and not a list of names
///
/// A name list would be a second enumeration of baseplate's public model, kept in step by
/// hand, and it would be wrong in exactly the invisible way: `PromiseSpec` is re-exported,
/// a consumer matches on `spec.promise_type`, and `PromiseType` — a field type nobody
/// listed — is still unreachable. Re-exporting the module covers every item baseplate makes
/// public today and every one it adds later, with nothing to drift.
///
/// ## Why re-export at all
///
/// The same argument `CONTRACT.md` already makes for the cascadr re-exports, applied to the
/// dependency it was not applied to. A consumer who runs `cargo add baseplate` picks their
/// own version requirement; the moment it is semver-incompatible with attestr's, cargo links
/// both into one graph and the two `ReviewDecision` types stop being the same type — no
/// compile error, just a consumer wired to the wrong one. That is attestr#13, which already
/// happened once with cascadr. Going through this path means attestr's `Cargo.toml` decides
/// the version, which is the point.
///
/// **So the baseplate pin is part of this contract too.** A baseplate bump that changes a
/// type reachable from here is a breaking change to attestr and takes attestr's own minor
/// slot under 0.x. `cargo tree -d` showing a duplicated baseplate is the symptom.
pub use baseplate::model;

/// The structural pillar's own dependency: [`CxpakClient`](baseplate::cxpak::CxpakClient) and
/// the capability traits it implements, re-exported for the same reason [`model`] is (attestr#35).
///
/// `verify::structural::verify_all`'s first parameter is `&dyn CxpakClient`, a trait that lives
/// in `baseplate::cxpak`. Before this re-export, `model` covered every *value* type a consumer
/// needs but not this one — the only trait on the structural pillar's surface — so §Surface's
/// `verify_all` row named a parameter type no consumer could spell without a second, undeclared
/// `baseplate` dependency (the thing `model`'s own re-export exists to make unnecessary).
pub use baseplate::cxpak;
