//! In-memory `TripleStore` implementation.
//!
//! Three types collaborate:
//! - `InMemoryStorage` — owns the sharded committed-fact buckets; implements
//!   `Storage<StoredFact>`.
//! - `InMemoryTransaction<'a>` — owns a private tentative buffer and a borrow
//!   of the store; implements `Transaction<StoredFact>`. Rolling back is free:
//!   dropping the transaction drops the buffer.
//! - `InMemoryTripleStore` — owns storage + clock + TxId counter; implements
//!   `TripleStore<StoredFact>` and drives the commit path.
//!
//! Sharding by `EntityId` (default 16 shards) keeps lock contention
//! proportional to the number of entities sharing a shard, not total
//! concurrent entity count (PRD §3.3 / §6).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::fact::{BitemporalFact, Fact, StoredFact};
use crate::id::{EntityId, TxId};
use crate::pattern::{Match, MatchAsserter, Pattern};
use crate::predicates::OrderingKey;
use crate::queryable::Queryable;
use crate::storage::{CommitChange, Error as StorageError, Storage};
use crate::term::{Predicate, Term};
use crate::time::{Clock, SystemClock, Timestamp};
use crate::transaction::{Error as TxError, Transaction};
use crate::triple_store::TripleStore;

const DEFAULT_SHARD_COUNT: usize = 16;

type SpKey = (EntityId, Predicate);

#[derive(Default)]
struct Shard {
    facts: RwLock<HashMap<SpKey, Vec<StoredFact>>>,
}

// ── InMemoryStorage ──────────────────────────────────────────────────────────

/// Sharded committed-fact storage.
pub struct InMemoryStorage {
    shards: Box<[Shard]>,
}

impl InMemoryStorage {
    fn new(shard_count: usize) -> Self {
        assert!(shard_count >= 1, "shard count must be >= 1");
        Self {
            shards: (0..shard_count).map(|_| Shard::default()).collect(),
        }
    }

    fn shard_for(&self, entity: EntityId) -> &Shard {
        &self.shards[(entity.0 as usize) % self.shards.len()]
    }
}

impl Storage<StoredFact> for InMemoryStorage {
    fn write(&self, subject: EntityId, predicate: Predicate, fact: StoredFact) {
        self.shard_for(subject)
            .facts
            .write()
            .entry((subject, predicate))
            .or_default()
            .push(fact);
    }

    fn supersede<F>(
        &self,
        subject: EntityId,
        predicate: Predicate,
        find: F,
        replacement: StoredFact,
    ) -> Result<StoredFact, StorageError>
    where
        F: Fn(&StoredFact) -> bool,
    {
        let mut guard = self.shard_for(subject).facts.write();
        let bucket = guard.entry((subject, predicate)).or_default();
        match bucket.iter().position(|f| f.asserted.is_open() && find(f)) {
            Some(i) => {
                // Asserted intervals meet: prior closes where replacement opens.
                bucket[i].asserted.close(replacement.asserted.start);
                let closed = bucket[i].clone();
                bucket.push(replacement);
                Ok(closed)
            }
            None => Err(StorageError::FactNotFound),
        }
    }

    fn close<F>(
        &self,
        subject: EntityId,
        predicate: Predicate,
        find: F,
        at: Timestamp,
    ) -> Result<StoredFact, StorageError>
    where
        F: Fn(&StoredFact) -> bool,
    {
        let mut guard = self.shard_for(subject).facts.write();
        let bucket = guard.entry((subject, predicate)).or_default();
        match bucket.iter().position(|f| f.asserted.is_open() && find(f)) {
            Some(i) => {
                bucket[i].asserted.close(at);
                Ok(bucket[i].clone())
            }
            None => Err(StorageError::FactNotFound),
        }
    }

    fn with_facts<F, R>(&self, subject: EntityId, predicate: Predicate, f: F) -> R
    where
        F: FnOnce(&[StoredFact]) -> R,
    {
        let guard = self.shard_for(subject).facts.read();
        let bucket = guard
            .get(&(subject, predicate))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        f(bucket)
    }
}

// ── Pattern matching ─────────────────────────────────────────────────────────

