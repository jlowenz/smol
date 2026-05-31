//! The client-facing interface to the bi-temporal triple store.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use parking_lot::Mutex;

use crate::change::{ChangeEvent, Subscription};
use crate::exec_context::{AccessError, ExecContext};
use crate::fact::{Fact, StoredFact};
use crate::id::{EntityId, SubscriptionId};
use crate::in_memory_store::InMemoryTripleStore;
use crate::pattern::{AsOf, Pattern};
use crate::predicate_registry::{PredicateError, PredicateMeta, PredicateRegistry, Visibility};
use crate::predicates::{Cardinality, OrderingKey};
use crate::storage::CommitChange;
use crate::term::{Predicate, Term};
use crate::time::Timestamp;
use crate::transaction::{Transaction, TxError};
use crate::triple_store::TripleStore;
use crate::value_store::{StoredValue, ValueStore};

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    AccessDenied {
        predicate: PredicateMeta,
        reason: &'static str,
    },
    DeclarationConflict(PredicateError),
    FactNotFound,
    Internal(&'static str),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::AccessDenied { predicate, reason } => {
                write!(f, "access denied on predicate {:?}: {}", predicate, reason)
            }
            StoreError::DeclarationConflict(e) => write!(f, "declaration conflict: {e:?}"),
            StoreError::FactNotFound => write!(f, "fact not found"),
            StoreError::Internal(msg) => write!(f, "internal store error: {msg}"),
        }
    }
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
/// `Store` delegates all fact storage to an injected backend. The predicate
/// registry, entity ID counter, and backend are shared across all handles
/// produced by `clone_handle`. Every mutation and query is gated by an
/// `ExecContext`.
pub struct Store {
    backend: Arc<dyn TripleStore<StoredFact>>,
    /// Shared live registry; readers pay one atomic load per access, no lock.
    registry: Arc<ArcSwap<PredicateRegistry>>,
    /// Serialises concurrent `declare` calls across all handles.
    declare_lock: Arc<Mutex<()>>,
    /// Monotonic entity ID counter; shared across all handles in a group.
    next_entity: Arc<AtomicU64>,
    /// Content-addressed atemporal registry for value object structures.
    value_store: Arc<ValueStore>,
}

