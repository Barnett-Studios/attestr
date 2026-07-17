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
4. **Trust is monotone in evidence, not caller-set.** Per-agent trust is an exponential moving
   average (`trust::apply_ema`) over run observations. Callers read a tier (`trust::trust_tier`)
   and record observations; they do not hand-set trust except through the recorded path.

## Surface

| Item | Shape |
|---|---|
| `verify::structural::verify_all(files, ctx) -> Result<Vec<Finding>>` | async; cxpak-backed structural findings for a turn's changed files. |
| `verify::standing::verify(promise, diff) -> Vec<Finding>` | registry-driven grep/pattern assessment of a standing promise. |
| `trust::TrustStore::open(path)` / `.get(agent)` / `.set(agent, t, now)` / `.update_atomic(..)` | the SQLite-backed per-agent trust store; `update_atomic` folds an observation under a transaction. |
| `trust::compute_run_observation`, `apply_ema`, `trust_tier`, `Tier` | the EMA machinery: run results → observation → updated trust → tier. |
| `reviewer::Reviewer::with_skills(dispatch, skills).review(req) -> ReviewDecision` | async; dispatches an informed reviewer via a `cascadr` `Provider` and returns a structured `{action, feedback}`. |
| `reviewer::ReviewRequest`, `parse_decision`, `pick_reviewer_skill`, `build_prompt` | reviewer inputs, prompt assembly, and the parser that turns reviewer output into a `DecisionCore`. |

`Finding` and `ReviewDecision` are the shared value types from
[`baseplate`](https://crates.io/crates/baseplate) (`model`), so they cross the Verifier boundary
as stable serialized types.

## Dependencies (publish order)

attestr depends by version on [`cascadr`](https://crates.io/crates/cascadr) (the reviewer's
dispatch provider) and [`baseplate`](https://crates.io/crates/baseplate) (shared types +
registry). Both must be on crates.io **before** attestr publishes. The source workspace
redirects those version requirements to the local members via `[patch.crates-io]`; that patch is
not part of this crate and does not travel to crates.io.

## What attestr does not do

- It does not gate a commit or block a turn — that is a policy gate's job (e.g. [commitward](https://github.com/Barnett-Studios/commitward)).
- It does not execute the agent loop or apply edits — that is the host loop runner's job.
- It does not decide budget or admission — that is a cost/admission governor's job.
