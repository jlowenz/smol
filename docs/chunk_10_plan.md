# Chunk 10: dyn-Compatible TripleStore and the Store Façade

## Status

Chunk 9 shipped `PredicateRegistry` with `Visibility` and `Cardinality`. Chunk
10 has two distinct phases:

1. **Refactor** — make `TripleStore<TFact>` dyn-compatible, introduce
   `Applier<TFact>` as the commit-callback seam, restructure
   `InMemoryTripleStore` around `Arc<Inner>`, and move `PredicateRegistry`
   out of the store backend.
2. **Store** — implement `ExecContext`, `AccessError`, and the `Store` façade
   (PRD §3.5, §4) on top of the now-dyn-compatible `TripleStore<TFact>`.

---

## Architectural Decisions

### Why `TripleStore<TFact>` was not dyn-compatible

The GAT in the current trait:

```rust
type Tx<'a>: Transaction<TFact> where Self: 'a;
fn new_transaction(&self) -> Self::Tx<'_>;
```

is the sole blocker. GATs are never dyn-compatible: the vtable would require a
different slot per concrete `Tx` type, but a vtable is fixed when `dyn
TripleStore<TFact>` is constructed. The `TFact` type parameter itself is not an
obstacle — `Arc<dyn TripleStore<StoredFact>>` is valid once the GAT is removed.

### Solution: `Arc<Inner>` and the `Applier<TFact>` seam

The GAT exists because `InMemoryTransaction<'a>` borrows directly from the store
(`store: &'a InMemoryTripleStore`). The transaction needs access to shared state
after `new_transaction` returns.

The borrow is replaced by **shared ownership**: the store's mutable state is
extracted into a private `Inner` struct held behind `Arc<Inner>`.
`new_transaction` clones that Arc into the new transaction. No borrow, no
lifetime, no GAT.

`Transaction<TFact>` is a public type in `transaction.rs`; `Inner` is private to
`in_memory_store.rs`. The `Applier<TFact>` trait is the seam that lets
`Transaction` call back into the store at commit time without a direct dependency
on the concrete type:

```
InMemoryTripleStore            Transaction<StoredFact>
 └─ Arc<Inner>  ──clone──►     └─ Arc<dyn Applier<StoredFact>>
                                       │
                           (Inner implements Applier<StoredFact>)
```

`Applier<TFact>` has a single method — the commit path. It is `pub(crate)` and
has exactly one implementation in this crate.

### `Transaction<TFact>` trait is retired; `Transaction<TFact>` becomes a struct

The trait existed solely to name the associated type in the GAT. With
`Transaction<TFact>` now a concrete struct returned by value from
`new_transaction`, the trait has no structural role. Assert/retract/commit
become inherent methods. The `TFact` parameter is retained on the struct because
`commit` returns `Vec<CommitChange<TFact>>` — it has real meaning there.

### `TripleStore<TFact>` retains its type parameter

`TFact` governs the return type of query methods and commit changes. It is not
the source of the dyn-compatibility problem. `Store` holds
`Arc<dyn TripleStore<StoredFact>>`, fixing the fact type at the consumer boundary
— a deliberate choice, not a Rust constraint.

### `subscribe` / `unsubscribe` added to `TripleStore<TFact>`

Subscriptions are dispatch-coupled to commits inside `TripleStore`
implementations. They belong on the trait. `Subscription` is a concrete type, so
these methods are dyn-compatible.

### `PredicateRegistry` moves to `Store`

`Inner` does not hold a predicate registry. `Store` owns
`RwLock<PredicateRegistry>`. The backend is unaware of predicate metadata.

---

## What Already Exists (unchanged by this chunk)

- `Storage<TFact>` trait + `InMemoryStorage` — not modified.
- `Queryable<TFact>` trait + `InMemoryStorage` impl — not modified.
- `PredicateRegistry` / `Visibility` / `Cardinality` / `PredicateMeta` — not modified.
- `Fact`, `StoredFact`, `BitemporalFact` — not modified.
- `ChangeEvent`, `ChangeKind`, `Subscription`, `Subscriber` — not modified.
- `TentativeOp` — stays inside `in_memory_store.rs`; not part of any public API.

---

## Phase 1 — Refactor for dyn-Compatibility

**Files touched:** `in_memory_store.rs`, `triple_store.rs`, `transaction.rs`, `lib.rs`