impl Store {
    /// Requires: `backend` is a fully initialised `TripleStore<StoredFact>`.
    /// Ensures: predicate registry and entity counter are seeded from any prior
    ///   durable session via `recovered_registry` and `recovered_next_entity`.
    pub fn new(backend: Arc<dyn TripleStore<StoredFact>>) -> Self {
        let initial_registry = backend.recovered_registry();
        let next_entity = backend.recovered_next_entity();
        Store {
            backend,
            registry: Arc::new(ArcSwap::from_pointee(initial_registry)),
            declare_lock: Arc::new(Mutex::new(())),
            next_entity: Arc::new(AtomicU64::new(next_entity)),
            value_store: Arc::new(ValueStore::new()),
        }
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
    ///   identical metadata is idempotent. The declaration is durable before
    ///   it is published to readers: if persistence fails the `ArcSwap` is not
    ///   updated and the error propagates. Safe to call from `&self`; a shared
    ///   lock serialises concurrent declarations across all handles.
    pub fn declare(
        &self,
        name: &str,
        visibility: Visibility,
        cardinality: Cardinality,
    ) -> Result<Predicate, StoreError> {
        let _guard = self.declare_lock.lock();
        let current: Arc<PredicateRegistry> = self.registry.load_full();
        let mut next = (*current).clone();
        // Validate metadata before any I/O; fail fast on conflict.
        let id = next
            .declare(name, visibility, cardinality)
            .map_err(StoreError::DeclarationConflict)?;
        // Persist; on failure the ArcSwap is NOT updated.
        self.backend
            .persist_declaration(name, visibility, cardinality)
            .map_err(|_| StoreError::Internal("declaration persist failed"))?;
        self.registry.store(Arc::new(next));
        Ok(id)
    }

    /// Look up the handle for a previously-declared or interned predicate name.
    ///
    /// Ensures: `Some(id)` iff `name` is known to this registry; else `None`.
    pub fn lookup(&self, name: &str) -> Option<Predicate> {
        self.registry.load().lookup(name)
    }

    /// Return metadata for a predicate.
    ///
    /// Ensures: never panics for any `Predicate` issued by this store. Unknown
    ///   predicates return `Visibility::Private` and `Cardinality::Single`.
    pub fn meta(&self, id: Predicate) -> PredicateMeta {
        self.registry.load().meta(id)
    }

    /// Resolve a predicate handle to its name.
    ///
    /// Ensures: `Some(name)` iff `id` was issued by this store; else `None`.
    pub fn resolve(&self, id: Predicate) -> Option<Arc<str>> {
        self.registry.load_full().resolve(id)
    }

    // ── Entity allocation ─────────────────────────────────────────────────────

    /// Allocate a fresh, unique `EntityId` within this store group.
    ///
    /// Ensures: the returned id is distinct from all ids previously returned
    ///   by any handle sharing this store's entity counter.
    pub fn alloc_entity(&self) -> EntityId {
        EntityId(self.next_entity.fetch_add(1, Ordering::Relaxed))
    }

    // ── Handle sharing ────────────────────────────────────────────────────────

    /// Return a cheap handle to the same store.
    ///
    /// The returned handle shares the backend, predicate registry, declare
    /// lock, and entity ID counter with `self`. Writes through either handle
    /// are immediately visible through the other.
    ///
    /// Ensures: `clone_handle().alloc_entity()` returns an id distinct from
    ///   all ids previously allocated through any handle in this group.
    pub fn clone_handle(&self) -> Store {
        Store {
            backend: Arc::clone(&self.backend),
            registry: Arc::clone(&self.registry),
            declare_lock: Arc::clone(&self.declare_lock),
            next_entity: Arc::clone(&self.next_entity),
            value_store: Arc::clone(&self.value_store),
        }
    }

    // ── Value store ──────────────────────────────────────────────────────────

    /// Look up a previously committed value object by its content-hash id.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
    /// Ensures: returns `Some(v)` iff a `StoredValue` was committed with this `id`
    ///   (via `OwnedStoreTransaction::buffer_value` + `commit`); otherwise `None`.
    pub fn lookup_value(&self, id: EntityId) -> Option<Arc<StoredValue>> {
        self.value_store.lookup(id)
    }

    // ── Clock ─────────────────────────────────────────────────────────────────

    /// The current time according to the store's clock.
    pub fn now(&self) -> Timestamp {
        self.backend.now()
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────

    /// Register a visibility-filtered pattern subscription.
    ///
    /// Events are filtered by `ctx` before delivery: the subscriber receives
    /// only events for facts that `ctx` could read via `query_open`. The
    /// filter captures a live reference to the shared registry; predicates
    /// declared after this call are immediately visible to the filter.
    ///
    /// Ensures: returned `Subscription` id is unique within this store instance.
    pub fn subscribe(&self, ctx: ExecContext, pattern: Pattern) -> Subscription {
        let registry = Arc::clone(&self.registry);
        let filter: Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync> =
            Box::new(move |event: &ChangeEvent| {
                let meta = registry.load().meta(event.predicate);
                ctx.check_read(event.subject, meta).is_ok()
            });
        self.backend.subscribe(pattern, Some(filter))
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
        let meta = self.registry.load().meta(fact.predicate);
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
        let meta = self.registry.load().meta(fact.predicate);
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
        let meta = self.registry.load().meta(fact.predicate);
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
        let meta = self.registry.load().meta(predicate);
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
        let meta = self.registry.load().meta(predicate);
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
        let meta = self.registry.load().meta(predicate);
        ctx.check_assert(subject, meta)?;
        let mut tx = self.backend.new_transaction();
        tx.retract_indexed(subject, predicate, asserter, index_key);
        tx.commit()?;
        Ok(())
    }

    // ── Transactions ─────────────────────────────────────────────────────────

    /// Begin a multi-operation transaction.
    ///
    /// Ensures: returned transaction has an empty buffer and a unique `TxId`.
    pub fn begin(&self, ctx: ExecContext) -> StoreTransaction<'_> {
        StoreTransaction {
            store: self,
            ctx,
            tx: self.backend.new_transaction(),
        }
    }

    /// Begin a multi-operation transaction without borrowing `self`.
    ///
    /// Clones a cheap handle to the store internally, so the returned
    /// `OwnedStoreTransaction` has no lifetime parameter and can be stored
    /// in a `RefCell` or moved across coroutine boundaries.
    ///
    /// Ensures: returned transaction has an empty buffer; shares the same
    ///   backend, registry, and entity counter as `self`.
    pub fn begin_owned(&self, ctx: ExecContext) -> OwnedStoreTransaction {
        OwnedStoreTransaction {
            store: self.clone_handle(),
            ctx,
            tx: self.backend.new_transaction(),
            value_buf: Vec::new(),
        }
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
        let meta = self.registry.load().meta(predicate);
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
        let meta = self.registry.load().meta(predicate);
        ctx.check_read(subject, meta)?;
        Ok(self
            .backend
            .query_at(subject, predicate, tx_time, valid_time, pattern))
    }

    /// Facts visible at the bitemporal point described by `as_of`, resolving
    /// `None` fields against `self.now()`.
    ///
    /// Requires: `ctx` permits read on `subject` for `predicate`.
    /// Ensures: equivalent to calling `query_at` with
    ///   `tx_time = as_of.tx_time.unwrap_or_else(|| self.now())`
    ///   `valid_time = as_of.valid_time.unwrap_or_else(|| self.now())`.
    pub fn query_as_of(
        &self,
        ctx: &ExecContext,
        subject: EntityId,
        predicate: Predicate,
        as_of: AsOf,
        pattern: Option<&Pattern>,
    ) -> Result<Vec<StoredFact>, StoreError> {
        let now = self.backend.now();
        let tx_time = as_of.tx_time.unwrap_or(now);
        let valid_time = as_of.valid_time.unwrap_or(now);
        self.query_at(ctx, subject, predicate, tx_time, valid_time, pattern)
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
        let meta = self.registry.load().meta(predicate);
        ctx.check_read(subject, meta)?;
        Ok(self.backend.history(subject, predicate, pattern))
    }
}

// ── StoreTransaction ──────────────────────────────────────────────────────────

/// An access-checked, multi-operation transaction handle.
///
/// Obtained from `Store::begin`. Buffer methods check access eagerly and return
/// `Err(AccessError)` on denial without touching the buffer; the transaction
/// remains usable. Storage errors (e.g. `FactNotFound` on retract) surface only
/// at `commit` time.
pub struct StoreTransaction<'store> {
    store: &'store Store,
    ctx: ExecContext,
    tx: Transaction<StoredFact>,
}

