//! Transaction types — cardinality-aware fact accumulation and commit.
//!
//! `Transaction<TFact>` accumulates assertions and retractions in a private
//! buffer, then either commits them atomically to the store backend via
//! `Applier<TFact>` or rolls back (discarding the buffer). Commit and rollback
//! consume `self`, making post-terminal use a compile-time error.

use std::sync::Arc;

use crate::fact::Fact;
use crate::id::{EntityId, TxId};
use crate::predicates::OrderingKey;
use crate::storage::{CommitChange, Error as StorageError};
use crate::term::{Predicate, Term};

/// Errors from transaction operations.
#[derive(Debug, PartialEq, Eq)]
pub enum TxError {
    /// A retract found no asserted-open fact at the target identity key.
    OpenFactNotFound,
    /// Backend-internal failure.
    Internal(&'static str),
}

impl From<StorageError> for TxError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::FactNotFound => TxError::OpenFactNotFound,
            StorageError::Internal(s) => TxError::Internal(s),
        }
    }
}

/// One operation buffered in a transaction. Carries cardinality in its variant
/// so the commit path can dispatch without a predicate registry (PRD §3.2).
///
/// Precondition (checked via `debug_assert` in `Applier::apply`):
/// no identity key appears in both an Assert and a Retract in the same
/// transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TentativeOp {
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

/// Commit-callback seam between `Transaction<TFact>` and the store backend.
///
/// `Transaction<TFact>` holds `Arc<dyn Applier<TFact>>`. When `commit` is
/// called the transaction delegates here, keeping `Transaction` independent
/// of any concrete store type.
pub(crate) trait Applier<TFact>: Send + Sync {
    /// Apply `ops` as transaction `tx_id` and return the resulting change set.
    ///
    /// Requires: `tx_id` is unique within this store instance.
    /// Ensures: all ops are applied atomically (per shard); change events are
    ///   dispatched to matching subscribers before returning.
    fn apply(
        &self,
        tx_id: TxId,
        ops: Vec<TentativeOp>,
    ) -> Result<Vec<CommitChange<TFact>>, TxError>;
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
/// asserted-interval semantics). Enforced via `debug_assert` in `Applier::apply`.
pub struct Transaction<TFact> {
    applier: Arc<dyn Applier<TFact>>,
    tx_id: TxId,
    ops: Vec<TentativeOp>,
}

impl<TFact> Transaction<TFact> {
    pub(crate) fn new(applier: Arc<dyn Applier<TFact>>, tx_id: TxId) -> Self {
        Self {
            applier,
            tx_id,
            ops: Vec::new(),
        }
    }

    // === Buffer assertions (infallible) ===

    /// Buffer a `Single`-cardinality assertion.
    ///
    /// Ensures: at commit, any prior asserted-open fact at
    ///   `(subject, predicate, asserter)` is superseded; a new fact is opened.
    pub fn assert_single(&mut self, fact: Fact) {
        self.ops.push(TentativeOp::AssertSingle(fact));
    }

    /// Buffer a `Set`-cardinality assertion.
    ///
    /// Ensures: at commit, if `(subject, predicate, asserter, object)` is
    ///   already asserted-open the operation is a no-op; otherwise a new fact
    ///   is opened.
    pub fn assert_set(&mut self, fact: Fact) {
        self.ops.push(TentativeOp::AssertSet(fact));
    }

    /// Buffer an `Indexed`-cardinality assertion.
    ///
    /// Ensures: at commit, any prior asserted-open fact at
    ///   `(subject, predicate, asserter, index_key)` is superseded.
    pub fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey) {
        self.ops
            .push(TentativeOp::AssertIndexed { fact, index_key });
    }

    // === Buffer retractions (infallible) ===

    /// Buffer a `Single`-cardinality retraction.
    ///
    /// Ensures: at commit, the asserted-open fact at
    ///   `(subject, predicate, asserter)` has its `asserted` interval closed.
    ///   Returns `TxError::OpenFactNotFound` at commit if no such fact exists.
    pub fn retract_single(&mut self, subject: EntityId, predicate: Predicate, asserter: EntityId) {
        self.ops.push(TentativeOp::RetractSingle {
            subject,
            predicate,
            asserter,
        });
    }

    /// Buffer a `Set`-cardinality retraction.
    pub fn retract_set(
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

    /// Buffer an `Indexed`-cardinality retraction.
    pub fn retract_indexed(
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

    // === Terminal operations (consume self) ===

    /// Apply all buffered operations atomically.
    ///
    /// Ensures: all ops committed or none (per shard); change events dispatched
    ///   to matching subscribers before returning.
    pub fn commit(self) -> Result<Vec<CommitChange<TFact>>, TxError> {
        self.applier.apply(self.tx_id, self.ops)
    }

    /// Discard all buffered operations. Consumes the transaction.
    pub fn rollback(self) {
        // Consuming self drops `ops`; no storage side-effects.
    }
}