### Step 1 — Extract `Inner` from `InMemoryTripleStore`

Create a `pub(crate)` struct `Inner` inside `in_memory_store.rs` that owns all
mutable store state:

```rust
pub(crate) struct Inner {
    storage:           InMemoryStorage,
    clock:             Arc<dyn Clock>,
    next_tx:           AtomicU64,
    next_subscription: AtomicU64,
    subscribers:       RwLock<Vec<Subscriber>>,
}
```

`predicates` is intentionally absent — it moves to `Store` in Phase 2.

`InMemoryTripleStore` becomes a thin public wrapper that is cheaply cloneable:

```rust
pub struct InMemoryTripleStore {
    inner: Arc<Inner>,
}

impl Clone for InMemoryTripleStore {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}
```

### Step 2 — Introduce `Applier<TFact>` and implement it on `Inner`

Add `Applier<TFact>` to `transaction.rs` (it is used by `Transaction<TFact>`
which lives there):

```rust
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
```

Implement `Applier<StoredFact>` on `Inner` by moving the existing
`InMemoryTripleStore::apply` logic there.

### Step 3 — Replace `Transaction<TFact>` trait with a concrete struct

Remove the `Transaction<TFact>: Sized` trait from `transaction.rs`. Replace it
with a concrete struct:

```rust
pub struct Transaction<TFact> {
    applier: Arc<dyn Applier<TFact>>,
    tx_id:   TxId,
    ops:     Vec<TentativeOp>,
}

impl<TFact> Transaction<TFact> {
    pub(crate) fn new(applier: Arc<dyn Applier<TFact>>, tx_id: TxId) -> Self {
        Self { applier, tx_id, ops: Vec::new() }
    }

    pub fn assert_single(&mut self, fact: Fact) { ... }
    pub fn assert_set(&mut self, fact: Fact) { ... }
    pub fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey) { ... }
    pub fn retract_single(&mut self, subject: EntityId, predicate: Predicate, asserter: EntityId) { ... }
    pub fn retract_set(&mut self, subject: EntityId, predicate: Predicate, asserter: EntityId, object: Term) { ... }
    pub fn retract_indexed(&mut self, subject: EntityId, predicate: Predicate, asserter: EntityId, index_key: OrderingKey) { ... }

    /// Apply all buffered operations atomically.
    ///
    /// Ensures: all ops committed or none (per shard); change events dispatched
    ///   to matching subscribers before returning.
    pub fn commit(self) -> Result<Vec<CommitChange<TFact>>, TxError> {
        self.applier.apply(self.tx_id, self.ops)
    }

    /// Discard all buffered operations.
    pub fn rollback(self) {}
}
```

`TxError` and its `From<StorageError>` impl remain in `transaction.rs`.

### Step 4 — Update `TripleStore<TFact>` trait

Replace the current trait in `triple_store.rs`. The GAT and associated
`Transaction<TFact>` supertrait are removed; query methods are inlined (the
`Queryable<TFact>` supertrait is no longer needed at this level);
`subscribe`/`unsubscribe` are added:

```rust
pub trait TripleStore<TFact: BitemporalFact>: Send + Sync {
    /// Begin a new transaction.
    ///
    /// Ensures: returned transaction has an empty buffer and a unique `TxId`.
    fn new_transaction(&self) -> Transaction<TFact>;

    /// The current time according to the store's clock.
    fn now(&self) -> Timestamp;

    fn query_open(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact>;

    fn query_at(
        &self,
        subject: EntityId,
        predicate: Predicate,
        tx_time: Timestamp,
        valid_time: Timestamp,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact>;

    fn history(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact>;

    /// Register a pattern subscription.
    ///
    /// Ensures: returned `Subscription` id is unique within this store instance.
    fn subscribe(&self, pattern: Pattern) -> Subscription;

    /// Cancel a subscription. No-op if `id` is unknown.
    fn unsubscribe(&self, id: SubscriptionId);
}
```

All methods use only concrete types in their signatures. `TripleStore<StoredFact>`
is fully dyn-compatible.

### Step 5 — Update `InMemoryTripleStore` to implement new `TripleStore<TFact>`

`new_transaction` clones `Arc<Inner>` into a `Transaction<StoredFact>`:

