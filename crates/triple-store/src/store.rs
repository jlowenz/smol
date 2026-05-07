//! The client-facing interface to the bi-temporal triple store.

use std::sync::Arc;

use crate::change::Subscription;
use crate::exec_context::{AccessError, ExecContext};
use crate::fact::{Fact, StoredFact};
use crate::id::{EntityId, SubscriptionId};
use crate::in_memory_store::InMemoryTripleStore;
use crate::pattern::Pattern;
use crate::predicate_registry::{PredicateError, PredicateMeta, PredicateRegistry, Visibility};
use crate::predicates::{Cardinality, OrderingKey};
use crate::queryable::Queryable;
use crate::term::{Predicate, Term};
use crate::time::Timestamp;
use crate::transaction::TxError;
use crate::triple_store::TripleStore;

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    AccessDenied {
        predicate: PredicateMeta,
        reason: &'static str,
    },
    FactNotFound,
    Internal(&'static str),
}

impl From<AccessError> for StoreError {
    fn from(err: AccessError) -> StoreError {
        match err {
            AccessError::Denied { predicate, reason } => {
                StoreError::AccessDenied { predicate, reason }
            }
        }
    }
}

impl From<TxError> for StoreError {
    fn from(err: TxError) -> StoreError {
        match err {
            TxError::OpenFactNotFound => StoreError::FactNotFound,
            TxError::Internal(msg) => StoreError::Internal(msg),
        }
    }
}

/// Access-controlled façade over a `TripleStore<StoredFact>`.
///
/// `Store` owns a `PredicateRegistry` and delegates all fact storage to an
/// injected backend. Every mutation and query is gated by an `ExecContext`.
pub struct Store {
    backend: Arc<dyn TripleStore<StoredFact>>,
    registry: PredicateRegistry,
}

impl Store {
    /// Requires: `backend` is a fully initialised `TripleStore<StoredFact>`.
    pub fn new(backend: Arc<dyn TripleStore<StoredFact>>) -> Self {
        Store { backend, registry: PredicateRegistry::new() }
    }

