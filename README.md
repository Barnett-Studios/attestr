# attestr

[![CI](https://github.com/Barnett-Studios/attestr/actions/workflows/ci.yml/badge.svg)](https://github.com/Barnett-Studios/attestr/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/attestr)](https://crates.io/crates/attestr)
[![docs.rs](https://img.shields.io/docsrs/attestr)](https://docs.rs/attestr)
[![ghcr.io](https://img.shields.io/badge/ghcr.io-attestr-blue?logo=docker)](https://github.com/Barnett-Studios/attestr/pkgs/container/attestr)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**Integrity plane · Active** — under development; the surface still moves.
See the [component map](https://github.com/Barnett-Studios) for how this fits the rest.

**Promise-Theory verification for an agentic coding loop — assess a turn's output against the
promises its agent declared, emit findings and a per-agent trust delta, and (only on a
high-confidence broken finding) dispatch an informed reviewer.**

attestr is the **observer**. An agent voluntarily declares promises; attestr assesses the
turn's actual output *independently and after the fact* — grep-and-structural checks first,
then, when a finding is broken with high confidence, a `claude -p` reviewer that returns a
structured `{action, feedback}` decision. Per-agent trust evolves as an EMA over these
assessments. The agent never sees its own assessment: this is **telemetry, not a control
signal injected into the live loop**. Feeding an assessment back mid-turn is held to degrade
outcomes — a **design position, not a measured result**: coupling per-turn telemetry to control
flow coincides the measurement channel with the thing being measured, and makes the observer's
own errors load-bearing.

> Part of the Barnett Studios agentic-harness toolkit → cxpak · commitward · abproof · cascadr ·
> cordon · slicr · **attestr**

## The three parts

| Module | Responsibility |
|---|---|
| `verify` | Assess a turn's diff against declared promises. `structural` (cxpak-backed) and `standing` (registry-driven grep) checks produce findings; `behavioral` covers per-turn promise blocks. |
| `trust` | The per-agent trust store (SQLite): each assessment nudges an exponential moving average; callers read a trust tier, they don't write it. |
| `reviewer` | On a high-confidence broken finding, dispatch an informed reviewer via a [`cascadr`](https://crates.io/crates/cascadr) provider and return a structured `{action, feedback}` decision rather than a clean-prompt resample. Handing the next attempt a specific signal is a different mechanism from resampling the same prompt — a **design hypothesis**; its advantage over resampling is **unmeasured**. |

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

## `attestr verify` — the behavioral pillar as a one-shot container

Any harness — not just a Rust one — can run attestr's **behavioral** verifiers without linking
the crate, via a self-contained CLI shipped as a container image. It reads a JSON request on
stdin and writes an [ADR-0052](https://github.com/Barnett-Studios/attestr) response envelope on
stdout. It touches no network and no filesystem, so it runs fully sandboxed:

```console
$ echo '{"trace":[],"changed_files":["src/foo.rs"]}' \
    | docker run --rm -i --network none ghcr.io/barnett-studios/attestr verify
{"schema_version":"1","status":"ok","body":{"findings":[{"promise_id":"read-before-write",...}]}}
```

The request is `{trace, changed_files, blast_radius?, docs_currency?}` (every field optional; the
verifiers fail open on absent inputs). The envelope is `{schema_version, status, body}` — a
consumer treats any `status != "ok"` as an infrastructure failure and falls back to its
in-process path rather than trusting the result. Bad input is a hard error (`status: "error"`,
non-zero exit), never a silent clean pass.

**In scope for the container: the behavioral pillar only.** The *structural* pillar is
cxpak-backed (needs a live client) and the *standing-promise* pillar needs the host's resolved
registry — both stay in-process. A harness wanting the full three-pillar verify links the crate
(above); this image gives any harness the behavioral pillar via one `docker run`.

The same binary is on the [Homebrew tap](https://github.com/Barnett-Studios/homebrew-tap)
(`brew install barnett-studios/tap/attestr`) and attached to each GitHub Release. Building it
from source needs the `cli` feature: `cargo build --release --features cli`.

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
