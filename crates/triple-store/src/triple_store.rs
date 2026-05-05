//! TripleStore trait — top-level composition of storage and transaction management.
//!
//! `TripleStore<TFact>` is the primary entry point for application code. It
//! owns a `Storage<TFact>` backend and creates `Transaction<TFact>` handles.
//! It is also a `Queryable<TFact>`, adding `query_as_of` for callers that
//! want `AsOf::None` resolved against the store's clock.

use crate::fact::BitemporalFact;
use crate::id::EntityId;
use crate::pattern::Pattern;
use crate::queryable::Queryable;
use crate::term::Predicate;
use crate::pattern::AsOf;
use crate::time::Timestamp;
use crate::transaction::Transaction;

/// The top-level triple store interface.
///
/// Implementations own a `Storage<TFact>` backend and a clock. Application
/// code interacts with the store through transactions and queries:
///
/// ```ignore
/// let mut tx = store.new_transaction();
/// tx.assert_single(fact);
/// let changes = tx.commit()?;
///
/// let results = store.query_as_of(subject, predicate, AsOf::default(), None);
/// ```
pub trait TripleStore<TFact: BitemporalFact>: Queryable<TFact> + Send + Sync {
    /// The transaction type produced by this store.
    ///
    /// The `'a` lifetime binds the transaction to the store: the transaction
    /// must not outlive the store that created it.
    type Tx<'a>: Transaction<TFact>
    where
        Self: 'a;

    /// Begin a new transaction.
    ///
    /// Ensures: the returned transaction has an empty buffer and a unique
    /// `TxId` (stamped on facts at commit time). The transaction borrows
    /// from `self` and must be committed or rolled back before `self` is dropped.
    fn new_transaction(&self) -> Self::Tx<'_>;

    /// The current time according to the store's clock.
    fn now(&self) -> Timestamp;

    /// Bitemporal point-in-time query, resolving `AsOf::None` fields against `self.now()`.
    ///
    /// `as_of.tx_time = None` means "as of the current transaction time" (now).
    /// `as_of.valid_time = None` means "as of the current valid time" (now).
    ///
    /// Delegates to `Queryable::query_at` after resolving timestamps.
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
