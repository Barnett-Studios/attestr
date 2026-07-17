//! Trust: EMA math + tiers (this file's pure half) and the SQLite atomic store
//! (`TrustStore`, added below).

use dotclaude_support::model::VerificationResult;

pub const DEFAULT_DECAY: f64 = 0.85;
pub const DEFAULT_TRUST: f64 = 0.5;

#[derive(Debug, PartialEq, Eq)]
pub enum TrustError {
    /// `computeRunObservation([])` throws in JS — no results to observe.
    EmptyResults,
}

// EMA weights are single-sourced on the model: `Observation::value()`
// (skipped → None, excluded) and `Confidence::weight()` (high 1.0 / medium 0.6
// / low 0.3). Unknown confidence is unrepresentable via the typed enums.
// Do NOT redefine them here.

/// Weighted-average observation in [0,1], `None` if every result was skipped.
/// `Err(EmptyResults)` on empty input (JS throws).
pub fn compute_run_observation(results: &[VerificationResult]) -> Result<Option<f64>, TrustError> {
    if results.is_empty() {
        return Err(TrustError::EmptyResults);
    }
    let mut weighted_sum = 0.0_f64;
    let mut total_weight = 0.0_f64;
    let mut observable = 0usize;
    for r in results {
        let Some(value) = r.result.value() else {
            continue; // skipped — exclude
        };
        let weight = r.confidence.weight();
        weighted_sum += value * weight;
        total_weight += weight;
        observable += 1;
    }
    if observable == 0 {
        return Ok(None);
    }
    Ok(Some(weighted_sum / total_weight))
}

/// When all results are skipped (no signal), preserve the current trust.
pub fn update_trust(
    current: f64,
    results: &[VerificationResult],
    decay: f64,
) -> Result<f64, TrustError> {
    match compute_run_observation(results)? {
        None => Ok(current),
        Some(obs) => Ok(decay * current + (1.0 - decay) * obs),
    }
}

