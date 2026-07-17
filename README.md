# attestr

[![CI](https://github.com/Barnett-Studios/attestr/actions/workflows/ci.yml/badge.svg)](https://github.com/Barnett-Studios/attestr/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/attestr)](https://crates.io/crates/attestr)
[![docs.rs](https://img.shields.io/docsrs/attestr)](https://docs.rs/attestr)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**Promise-Theory verification for an agentic coding loop — assess a turn's output against the
promises its agent declared, emit findings and a per-agent trust delta, and (only on a
high-confidence broken finding) dispatch an informed reviewer.**

attestr is the **observer**. An agent voluntarily declares promises; attestr assesses the
turn's actual output *independently and after the fact* — grep-and-structural checks first,
then, when a finding is broken with high confidence, a `claude -p` reviewer that returns a
structured `{action, feedback}` decision. Per-agent trust evolves as an EMA over these
assessments. The agent never sees its own assessment: this is **telemetry, not a control
signal injected into the live loop** (injecting it measured harmful).

> Part of the Barnett Studios agentic-harness toolkit → cxpak · commitward · abproof · cascadr ·
> cordon · slicr · **attestr**

## The three parts

| Module | Responsibility |
|---|---|
| `verify` | Assess a turn's diff against declared promises. `structural` (cxpak-backed) and `standing` (registry-driven grep) checks produce findings; `behavioral` covers per-turn promise blocks. |
| `trust` | The per-agent trust store (SQLite): each assessment nudges an exponential moving average; callers read a trust tier, they don't write it. |
| `reviewer` | On a high-confidence broken finding, dispatch an informed reviewer via a [`cascadr`](https://crates.io/crates/cascadr) provider and return a structured `{action, feedback}` decision — never a clean-prompt resample (that measured as a regression). |

## Use

```toml
[dependencies]
attestr = "0.2"
```

```rust
use attestr::{trust, verify};

// Structural (cxpak-backed) assessment of a turn's changed files → findings.
let findings = verify::structural::verify_all(&changed_files, &ctx).await?;

// Fold the run's results into the per-agent trust EMA, then read a tier.
let store = trust::TrustStore::open(&db_path)?;
let tier = trust::trust_tier(store.get(agent_id)?.unwrap_or(0.5));
```

## Constitution

attestr honours the toolkit's invariants: **fail-open** (a verifier that can't run yields no
finding, never a false block), **no live-loop feedback** (assessment is post-hoc telemetry),
and **deterministic-first** (grep + structural checks gate the expensive reviewer, which runs
only on high-confidence broken findings).

## Publishing / building

attestr depends on two sibling components — [`cascadr`](https://crates.io/crates/cascadr) and
[`baseplate`](https://crates.io/crates/baseplate) — resolved by version. They must be published
to crates.io **before** attestr (that is the family's publish order). In the source workspace
they resolve to the local members via a `[patch.crates-io]` redirect.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
Unless you explicitly state otherwise, any contribution you intentionally submit for
inclusion in the work shall be dual-licensed as above, without any additional terms.

---

Built by [Barnett Studios](https://barnett-studios.com/) — part of the agentic-harness
toolkit: [cxpak](https://github.com/Barnett-Studios/cxpak) ·
[commitward](https://github.com/Barnett-Studios/commitward) ·
[cascadr](https://github.com/Barnett-Studios/cascadr) ·
[abproof](https://github.com/Barnett-Studios/abproof) ·
[cordon](https://github.com/Barnett-Studios/cordon) ·
[slicr](https://github.com/Barnett-Studios/slicr) · **attestr**.