fn matches_pattern(fact: &StoredFact, pattern: Option<&Pattern>) -> bool {
    let Some(pat) = pattern else {
        return true;
    };
    if let Match::Is(s) = &pat.subject {
        if fact.subject != *s {
            return false;
        }
    }
    if let Match::Is(p) = &pat.predicate {
        if fact.predicate != *p {
            return false;
        }
    }
    if let Match::Is(o) = &pat.object {
        if fact.object != *o {
            return false;
        }
    }
    match &pat.asserter {
        MatchAsserter::Any => {}
        MatchAsserter::Specific(a) => {
            if fact.asserter != *a {
                return false;
            }
        }
        MatchAsserter::SameAsSubject => {
            if fact.asserter != fact.subject {
                return false;
            }
        }
        MatchAsserter::DifferentFromSubject => {
            if fact.asserter == fact.subject {
                return false;
            }
        }
    }
    true
}

// ── Queryable for InMemoryStorage ─────────────────────────────────────────────

impl Queryable<StoredFact> for InMemoryStorage {
    fn query_open(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.with_facts(subject, predicate, |facts| {
            facts
                .iter()
                .filter(|f| f.asserted.is_open() && matches_pattern(f, pattern))
                .cloned()
                .collect()
        })
    }

    fn query_at(
        &self,
        subject: EntityId,
        predicate: Predicate,
        tx_time: Timestamp,
        valid_time: Timestamp,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.with_facts(subject, predicate, |facts| {
            facts
                .iter()
                .filter(|f| {
                    f.asserted().contains(tx_time)
                        && f.valid().contains(valid_time)
                        && matches_pattern(f, pattern)
                })
                .cloned()
                .collect()
        })
    }

    fn history(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.with_facts(subject, predicate, |facts| {
            facts
                .iter()
                .filter(|f| matches_pattern(f, pattern))
                .cloned()
                .collect()
        })
    }
}

// ── TentativeOp ──────────────────────────────────────────────────────────────

/// One operation buffered in a transaction. Carries cardinality in its type so
/// the commit path can dispatch without a predicate registry (PRD §3.2).
///
/// Precondition (checked via `debug_assert` in `InMemoryTripleStore::apply`):
/// no identity key appears in both an Assert and a Retract in the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TentativeOp {
    AssertSingle(Fact),
    AssertSet(Fact),
    AssertIndexed {
        fact: Fact,
        index_key: OrderingKey,
    },
    RetractSingle {
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
    },
    RetractSet {
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: Term,
    },
    RetractIndexed {
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
    },
}

// ── InMemoryTransaction ───────────────────────────────────────────────────────

/// An in-flight transaction: owns its tentative buffer and borrows the store.
///
/// Dropping this type without calling `commit` or `rollback` is equivalent to
/// `rollback` — the buffer is simply freed. The explicit `rollback` method
/// exists to satisfy the `Transaction` trait and to make intent clear at call
/// sites.
pub struct InMemoryTransaction<'a> {
    store: &'a InMemoryTripleStore,
    tx_id: TxId,
    ops: Vec<TentativeOp>,
}

impl<'a> Transaction<StoredFact> for InMemoryTransaction<'a> {
    fn assert_single(&mut self, fact: Fact) {
        self.ops.push(TentativeOp::AssertSingle(fact));
    }

    fn assert_set(&mut self, fact: Fact) {
        self.ops.push(TentativeOp::AssertSet(fact));
    }

    fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey) {
        self.ops
            .push(TentativeOp::AssertIndexed { fact, index_key });
    }

    fn retract_single(&mut self, subject: EntityId, predicate: Predicate, asserter: EntityId) {
        self.ops.push(TentativeOp::RetractSingle {
            subject,
            predicate,
            asserter,
        });
    }

    fn retract_set(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: Term,
    ) {
        self.ops.push(TentativeOp::RetractSet {
            subject,
            predicate,
            asserter,
            object,
        });
    }

    fn retract_indexed(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
    ) {
        self.ops.push(TentativeOp::RetractIndexed {
            subject,
            predicate,
            asserter,
            index_key,
        });
    }

    fn commit(self) -> Result<Vec<CommitChange<StoredFact>>, TxError> {
        self.store.apply(self.tx_id, self.ops)
    }

    fn rollback(self) {
        // Consuming self drops `ops`; no storage side-effects.
    }
}

// ── InMemoryTripleStore ───────────────────────────────────────────────────────

/// In-memory `TripleStore`: owns storage, clock, and `TxId` generation.
pub struct InMemoryTripleStore {
    storage: InMemoryStorage,
    clock: Arc<dyn Clock>,
    next_tx: AtomicU64,
}

impl InMemoryTripleStore {
    /// Construct with the default shard count (16) and `SystemClock`.
    pub fn new() -> Self {
        Self::build(DEFAULT_SHARD_COUNT, Arc::new(SystemClock))
    }

