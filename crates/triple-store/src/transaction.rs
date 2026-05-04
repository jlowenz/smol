//! Transaction trait — cardinality-aware fact accumulation and commit.
//!
//! A `Transaction<TFact>` accumulates assertions and retractions in a private
//! buffer, then either commits them atomically to a `Storage<TFact>` backend or
//! rolls back (discarding the buffer). The caller owns the transaction object;
//! there is no external `TxId` to thread through individual calls. `commit` and
//! `rollback` consume `self`, making post-terminal use a compile-time error.

use crate::fact::Fact;
use crate::id::EntityId;
use crate::predicates::OrderingKey;
use crate::storage::{CommitChange, Error as StorageError};
use crate::term::{Predicate, Term};

/// Errors from transaction operations.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// A retract found no asserted-open fact at the target identity key.
    OpenFactNotFound,
    /// Backend-internal failure.
    Internal(&'static str),
}

impl From<StorageError> for Error {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::FactNotFound => Error::OpenFactNotFound,
            StorageError::Internal(s) => Error::Internal(s),
        }
    }
}

/// A unit of work: buffers assertions and retractions, then commits or rolls
/// back atomically.
///
/// Buffer methods (`assert_*`, `retract_*`) are infallible — they simply record
/// intent. All cardinality enforcement and fact-existence checks occur at
/// `commit` time.
///
/// Precondition: no identity key may appear in both an assert and a retract
/// within the same transaction (same commit timestamp, same slot → undefined
/// asserted-interval semantics). Enforced via `debug_assert` in implementations.
pub trait Transaction<TFact>: Sized {
    // === Buffer assertions (infallible) ===

    /// Buffer a `Single`-cardinality assertion.
    ///
    /// Ensures: at commit, any prior asserted-open fact at
    /// `(subject, predicate, asserter)` is superseded; a new fact is opened.
    fn assert_single(&mut self, fact: Fact);

    /// Buffer a `Set`-cardinality assertion.
    ///
    /// Ensures: at commit, if `(subject, predicate, asserter, object)` is already
    /// asserted-open the operation is a no-op; otherwise a new fact is opened.
    fn assert_set(&mut self, fact: Fact);

    /// Buffer an `Indexed`-cardinality assertion.
    ///
    /// Ensures: at commit, any prior asserted-open fact at
    /// `(subject, predicate, asserter, index_key)` is superseded.
    fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey);

    // === Buffer retractions (infallible) ===

    /// Buffer a `Single`-cardinality retraction.
    ///
    /// Ensures: at commit, the asserted-open fact at
    /// `(subject, predicate, asserter)` has its `asserted` interval closed.
    /// Returns `Error::OpenFactNotFound` at commit if no such fact exists.
    fn retract_single(&mut self, subject: EntityId, predicate: Predicate, asserter: EntityId);

    /// Buffer a `Set`-cardinality retraction.
    fn retract_set(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: Term,
    );

    /// Buffer an `Indexed`-cardinality retraction.
    fn retract_indexed(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
    );

    // === Terminal operations (consume self) ===

    /// Apply all buffered operations to the backing storage and return the
    /// change set. Consumes the transaction; post-commit use is a compile error.
    ///
    /// Ensures: all buffered ops are applied atomically from the caller's
    /// perspective (no other transaction's ops interleave within a shard lock).
    fn commit(self) -> Result<Vec<CommitChange<TFact>>, Error>;

    /// Discard all buffered operations. Consumes the transaction.
    fn rollback(self);
}
