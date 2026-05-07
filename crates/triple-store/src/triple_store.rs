//! TripleStore trait — dyn-compatible top-level store interface.
//!
//! `TripleStore<TFact>` is the primary entry point for application code. It
//! creates `Transaction<TFact>` handles, answers queries (via the `Queryable`
//! supertrait), and manages change-event subscriptions. Fixing `TFact` to
//! `StoredFact` yields a fully dyn-compatible trait
//! (`Arc<dyn TripleStore<StoredFact>>`).

use crate::change::{ChangeEvent, Subscription};
use crate::fact::BitemporalFact;
use crate::id::{EntityId, SubscriptionId};
use crate::pattern::{AsOf, Pattern};
use crate::queryable::Queryable;
use crate::term::Predicate;
use crate::time::Timestamp;
use crate::transaction::Transaction;

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

    /// The current time according to the store's clock.
    fn now(&self) -> Timestamp;

    /// Register a pattern subscription with an optional visibility filter.
    ///
    /// Ensures: returned `Subscription` id is unique within this store instance.
    ///   If `filter` is `Some(f)`, only events for which `f(event)` returns `true`
    ///   are delivered. If `None`, all pattern-matching events are delivered.
    fn subscribe(
        &self,
        pattern: Pattern,
        filter: Option<Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync>>,
    ) -> Subscription;

    /// Cancel a subscription. No-op if `id` is unknown.
    fn unsubscribe(&self, id: SubscriptionId);

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