```rust
fn new_transaction(&self) -> Transaction<StoredFact> {
    let tx_id = TxId(self.inner.next_tx.fetch_add(1, Ordering::Relaxed));
    Transaction::new(Arc::clone(&self.inner) as Arc<dyn Applier<StoredFact>>, tx_id)
}
```

Query methods and `subscribe`/`unsubscribe` delegate to `self.inner`.

The predicate-registry methods (`declare_predicate`, `predicate`, `predicate_meta`,
`predicate_name`) are removed from `InMemoryTripleStore`; they will appear on
`Store` in Phase 2.

### Invariant — all existing tests pass

Phase 1 must not break any of the 103 existing tests. Tests that directly called
predicate-registry methods on `InMemoryTripleStore` are migrated to use `Store`
in Phase 2.

---

## Phase 2 — `ExecContext`, `AccessError`, and `Store`

### `ExecContext` and `AccessError` — `exec_context.rs`

See **Exercise A** below.

### `StoreError` — `store.rs`

See **Exercise B** below.

### `Store` struct — `store.rs`

`Store` is a concrete, non-generic struct:

```rust
pub struct Store {
    backend:  Arc<dyn TripleStore<StoredFact>>,
    registry: RwLock<PredicateRegistry>,
}

impl Store {
    /// Requires: `backend` is a fully initialised `TripleStore<StoredFact>`.
    pub fn new(backend: Arc<dyn TripleStore<StoredFact>>) -> Self { ... }

    /// Convenience constructor wrapping an `InMemoryTripleStore`.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(InMemoryTripleStore::new()))
    }
}
```

**Mutation methods** each:
1. Resolve predicate metadata from `self.registry`.
2. Call the appropriate `ExecContext` check method.
3. Call `self.backend.new_transaction()`, buffer one operation, commit.
4. Propagate errors as `StoreError`.

**Query methods** each return `Result<Vec<StoredFact>, StoreError>`:
1. Resolve predicate metadata from `self.registry`.
2. Call `ctx.check_read(subject, meta)?` — propagates `AccessError` as
   `StoreError::Access`. Callers can distinguish access denial from empty
   results; silent `vec![]` was rejected because it conflates the two.
3. Delegate to the corresponding `self.backend` query method.

**Predicate-registry methods** (`declare_predicate`, `predicate`,
`predicate_meta`, `predicate_name`) operate on `self.registry` directly; the
backend is not involved.

**Subscription methods** delegate to `self.backend`.

---

## Exercise Tasks

### Exercise A — `ExecContext` and `AccessError`

**File:** `crates/triple-store/src/exec_context.rs` (new file)

Implement the `ExecContext` enum, `AccessError` enum, and two check methods
encoding the PRD §3.5 rules. Derive `Copy + Clone + Debug + PartialEq + Eq` on
`ExecContext`; derive `Debug + PartialEq + Eq` on `AccessError`.

```rust
pub enum ExecContext { Entity(EntityId), System }

pub enum AccessError {
    Denied { predicate: PredicateMeta, reason: &'static str },
}

impl ExecContext {
    pub fn check_assert(&self, subject: EntityId, meta: PredicateMeta) -> Result<(), AccessError>;
    pub fn check_read(&self, subject: EntityId, meta: PredicateMeta) -> Result<(), AccessError>;
}
```

`check_assert` unifies what the original plan split into `check_internal_assert`
and `check_external_assert`. Internal vs. external is a **fact property**
(`subject == asserter`), not a separate assert operation: the same single-method
check covers both cases. `System` always permits because it acts on behalf of
the entity (producing internal facts); visibility does not gate System asserts.

`AccessError::Denied` carries `PredicateMeta` rather than just `Predicate` so
error messages and tests have access to visibility and cardinality without a
second registry lookup.

Include a `#[cfg(test)]` module covering at minimum:
- `System` permits assert for any subject and any predicate visibility.
- `Entity(1)` permits assert for subject 1 (any predicate visibility).
- `Entity(1)` denies assert for subject 2 on `Private` predicate.
- `Entity(1)` denies assert for subject 2 on `Shared` predicate.
- `Entity(1)` permits assert for subject 2 on `Public` predicate.
- Any context permits assert on `Public` predicate.
- `System` permits read of any predicate for any subject.
- `Entity(x)` permits read of any predicate on owned subject.
- `Entity(x)` denies read of `Private` predicate on another subject.
- `Entity(x)` permits read of `Shared` and `Public` predicates on any subject.