/// Apply the EMA to a precomputed observation. Used by the atomic store, which
/// computes the observation once (pure) and folds it inside the DB transaction.
pub fn apply_ema(current: f64, observation: Option<f64>, decay: f64) -> f64 {
    match observation {
        None => current,
        Some(obs) => decay * current + (1.0 - decay) * obs,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Tier {
    High,
    Medium,
    Low,
    Critical,
}

/// Map a trust score to a tier: `>0.8 → High`, `>=0.5 → Medium`, `>=0.2 → Low`, else `Critical`.
pub fn trust_tier(t: f64) -> Tier {
    if t > 0.8 {
        Tier::High
    } else if t >= 0.2 {
        if t >= 0.5 {
            Tier::Medium
        } else {
            Tier::Low
        }
    } else {
        Tier::Critical
    }
}

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;
use std::time::Duration;

/// Atomic durable trust in SQLite. The read-modify-write of a single agent's
/// scalar runs inside one `BEGIN IMMEDIATE` transaction, so concurrent
/// processes serialize (busy_timeout), preventing a last-writer-wins race.
pub struct TrustStore {
    conn: Connection,
}

impl TrustStore {
    pub fn open(db_path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        // WAL: a writer no longer blocks (and is not blocked by) readers, so
        // concurrent `update_atomic` writers serialize on the busy_timeout
        // instead of returning SQLITE_BUSY under load. `busy_timeout` alone is
        // not enough — with the default rollback journal, a piled-up writer can
        // get "database is locked" before the timeout elapses (seen flaking the
        // 50-thread contention test on the loaded CI runner). The RMW path
        // (`update_atomic`/`set`) is retry-guarded via `immediate_tx_retry`, but
        // this open()'s first-contact DDL — the WAL pragma and `CREATE TABLE IF
        // NOT EXISTS` below — races across processes too: N processes hitting a
        // not-yet-materialized `trust.db` simultaneously can each observe
        // `SQLITE_BUSY`/`SQLITE_LOCKED` on these autocommit statements before
        // `busy_timeout` absorbs it (seen flaking the 16-process CLI contention
        // test). So both statements are individually retry-guarded via
        // `busy_retry` as well.
        busy_retry(|| conn.pragma_update(None, "journal_mode", "WAL"))?;
        busy_retry(|| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS trust(
                     agent_id TEXT PRIMARY KEY,
                     trust REAL NOT NULL,
                     updated_at TEXT NOT NULL
                 );",
            )
        })?;
        Ok(Self { conn })
    }

    pub fn get(&self, agent_id: &str) -> rusqlite::Result<Option<f64>> {
        self.conn
            .query_row(
                "SELECT trust FROM trust WHERE agent_id = ?1",
                [agent_id],
                |r| r.get(0),
            )
            .optional()
    }

    /// Atomic RMW. `observation == None` (all skipped) preserves trust. Returns
    /// `(before, after)`; `before` defaults to 0.5 for a never-seen agent.
    pub fn update_atomic(
        &mut self,
        agent_id: &str,
        observation: Option<f64>,
        decay: f64,
        now: &str,
    ) -> rusqlite::Result<(f64, f64)> {
        immediate_tx_retry(&mut self.conn, |tx| {
            let before: f64 = tx
                .query_row(
                    "SELECT trust FROM trust WHERE agent_id = ?1",
                    [agent_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(DEFAULT_TRUST);
            let after = apply_ema(before, observation, decay);
            tx.execute(
                "INSERT INTO trust(agent_id, trust, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(agent_id) DO UPDATE SET trust = ?2, updated_at = ?3",
                rusqlite::params![agent_id, after, now],
            )?;
            Ok((before, after))
        })
    }

    /// Unconditional set (for `trust reset`). Returns the previous value (or 0.5).
    pub fn set(&mut self, agent_id: &str, trust: f64, now: &str) -> rusqlite::Result<f64> {
        immediate_tx_retry(&mut self.conn, |tx| {
            let before: f64 = tx
                .query_row(
                    "SELECT trust FROM trust WHERE agent_id = ?1",
                    [agent_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(DEFAULT_TRUST);
            tx.execute(
                "INSERT INTO trust(agent_id, trust, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(agent_id) DO UPDATE SET trust = ?2, updated_at = ?3",
                rusqlite::params![agent_id, trust, now],
            )?;
            Ok(before)
        })
    }
}

/// Classify a `rusqlite::Error` as a transient contention error worth retrying
/// (`SQLITE_BUSY` / `SQLITE_LOCKED`), as opposed to a real (non-transient)
/// failure that must surface immediately.
fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(err, _)
            if matches!(
                err.code,
                rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked
            )
    )
}

/// Bounded retry for a fallible operation that may transiently fail with
/// `SQLITE_BUSY`/`SQLITE_LOCKED` under writer contention. Connection-independent
/// so it is unit-testable without a real database. Non-busy errors propagate
/// immediately; busy errors retry with a short linear backoff, capped at
/// `MAX_ATTEMPTS` total attempts.
fn busy_retry<T>(mut op: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    const MAX_ATTEMPTS: u32 = 20;
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if is_busy(&e) && attempt + 1 < MAX_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(
                    (attempt as u64 * 3).min(50),
                ));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Run `body` inside a fresh `BEGIN IMMEDIATE` transaction and commit it,
/// retrying the whole attempt (fresh transaction included) on a transient
/// `SQLITE_BUSY`/`SQLITE_LOCKED`. The transaction never escapes an attempt —
/// it is created and either committed or dropped (on error) within the same
/// closure invocation — so it cannot outlive `conn`'s mutable borrow.
fn immediate_tx_retry<T>(
    conn: &mut Connection,
    mut body: impl FnMut(&rusqlite::Transaction) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    busy_retry(|| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let v = body(&tx)?;
        tx.commit()?;
        Ok(v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dotclaude_support::model::{Confidence, Observation, VerificationResult};

    fn r(result: Observation, confidence: Confidence) -> VerificationResult {
        VerificationResult {
            promise_id: String::new(),
            method: String::new(),
            confidence,
            result,
            evidence: String::new(),
            timestamp: String::new(),
        }
    }

    #[test]
    fn empty_is_error() {
        assert_eq!(compute_run_observation(&[]), Err(TrustError::EmptyResults));
    }

    #[test]
    fn all_skipped_preserves_trust() {
        let rs = [r(Observation::Skipped, Confidence::High)];
        assert_eq!(update_trust(0.7, &rs, 0.85), Ok(0.7));
    }

    #[test]
    fn single_kept_from_critical() {
        // 0.85*0.1 + 0.15*1.0 = 0.235
        let rs = [r(Observation::Kept, Confidence::High)];
        assert_eq!(update_trust(0.1, &rs, 0.85), Ok(0.235_000_000_000_000_04));
    }

    #[test]
    fn tiers_at_boundaries() {
        assert_eq!(trust_tier(0.81), Tier::High);
        assert_eq!(trust_tier(0.8), Tier::Medium); // >0.8 required for high
        assert_eq!(trust_tier(0.5), Tier::Medium);
        assert_eq!(trust_tier(0.49), Tier::Low);
        assert_eq!(trust_tier(0.2), Tier::Low);
        assert_eq!(trust_tier(0.19), Tier::Critical);
    }

    fn busy_err() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None)
    }

    fn constraint_err() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            None,
        )
    }

    #[test]
    fn is_busy_classifies_busy_and_locked_true_others_false() {
        let locked = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            None,
        );
        assert!(is_busy(&busy_err()));
        assert!(is_busy(&locked));
        assert!(!is_busy(&constraint_err()));
    }

    #[test]
    fn busy_retry_succeeds_after_transient_busy() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let result = busy_retry(|| {
            calls.set(calls.get() + 1);
            if calls.get() <= 2 {
                Err(busy_err())
            } else {
                Ok(42)
            }
        });
        assert_eq!(result, Ok(42));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn busy_retry_gives_up_after_max_attempts() {
        use std::cell::Cell;
        const MAX_ATTEMPTS: u32 = 20;
        let calls = Cell::new(0u32);
        let result = busy_retry(|| {
            calls.set(calls.get() + 1);
            Err::<(), _>(busy_err())
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), MAX_ATTEMPTS);
    }

    #[test]
    fn busy_retry_propagates_non_busy_error_immediately() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let result = busy_retry(|| {
            calls.set(calls.get() + 1);
            Err::<(), _>(constraint_err())
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn store_single_update_and_default() {
        let db = std::env::temp_dir().join(format!("trust-single-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let mut store = TrustStore::open(&db).unwrap();
        assert_eq!(store.get("new").unwrap(), None); // never-seen
                                                     // obs 1.0 from default 0.5: 0.85*0.5 + 0.15*1.0 = 0.575
        let (before, after) = store.update_atomic("a", Some(1.0), 0.85, "<TS>").unwrap();
        assert_eq!(before, 0.5);
        assert_eq!(after, 0.85 * 0.5 + (1.0 - 0.85) * 1.0);
        assert_eq!(store.get("a").unwrap(), Some(after));
        // None observation preserves.
        let (b2, a2) = store.update_atomic("a", None, 0.85, "<TS>").unwrap();
        assert_eq!(b2, after);
        assert_eq!(a2, after);
        std::fs::remove_file(&db).ok();
    }

    #[test]
    fn atomic_update_no_lost_writes_under_contention() {
        use std::path::PathBuf;
        use std::thread;

        let db: PathBuf =
            std::env::temp_dir().join(format!("trust-race-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        TrustStore::open(&db).unwrap(); // create the table once

        const N: usize = 50;
        let mut handles = Vec::new();
        for _ in 0..N {
            let db = db.clone();
            handles.push(thread::spawn(move || {
                let mut store = TrustStore::open(&db).unwrap();
                // Identical observation (1.0) each time → the fold is
                // order-independent and deterministic.
                store
                    .update_atomic("agent", Some(1.0), 0.85, "<TS>")
                    .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let store = TrustStore::open(&db).unwrap();
        let got = store.get("agent").unwrap().unwrap();
        // Expected = iterate apply_ema's exact ops N times (bit-faithful).
        let mut want = 0.5_f64;
        for _ in 0..N {
            want = 0.85 * want + (1.0 - 0.85) * 1.0;
        }
        assert!(
            (got - want).abs() < 1e-9,
            "lost updates: got {got}, want {want} (a racy store yields ~0.575)"
        );
        std::fs::remove_file(&db).ok();
    }
}