impl<'store> StoreTransaction<'store> {
    /// Buffer a `Single`-cardinality assertion.
    ///
    /// Requires: `ctx` (captured at `begin`) permits assert on `fact.subject`
    ///   for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, the buffer
    ///   is unchanged; the transaction remains usable.
    pub fn assert_single(&mut self, fact: Fact) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(fact.predicate);
        self.ctx.check_assert(fact.subject, meta)?;
        self.tx.assert_single(fact);
        Ok(())
    }

    /// Buffer a `Set`-cardinality assertion.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn assert_set(&mut self, fact: Fact) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(fact.predicate);
        self.ctx.check_assert(fact.subject, meta)?;
        self.tx.assert_set(fact);
        Ok(())
    }

    /// Buffer an `Indexed`-cardinality assertion.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(fact.predicate);
        self.ctx.check_assert(fact.subject, meta)?;
        self.tx.assert_indexed(fact, index_key);
        Ok(())
    }

    /// Buffer a `Single`-cardinality retraction.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    ///   `StoreError::FactNotFound` is returned at `commit` time if no
    ///   asserted-open fact exists; it cannot be detected at buffer time.
    pub fn retract_single(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
    ) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(predicate);
        self.ctx.check_assert(subject, meta)?;
        self.tx.retract_single(subject, predicate, asserter);
        Ok(())
    }

    /// Buffer a `Set`-cardinality retraction.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn retract_set(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        object: Term,
    ) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(predicate);
        self.ctx.check_assert(subject, meta)?;
        self.tx.retract_set(subject, predicate, asserter, object);
        Ok(())
    }

    /// Buffer an `Indexed`-cardinality retraction.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn retract_indexed(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
    ) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(predicate);
        self.ctx.check_assert(subject, meta)?;
        self.tx
            .retract_indexed(subject, predicate, asserter, index_key);
        Ok(())
    }

    /// Commit all buffered operations atomically.
    ///
    /// Ensures: all buffered ops applied or none (per shard); change events
    ///   dispatched to matching subscribers before returning.
    pub fn commit(self) -> Result<Vec<CommitChange<StoredFact>>, StoreError> {
        self.tx.commit().map_err(Into::into)
    }

    /// Discard all buffered operations.
    pub fn rollback(self) {
        self.tx.rollback();
    }
}

// ── OwnedStoreTransaction ─────────────────────────────────────────────────────

/// An access-checked, multi-operation transaction that owns its store handle.
///
/// Unlike `StoreTransaction<'store>`, this type borrows no lifetime from the
/// store. It is `'static` and can be stored in a `RefCell` or moved across
/// coroutine boundaries. Obtained from `Store::begin_owned`.
pub struct OwnedStoreTransaction {
    store: Store,
    ctx: ExecContext,
    tx: Transaction<StoredFact>,
    /// Values staged for commit; flushed only after the triple-store commit succeeds.
    value_buf: Vec<(EntityId, StoredValue)>,
}

