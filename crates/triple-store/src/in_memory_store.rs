//! In-memory `FactStore` backend (v0).
//!
//! Per PRD §3.3 / §6: prioritizes correctness and concurrent access without a
//! global lock. Sharding by `EntityId` (default 16 shards) keeps lock
//! contention proportional to the number of entities sharing a shard, not the
//! total concurrent entity count.

// Temporary: storage fields and helpers are introduced in this chunk and
// consumed by mutation methods in subsequent chunks. Remove this allow once
// the trait impl is in place.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::fact::{Fact, PendingFact};
use crate::fact_store::Error;
use crate::id::{EntityId, TxId};
use crate::term::Predicate;

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
/// the store interprets these in order to produce the new committed state.
///
/// More variants will be added as the trait grows: `Retract { ... }` when the
/// retract chunk lands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TentativeOp {
    Assert(PendingFact),
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
}

impl InMemoryStore {
    /// Construct with the default shard count (16).
    pub fn new() -> Self {
        Self::with_shards(DEFAULT_SHARD_COUNT)
    }

    /// Construct with an explicit shard count.
    ///
    /// Requires: `shards >= 1`.
    pub fn with_shards(shards: usize) -> Self {
        assert!(shards >= 1, "shard count must be >= 1");
        let shards = (0..shards).map(|_| Shard::default()).collect();
        Self {
            shards,
            tentative: TentativeBuffer::default(),
            next_tx: AtomicU64::new(1),
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
        entries.push(TentativeOp::Assert(fact));
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
}