    /// Construct with the default shard count and an injected clock.
    ///
    /// Useful in tests that need deterministic commit timestamps.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::build(DEFAULT_SHARD_COUNT, clock)
    }

    /// Construct with an explicit shard count and `SystemClock`.
    ///
    /// Requires: `shards >= 1`.
    pub fn with_shards(shards: usize) -> Self {
        Self::build(shards, Arc::new(SystemClock))
    }

    fn build(shard_count: usize, clock: Arc<dyn Clock>) -> Self {
        Self {
            storage: InMemoryStorage::new(shard_count),
            clock,
            next_tx: AtomicU64::new(1),
        }
    }

    /// Apply a transaction's ops to storage. Called by `InMemoryTransaction::commit`.
    fn apply(
        &self,
        tx_id: TxId,
        ops: Vec<TentativeOp>,
    ) -> Result<Vec<CommitChange<StoredFact>>, TxError> {
        // ── First pass: collapse maps ─────────────────────────────────────
        // For each cardinality, record the index of the last op per identity
        // key. Only that op is applied; earlier ops for the same key are
        // discarded (they were never observable, so committing them would
        // create empty intervals).
        let mut last_single: HashMap<(EntityId, Predicate, EntityId), usize> =
            HashMap::with_capacity(ops.len());
        let mut last_set: HashMap<(EntityId, Predicate, EntityId, Term), usize> =
            HashMap::with_capacity(ops.len());
        let mut last_indexed: HashMap<(EntityId, Predicate, EntityId, OrderingKey), usize> =
            HashMap::with_capacity(ops.len());
        let mut last_retract_single: HashMap<(EntityId, Predicate, EntityId), usize> =
            HashMap::new();
        let mut last_retract_set: HashMap<(EntityId, Predicate, EntityId, Term), usize> =
            HashMap::new();
        let mut last_retract_indexed: HashMap<(EntityId, Predicate, EntityId, OrderingKey), usize> =
            HashMap::new();

        for (i, op) in ops.iter().enumerate() {
            match op {
                TentativeOp::AssertSingle(f) => {
                    last_single.insert((f.subject, f.predicate, f.asserter), i);
                }
                TentativeOp::AssertSet(f) => {
                    last_set.insert((f.subject, f.predicate, f.asserter, f.object.clone()), i);
                }
                TentativeOp::AssertIndexed { fact: f, index_key } => {
                    last_indexed.insert((f.subject, f.predicate, f.asserter, *index_key), i);
                }
                TentativeOp::RetractSingle {
                    subject,
                    predicate,
                    asserter,
                } => {
                    last_retract_single.insert((*subject, *predicate, *asserter), i);
                }
                TentativeOp::RetractSet {
                    subject,
                    predicate,
                    asserter,
                    object,
                } => {
                    last_retract_set.insert((*subject, *predicate, *asserter, object.clone()), i);
                }
                TentativeOp::RetractIndexed {
                    subject,
                    predicate,
                    asserter,
                    index_key,
                } => {
                    last_retract_indexed.insert((*subject, *predicate, *asserter, *index_key), i);
                }
            }
        }

        // Assert + retract of the same identity key in one tx is a precondition
        // violation: two ops, one commit timestamp, same slot → undefined
        // asserted-interval semantics.
        #[cfg(debug_assertions)]
        {
            for key in last_single.keys() {
                debug_assert!(
                    !last_retract_single.contains_key(key),
                    "assert and retract same Single key in one tx: {key:?}"
                );
            }
            for key in last_set.keys() {
                debug_assert!(
                    !last_retract_set.contains_key(key),
                    "assert and retract same Set key in one tx: {key:?}"
                );
            }
            for key in last_indexed.keys() {
                debug_assert!(
                    !last_retract_indexed.contains_key(key),
                    "assert and retract same Indexed key in one tx: {key:?}"
                );
            }
        }

        // Collect selected indices. No sort needed: selected ops have disjoint
        // identity keys and share one commit timestamp, so application order
        // has no semantic effect on assert operations. Retracts are also
        // order-independent among themselves; the assert/retract precondition
        // above eliminates the only case where order would matter.
        let selected: Vec<usize> = last_single
            .values()
            .chain(last_set.values())
            .chain(last_indexed.values())
            .chain(last_retract_single.values())
            .chain(last_retract_set.values())
            .chain(last_retract_indexed.values())
            .copied()
            .collect();

        // ── Second pass: apply ────────────────────────────────────────────
        let commit_time = self.clock.now();
        let mut changes = Vec::with_capacity(selected.len());

        // Wrap ops in Option so each selected element can be moved out by
        // index. The unwrap is safe: every index in `selected` came from
        // enumerating this same `ops` slice.
        let mut ops: Vec<Option<TentativeOp>> = ops.into_iter().map(Some).collect();

        for op in selected.into_iter().map(|i| ops[i].take().unwrap()) {
            match op {
                TentativeOp::AssertSingle(fact) => {
                    let sf = fact.into_stored(commit_time, tx_id, None);
                    let change = match self.storage.supersede(
                        sf.subject,
                        sf.predicate,
                        |f| f.asserter == sf.asserter,
                        sf.clone(),
                    ) {
                        Ok(closed) => CommitChange::Updated { closed, opened: sf },
                        Err(StorageError::FactNotFound) => {
                            self.storage.write(sf.subject, sf.predicate, sf.clone());
                            CommitChange::Asserted { fact: sf }
                        }
                        Err(e) => return Err(e.into()),
                    };
                    changes.push(change);
                }

                TentativeOp::AssertSet(fact) => {
                    let sf = fact.into_stored(commit_time, tx_id, None);
                    // Set: idempotent on (subject, predicate, asserter, object).
                    // Read then write is non-atomic but acceptable in v0 since
                    // the Store API layer serialises writes per entity.
                    let already_open = self.storage.with_facts(sf.subject, sf.predicate, |facts| {
                        facts.iter().any(|f| {
                            f.asserted.is_open()
                                && f.asserter == sf.asserter
                                && f.object == sf.object
                        })
                    });
                    if !already_open {
                        self.storage.write(sf.subject, sf.predicate, sf.clone());
                        changes.push(CommitChange::Asserted { fact: sf });
                    }
                }

                TentativeOp::AssertIndexed { fact, index_key } => {
                    let sf = fact.into_stored(commit_time, tx_id, Some(index_key));
                    let change = match self.storage.supersede(
                        sf.subject,
                        sf.predicate,
                        |f| f.asserter == sf.asserter && f.index_key == Some(index_key),
                        sf.clone(),
                    ) {
                        Ok(closed) => CommitChange::Updated { closed, opened: sf },
                        Err(StorageError::FactNotFound) => {
                            self.storage.write(sf.subject, sf.predicate, sf.clone());
                            CommitChange::Asserted { fact: sf }
                        }
                        Err(e) => return Err(e.into()),
                    };
                    changes.push(change);
                }

                TentativeOp::RetractSingle {
                    subject,
                    predicate,
                    asserter,
                } => {
                    let closed = self
                        .storage
                        .close(subject, predicate, |f| f.asserter == asserter, commit_time)
                        .map_err(TxError::from)?;
                    changes.push(CommitChange::Retracted { closed });
                }

                TentativeOp::RetractSet {
                    subject,
                    predicate,
                    asserter,
                    object,
                } => {
                    let closed = self
                        .storage
                        .close(
                            subject,
                            predicate,
                            |f| f.asserter == asserter && f.object == object,
                            commit_time,
                        )
                        .map_err(TxError::from)?;
                    changes.push(CommitChange::Retracted { closed });
                }

                TentativeOp::RetractIndexed {
                    subject,
                    predicate,
                    asserter,
                    index_key,
                } => {
                    let closed = self
                        .storage
                        .close(
                            subject,
                            predicate,
                            |f| f.asserter == asserter && f.index_key == Some(index_key),
                            commit_time,
                        )
                        .map_err(TxError::from)?;
                    changes.push(CommitChange::Retracted { closed });
                }
            }
        }

        Ok(changes)
    }
}

