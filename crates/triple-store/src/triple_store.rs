//! TripleStore trait — dyn-compatible top-level store interface.
//!
//! `TripleStore<TFact>` is the primary entry point for application code. It
//! creates `Transaction<TFact>` handles, answers queries (via the `Queryable`
//! supertrait), and manages change-event subscriptions. Fixing `TFact` to
//! `StoredFact` yields a fully dyn-compatible trait
//! (`Arc<dyn TripleStore<StoredFact>>`).

use std::io;
use std::sync::Arc;

use crate::change::{EventFilter, Subscription};
use crate::fact::BitemporalFact;
use crate::id::{EntityId, SubscriptionId};
use crate::pattern::{AsOf, Pattern};
use crate::predicate_registry::{PredicateRegistry, Visibility};
use crate::predicates::Cardinality;
use crate::queryable::Queryable;
use crate::term::Predicate;
use crate::time::Timestamp;
use crate::transaction::Transaction;
use crate::value_store::StoredValue;

/// The top-level triple store interface.
///
/// Inherits clock-free read methods (`query_open`, `query_at`, `history`) from
/// `Queryable<TFact>`. All methods use only concrete types so
/// `TripleStore<StoredFact>` is fully dyn-compatible. Application code
/// interacts through transactions, queries, and subscriptions:
///
/// ```ignore
/// let mut tx = store.new_transaction();
/// tx.assert_single(fact);
/// let changes = tx.commit()?;
///
/// let results = store.query_open(subject, predicate, None);
/// ```
pub trait TripleStore<TFact: BitemporalFact>: Queryable<TFact> + Send + Sync {
    /// Begin a new transaction.
    ///
    /// Ensures: returned transaction has an empty buffer and a unique `TxId`.
    fn new_transaction(&self) -> Transaction<TFact>;

    /// Allocate a fresh, unique `TxId` without opening a transaction buffer.
    ///
    /// Ensures: the returned id is distinct from every id previously returned by
    ///   `new_transaction`, `new_transaction_with_id` (via prior `alloc_tx_id`),
    ///   or `alloc_tx_id` on this store instance.
    fn alloc_tx_id(&self) -> crate::id::TxId;

    /// Begin a new transaction bound to a caller-supplied `TxId`.
    ///
    /// Requires: `tx_id` was obtained from `alloc_tx_id` on this store instance
    ///   (so it does not collide with any auto-allocated id).
    /// Ensures: returned transaction has an empty buffer and commits under `tx_id`.
    ///   Used for multi-participant transactions where several buffers must share
    ///   one `TxId` so their commit is atomic.
    fn new_transaction_with_id(&self, tx_id: crate::id::TxId) -> Transaction<TFact>;

    /// The current time according to the store's clock.
    fn now(&self) -> Timestamp;

    /// Register a pattern subscription with an optional visibility filter.
    ///
    /// Ensures: returned `Subscription` id is unique within this store instance.
    ///   If `filter` is `Some(f)`, only events for which `f(event)` returns `true`
    ///   are delivered. If `None`, all pattern-matching events are delivered.
    fn subscribe(&self, pattern: Pattern, filter: Option<EventFilter>) -> Subscription;

    /// Cancel a subscription. No-op if `id` is unknown.
    fn unsubscribe(&self, id: SubscriptionId);

    /// Look up a previously committed value object by its content-hash id.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
    /// Ensures: `Some(v)` iff the value was committed in this session or recovered
    ///   from a prior session; `None` otherwise.
    fn lookup_value(&self, id: EntityId) -> Option<Arc<StoredValue>>;

    /// Persist a declaration record so it survives process restart.
    ///
    /// Requires: caller serialises concurrent declarations (e.g. holds a
    ///   declare lock) before calling.
    /// Ensures: on `Ok(())`, the declaration is durable. Default impl is a no-op.
    fn persist_declaration(
        &self,
        _name: &str,
        _visibility: Visibility,
        _cardinality: Cardinality,
    ) -> io::Result<()> {
        Ok(())
    }

    /// Return the `PredicateRegistry` recovered from a prior session.
    ///
    /// Ensures: returned registry contains exactly the declarations replayed
    ///   from the WAL; empty for non-durable backends.
    fn recovered_registry(&self) -> PredicateRegistry {
        PredicateRegistry::new()
    }

    /// Return the first entity ID to use after recovering from a prior session.
    ///
    /// Ensures: returned value is strictly greater than every entity ID
    ///   committed to the store in any prior session; returns 1 for non-durable
    ///   backends.
    fn recovered_next_entity(&self) -> u64 {
        1
    }

    /// Bitemporal point-in-time query, resolving `AsOf::None` fields against
    /// `self.now()`.
    ///
    /// `as_of.tx_time = None` means "as of the current transaction time" (now).
    /// `as_of.valid_time = None` means "as of the current valid time" (now).
    ///
    /// Ensures: equivalent to calling `query_at` with
    ///   `tx_time = as_of.tx_time.unwrap_or_else(|| self.now())`
    ///   `valid_time = as_of.valid_time.unwrap_or_else(|| self.now())`.
    fn query_as_of(
        &self,
        subject: EntityId,
        predicate: Predicate,
        as_of: AsOf,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact> {
        let now = self.now();
        let tx_time = as_of.tx_time.unwrap_or(now);
        let valid_time = as_of.valid_time.unwrap_or(now);
        self.query_at(subject, predicate, tx_time, valid_time, pattern)
    }
}