### Exercise B — `StoreError` and `Store` skeleton

**File:** `crates/triple-store/src/store.rs` (new file)

1. Implement `StoreError`:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    Access(AccessError),
    Transaction(TxError),
}
```

With `From<AccessError> for StoreError` and `From<TxError> for StoreError`.

2. Implement `Store::new`, `Store::in_memory`, and the four predicate-registry
   methods. These require no access checks and operate on `self.registry`.

3. Implement `Store::subscribe` and `Store::unsubscribe` as single-line
   delegations to `self.backend`.

---

## PRD §3.5 Rules Reference

### Assert (`check_assert`)

Internal vs. external is a **derived fact property** (`subject == asserter`),
not a separate operation. A single check covers both:

```
Entity(caller), caller == subject                       → permit (internal)
Entity(caller), caller != subject, visibility == Public → permit (external)
Entity(caller), caller != subject, otherwise            → deny
System                                                  → always permit
```

`System` always permits because it acts on behalf of the entity, producing
facts where `asserter = subject` (internal facts). There is no "System external
assert" scenario — System bypasses ownership, not visibility, and ownership is
the only gate on internal asserts.

### Read (`check_read`)

```
Private → permit iff ctx == Entity(subject) || ctx == System
Shared  → always permit
Public  → always permit
```

Store query methods return `Result<Vec<StoredFact>, StoreError>`. An
`AccessError` surfaces as `StoreError::Access`; callers can distinguish denial
from genuinely empty results. Silent `vec![]` was rejected.

---

## Remaining Deliverables (not exercises)

After the exercises are complete:

- All six `Store` mutation methods with access checks.
- Three `Store` query methods returning `Result<Vec<StoredFact>, StoreError>`
  with visibility filtering via `check_read`.
- `lib.rs` updated to declare `exec_context` and `store` modules.
- Tests covering: assert permitted/denied (internal and external cases), query
  access denial distinguished from empty results, `System` bypass on assert,
  retract-not-found propagated as `StoreError::Transaction`.

---

## Expected Deliverables

**Phase 1 (refactor):**
- `in_memory_store.rs` — `Inner` extracted; `InMemoryTripleStore` thinned to
  `Arc<Inner>` wrapper with `Clone`; predicate-registry methods removed;
  `Inner` implements `Applier<StoredFact>`.
- `transaction.rs` — `Transaction<TFact>` trait removed; `Applier<TFact>` trait
  added; concrete `Transaction<TFact>` struct with inherent methods; `TxError` retained.
- `triple_store.rs` — GAT removed; `TFact` parameter retained; query methods
  inlined; `subscribe`/`unsubscribe` added; `Queryable` supertrait removed.
- All 103 existing tests pass (modulo predicate-registry tests migrated to Phase 2).

**Phase 2 (Store):**
- `crates/triple-store/src/exec_context.rs` — new file.
- `crates/triple-store/src/store.rs` — new file.
- `lib.rs` updated.
- All tests pass.

---

## Open Questions

### System provenance

When `System` asserts a fact on behalf of an entity, the stored fact has
`asserter == subject`, making it structurally indistinguishable from a fact the
entity asserted itself. The audit trail for System writes is lost.

Options to consider (deferred):
- Reserve a well-known `EntityId` for System (e.g. `EntityId(0)`) and use it as
  `asserter` when `ExecContext::System` is active. The fact becomes external
  (`asserter != subject`) but is committed without a visibility gate. Requires
  a carve-out in `check_assert` or a distinct commit path.
- Add an optional `system: bool` flag to `StoredFact` (breaks the clean
  subject/asserter provenance model).
- Accept the loss for now; `System` asserts are administrative and rare.

This question should be resolved before exposing `Store` to untrusted callers or
building audit tooling.

---

## What This Sets Up (Chunk 11 Preview)

- Multi-operation transaction API on `Store`: `Store::begin() -> Transaction<StoredFact>`
  exposes the transaction directly to callers.
- `Store::clone_handle()` is trivial — `Arc::clone(&self.backend)` plus a
  decision on registry-sharing strategy.
- Subscriber identity from `ExecContext` captured at subscribe time and used to
  filter dispatch by visibility.
- A `DurableFileTripleStore` plugs in by implementing `TripleStore<StoredFact>`
  (and `Applier<StoredFact>` on its inner state) with zero changes to `Store` or
  any caller.
