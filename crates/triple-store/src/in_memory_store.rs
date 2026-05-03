//! In-memory `FactStore` backend (v0).
//!
//! Per PRD §3.3 / §6: prioritizes correctness and concurrent access without a
//! global lock. Sharding by `EntityId` (default 16 shards) keeps lock
//! contention proportional to the number of entities sharing a shard, not the
//! total concurrent entity count.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::fact::{Fact, PendingFact};
use crate::fact_store::{CommitChange, Error};
use crate::id::{EntityId, TxId};
use crate::term::Predicate;
use crate::time::{Clock, SystemClock};

const DEFAULT_SHARD_COUNT: usize = 16;

/// (subject, predicate) bucket key. Every fact for a given S+P lives in one
/// `Vec<Fact>`, ordered by commit order. Open and closed facts coexist; an
/// open fact is one with `valid.end == None`.
type SpKey = (EntityId, Predicate);

/// One shard of the primary fact storage. Each shard owns its own lock, so
/// reads or writes on one shard do not contend with reads or writes on any
/// other shard.
#[derive(Default)]
struct Shard {
    facts: RwLock<HashMap<SpKey, Vec<Fact>>>,
}

/// One operation recorded in a transaction's tentative log. At commit time,
/// the store interprets these per-cardinality.
///
/// Each variant carries its cardinality in the type, so commit can dispatch
/// without consulting the predicate registry (which is above the FactStore
/// boundary, per PRD §3.2). `AssertSet`, `AssertIndexed`, and `Retract` land
/// in subsequent chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TentativeOp {
    AssertSingle(PendingFact),
}

/// Per-transaction tentative buffer. A single lock is acceptable in v0: each
/// transaction is expected to be operated on by one entity task at a time, so
/// the only contention is `begin_tx` / `commit` / `rollback` adding/removing
/// entries — a small, infrequent operation.
#[derive(Default)]
struct TentativeBuffer {
    by_tx: RwLock<HashMap<TxId, Vec<TentativeOp>>>,
}

pub struct InMemoryStore {
    /// Fixed-size after construction. `Box<[T]>` rather than `Vec<T>` makes
    /// that explicit and saves a `usize` of capacity bookkeeping.
    shards: Box<[Shard]>,
    tentative: TentativeBuffer,
    /// Lock-free monotonic source for `TxId`s.
    next_tx: AtomicU64,
    /// Source of commit timestamps. `Arc<dyn Clock>` so tests can keep a
    /// shared handle and drive time deterministically.
    clock: Arc<dyn Clock>,
}

impl InMemoryStore {
    /// Construct with the default shard count (16) and `SystemClock`.
    pub fn new() -> Self {
        Self::build(DEFAULT_SHARD_COUNT, Arc::new(SystemClock))
    }