impl Default for InMemoryTripleStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Queryable<StoredFact> for InMemoryTripleStore {
    fn query_open(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.storage.query_open(subject, predicate, pattern)
    }

    fn query_at(
        &self,
        subject: EntityId,
        predicate: Predicate,
        tx_time: Timestamp,
        valid_time: Timestamp,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.storage.query_at(subject, predicate, tx_time, valid_time, pattern)
    }

    fn history(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.storage.history(subject, predicate, pattern)
    }
}

impl TripleStore<StoredFact> for InMemoryTripleStore {
    type Tx<'a> = InMemoryTransaction<'a>;

    fn new_transaction(&self) -> InMemoryTransaction<'_> {
        InMemoryTransaction {
            store: self,
            tx_id: TxId(self.next_tx.fetch_add(1, Ordering::Relaxed)),
            ops: Vec::new(),
        }
    }

    fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::CommitChange;
    use crate::term::Term;
    use crate::time::Timestamp;

    // ── Test clock ──────────────────────────────────────────────────────────

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

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn mk_fact(subject: u64, asserter: u64, value: i64) -> Fact {
        Fact {
            subject: EntityId(subject),
            predicate: Predicate(0),
            object: Term::Int(value),
            asserter: EntityId(asserter),
            valid: None,
        }
    }

    fn key(v: f64) -> OrderingKey {
        OrderingKey::new(v).unwrap()
    }

    // ── Construction ────────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "shard count must be >= 1")]
    fn with_shards_zero_panics() {
        InMemoryTripleStore::with_shards(0);
    }

    // ── Transaction lifecycle ────────────────────────────────────────────────

    #[test]
    fn new_transactions_get_distinct_tx_ids() {
        let s = InMemoryTripleStore::new();
        let mut tx1 = s.new_transaction();
        let mut tx2 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 1));
        tx2.assert_single(mk_fact(2, 2, 2));
        let c1 = tx1.commit().unwrap();
        let c2 = tx2.commit().unwrap();
        let id1 = match &c1[0] {
            CommitChange::Asserted { fact } => fact.tx_id,
            _ => panic!(),
        };
        let id2 = match &c2[0] {
            CommitChange::Asserted { fact } => fact.tx_id,
            _ => panic!(),
        };
        assert!(id1.0 < id2.0);
    }

    #[test]
    fn commit_empty_tx_yields_no_changes() {
        let s = InMemoryTripleStore::new();
        let tx = s.new_transaction();
        assert!(tx.commit().unwrap().is_empty());
    }

    #[test]
    fn rollback_means_no_changes_in_store() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.rollback();

        // The rolled-back assertion should leave no trace: a fresh assert
        // on the same key yields Asserted (not Updated).
        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        let changes = tx2.commit().unwrap();
        assert!(matches!(&changes[0], CommitChange::Asserted { .. }));
    }

    #[test]
    fn two_transactions_are_isolated() {
        let s = InMemoryTripleStore::new();
        let mut tx_a = s.new_transaction();
        let mut tx_b = s.new_transaction();
        tx_a.assert_single(mk_fact(1, 1, 10));
        tx_b.assert_single(mk_fact(2, 2, 20));
        let ca = tx_a.commit().unwrap();
        let cb = tx_b.commit().unwrap();
        assert_eq!(ca.len(), 1);
        assert_eq!(cb.len(), 1);
        let fa = match &ca[0] {
            CommitChange::Asserted { fact } => fact,
            _ => panic!(),
        };
        let fb = match &cb[0] {
            CommitChange::Asserted { fact } => fact,
            _ => panic!(),
        };
        assert_eq!(fa.subject, EntityId(1));
        assert_eq!(fb.subject, EntityId(2));
    }

    // ── Single cardinality ───────────────────────────────────────────────────

    #[test]
    fn commit_single_with_no_prior_yields_asserted() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());
        let mut tx = s.new_transaction();
        tx.assert_single(mk_fact(1, 1, 42));
        let changes = tx.commit().unwrap();

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
        assert!(fact.is_internal());
    }

    #[test]
    fn commit_single_with_prior_open_supersedes() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        assert!(matches!(
            tx1.commit().unwrap()[0],
            CommitChange::Asserted { .. }
        ));

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 43));
        let changes = tx2.commit().unwrap();

        let CommitChange::Updated { closed, opened } = &changes[0] else {
            panic!("expected Updated, got {:?}", changes[0]);
        };
        // Prior fact: asserted interval closed at 200; valid preserved.
        assert_eq!(closed.object, Term::Int(42));
        assert_eq!(closed.asserted.end, Some(Timestamp::from_nanos(200)));
        assert!(closed.valid.is_open());
        // New fact: asserted interval opened at 200 (meets prior's close).
        assert_eq!(opened.object, Term::Int(43));
        assert_eq!(opened.asserted.start, Timestamp::from_nanos(200));
        assert!(opened.asserted.is_open());
        assert_eq!(opened.valid.start, Timestamp::from_nanos(200));
    }

    #[test]
    fn commit_collapses_multi_assert_to_same_key_to_last() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_single(mk_fact(1, 1, 42));
        tx.assert_single(mk_fact(1, 1, 43));
        tx.assert_single(mk_fact(1, 1, 44));
        let changes = tx.commit().unwrap();

        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert_eq!(fact.object, Term::Int(44));
    }

    #[test]
    fn commit_collapses_per_key_independently() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_single(mk_fact(1, 1, 42));
        tx.assert_single(mk_fact(2, 2, 10));
        tx.assert_single(mk_fact(1, 1, 43));
        tx.assert_single(mk_fact(2, 2, 20));
        let changes = tx.commit().unwrap();

        assert_eq!(changes.len(), 2);
        let by_subject: HashMap<EntityId, &StoredFact> = changes
            .iter()
            .map(|c| match c {
                CommitChange::Asserted { fact } => (fact.subject, fact),
                _ => panic!("expected only Asserted"),
            })
            .collect();
        assert_eq!(by_subject[&EntityId(1)].object, Term::Int(43));
        assert_eq!(by_subject[&EntityId(2)].object, Term::Int(20));
    }

    #[test]
    fn commit_separate_asserters_do_not_supersede() {
        let s = InMemoryTripleStore::new();

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42)); // asserter=1 (internal)
        s.new_transaction(); // ensure tx ids are distinct
        tx1.commit().unwrap();

        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 2, 99)); // asserter=2 (external)
        let changes = tx2.commit().unwrap();

        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert!(fact.is_external());
        assert_eq!(fact.asserter, EntityId(2));
    }

    // ── Set cardinality ──────────────────────────────────────────────────────

    #[test]
    fn commit_set_first_assert_yields_asserted() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 42));
        let changes = tx.commit().unwrap();

        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert_eq!(fact.object, Term::Int(42));
        assert!(fact.valid.is_open());
        assert_eq!(fact.index_key, None);
    }

    #[test]
    fn commit_set_already_open_is_noop() {
        let s = InMemoryTripleStore::new();
        let mut tx1 = s.new_transaction();
        tx1.assert_set(mk_fact(1, 1, 42));
        assert_eq!(tx1.commit().unwrap().len(), 1);

        let mut tx2 = s.new_transaction();
        tx2.assert_set(mk_fact(1, 1, 42));
        assert!(
            tx2.commit().unwrap().is_empty(),
            "re-assert of open Set fact must be no-op"
        );
    }

    #[test]
    fn commit_set_distinct_objects_coexist() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| matches!(c, CommitChange::Asserted { .. }))
        );
    }

    #[test]
    fn commit_set_deduplicates_same_object_within_tx() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 42));
        tx.assert_set(mk_fact(1, 1, 42));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert_eq!(fact.object, Term::Int(42));
    }

    #[test]
    fn commit_set_different_asserters_do_not_interfere() {
        let s = InMemoryTripleStore::new();
        let mut tx1 = s.new_transaction();
        tx1.assert_set(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        let mut tx2 = s.new_transaction();
        tx2.assert_set(mk_fact(1, 2, 42)); // asserter=2
        let changes = tx2.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert_eq!(fact.asserter, EntityId(2));
    }

    // ── Indexed cardinality ──────────────────────────────────────────────────

    #[test]
    fn commit_indexed_first_assert_yields_asserted() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_indexed(mk_fact(1, 1, 42), key(0.0));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert_eq!(fact.object, Term::Int(42));
        assert_eq!(fact.index_key, Some(key(0.0)));
    }

    #[test]
    fn commit_indexed_same_slot_supersedes() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_indexed(mk_fact(1, 1, 10), key(1.0));
        assert!(matches!(
            tx1.commit().unwrap()[0],
            CommitChange::Asserted { .. }
        ));

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_indexed(mk_fact(1, 1, 20), key(1.0));
        let changes = tx2.commit().unwrap();

        let CommitChange::Updated { closed, opened } = &changes[0] else {
            panic!("expected Updated, got {:?}", changes[0]);
        };
        assert_eq!(closed.object, Term::Int(10));
        assert_eq!(closed.asserted.end, Some(Timestamp::from_nanos(200)));
        assert!(closed.valid.is_open());
        assert_eq!(opened.object, Term::Int(20));
        assert_eq!(opened.asserted.start, Timestamp::from_nanos(200));
        assert!(opened.asserted.is_open());
        assert_eq!(opened.index_key, Some(key(1.0)));
    }

    #[test]
    fn commit_indexed_distinct_keys_coexist() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_indexed(mk_fact(1, 1, 10), key(0.0));
        tx.assert_indexed(mk_fact(1, 1, 20), key(1.0));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 2);
        let keys: Vec<_> = changes
            .iter()
            .map(|c| match c {
                CommitChange::Asserted { fact } => fact.index_key,
                _ => panic!(),
            })
            .collect();
        assert!(keys.contains(&Some(key(0.0))));
        assert!(keys.contains(&Some(key(1.0))));
    }

    #[test]
    fn commit_indexed_collapses_same_slot_within_tx() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_indexed(mk_fact(1, 1, 10), key(0.0));
        tx.assert_indexed(mk_fact(1, 1, 20), key(0.0));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Asserted { fact } = &changes[0] else {
            panic!()
        };
        assert_eq!(fact.object, Term::Int(20));
    }

    // ── Retract: Single ──────────────────────────────────────────────────────

    #[test]
    fn commit_retract_single_closes_asserted_preserves_valid() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        let changes = tx2.commit().unwrap();

        let CommitChange::Retracted { closed } = &changes[0] else {
            panic!("expected Retracted, got {:?}", changes[0]);
        };
        assert_eq!(closed.object, Term::Int(42));
        assert_eq!(closed.asserted.end, Some(Timestamp::from_nanos(200)));
        assert!(closed.valid.is_open());
    }

    #[test]
    fn commit_retract_single_not_found_errors() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.retract_single(EntityId(1), Predicate(0), EntityId(1));
        assert_eq!(tx.commit().unwrap_err(), TxError::OpenFactNotFound);
    }

    #[test]
    fn commit_retract_single_leaves_no_open_fact() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        tx2.commit().unwrap();

        // Subsequent assert should see no prior open fact → Asserted not Updated.
        clock.set(300);
        let mut tx3 = s.new_transaction();
        tx3.assert_single(mk_fact(1, 1, 99));
        let changes = tx3.commit().unwrap();
        assert!(matches!(&changes[0], CommitChange::Asserted { .. }));
    }

    #[test]
    fn commit_collapses_duplicate_retracts_within_tx() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        let changes = tx2.commit().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], CommitChange::Retracted { .. }));
    }

    // ── Retract: Set ─────────────────────────────────────────────────────────

    #[test]
    fn commit_retract_set_removes_matching_object_leaves_others() {
        let s = InMemoryTripleStore::new();

        let mut tx1 = s.new_transaction();
        tx1.assert_set(mk_fact(1, 1, 10));
        tx1.assert_set(mk_fact(1, 1, 20));
        tx1.commit().unwrap();

        let mut tx2 = s.new_transaction();
        tx2.retract_set(EntityId(1), Predicate(0), EntityId(1), Term::Int(10));
        let changes = tx2.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Retracted { closed } = &changes[0] else {
            panic!()
        };
        assert_eq!(closed.object, Term::Int(10));
        assert!(closed.valid.is_open());

        // object=20 still asserted-open: re-asserting is a no-op.
        let mut tx3 = s.new_transaction();
        tx3.assert_set(mk_fact(1, 1, 20));
        assert!(tx3.commit().unwrap().is_empty());
    }

    #[test]
    fn commit_retract_set_not_found_errors() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.retract_set(EntityId(1), Predicate(0), EntityId(1), Term::Int(42));
        assert_eq!(tx.commit().unwrap_err(), TxError::OpenFactNotFound);
    }

    // ── Retract: Indexed ─────────────────────────────────────────────────────

    #[test]
    fn commit_retract_indexed_removes_slot_leaves_others() {
        let s = InMemoryTripleStore::new();

        let mut tx1 = s.new_transaction();
        tx1.assert_indexed(mk_fact(1, 1, 10), key(0.0));
        tx1.assert_indexed(mk_fact(1, 1, 20), key(1.0));
        tx1.commit().unwrap();

        let mut tx2 = s.new_transaction();
        tx2.retract_indexed(EntityId(1), Predicate(0), EntityId(1), key(0.0));
        let changes = tx2.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Retracted { closed } = &changes[0] else {
            panic!()
        };
        assert_eq!(closed.object, Term::Int(10));
        assert_eq!(closed.index_key, Some(key(0.0)));
        assert!(closed.valid.is_open());
    }

    #[test]
    fn commit_retract_indexed_not_found_errors() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.retract_indexed(EntityId(1), Predicate(0), EntityId(1), key(0.0));
        assert_eq!(tx.commit().unwrap_err(), TxError::OpenFactNotFound);
    }

    // ── Query: query_open ────────────────────────────────────────────────────

    #[test]
    fn query_open_returns_only_asserted_open_facts() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        // Supersede at t=200: first fact is closed, second is open.
        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        tx2.commit().unwrap();

        let results = s.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(99));
        assert!(results[0].asserted.is_open());
    }

    #[test]
    fn query_open_empty_when_no_facts() {
        let s = InMemoryTripleStore::new();
        let results = s.query_open(EntityId(1), Predicate(0), None);
        assert!(results.is_empty());
    }

    #[test]
    fn query_open_empty_after_retract() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        tx2.commit().unwrap();

        assert!(s.query_open(EntityId(1), Predicate(0), None).is_empty());
    }

    #[test]
    fn query_open_set_returns_all_open_members() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        tx.assert_set(mk_fact(1, 1, 30));
        tx.commit().unwrap();

        let results = s.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(results.len(), 3);
    }

    // ── Query: query_at ──────────────────────────────────────────────────────

    #[test]
    fn query_at_returns_fact_visible_at_both_tx_and_valid_time() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        // Fact asserted at t=100, open. Valid at tx_time=150, valid_time=150.
        let t150 = Timestamp::from_nanos(150);
        let results = s.query_at(EntityId(1), Predicate(0), t150, t150, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(42));
    }

    #[test]
    fn query_at_excludes_fact_not_yet_asserted() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        // Query at t=50, before the fact was asserted.
        let t50 = Timestamp::from_nanos(50);
        let results = s.query_at(EntityId(1), Predicate(0), t50, t50, None);
        assert!(results.is_empty());
    }

    #[test]
    fn query_at_sees_closed_fact_at_its_asserted_time() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        tx2.commit().unwrap();

        // At t=150 (between 100 and 200), the original fact was asserted-open.
        let t150 = Timestamp::from_nanos(150);
        let results = s.query_at(EntityId(1), Predicate(0), t150, t150, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(42));

        // At t=250 only the replacement is asserted-open.
        let t250 = Timestamp::from_nanos(250);
        let results = s.query_at(EntityId(1), Predicate(0), t250, t250, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(99));
    }

    // ── Query: history ───────────────────────────────────────────────────────

    #[test]
    fn history_returns_all_facts_open_and_closed() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        tx2.commit().unwrap();

        let all = s.history(EntityId(1), Predicate(0), None);
        assert_eq!(all.len(), 2);
        let objects: Vec<_> = all.iter().map(|f| &f.object).collect();
        assert!(objects.contains(&&Term::Int(42)));
        assert!(objects.contains(&&Term::Int(99)));
    }

    #[test]
    fn history_empty_when_no_facts() {
        let s = InMemoryTripleStore::new();
        assert!(s.history(EntityId(1), Predicate(0), None).is_empty());
    }

    // ── Query: query_as_of ───────────────────────────────────────────────────

    #[test]
    fn query_as_of_none_resolves_to_clock_now() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        // AsOf::default() has both fields None — should resolve to clock's now (100),
        // and the fact asserted at 100 is visible there.
        use crate::pattern::AsOf;
        let results = s.query_as_of(EntityId(1), Predicate(0), AsOf::default(), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(42));
    }

    #[test]
    fn query_as_of_explicit_timestamps_used_directly() {
        let clock = TickClock::new(100);
        let s = InMemoryTripleStore::with_clock(clock.clone());

        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        clock.set(200);
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        tx2.commit().unwrap();

        use crate::pattern::AsOf;
        // Explicit tx_time=150 (between the two commits), valid_time=150.
        let as_of = AsOf {
            tx_time: Some(Timestamp::from_nanos(150)),
            valid_time: Some(Timestamp::from_nanos(150)),
        };
        let results = s.query_as_of(EntityId(1), Predicate(0), as_of, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(42));
    }

    // ── Query: pattern filtering ─────────────────────────────────────────────

    #[test]
    fn query_open_filters_by_object() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        tx.commit().unwrap();

        use crate::pattern::{Match, MatchAsserter, Pattern};
        let pat = Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Is(Term::Int(10)),
            asserter: MatchAsserter::Any,
        };
        let results = s.query_open(EntityId(1), Predicate(0), Some(&pat));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(10));
    }

    #[test]
    fn query_open_filters_internal_vs_external_asserter() {
        let s = InMemoryTripleStore::new();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10)); // internal: subject=1, asserter=1
        tx.assert_set(mk_fact(1, 2, 20)); // external: subject=1, asserter=2
        tx.commit().unwrap();

        use crate::pattern::{Match, MatchAsserter, Pattern};
        let internal_pat = Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Any,
            asserter: MatchAsserter::SameAsSubject,
        };
        let results = s.query_open(EntityId(1), Predicate(0), Some(&internal_pat));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].asserter, EntityId(1));

        let external_pat = Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Any,
            asserter: MatchAsserter::DifferentFromSubject,
        };
        let results = s.query_open(EntityId(1), Predicate(0), Some(&external_pat));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].asserter, EntityId(2));
    }
}