    /// Convenience constructor wrapping an `InMemoryTripleStore`.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(InMemoryTripleStore::new()))
    }

    // ── Predicate registry ────────────────────────────────────────────────────

    /// Declare a predicate with explicit visibility and cardinality.
    ///
    /// Requires: if `name` has been declared before, prior visibility and
    ///   cardinality must equal the values supplied here.
    /// Ensures: `Ok(id)` implies `self.meta(id).visibility == visibility` and
    ///   `self.meta(id).cardinality == cardinality`. Re-declaration with
    ///   identical metadata is idempotent.
    pub fn declare(
        &mut self,
        name: &str,
        visibility: Visibility,
        cardinality: Cardinality,
    ) -> Result<Predicate, PredicateError> {
        self.registry.declare(name, visibility, cardinality)
    }

    /// Look up the handle for a previously-declared or interned predicate name.
    ///
    /// Ensures: `Some(id)` iff `name` is known to this registry; else `None`.
    pub fn lookup(&self, name: &str) -> Option<Predicate> {
        self.registry.lookup(name)
    }

    /// Return metadata for a predicate.
    ///
    /// Ensures: never panics for any `Predicate` issued by this store. Unknown
    ///   predicates return `Visibility::Private` and `Cardinality::Single`.
    pub fn meta(&self, id: Predicate) -> PredicateMeta {
        self.registry.meta(id)
    }

    /// Resolve a predicate handle to its name.
    ///
    /// Ensures: `Some(name)` iff `id` was issued by this store; else `None`.
    pub fn resolve(&self, id: Predicate) -> Option<&str> {
        self.registry.resolve(id)
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────

    /// Register a pattern subscription.
    ///
    /// Ensures: returned `Subscription` id is unique within this store instance.
    pub fn subscribe(&self, pattern: Pattern) -> Subscription {
        self.backend.subscribe(pattern)
    }

    /// Cancel a subscription. No-op if `id` is unknown.
    pub fn unsubscribe(&self, id: SubscriptionId) {
        self.backend.unsubscribe(id);
    }

    // ── Mutations ─────────────────────────────────────────────────────────────

    /// Assert a `Single`-cardinality fact.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, any prior asserted-open fact at
    ///   `(subject, predicate, asserter)` is superseded; a new fact is opened.
    pub fn assert_single(&self, ctx: &ExecContext, fact: Fact) -> Result<(), StoreError> {
        let meta = self.registry.meta(fact.predicate);
        ctx.check_assert(fact.subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.assert_single(fact);
        tx.commit()?;
        Ok(())
    }

    /// Assert a `Set`-cardinality fact.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, the fact is committed; a duplicate
    ///   `(subject, predicate, asserter, object)` is a no-op.
    pub fn assert_set(&self, ctx: &ExecContext, fact: Fact) -> Result<(), StoreError> {
        let meta = self.registry.meta(fact.predicate);
        ctx.check_assert(fact.subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.assert_set(fact);
        tx.commit()?;
        Ok(())
    }

    /// Assert an `Indexed`-cardinality fact.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, any prior asserted-open fact at
    ///   `(subject, predicate, asserter, index_key)` is superseded.
    pub fn assert_indexed(
        &self,
        ctx: &ExecContext,
        fact: Fact,
        index_key: OrderingKey,
    ) -> Result<(), StoreError> {
        let meta = self.registry.meta(fact.predicate);
        ctx.check_assert(fact.subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.assert_indexed(fact, index_key);
        tx.commit()?;
        Ok(())
    }

    /// Retract a `Single`-cardinality fact.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, the asserted-open fact at
    ///   `(subject, predicate, asserter)` has its asserted interval closed.
    ///   Returns `StoreError::FactNotFound` if no such fact is open.
    pub fn retract_single(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
    ) -> Result<(), StoreError> {
        let meta = self.registry.meta(predicate);
        ctx.check_assert(subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.retract_single(subject, predicate, asserter);
        tx.commit()?;
        Ok(())
    }

    /// Retract a `Set`-cardinality fact.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, the asserted-open fact at
    ///   `(subject, predicate, asserter, object)` has its asserted interval closed.
    ///   Returns `StoreError::FactNotFound` if no such fact is open.
    pub fn retract_set(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: Term,
    ) -> Result<(), StoreError> {
        let meta = self.registry.meta(predicate);
        ctx.check_assert(subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.retract_set(subject, predicate, asserter, object);
        tx.commit()?;
        Ok(())
    }

    /// Retract an `Indexed`-cardinality fact.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, the asserted-open fact at
    ///   `(subject, predicate, asserter, index_key)` has its asserted interval closed.
    ///   Returns `StoreError::FactNotFound` if no such fact is open.
    pub fn retract_indexed(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
    ) -> Result<(), StoreError> {
        let meta = self.registry.meta(predicate);
        ctx.check_assert(subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.retract_indexed(subject, predicate, asserter, index_key);
        tx.commit()?;
        Ok(())
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// All currently asserted-open facts at `(subject, predicate)`.
    ///
    /// Requires: `ctx` permits read on `subject` for `predicate`.
    /// Ensures: every returned fact `f` satisfies `f.asserted.is_open()`.
    ///   Returns `StoreError::AccessDenied` rather than an empty vec on denial,
    ///   so callers can distinguish denial from genuinely absent facts.
    pub fn query_open(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Result<Vec<StoredFact>, StoreError> {
        let meta = self.registry.meta(predicate);
        ctx.check_read(subject, meta)?;
        Ok(self.backend.query_open(subject, predicate, pattern))
    }

    /// Facts visible at the bitemporal point `(tx_time, valid_time)`.
    ///
    /// Requires: `ctx` permits read on `subject` for `predicate`.
    /// Ensures: every returned fact `f` satisfies
    ///   `f.asserted.contains(tx_time) && f.valid.contains(valid_time)`.
    pub fn query_at(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        tx_time: Timestamp,
        valid_time: Timestamp,
        pattern: Option<&Pattern>,
    ) -> Result<Vec<StoredFact>, StoreError> {
        let meta = self.registry.meta(predicate);
        ctx.check_read(subject, meta)?;
        Ok(self.backend.query_at(subject, predicate, tx_time, valid_time, pattern))
    }

    /// Every fact ever committed at `(subject, predicate)`, open and closed.
    ///
    /// Requires: `ctx` permits read on `subject` for `predicate`.
    pub fn history(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Result<Vec<StoredFact>, StoreError> {
        let meta = self.registry.meta(predicate);
        ctx.check_read(subject, meta)?;
        Ok(self.backend.history(subject, predicate, pattern))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_context::ExecContext;
    use crate::fact::Fact;
    use crate::id::EntityId;
    use crate::predicate_registry::Visibility;
    use crate::predicates::Cardinality;
    use crate::term::Term;

    fn e(n: u64) -> EntityId {
        EntityId(n)
    }

    fn fact(subject: EntityId, predicate: Predicate, asserter: EntityId) -> Fact {
        Fact { subject, predicate, object: Term::Int(0), asserter, valid: None }
    }

    fn store_3() -> (Store, Predicate, Predicate, Predicate) {
        let mut store = Store::in_memory();
        let prv = store.declare("priv", Visibility::Private, Cardinality::Single).unwrap();
        let shr = store.declare("shared", Visibility::Shared, Cardinality::Single).unwrap();
        let pub_ = store.declare("public", Visibility::Public, Cardinality::Single).unwrap();
        (store, prv, shr, pub_)
    }

    // ── Assert access control ─────────────────────────────────────────────────

    #[test]
    fn internal_assert_permitted_for_any_visibility() {
        let (store, prv, shr, pub_) = store_3();
        let ctx = ExecContext::Entity(e(1));
        assert_eq!(store.assert_single(&ctx, fact(e(1), prv, e(1))), Ok(()));
        assert_eq!(store.assert_single(&ctx, fact(e(1), shr, e(1))), Ok(()));
        assert_eq!(store.assert_single(&ctx, fact(e(1), pub_, e(1))), Ok(()));
    }

    #[test]
    fn external_assert_denied_for_private_predicate() {
        let (store, prv, _, _) = store_3();
        let ctx = ExecContext::Entity(e(1));
        let err = store.assert_single(&ctx, fact(e(2), prv, e(1))).unwrap_err();
        assert!(matches!(err, StoreError::AccessDenied { .. }));
    }

    #[test]
    fn external_assert_denied_for_shared_predicate() {
        let (store, _, shr, _) = store_3();
        let ctx = ExecContext::Entity(e(1));
        let err = store.assert_single(&ctx, fact(e(2), shr, e(1))).unwrap_err();
        assert!(matches!(err, StoreError::AccessDenied { .. }));
    }

    #[test]
    fn external_assert_permitted_for_public_predicate() {
        let (store, _, _, pub_) = store_3();
        let ctx = ExecContext::Entity(e(1));
        assert_eq!(store.assert_single(&ctx, fact(e(2), pub_, e(1))), Ok(()));
    }

    #[test]
    fn system_bypasses_assert_access_control() {
        let (store, prv, shr, _) = store_3();
        let ctx = ExecContext::System;
        assert_eq!(store.assert_single(&ctx, fact(e(2), prv, e(1))), Ok(()));
        assert_eq!(store.assert_single(&ctx, fact(e(2), shr, e(1))), Ok(()));
    }

    // ── Query access control ──────────────────────────────────────────────────

    #[test]
    fn query_denial_differs_from_empty_result() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;

        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();

        let owner = ExecContext::Entity(e(1));
        assert_eq!(store.query_open(&owner, e(1), prv, None).unwrap().len(), 1);

        let other = ExecContext::Entity(e(2));
        assert!(matches!(
            store.query_open(&other, e(1), prv, None),
            Err(StoreError::AccessDenied { .. })
        ));
    }

    #[test]
    fn query_returns_empty_for_absent_fact_with_read_permission() {
        let (store, _, shr, _) = store_3();
        let ctx = ExecContext::Entity(e(2));
        assert!(store.query_open(&ctx, e(1), shr, None).unwrap().is_empty());
    }

    #[test]
    fn system_can_read_any_predicate() {
        let (store, prv, _, _) = store_3();
        let ctx = ExecContext::System;
        assert!(store.query_open(&ctx, e(1), prv, None).is_ok());
    }

    // ── Retract errors ────────────────────────────────────────────────────────

    #[test]
    fn retract_not_found_returns_fact_not_found() {
        let (store, prv, _, _) = store_3();
        let ctx = ExecContext::Entity(e(1));
        let err = store.retract_single(&ctx, e(1), prv, e(1)).unwrap_err();
        assert_eq!(err, StoreError::FactNotFound);
    }

    #[test]
    fn retract_access_denied_before_existence_check() {
        let (store, prv, _, _) = store_3();
        let ctx = ExecContext::Entity(e(2));
        let err = store.retract_single(&ctx, e(1), prv, e(1)).unwrap_err();
        assert!(matches!(err, StoreError::AccessDenied { .. }));
    }
}
