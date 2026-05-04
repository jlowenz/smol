//! TripleStore trait — top-level composition of storage and transaction management.
//!
//! `TripleStore<TFact>` is the primary entry point for application code. It
//! owns a `Storage<TFact>` backend and creates `Transaction<TFact>` handles.
//! Query and change-handler methods are deferred to a later phase; this trait
//! currently exposes only transaction creation.

use crate::transaction::Transaction;

/// The top-level triple store interface.
///
/// Implementations own a `Storage<TFact>` backend and a clock. Application
/// code interacts with the store exclusively through transactions:
///
/// ```ignore
/// let mut tx = store.new_transaction();
/// tx.assert_single(fact);
/// let changes = tx.commit()?;
/// ```
///
/// Query operations (pass-through to storage) and change-handler registration
/// are deferred to a later phase.
pub trait TripleStore<TFact>: Send + Sync {
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
}
