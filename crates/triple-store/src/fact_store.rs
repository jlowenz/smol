//! FactStore backend interface — the raw Fact manipulation interface,
//! decoupled from the higher-level user-facing Store.
//!
//! Cardinality enforcement is per-method: callers pick the assert/retract
//! method that matches the predicate's declared cardinality. The Store API
//! (above this trait) consults the predicate registry and dispatches; the
//! FactStore itself stays registry-free (PRD §3.2).

use crate::fact::{Fact, PendingFact};
use crate::id::{EntityId, TxId};
use crate::predicates::OrderingKey;
use crate::term::{Predicate, Term};
use crate::time::Timestamp;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// `tx_id` is not a known active transaction in this store.
    UnknownTransaction(TxId),
    /// Retract target was not found among open facts at the requested key.
    OpenFactNotFound,
    /// Backend-internal failure that callers cannot resolve.
    Internal(&'static str),
}

/// One change produced by `commit`, suitable for the Store API to translate
/// into a `ChangeEvent` for subscribers (PRD §3.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitChange {
    /// A fact was opened with no prior open fact at its identity key.
    Asserted { fact: Fact },
    /// A prior open fact was superseded by a new one
    /// (Single or Indexed cardinality assert).
    Updated { closed: Fact, opened: Fact },
    /// A prior open fact was closed via an explicit retract.
    Retracted { closed: Fact },
}

pub trait FactStore: Send + Sync {
    // === Mutations (must be within a transaction) ===

    /// Assert a fact under `Cardinality::Single`.
    ///
    /// Requires: `tx_id` is active.
    /// Ensures: any prior open fact at `(subject, predicate, asserter)` is
    /// closed at the new fact's `valid.start`; a new open fact is added.
    fn assert_single(&self, tx_id: TxId, fact: &PendingFact) -> Result<(), Error>;

    /// Assert a fact under `Cardinality::Set`.
    ///
    /// Requires: `tx_id` is active.
    /// Ensures: if `(subject, predicate, asserter, object)` is already open,
    /// no change; else a new open fact is added. Distinct objects coexist.
    fn assert_set(&self, tx_id: TxId, fact: &PendingFact) -> Result<(), Error>;

    /// Assert a fact under `Cardinality::Indexed`.
    ///
    /// Requires: `tx_id` is active.
    /// Ensures: any prior open fact at `(subject, predicate, asserter,
    /// index_key)` is closed at the new fact's `valid.start`; a new open fact
    /// with the given `index_key` is added.
    fn assert_indexed(
        &self,
        tx_id: TxId,
        fact: &PendingFact,
        index_key: OrderingKey,
    ) -> Result<(), Error>;

    /// Retract under `Cardinality::Single`: close the unique open fact at
    /// `(subject, predicate, asserter)`.
    ///
    /// Requires: an open fact exists at that key; `tx_id` is active.
    /// Ensures: the open fact's `valid.end` is set to `valid_end` if `Some`,
    /// else to the commit timestamp.
    fn retract_single(
        &self,
        tx_id: TxId,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        valid_end: Option<Timestamp>,
    ) -> Result<(), Error>;

    /// Retract under `Cardinality::Set`: close the open fact matching
    /// `(subject, predicate, asserter, object)`.
    ///
    /// Requires: an open fact exists at that key; `tx_id` is active.
    /// Ensures: the open fact's `valid.end` is set to `valid_end` if `Some`,
    /// else to the commit timestamp.
    fn retract_set(
        &self,
        tx_id: TxId,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: &Term,
        valid_end: Option<Timestamp>,
    ) -> Result<(), Error>;

    /// Retract under `Cardinality::Indexed`: close the open fact at
    /// `(subject, predicate, asserter, index_key)`.
    ///
    /// Requires: an open fact exists at that key; `tx_id` is active.
    /// Ensures: the open fact's `valid.end` is set to `valid_end` if `Some`,
    /// else to the commit timestamp.
    fn retract_indexed(
        &self,
        tx_id: TxId,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
        valid_end: Option<Timestamp>,
    ) -> Result<(), Error>;

    // === Commit / rollback ===

    /// Commit `tx_id`: promote tentative facts to permanent state, set their
    /// `asserted` interval to `[commit_time, _)`, and return the change set.
    ///
    /// Requires: `tx_id` is an active transaction.
    /// Ensures: subsequent reads in any transaction see these facts as
    /// committed; further calls with the same `tx_id` return
    /// `Error::UnknownTransaction`.
    fn commit(&self, tx_id: TxId) -> Result<Vec<CommitChange>, Error>;

    /// Rollback `tx_id`: discard all tentative facts.
    ///
    /// Requires: `tx_id` is an active transaction.
    /// Ensures: no trace of the rolled-back tentative facts remains; further
    /// calls with the same `tx_id` return `Error::UnknownTransaction`.
    fn rollback(&self, tx_id: TxId) -> Result<(), Error>;

    // === Query (deferred to next phase; trait will gain `query`/`history`) ===
}
