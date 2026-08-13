# attestr — contract

attestr is the **Verifier** component: it observes a turn's output and returns telemetry. Its
socket is `verify(diff, promises) → {findings, trust_delta}` plus a gated reviewer decision.
Anything conforming to this contract can drop into the Verifier slot.

## Guarantees

1. **Post-hoc, never in-loop.** An assessment is produced *after* a turn and is never returned
   to the agent that produced the turn. Callers treat findings and trust as telemetry — feeding
   an assessment back into the live loop is outside this contract (it measured harmful).
2. **Fail-open.** A verifier that cannot run (cxpak absent, registry unreadable, db locked)
   yields *no finding* and a *no-op trust delta* — never a fabricated finding or a false block.
   The absence of attestr degrades observability, never correctness.
3. **Deterministic-first, reviewer-gated.** Grep (`verify::standing`) and structural
   (`verify::structural`, cxpak-backed) checks run first and are pure functions of their input.
   The `claude -p` reviewer is dispatched **only** on a finding that is broken *with high
   confidence* — never speculatively, never as a clean-prompt resample.
4. **Reviewed content is framed as data.** The material the reviewer assesses is
   produced by the agent whose trustworthiness is in question, so `build_prompt` frames it
   between `reviewer::UNTRUSTED_OPEN` / `UNTRUSTED_CLOSE` and tells the reviewer that a
   directive found inside the frame is evidence, not an order. The frame is structural, not
   decorative: the framed region is a `serde_json` document, and JSON escapes control
   characters inside string values, so no agent-authored text can begin a line — and
   therefore none can forge a marker or close the fence. Replacing the serializer with raw
   interpolation breaks that guarantee, and the tests say so.

   **The reply is parsed under the same assumption.** Framing governs how the reviewer is
   *told* to read the payload. It says nothing about how the reviewer's reply is read, and
   that was the load-bearing half: `parse_decision` took the **first** decision block, models
   routinely quote their input, and the agent under review writes that input — so a planted
   decision object, echoed back by a reviewer doing its job perfectly, outranked the real
   verdict with no LLM cooperation at all (attestr#10).

   The reviewer now marks its verdict with a per-dispatch tag (`reviewer::new_decision_tag`,
   asked for by `build_prompt_with_tag`, required by `parse_decision_with_tag`). The payload
   was authored before that value existed, so nothing inside it can wear the tag, whichever
   side of the verdict it is quoted on. Untagged replies fall back to the **last** decision
   block — the prompt ends "emit your decision now", so the verdict is contractually last.

   Unchanged by this: the fail-open posture. No valid candidate still yields `accept` with
   `ReviewParser::Failed`. What changed is *which* block is believed, not what happens when
   none is.
5. **Trust is monotone in evidence, not caller-set.** Per-agent trust is an exponential moving
   average (`trust::apply_ema`) over run observations. Callers read a tier (`trust::trust_tier`)
   and record observations; they do not hand-set trust except through the recorded path.

## Surface

| Item | Shape |
|---|---|
| `verify::structural::verify_all(files, ctx) -> Result<Vec<Finding>>` | async; cxpak-backed structural findings for a turn's changed files. |
| `verify::standing::verify(promise, diff) -> Vec<Finding>` | registry-driven grep/pattern assessment of a standing promise. |
| `trust::TrustStore::open(path)` / `.get(agent)` / `.set(agent, t, now)` / `.update_atomic(..)` | the durable per-agent trust store; `update_atomic` folds an observation under a transaction. Every method returns `Result<_, trust::TrustError>`. |
| `trust::TrustError`, `trust::SCHEMA_VERSION` | the store's own error type and on-disk schema version. |
| `trust::compute_run_observation`, `apply_ema`, `trust_tier`, `Tier` | the EMA machinery: run results → observation → updated trust → tier. |
| `reviewer::Reviewer::with_skills(dispatch, skills).review(req) -> ReviewDecision` | async; dispatches an informed reviewer via a `cascadr` `Provider` and returns a structured `{action, feedback}`. |
| `reviewer::ReviewRequest`, `parse_decision`, `pick_reviewer_skill`, `build_prompt` | reviewer inputs, prompt assembly, and the parser that turns reviewer output into a `DecisionCore`. |
| `reviewer::UNTRUSTED_OPEN`, `reviewer::UNTRUSTED_CLOSE` | the markers framing untrusted reviewed content in the prompt — `pub` so a consumer can assert on the framing rather than trust it. |
| `reviewer::new_decision_tag`, `build_prompt_with_tag`, `parse_decision_with_tag` | the per-dispatch decision tag: one mechanism in two halves, additive to the untagged pair above. A consumer driving its own prompt/parse loop should use these; asking for a tag it does not parse (or parsing one it never asked for) is inert. Ignoring the tag entirely leaves you on the last-block fallback — the echo-*before*-verdict attack is still closed, the mirror image after it is not. `Reviewer::review` uses the tagged pair. |

| `model` (`attestr::model`) | the whole of `baseplate::model`, re-exported — the value types every signature above is written in. |
| `reviewer::{ReviewAction, ReviewDecision, ReviewParser}` · `verify::{Confidence, Method, MethodOutcome, Observation, PromiseSpec, VerificationResult}` | the same types, aliased next to the functions that use them. Convenience; `attestr::model` is what makes the surface complete. |

### The trust store owns its error type and its schema

`TrustStore` is backed by SQLite, and that is an implementation detail rather than part of the
contract. Its methods returned `rusqlite::Result` until attestr#7, which made every consumer
depend on `rusqlite` to handle a store failure and turned that crate's next major bump into a
breaking change to *this* surface — for a dependency the consumer never chose. They now return
`Result<_, trust::TrustError>`: `EmptyResults`, `Storage { retryable, message }`, or
`UnsupportedSchema`. Branch on `retryable`, never on `message`. `retryable` means contention
(`SQLITE_BUSY`/`SQLITE_LOCKED`) that the store has **already** retried to exhaustion — "under
sustained load, come back later", not "retry immediately".

The file carries `PRAGMA user_version`, and `open()` migrates forward from whatever it finds.
A file predating the stamp reads as version 0, which is indistinguishable from a new one and
does not need to be distinguished: v1's schema is what those files already hold, so the same
`CREATE TABLE IF NOT EXISTS` covers both and the stamp records it — existing trust history is
adopted, not discarded. A file from a **newer** attestr is refused with `UnsupportedSchema`
rather than read: this is the half that cannot be retrofitted, because an old binary that
ignores the version happily reads a schema it does not know and returns a plausible wrong
number. Bump `SCHEMA_VERSION` only together with a migration arm in `open()`.

The value types above come from [`baseplate`](https://crates.io/crates/baseplate) (`model`), so
they cross the Verifier boundary as stable serialized types.

**A consumer never needs to depend on baseplate to use this crate.** That was not true until
attestr#22: `parse_decision` was public, listed in this table, and returned a `DecisionCore`
whose `action` and `parser` a consumer could not name — the only fix was `cargo add baseplate`
and choosing a version by hand, which is precisely the two-graph hazard the cascadr paragraph
below documents as having already happened once.

The re-export is of the **module**, deliberately, not a list of names. A list is a second
enumeration of baseplate's model kept in step by hand, and it fails invisibly: `PromiseSpec`
is exported, a consumer matches on `spec.promise_type`, and `PromiseType` — a field type
nobody listed — is still unreachable. `tests/consumer_surface.rs` is written entirely in
`attestr::` paths and does not compile if any of this stops being true.

## Dependencies (publish order)

attestr depends by version on [`cascadr`](https://crates.io/crates/cascadr) (the reviewer's
dispatch provider) and [`baseplate`](https://crates.io/crates/baseplate) (shared types +
registry). Both must be on crates.io **before** attestr publishes.

**Both pins are part of this contract, for the same reason** — attestr re-exports types from
each, so a consumer receives whatever version *this* crate's `Cargo.toml` resolves. The cascadr
case is written out below because it is the one that broke; the baseplate case is identical in
structure and currently latent: only 0.2.0 and 0.2.1 are published and attestr requires
`"0.2"`, so a consumer's own `cargo add baseplate` lands inside attestr's range. It goes live
the day baseplate 0.3.0 ships. A baseplate bump that changes a re-exported type is a breaking
change to attestr, takes attestr's own minor slot under 0.x, and `cargo tree -d` showing a
duplicated baseplate is the mechanical symptom. The source workspace
redirects those version requirements to the local members via `[patch.crates-io]`; that patch is
not part of this crate and does not travel to crates.io.

**The cascadr re-export is part of this contract, so the pin is too.** `reviewer.rs` re-exports
`ClaudeCliDispatch`, `OpenAiCompat`, `Provider`, `ProviderError`, `Router` and `filter_child_env`
under `attestr::reviewer::*`, which means a consumer constructing a dispatch through attestr
receives whatever cascadr version *this* pin resolves — attestr's `Cargo.toml` decides it, not the
consumer's.

That is not a detail. Holding `cascadr = "0.1"` while cascadr shipped 0.2.0 handed downstreams the
three-field `ClaudeCliDispatch` that always passes `--dangerously-skip-permissions`, silently, while
attestr's own release notes described a security fix (attestr#13). A `"0.1"` and a `"0.2"` requirement
are semver-incompatible, so cargo links **both** into one graph and the two `ClaudeCliDispatch` types
stop being the same type — no compile error, just a consumer wired to the old one.

So: a cascadr version bump that changes a re-exported type is a **breaking change to attestr**, takes
attestr's own minor slot under 0.x, and lands in lockstep rather than whenever. `cargo tree -d` showing
a duplicated cascadr is the mechanical symptom to watch for.

## What attestr does not do

- It does not gate a commit or block a turn — that is a policy gate's job (e.g. [commitward](https://github.com/Barnett-Studios/commitward)).
- It does not execute the agent loop or apply edits — that is the host loop runner's job.
- It does not decide budget or admission — that is a cost/admission governor's job.