    /// Construct with the default shard count and an explicit clock — useful
    /// for tests that need deterministic commit timestamps.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::build(DEFAULT_SHARD_COUNT, clock)
    }

    /// Construct with an explicit shard count and the `SystemClock`.
    ///
    /// Requires: `shards >= 1`.
    pub fn with_shards(shards: usize) -> Self {
        Self::build(shards, Arc::new(SystemClock))
    }

    fn build(shards: usize, clock: Arc<dyn Clock>) -> Self {
        assert!(shards >= 1, "shard count must be >= 1");
        let shards = (0..shards).map(|_| Shard::default()).collect();
        Self {
            shards,
            tentative: TentativeBuffer::default(),
            next_tx: AtomicU64::new(1),
            clock,
        }
    }

    // === Transactions ===

    /// Begin a new transaction.
    ///
    /// Ensures: each call returns a `TxId` not previously returned by this store,
    /// and registers it as active. Any subsequent `assert_*` / `retract_*` /
    /// `commit` / `rollback` against this `TxId` will see it as active until
    /// `commit` or `rollback` is called.
    pub fn begin_tx(&self) -> TxId {
        let id = TxId(self.next_tx.fetch_add(1, Ordering::Relaxed));
        self.tentative.by_tx.write().insert(id, Vec::new());
        id
    }

    /// Roll back a transaction: discard its tentative facts without trace.
    ///
    /// Requires: `tx_id` is an active transaction.
    /// Ensures: subsequent operations on `tx_id` return `Error::UnknownTransaction`.
    pub fn rollback(&self, tx_id: TxId) -> Result<(), Error> {
        let mut buf = self.tentative.by_tx.write();
        buf.remove(&tx_id).ok_or(Error::UnknownTransaction(tx_id))?;
        Ok(())
    }

    /// Commit a transaction: drain its tentative ops, apply them to the
    /// per-shard fact buckets, and return one `CommitChange` per *kept* op.
    ///
    /// **Collapse-to-last (Single cardinality).** Within one transaction, the
    /// only externally observable state for a `(subject, predicate, asserter)`
    /// key is the *final* asserted value — intermediate values during the tx
    /// were tentative and never visible. Commit therefore keeps only the last
    /// `TentativeOp::AssertSingle` per key (insertion order preserved across
    /// distinct keys), discarding any earlier asserts for the same key.
    ///
    /// Requires: `tx_id` is an active transaction.
    /// Ensures: the transaction is no longer active after this call (subsequent
    /// `assert_*` / `retract_*` / `commit` / `rollback` return
    /// `Error::UnknownTransaction`); each *kept* tentative assert is reflected
    /// as a `CommitChange::Asserted` (no prior open fact at the key) or
    /// `CommitChange::Updated` (a prior open fact existed and has been closed
    /// at the new fact's `valid.start`).
    pub fn commit(&self, tx_id: TxId) -> Result<Vec<CommitChange>, Error> {
        // Drain the tentative ops atomically; any concurrent operation on
        // this tx_id afterward will fail with UnknownTransaction.
        let ops = self
            .tentative
            .by_tx
            .write()
            .remove(&tx_id)
            .ok_or(Error::UnknownTransaction(tx_id))?;

        // First pass: record the index of the last Assert per
        // (subject, predicate, asserter) key, so we can collapse intermediate
        // asserts in the second pass.
        let mut last_assert_idx: HashMap<(EntityId, Predicate, EntityId), usize> =
            HashMap::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            match op {
                TentativeOp::AssertSingle(p) => {
                    last_assert_idx.insert((p.subject, p.predicate, p.asserter), i);
                }
            }
        }

        let commit_time = self.clock.now();
        let mut changes = Vec::with_capacity(last_assert_idx.len());

        // Second pass: apply only the last assert per key, in original order.
        for (i, op) in ops.into_iter().enumerate() {
            match op {
                TentativeOp::AssertSingle(pending) => {
                    let key = (pending.subject, pending.predicate, pending.asserter);
                    if last_assert_idx[&key] != i {
                        continue;
                    }

                    let new_fact = pending.into_fact(commit_time, tx_id, None);
                    let shard = self.shard_for(new_fact.subject);
                    let mut facts = shard.facts.write();
                    let bucket = facts
                        .entry((new_fact.subject, new_fact.predicate))
                        .or_default();

                    // Single cardinality: at most one open fact per
                    // (subject, predicate, asserter). Locate it (if any) and
                    // close its valid interval at the new fact's start.
                    let prior_idx = bucket
                        .iter()
                        .position(|f| f.valid.is_open() && f.asserter == new_fact.asserter);

                    match prior_idx {
                        Some(idx) => {
                            bucket[idx].valid.close(new_fact.valid.start);
                            let closed = bucket[idx].clone();
                            bucket.push(new_fact.clone());
                            changes.push(CommitChange::Updated {
                                closed,
                                opened: new_fact,
                            });
                        }
                        None => {
                            bucket.push(new_fact.clone());
                            changes.push(CommitChange::Asserted { fact: new_fact });
                        }
                    }
                }
            }
        }

        Ok(changes)
    }

    // === Mutations (must be within a transaction) ===

    /// Append a `Single`-cardinality assertion to the tentative buffer for `tx_id`.
    ///
    /// Takes `fact` by value: the buffer takes ownership and the caller has no
    /// further need of the value. This avoids a clone on every assert.
    ///
    /// Requires: `tx_id` is an active transaction.
    /// Ensures: the assertion is visible to `commit(tx_id)` and discarded by
    /// `rollback(tx_id)`. Supersession of any prior open fact at
    /// `(subject, predicate, asserter)` is performed at commit time, not here.
    pub fn assert_single(&self, tx_id: TxId, fact: PendingFact) -> Result<(), Error> {
        let mut buf = self.tentative.by_tx.write();
        let entries = buf
            .get_mut(&tx_id)
            .ok_or(Error::UnknownTransaction(tx_id))?;
        entries.push(TentativeOp::AssertSingle(fact));
        Ok(())
    }

    // === Internal helpers ===

    /// Hash-mod the entity to its owning shard.
    fn shard_for(&self, entity: EntityId) -> &Shard {
        &self.shards[(entity.0 as usize) % self.shards.len()]
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Term;
    use crate::time::Timestamp;

    /// Test clock with externally-controlled time. `now()` only reads; tests
    /// call `set` to advance.
    struct TickClock {
        nanos: AtomicU64,
    }

    impl TickClock {
        fn new(start: u64) -> Arc<Self> {
            Arc::new(Self {
                nanos: AtomicU64::new(start),
            })
        }
        fn set(&self, t: u64) {
            self.nanos.store(t, Ordering::Relaxed);
        }
    }

    impl Clock for TickClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_nanos(self.nanos.load(Ordering::Relaxed))
        }
    }

    /// Test-only peek at the size of a transaction's tentative buffer.
    /// `None` means the transaction is not active.
    fn pending_count(s: &InMemoryStore, tx: TxId) -> Option<usize> {
        s.tentative.by_tx.read().get(&tx).map(|v| v.len())
    }

    fn pending(subject: u64, asserter: u64, value: i64) -> PendingFact {
        PendingFact {
            subject: EntityId(subject),
            predicate: Predicate(0),
            object: Term::Int(value),
            asserter: EntityId(asserter),
            valid: None,
        }
    }

    // --- Construction & shard layout ---

    #[test]
    fn shard_for_is_deterministic() {
        let s = InMemoryStore::with_shards(4);
        let e = EntityId(42);
        // Two calls yield the *same* reference, not just an equal one.
        assert!(std::ptr::eq(s.shard_for(e), s.shard_for(e)));
    }

    #[test]
    fn entities_distribute_across_shards() {
        let s = InMemoryStore::with_shards(4);
        let mut hits = [0usize; 4];
        for i in 0..100 {
            let shard = s.shard_for(EntityId(i));
            let idx = s
                .shards
                .iter()
                .position(|sh| std::ptr::eq(sh, shard))
                .unwrap();
            hits[idx] += 1;
        }
        assert!(
            hits.iter().all(|&n| n > 0),
            "uneven shard distribution: {hits:?}"
        );
    }

    #[test]
    #[should_panic(expected = "shard count must be >= 1")]
    fn with_shards_zero_panics() {
        InMemoryStore::with_shards(0);
    }

    // --- Transaction lifecycle ---

    #[test]
    fn begin_tx_is_monotonic() {
        let s = InMemoryStore::new();
        let a = s.begin_tx();
        let b = s.begin_tx();
        let c = s.begin_tx();
        assert!(a.0 < b.0 && b.0 < c.0);
    }

    #[test]
    fn begin_tx_registers_active_transaction() {
        let s = InMemoryStore::new();
        let tx = s.begin_tx();
        assert_eq!(pending_count(&s, tx), Some(0));
    }

    #[test]
    fn assert_single_appends_to_buffer() {
        let s = InMemoryStore::new();
        let tx = s.begin_tx();
        s.assert_single(tx, pending(1, 1, 42)).unwrap();
        s.assert_single(tx, pending(1, 1, 43)).unwrap();
        assert_eq!(pending_count(&s, tx), Some(2));
    }

    #[test]
    fn assert_single_unknown_tx_errors() {
        let s = InMemoryStore::new();
        let bogus = TxId(999);
        let err = s.assert_single(bogus, pending(1, 1, 0)).unwrap_err();
        assert_eq!(err, Error::UnknownTransaction(bogus));
    }

    #[test]
    fn rollback_discards_tentative_facts() {
        let s = InMemoryStore::new();
        let tx = s.begin_tx();
        s.assert_single(tx, pending(1, 1, 42)).unwrap();
        assert_eq!(pending_count(&s, tx), Some(1));
        s.rollback(tx).unwrap();
        assert_eq!(pending_count(&s, tx), None);
    }

    #[test]
    fn rollback_unknown_tx_errors() {
        let s = InMemoryStore::new();
        let bogus = TxId(999);
        let err = s.rollback(bogus).unwrap_err();
        assert_eq!(err, Error::UnknownTransaction(bogus));
    }

    #[test]
    fn distinct_transactions_have_isolated_buffers() {
        let s = InMemoryStore::new();
        let tx_a = s.begin_tx();
        let tx_b = s.begin_tx();
        s.assert_single(tx_a, pending(1, 1, 1)).unwrap();
        s.assert_single(tx_a, pending(1, 1, 2)).unwrap();
        s.assert_single(tx_b, pending(1, 1, 99)).unwrap();
        assert_eq!(pending_count(&s, tx_a), Some(2));
        assert_eq!(pending_count(&s, tx_b), Some(1));
    }

    #[test]
    fn rollback_isolates_other_transactions() {
        let s = InMemoryStore::new();
        let tx_a = s.begin_tx();
        let tx_b = s.begin_tx();
        s.assert_single(tx_a, pending(1, 1, 1)).unwrap();
        s.assert_single(tx_b, pending(1, 1, 2)).unwrap();
        s.rollback(tx_a).unwrap();
        assert_eq!(pending_count(&s, tx_a), None);
        assert_eq!(pending_count(&s, tx_b), Some(1));
    }

    // --- Commit ---

    #[test]
    fn commit_unknown_tx_errors() {
        let s = InMemoryStore::new();
        let bogus = TxId(999);
        let err = s.commit(bogus).unwrap_err();
        assert_eq!(err, Error::UnknownTransaction(bogus));
    }

    #[test]
    fn commit_consumes_transaction() {
        let s = InMemoryStore::new();
        let tx = s.begin_tx();
        s.commit(tx).unwrap();
        // Subsequent operations on this tx must report it as unknown.
        assert_eq!(s.rollback(tx).unwrap_err(), Error::UnknownTransaction(tx));
        assert_eq!(
            s.assert_single(tx, pending(1, 1, 0)).unwrap_err(),
            Error::UnknownTransaction(tx)
        );
    }

    #[test]
    fn commit_empty_tx_yields_no_changes() {
        let s = InMemoryStore::new();
        let tx = s.begin_tx();
        let changes = s.commit(tx).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn commit_single_with_no_prior_yields_asserted() {
        let clock = TickClock::new(100);
        let s = InMemoryStore::with_clock(clock.clone());
        let tx = s.begin_tx();
        s.assert_single(tx, pending(1, 1, 42)).unwrap();
        let changes = s.commit(tx).unwrap();

        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!("expected Asserted, got {:?}", changes[0]);
        };
        assert_eq!(fact.subject, EntityId(1));
        assert_eq!(fact.asserter, EntityId(1));
        assert_eq!(fact.object, Term::Int(42));
        assert_eq!(fact.valid.start, Timestamp::from_nanos(100));
        assert!(fact.valid.is_open());
        assert_eq!(fact.asserted.start, Timestamp::from_nanos(100));
        assert!(fact.asserted.is_open());
        assert_eq!(fact.tx_id, tx);
        assert!(fact.is_internal());
    }

    #[test]
    fn commit_single_with_prior_open_supersedes() {
        let clock = TickClock::new(100);
        let s = InMemoryStore::with_clock(clock.clone());

        // First commit at t=100.
        let tx1 = s.begin_tx();
        s.assert_single(tx1, pending(1, 1, 42)).unwrap();
        let changes1 = s.commit(tx1).unwrap();
        assert!(matches!(&changes1[0], CommitChange::Asserted { .. }));

        // Second commit at t=200; should supersede the prior.
        clock.set(200);
        let tx2 = s.begin_tx();
        s.assert_single(tx2, pending(1, 1, 43)).unwrap();
        let changes2 = s.commit(tx2).unwrap();

        assert_eq!(changes2.len(), 1);
        let CommitChange::Updated { closed, opened } = &changes2[0] else {
            panic!("expected Updated, got {:?}", changes2[0]);
        };

        // Prior fact: was opened at 100, now closed at 200.
        assert_eq!(closed.object, Term::Int(42));
        assert_eq!(closed.valid.start, Timestamp::from_nanos(100));
        assert_eq!(closed.valid.end, Some(Timestamp::from_nanos(200)));

        // New fact: opened at 200, currently open.
        assert_eq!(opened.object, Term::Int(43));
        assert_eq!(opened.valid.start, Timestamp::from_nanos(200));
        assert!(opened.valid.is_open());
    }

    #[test]
    fn commit_collapses_multi_assert_to_same_key_to_last() {
        let clock = TickClock::new(100);
        let s = InMemoryStore::with_clock(clock.clone());
        let tx = s.begin_tx();
        s.assert_single(tx, pending(1, 1, 42)).unwrap();
        s.assert_single(tx, pending(1, 1, 43)).unwrap();
        s.assert_single(tx, pending(1, 1, 44)).unwrap();
        let changes = s.commit(tx).unwrap();

        // Only the last value (44) should commit; intermediate values 42 and 43
        // were never observable, so they leave no committed fact.
        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!("expected Asserted, got {:?}", changes[0]);
        };
        assert_eq!(fact.object, Term::Int(44));
    }

    #[test]
    fn commit_collapses_per_key_independently() {
        let s = InMemoryStore::new();
        let tx = s.begin_tx();
        // Two distinct keys, each asserted twice. Collapse should keep the
        // last per key (43 for subject=1, 20 for subject=2).
        s.assert_single(tx, pending(1, 1, 42)).unwrap();
        s.assert_single(tx, pending(2, 2, 10)).unwrap();
        s.assert_single(tx, pending(1, 1, 43)).unwrap();
        s.assert_single(tx, pending(2, 2, 20)).unwrap();
        let changes = s.commit(tx).unwrap();

        assert_eq!(changes.len(), 2);

        let by_subject: std::collections::HashMap<EntityId, &Fact> = changes
            .iter()
            .map(|c| match c {
                CommitChange::Asserted { fact } => (fact.subject, fact),
                _ => panic!("expected only Asserted variants"),
            })
            .collect();

        assert_eq!(by_subject[&EntityId(1)].object, Term::Int(43));
        assert_eq!(by_subject[&EntityId(2)].object, Term::Int(20));
    }

    #[test]
    fn commit_separate_asserters_do_not_supersede() {
        let s = InMemoryStore::new();

        // Internal: subject=1 asserts about itself.
        let tx1 = s.begin_tx();
        s.assert_single(tx1, pending(1, 1, 42)).unwrap();
        s.commit(tx1).unwrap();

        // External: entity 2 asserts about subject=1. Different asserter
        // namespace — must not close the internal fact.
        let tx2 = s.begin_tx();
        s.assert_single(tx2, pending(1, 2, 99)).unwrap();
        let changes = s.commit(tx2).unwrap();

        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!("expected Asserted, got {:?}", changes[0]);
        };
        assert!(fact.is_external());
        assert_eq!(fact.asserter, EntityId(2));
    }
}
