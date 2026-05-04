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
    /// A fact was asserted with no prior asserted-open fact at its identity key.
    Asserted { fact: Fact },
    /// A prior asserted-open fact was superseded: its `asserted` interval was
    /// closed at `commit_time` and a new fact was opened (Single or Indexed).
    Updated { closed: Fact, opened: Fact },
    /// A prior asserted-open fact was explicitly retracted: its `asserted`
    /// interval was closed at `commit_time`. The `valid` interval is preserved.
    Retracted { closed: Fact },
}

pub trait FactStore: Send + Sync {
    // === Mutations (must be within a transaction) ===

    /// Assert a fact under `Cardinality::Single`.
    ///
    /// Requires: `tx_id` is active.
    /// Ensures: any prior open fact at `(subject, predicate, asserter)` is
    /// closed at the new fact's `valid.start`; a new open fact is added.
    fn assert_single(&self, tx_id: TxId, fact: PendingFact) -> Result<(), Error>;

    /// Assert a fact under `Cardinality::Set`.
    ///
    /// Requires: `tx_id` is active.
    /// Ensures: if `(subject, predicate, asserter, object)` is already open,
    /// no change; else a new open fact is added. Distinct objects coexist.
    fn assert_set(&self, tx_id: TxId, fact: PendingFact) -> Result<(), Error>;

    /// Assert a fact under `Cardinality::Indexed`.
    ///
    /// Requires: `tx_id` is active.
    /// Ensures: any prior open fact at `(subject, predicate, asserter,
    /// index_key)` is closed at the new fact's `valid.start`; a new open fact
    /// with the given `index_key` is added.
    fn assert_indexed(
        &self,
        tx_id: TxId,
        fact: PendingFact,
        index_key: OrderingKey,
    ) -> Result<(), Error>;

    /// Retract under `Cardinality::Single`: close the asserted interval of the
    /// unique asserted-open fact at `(subject, predicate, asserter)`.
    ///
    /// Retraction only closes the `asserted` interval; the `valid` interval is
    /// preserved as part of the immutable historical record. The closed interval
    /// meets the successor's open interval at `commit_time`.
    ///
    /// Requires: an asserted-open fact exists at that key; `tx_id` is active.
    /// Ensures: `fact.asserted.end == Some(commit_time)`; fact remains queryable
    /// as historical knowledge for any assert-time `t < commit_time`.
    fn retract_single(
        &self,
        tx_id: TxId,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
    ) -> Result<(), Error>;

    /// Retract under `Cardinality::Set`: close the asserted interval of the
    /// open fact matching `(subject, predicate, asserter, object)`.
    ///
    /// Requires: an asserted-open fact exists at that key; `tx_id` is active.
    /// Ensures: `fact.asserted.end == Some(commit_time)`; `valid` preserved.
    fn retract_set(
        &self,
        tx_id: TxId,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: Term,
    ) -> Result<(), Error>;

    /// Retract under `Cardinality::Indexed`: close the asserted interval of
    /// the open fact at `(subject, predicate, asserter, index_key)`.
    ///
    /// Requires: an asserted-open fact exists at that key; `tx_id` is active.
    /// Ensures: `fact.asserted.end == Some(commit_time)`; `valid` preserved.
    fn retract_indexed(
        &self,
        tx_id: TxId,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
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