impl OwnedStoreTransaction {
    /// Buffer a `Single`-cardinality assertion.
    ///
    /// Requires: `ctx` (captured at `begin_owned`) permits assert on
    ///   `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, the buffer
    ///   is unchanged; the transaction remains usable.
    pub fn assert_single(&mut self, fact: Fact) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(fact.predicate);
        self.ctx.check_assert(fact.subject, meta)?;
        self.tx.assert_single(fact);
        Ok(())
    }

    /// Buffer a `Set`-cardinality assertion.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn assert_set(&mut self, fact: Fact) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(fact.predicate);
        self.ctx.check_assert(fact.subject, meta)?;
        self.tx.assert_set(fact);
        Ok(())
    }

    /// Buffer an `Indexed`-cardinality assertion.
    ///
    /// Requires: `ctx` permits assert on `fact.subject` for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(fact.predicate);
        self.ctx.check_assert(fact.subject, meta)?;
        self.tx.assert_indexed(fact, index_key);
        Ok(())
    }

    /// Buffer a `Single`-cardinality retraction.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    ///   `StoreError::FactNotFound` is returned at `commit` time if no
    ///   asserted-open fact exists; it cannot be detected at buffer time.
    pub fn retract_single(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
    ) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(predicate);
        self.ctx.check_assert(subject, meta)?;
        self.tx.retract_single(subject, predicate, asserter);
        Ok(())
    }

    /// Buffer an `Indexed`-cardinality retraction.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, unchanged.
    pub fn retract_indexed(
        &mut self,
        subject: EntityId,
        predicate: Predicate,
        asserter: EntityId,
        index_key: OrderingKey,
    ) -> Result<(), StoreError> {
        let meta = self.store.registry.load().meta(predicate);
        self.ctx.check_assert(subject, meta)?;
        self.tx
            .retract_indexed(subject, predicate, asserter, index_key);
        Ok(())
    }

    /// Stage a value for commit. Infallible; the value is not yet in the store.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0` (content-hash id space).
    /// Ensures: if `commit()` returns `Ok`, `store.lookup_value(id)` returns `Some`.
    ///   If `commit()` returns `Err`, the value is discarded with no side effects.
    pub fn buffer_value(&mut self, id: EntityId, value: StoredValue) {
        debug_assert!(
            id.0 & 0x8000_0000_0000_0000 != 0,
            "buffer_value: id is not in content-hash space"
        );
        self.value_buf.push((id, value));
    }

    /// Commit all buffered ivar facts and staged values.
    ///
    /// Ordering: facts are committed first; on `Err` the value buffer is dropped
    /// and the error propagates with no values stored. On `Ok`, the value buffer
    /// is flushed to the permanent value store (infallible for in-memory backends).
    ///
    /// Ensures: every `Term::ValueRef(id)` in a committed fact resolves via
    ///   `store.lookup_value(id)` immediately after this call returns `Ok`.
    pub fn commit(self) -> Result<Vec<CommitChange<StoredFact>>, StoreError> {
        let changes = self.tx.commit().map_err(StoreError::from)?;
        for (id, value) in self.value_buf {
            self.store.value_store.commit_value(id, value);
        }
        Ok(changes)
    }

    /// Discard all buffered operations. The value buffer is also dropped.
    pub fn rollback(self) {
        self.tx.rollback();
        // value_buf dropped here — no values are committed.
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
        Fact {
            subject,
            predicate,
            object: Term::Int(0),
            asserter,
            valid: None,
        }
    }

    fn store_3() -> (Store, Predicate, Predicate, Predicate) {
        let store = Store::in_memory();
        let prv = store
            .declare("priv", Visibility::Private, Cardinality::Single)
            .unwrap();
        let shr = store
            .declare("shared", Visibility::Shared, Cardinality::Single)
            .unwrap();
        let pub_ = store
            .declare("public", Visibility::Public, Cardinality::Single)
            .unwrap();
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
        let err = store
            .assert_single(&ctx, fact(e(2), prv, e(1)))
            .unwrap_err();
        assert!(matches!(err, StoreError::AccessDenied { .. }));
    }

    #[test]
    fn external_assert_denied_for_shared_predicate() {
        let (store, _, shr, _) = store_3();
        let ctx = ExecContext::Entity(e(1));
        let err = store
            .assert_single(&ctx, fact(e(2), shr, e(1)))
            .unwrap_err();
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

    // ── StoreTransaction ──────────────────────────────────────────────────────

    #[test]
    fn multi_op_commit_applies_all_buffered_ops() {
        let (store, prv, shr, _) = store_3();
        let sys = ExecContext::System;

        let mut tx = store.begin(sys);
        tx.assert_single(fact(e(1), prv, e(1))).unwrap();
        tx.assert_single(fact(e(1), shr, e(1))).unwrap();
        tx.commit().unwrap();

        assert_eq!(store.query_open(&sys, e(1), prv, None).unwrap().len(), 1);
        assert_eq!(store.query_open(&sys, e(1), shr, None).unwrap().len(), 1);
    }

    #[test]
    fn denied_buffer_call_leaves_prior_buffered_ops_intact() {
        let (store, prv, _, _) = store_3();
        let ctx = ExecContext::Entity(e(1));

        let mut tx = store.begin(ctx);
        tx.assert_single(fact(e(1), prv, e(1))).unwrap();
        assert!(tx.assert_single(fact(e(2), prv, e(1))).is_err()); // denied: external
        tx.commit().unwrap();

        let sys = ExecContext::System;
        assert_eq!(store.query_open(&sys, e(1), prv, None).unwrap().len(), 1);
        assert!(store.query_open(&sys, e(2), prv, None).unwrap().is_empty());
    }

    #[test]
    fn rollback_leaves_store_unchanged() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;

        let mut tx = store.begin(sys);
        tx.assert_single(fact(e(1), prv, e(1))).unwrap();
        tx.rollback();

        assert!(store.query_open(&sys, e(1), prv, None).unwrap().is_empty());
    }

    #[test]
    fn commit_fact_not_found_for_retract_without_prior() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;

        let mut tx = store.begin(sys);
        tx.retract_single(e(1), prv, e(1)).unwrap(); // access OK; not-found deferred
        let err = tx.commit().unwrap_err();
        assert_eq!(err, StoreError::FactNotFound);
    }

    #[test]
    fn mixed_assert_and_retract_in_one_transaction() {
        let (store, prv, shr, _) = store_3();
        let sys = ExecContext::System;

        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();

        let mut tx = store.begin(sys);
        tx.retract_single(e(1), prv, e(1)).unwrap();
        tx.assert_single(fact(e(1), shr, e(1))).unwrap();
        tx.commit().unwrap();

        assert!(store.query_open(&sys, e(1), prv, None).unwrap().is_empty());
        assert_eq!(store.query_open(&sys, e(1), shr, None).unwrap().len(), 1);
    }

    // ── query_as_of ───────────────────────────────────────────────────────────

    #[test]
    fn query_as_of_access_controlled_and_resolves_none_to_now() {
        use crate::pattern::AsOf;
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;

        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();

        let owner = ExecContext::Entity(e(1));
        let results = store
            .query_as_of(&owner, e(1), prv, AsOf::default(), None)
            .unwrap();
        assert_eq!(results.len(), 1);

        let other = ExecContext::Entity(e(2));
        assert!(matches!(
            store.query_as_of(&other, e(1), prv, AsOf::default(), None),
            Err(StoreError::AccessDenied { .. })
        ));
    }

    // ── Visibility-filtered subscriptions ─────────────────────────────────────

    fn any_pattern() -> Pattern {
        use crate::pattern::{Match, MatchAsserter};
        Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Any,
            asserter: MatchAsserter::Any,
        }
    }

    fn drain(sub: &Subscription) -> Vec<crate::change::ChangeEvent> {
        let mut out = Vec::new();
        while let Ok(Some(e)) = sub.try_recv() {
            out.push(e);
        }
        out
    }

    #[test]
    fn entity_subscriber_suppresses_private_events_for_other_entity() {
        let (store, prv, _, _) = store_3();
        let ctx_1 = ExecContext::Entity(e(1));
        let ctx_2 = ExecContext::Entity(e(2));

        let sub = store.subscribe(ctx_2, any_pattern());
        store.assert_single(&ctx_1, fact(e(1), prv, e(1))).unwrap();

        assert!(
            drain(&sub).is_empty(),
            "entity(2) must not see entity(1)'s private events"
        );
    }

    #[test]
    fn entity_subscriber_receives_shared_and_public_events_for_other_entity() {
        let (store, _, shr, pub_) = store_3();
        let ctx_1 = ExecContext::Entity(e(1));
        let ctx_2 = ExecContext::Entity(e(2));

        let sub = store.subscribe(ctx_2, any_pattern());
        store.assert_single(&ctx_1, fact(e(1), shr, e(1))).unwrap();
        store.assert_single(&ctx_1, fact(e(1), pub_, e(1))).unwrap();

        assert_eq!(drain(&sub).len(), 2);
    }

    #[test]
    fn system_subscriber_receives_all_events() {
        let (store, prv, shr, pub_) = store_3();
        let sys = ExecContext::System;

        let sub = store.subscribe(sys, any_pattern());
        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();
        store.assert_single(&sys, fact(e(1), shr, e(1))).unwrap();
        store.assert_single(&sys, fact(e(1), pub_, e(1))).unwrap();

        assert_eq!(drain(&sub).len(), 3);
    }

    #[test]
    fn entity_subscriber_receives_own_private_events() {
        let (store, prv, _, _) = store_3();
        let ctx_1 = ExecContext::Entity(e(1));

        let sub = store.subscribe(ctx_1, any_pattern());
        store.assert_single(&ctx_1, fact(e(1), prv, e(1))).unwrap();

        assert_eq!(
            drain(&sub).len(),
            1,
            "owner must receive their own private events"
        );
    }

    // ── clone_handle ──────────────────────────────────────────────────────────

    #[test]
    fn clone_handle_shares_backend() {
        let (store_a, prv, _, _) = store_3();
        let store_b = store_a.clone_handle();
        let sys = ExecContext::System;

        store_a.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();
        assert_eq!(store_b.query_open(&sys, e(1), prv, None).unwrap().len(), 1);
    }

    #[test]
    fn clone_handle_shares_predicate_registry() {
        let store_a = Store::in_memory();
        let store_b = store_a.clone_handle();

        let pred = store_a
            .declare("shared.pred", Visibility::Public, Cardinality::Single)
            .unwrap();
        assert_eq!(store_b.lookup("shared.pred"), Some(pred));
        assert_eq!(store_b.meta(pred).visibility, Visibility::Public);
    }

    #[test]
    fn clone_handle_shares_entity_counter() {
        let store_a = Store::in_memory();
        let store_b = store_a.clone_handle();

        let mut ids = std::collections::HashSet::new();
        for _ in 0..3 {
            ids.insert(store_a.alloc_entity());
            ids.insert(store_b.alloc_entity());
        }
        assert_eq!(ids.len(), 6, "all 6 allocated entity ids must be distinct");
    }

    // ── OwnedStoreTransaction value buffer ────────────────────────────────────

    const HIGH_BIT: u64 = 0x8000_0000_0000_0000;

    fn hash_id(n: u64) -> EntityId {
        EntityId(n | HIGH_BIT)
    }

    fn dummy_stored_value(class: &str) -> crate::value_store::StoredValue {
        crate::value_store::StoredValue {
            class_name: Arc::from(class),
            named_fields: vec![],
            indexed_fields: vec![],
        }
    }

    #[test]
    fn buffer_value_commit_makes_value_available_via_lookup() {
        let store = Store::in_memory();
        let ctx = ExecContext::System;
        let mut tx = store.begin_owned(ctx);

        let id = hash_id(1);
        tx.buffer_value(id, dummy_stored_value("Point"));
        tx.commit().unwrap();

        let got = store
            .lookup_value(id)
            .expect("value must be present after commit");
        assert_eq!(got.class_name.as_ref(), "Point");
    }

    #[test]
    fn buffer_value_rollback_leaves_store_empty() {
        let store = Store::in_memory();
        let ctx = ExecContext::System;
        let mut tx = store.begin_owned(ctx);

        let id = hash_id(2);
        tx.buffer_value(id, dummy_stored_value("Money"));
        tx.rollback();

        assert!(
            store.lookup_value(id).is_none(),
            "rollback must not commit values"
        );
    }

    #[test]
    fn buffer_value_fact_commit_failure_discards_values() {
        let (store, prv, _, _) = store_3();
        let ctx = ExecContext::System;
        let mut tx = store.begin_owned(ctx);

        // Buffer a retraction that will fail (no prior open fact).
        tx.retract_single(e(1), prv, e(1)).unwrap();

        let id = hash_id(3);
        tx.buffer_value(id, dummy_stored_value("Color"));

        // Commit must fail on FactNotFound; value must not be stored.
        assert_eq!(tx.commit().unwrap_err(), StoreError::FactNotFound);
        assert!(
            store.lookup_value(id).is_none(),
            "value must not be stored when facts fail"
        );
    }

    #[test]
    fn declare_after_subscribe_is_visible_to_filter() {
        let store = Store::in_memory();
        let sys = ExecContext::System;

        let sub = store.subscribe(sys, any_pattern());

        // declare a new Public predicate *after* subscribe
        let late_pred = store
            .declare("late.predicate", Visibility::Public, Cardinality::Single)
            .unwrap();

        // assert a fact on it; the filter must see Public visibility and deliver the event
        store
            .assert_single(&sys, fact(e(1), late_pred, e(1)))
            .unwrap();

        assert_eq!(drain(&sub).len(), 1, "filter must see the live registry");
    }

    #[test]
    fn clone_handle_write_visible_immediately() {
        let (store_a, prv, _, _) = store_3();
        let store_b = store_a.clone_handle();
        let sys = ExecContext::System;

        store_b
            .assert_single(&sys, fact(e(10), prv, e(10)))
            .unwrap();
        assert_eq!(store_a.query_open(&sys, e(10), prv, None).unwrap().len(), 1);
    }

    // ── now / resolve ─────────────────────────────────────────────────────────

    #[test]
    fn now_returns_a_timestamp() {
        let store = Store::in_memory();
        let t = store.now();
        // Two successive calls must be non-decreasing.
        let t2 = store.now();
        assert!(t2 >= t);
    }

    #[test]
    fn resolve_known_predicate_returns_name() {
        let store = Store::in_memory();
        let pred = store
            .declare("my.predicate", Visibility::Public, Cardinality::Single)
            .unwrap();
        let name = store.resolve(pred).unwrap();
        assert_eq!(name.as_ref(), "my.predicate");
    }

    #[test]
    fn resolve_unknown_predicate_returns_none() {
        let store = Store::in_memory();
        // A Predicate id not issued by this store has no name.
        let ghost = Predicate(99_999);
        assert!(store.resolve(ghost).is_none());
    }

    // ── declare conflict ──────────────────────────────────────────────────────

    #[test]
    fn declare_conflict_returns_declaration_conflict() {
        let store = Store::in_memory();
        store
            .declare("pred", Visibility::Public, Cardinality::Single)
            .unwrap();
        let err = store
            .declare("pred", Visibility::Private, Cardinality::Single)
            .unwrap_err();
        assert!(matches!(err, StoreError::DeclarationConflict(_)));
    }

    // ── history ───────────────────────────────────────────────────────────────

    #[test]
    fn history_returns_open_and_closed_facts() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;

        // First assert creates an open fact with object 0.
        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();
        // Second assert on the same Single key supersedes the first (closes it)
        // and opens a new fact with object 1.
        let mut f2 = fact(e(1), prv, e(1));
        f2.object = Term::Int(1);
        store.assert_single(&sys, f2).unwrap();

        let history = store.history(&sys, e(1), prv, None).unwrap();
        assert_eq!(
            history.len(),
            2,
            "both the superseded and current fact must appear"
        );
    }

    #[test]
    fn history_access_denied_for_other_entity() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;
        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();

        let other = ExecContext::Entity(e(2));
        assert!(matches!(
            store.history(&other, e(1), prv, None),
            Err(StoreError::AccessDenied { .. })
        ));
    }

    // ── Set cardinality ───────────────────────────────────────────────────────

    #[test]
    fn assert_set_and_retract_set_roundtrip() {
        let store = Store::in_memory();
        let set_pred = store
            .declare("tags", Visibility::Public, Cardinality::Set)
            .unwrap();
        let sys = ExecContext::System;
        let subject = e(1);
        let asserter = e(1);

        let f = Fact {
            subject,
            predicate: set_pred,
            object: Term::Int(42),
            asserter,
            valid: None,
        };
        store.assert_set(&sys, f.clone()).unwrap();
        assert_eq!(
            store
                .query_open(&sys, subject, set_pred, None)
                .unwrap()
                .len(),
            1
        );

        store
            .retract_set(&sys, subject, set_pred, asserter, Term::Int(42))
            .unwrap();
        assert!(
            store
                .query_open(&sys, subject, set_pred, None)
                .unwrap()
                .is_empty()
        );
    }

    // ── Indexed cardinality ───────────────────────────────────────────────────

    #[test]
    fn assert_indexed_and_retract_indexed_roundtrip() {
        use crate::predicates::OrderingKey;
        let store = Store::in_memory();
        let idx_pred = store
            .declare("items", Visibility::Public, Cardinality::Indexed)
            .unwrap();
        let sys = ExecContext::System;
        let subject = e(1);
        let asserter = e(1);
        let key = OrderingKey::new(1.0).unwrap();

        let f = Fact {
            subject,
            predicate: idx_pred,
            object: Term::Int(7),
            asserter,
            valid: None,
        };
        store.assert_indexed(&sys, f, key).unwrap();
        assert_eq!(
            store
                .query_open(&sys, subject, idx_pred, None)
                .unwrap()
                .len(),
            1
        );

        store
            .retract_indexed(&sys, subject, idx_pred, asserter, key)
            .unwrap();
        assert!(
            store
                .query_open(&sys, subject, idx_pred, None)
                .unwrap()
                .is_empty()
        );
    }

    // ── query_at with explicit timestamps ─────────────────────────────────────

    #[test]
    fn query_at_returns_fact_visible_at_current_time() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;

        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();
        let now = store.now();

        let results = store.query_at(&sys, e(1), prv, now, now, None).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn query_at_access_denied_for_other_entity() {
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;
        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();
        let now = store.now();

        let other = ExecContext::Entity(e(2));
        assert!(matches!(
            store.query_at(&other, e(1), prv, now, now, None),
            Err(StoreError::AccessDenied { .. })
        ));
    }

    // ── StoreTransaction Set / Indexed ops ────────────────────────────────────

    #[test]
    fn store_tx_set_assert_and_retract() {
        let store = Store::in_memory();
        let set_pred = store
            .declare("items", Visibility::Public, Cardinality::Set)
            .unwrap();
        let sys = ExecContext::System;
        let subject = e(1);
        let asserter = e(1);

        let f = Fact {
            subject,
            predicate: set_pred,
            object: Term::Int(1),
            asserter,
            valid: None,
        };

        let mut tx = store.begin(sys);
        tx.assert_set(f).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            store
                .query_open(&sys, subject, set_pred, None)
                .unwrap()
                .len(),
            1
        );

        let f2 = Fact {
            subject,
            predicate: set_pred,
            object: Term::Int(1),
            asserter,
            valid: None,
        };
        let mut tx2 = store.begin(sys);
        tx2.retract_set(subject, set_pred, asserter, f2.object)
            .unwrap();
        tx2.commit().unwrap();
        assert!(
            store
                .query_open(&sys, subject, set_pred, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn store_tx_indexed_assert_and_retract() {
        use crate::predicates::OrderingKey;
        let store = Store::in_memory();
        let idx_pred = store
            .declare("ranked", Visibility::Public, Cardinality::Indexed)
            .unwrap();
        let sys = ExecContext::System;
        let subject = e(1);
        let asserter = e(1);
        let key = OrderingKey::new(0.5).unwrap();

        let f = Fact {
            subject,
            predicate: idx_pred,
            object: Term::Int(99),
            asserter,
            valid: None,
        };

        let mut tx = store.begin(sys);
        tx.assert_indexed(f, key).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            store
                .query_open(&sys, subject, idx_pred, None)
                .unwrap()
                .len(),
            1
        );

        let mut tx2 = store.begin(sys);
        tx2.retract_indexed(subject, idx_pred, asserter, key)
            .unwrap();
        tx2.commit().unwrap();
        assert!(
            store
                .query_open(&sys, subject, idx_pred, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn store_tx_denied_set_buffer_call_leaves_buffer_intact() {
        let store = Store::in_memory();
        let set_pred = store
            .declare("tags", Visibility::Private, Cardinality::Set)
            .unwrap();
        let ctx = ExecContext::Entity(e(1));

        let f_own = Fact {
            subject: e(1),
            predicate: set_pred,
            object: Term::Int(1),
            asserter: e(1),
            valid: None,
        };
        let f_other = Fact {
            subject: e(2),
            predicate: set_pred,
            object: Term::Int(2),
            asserter: e(1),
            valid: None,
        };

        let mut tx = store.begin(ctx);
        tx.assert_set(f_own).unwrap();
        assert!(tx.assert_set(f_other).is_err()); // denied: external private
        tx.commit().unwrap();

        let sys = ExecContext::System;
        assert_eq!(
            store.query_open(&sys, e(1), set_pred, None).unwrap().len(),
            1
        );
        assert!(
            store
                .query_open(&sys, e(2), set_pred, None)
                .unwrap()
                .is_empty()
        );
    }

    // ── unsubscribe ───────────────────────────────────────────────────────────

    #[test]
    fn unsubscribe_through_store_stops_delivery() {
        use crate::pattern::{Match, MatchAsserter};
        let (store, prv, _, _) = store_3();
        let sys = ExecContext::System;
        let pattern = Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Any,
            asserter: MatchAsserter::Any,
        };

        let sub = store.subscribe(sys, pattern);
        let id = sub.id();
        store.unsubscribe(id);

        // After the subscription is cancelled, no new events arrive.
        // The channel may be closed or empty; either way no events should come through.
        store.assert_single(&sys, fact(e(1), prv, e(1))).unwrap();
        // try_recv returns either Ok(None) (empty) or Err(ChannelClosed); no event.
        match sub.try_recv() {
            Ok(None) | Err(_) => {} // expected: no events delivered
            Ok(Some(ev)) => panic!("unexpected event after unsubscribe: {ev:?}"),
        }
    }
}
