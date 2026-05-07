# Chunk 11: Multi-Op Transactions and Visibility-Filtered Subscriptions

## Status

Chunk 10 shipped `Store` with single-operation mutation methods, three query
methods, and `ExecContext`-gated access control. Chunk 11 adds two things:

1. **Multi-op transactions** — `Store::begin()` returns a `StoreTransaction`
   that buffers operations with access checks and commits them atomically.
2. **Visibility-filtered subscriptions** — `Store::subscribe` captures an
   `ExecContext` at subscription time; the backend filters dispatched events
   before they reach the subscriber's channel.

`Store::clone_handle()` and `DurableFileTripleStore` are explicitly deferred
(see Open Questions and Chunk 12 Preview).

---

## Architectural Decisions

### `StoreTransaction<'store>` borrows `&'store Store`

`Store::begin(ctx: ExecContext) -> StoreTransaction<'_>` takes `&self` and the
returned `StoreTransaction<'store>` holds `&'store Store`. While any
`StoreTransaction` is alive, the Rust borrow checker prevents calling
`Store::declare` (`&mut self`). This is the correct invariant: predicate
metadata must not change between the first buffered op and commit.

The `'store` lifetime also means `StoreTransaction` cannot escape the
`Store`'s scope without an explicit `Arc` restructuring — a useful constraint
at this stage.

### Access checks at buffer time, not commit time

`StoreTransaction` buffer methods (`assert_*`, `retract_*`) return
`Result<(), AccessError>` and check access eagerly. If access is denied, the
op is not added to the buffer and the error is returned immediately; the
transaction remains in a valid (if partial) state. The caller decides whether
to `rollback` or continue accumulating other ops.

This departs from the existing `Transaction<TFact>` design, where buffer
methods are infallible. The difference is intentional: storage-level errors
(e.g. `FactNotFound` on retract) cannot be known until commit time; access
errors can be checked immediately from `ExecContext` and registry metadata
alone. Fail-fast on access avoids assembling a transaction that will be
rejected in full.

A caller that commits a transaction where some buffer calls were rejected
gets only the successfully-buffered ops applied. This is defined and
consistent behaviour; callers that want all-or-nothing semantics must check
each buffer call and rollback on any error.

### Visibility filtering via subscriber filter callback

The backend's `Subscriber` gains an optional filter closure:

```rust
pub(crate) struct Subscriber {
    pub(crate) id: SubscriptionId,
    pub(crate) pattern: Pattern,
    pub(crate) sender: Sender<ChangeEvent>,
    pub(crate) filter: Option<Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync>>,
}
```

`Inner::dispatch` evaluates `filter` (when present) after the pattern matches
and before `try_send`. The backend remains unaware of visibility semantics;
the closure fully encapsulates them.

`TripleStore::subscribe` gains a second parameter:

```rust
fn subscribe(
    &self,
    pattern: Pattern,
    filter: Option<Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync>>,
) -> Subscription;
```

Direct callers (tests, code below `Store`) pass `None` to preserve existing
behaviour. `Store::subscribe` builds and passes a visibility-aware closure.

### Registry snapshot at subscribe time

The filter closure captures a snapshot of the `PredicateRegistry` at the
moment `Store::subscribe` is called. The snapshot is correct for the
transaction-lifetime reason above, but more usefully it is correct in general
because:

- `Store::declare` takes `&mut self`, and `Store::subscribe` takes `&self`.
  Once a subscription is live (held by the caller), Rust allows the caller to
  call `declare` again (the subscription does not borrow `Store`). The
  snapshot may therefore become stale if new predicates are declared after
  subscription.
- Staleness is conservative: undeclared predicates default to
  `Visibility::Private`, so events for a newly-declared predicate will be
  suppressed for subscribers who registered before the declaration. Callers
  that need visibility of new predicates must re-subscribe after declaring
  them.

This requires `PredicateRegistry: Clone` (and therefore `PredicateInterner:
Clone` and `StringInterner: Clone`). All three compose only `HashMap`, `Vec`,
and `Arc<str>`, so `Clone` is mechanical. See Step 0 in Phase 1.

---

## What Already Exists (unchanged by this chunk)

- `StoreError`, `Store`, all single-op mutation and query methods — not modified.
- `Transaction<TFact>`, `Applier<TFact>`, `TentativeOp` — not modified.
- `ExecContext`, `AccessError` — not modified.
- `ChangeEvent`, `ChangeKind`, `ChannelClosed`, `Subscription` — not modified.
  (`Subscriber` gains a `filter` field but is `pub(crate)`, not public API.)
- `PredicateRegistry`, `PredicateInterner`, `StringInterner` — gain `Clone`
  only; behaviour is unchanged.

---

## Phase 1 — Multi-Op Transaction API

**Files touched:** `store.rs`, `intern.rs`, `predicate_registry.rs`

### Step 0 — Derive `Clone` on interner and registry types

In `intern.rs`, add `Clone` to `StringInterner`, `PredicateInterner`, and
`SymbolInterner`. In `predicate_registry.rs`, add `Clone` to
`PredicateRegistry`. No logic changes.

### Step 1 — `Store::now` and `Store::query_as_of`

Add two delegating methods to `Store`:

```rust
/// The current time according to the store's clock.
pub fn now(&self) -> Timestamp {
    self.backend.now()
}

/// Bitemporal point-in-time query, resolving `AsOf::None` fields against
/// `self.now()`.
///
/// Requires: `ctx` permits read on `subject` for `predicate`.
/// Ensures: equivalent to calling `query_at` with resolved timestamps.
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
```

These are needed so callers do not have to reach into the backend directly for
the clock. `AsOf` is already in `crate::pattern`.

### Step 2 — `StoreTransaction<'store>`

Add to `store.rs`:

```rust
/// An access-checked transaction handle.
///
/// Obtained from `Store::begin`. While a `StoreTransaction` is alive, the
/// borrow checker prevents calling `Store::declare` on the same `Store`.
pub struct StoreTransaction<'store> {
    store: &'store Store,
    ctx: ExecContext,
    tx: Transaction<StoredFact>,
}
```

`Store::begin`:

```rust
/// Begin a multi-operation transaction.
///
/// Ensures: returned transaction has an empty buffer and a unique `TxId`.
///   Rust prevents `store.declare` from being called while the returned
///   `StoreTransaction` is live.
pub fn begin(&self, ctx: ExecContext) -> StoreTransaction<'_> {
    StoreTransaction {
        store: self,
        ctx,
        tx: self.backend.new_transaction(),
    }
}
```

`StoreTransaction` inherent methods:

```rust
impl<'store> StoreTransaction<'store> {
    /// Buffer a `Single`-cardinality assertion.
    ///
    /// Requires: `ctx` (captured at `begin`) permits assert on `fact.subject`
    ///   for `fact.predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer. On `Err`, the
    ///   transaction buffer is unchanged; the transaction remains usable.
    pub fn assert_single(&mut self, fact: Fact) -> Result<(), AccessError> { ... }

    pub fn assert_set(&mut self, fact: Fact) -> Result<(), AccessError> { ... }

    pub fn assert_indexed(&mut self, fact: Fact, index_key: OrderingKey)
        -> Result<(), AccessError> { ... }

    /// Buffer a `Single`-cardinality retraction.
    ///
    /// Requires: `ctx` permits assert on `subject` for `predicate`.
    /// Ensures: on `Ok(())`, op is appended to the buffer.
    ///   `StoreError::FactNotFound` is returned at `commit` time if no
    ///   asserted-open fact exists; it cannot be detected at buffer time.
    pub fn retract_single(&mut self, subject: EntityId, predicate: Predicate,
        asserter: EntityId) -> Result<(), AccessError> { ... }

    pub fn retract_set(&mut self, subject: EntityId, predicate: Predicate,
        asserter: EntityId, object: Term) -> Result<(), AccessError> { ... }

    pub fn retract_indexed(&mut self, subject: EntityId, predicate: Predicate,
        asserter: EntityId, index_key: OrderingKey) -> Result<(), AccessError> { ... }

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
```

The pattern in each assert buffer method:

```rust
pub fn assert_single(&mut self, fact: Fact) -> Result<(), AccessError> {
    let meta = self.store.registry.meta(fact.predicate);
    self.ctx.check_assert(fact.subject, meta)?;
    self.tx.assert_single(fact);
    Ok(())
}
```

Retract methods follow the same pattern: look up `meta`, call
`check_assert(subject, meta)?`, buffer the retract op.

### Step 3 — Tests for `StoreTransaction`

Include at minimum:

- Multi-op commit applies all buffered ops atomically (assert two facts,
  commit, query both).
- A denied buffer call leaves prior buffered ops intact; commit applies them.
- Rollback leaves the store unchanged.
- `commit` returning `StoreError::FactNotFound` when a buffered retract has
  no matching open fact.
- Mixed assert + retract in one transaction: retract an existing fact and
  assert a new one, both visible after commit.

---

## Phase 2 — Visibility-Filtered Subscriptions

**Files touched:** `change.rs`, `in_memory_store.rs`, `triple_store.rs`, `store.rs`

### Step 1 — Add filter to `Subscriber`

In `change.rs`, change `Subscriber` to:

```rust
pub(crate) struct Subscriber {
    pub(crate) id: SubscriptionId,
    pub(crate) pattern: Pattern,
    pub(crate) sender: Sender<ChangeEvent>,
    pub(crate) filter: Option<Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync>>,
}
```

### Step 2 — Update `Inner::dispatch` to evaluate the filter

In `in_memory_store.rs`, inside the dispatch loop, after `build_event`
returns `Some(event)` and before `try_send`, add:

```rust
if let Some(ref f) = sub.filter {
    if !f(&event) {
        continue;
    }
}
```

### Step 3 — Update `TripleStore::subscribe` signature

In `triple_store.rs`:

```rust
fn subscribe(
    &self,
    pattern: Pattern,
    filter: Option<Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync>>,
) -> Subscription;
```

Update `InMemoryTripleStore::subscribe` (in `in_memory_store.rs`) to pass
`filter` into the `Subscriber` it constructs and pushes to
`self.inner.subscribers`.

Update all existing call sites of `subscribe` (tests) to pass `None` as the
second argument.

### Step 4 — Update `Store::subscribe`

Change `Store::subscribe` signature to capture `ExecContext`:

```rust
pub fn subscribe(&self, ctx: ExecContext, pattern: Pattern) -> Subscription
```

Build and pass the filter closure:

```rust
pub fn subscribe(&self, ctx: ExecContext, pattern: Pattern) -> Subscription {
    let registry = self.registry.clone();
    let filter: Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync> =
        Box::new(move |event: &ChangeEvent| {
            let meta = registry.meta(event.predicate);
            ctx.check_read(event.subject, meta).is_ok()
        });
    self.backend.subscribe(pattern, Some(filter))
}
```

`Store::unsubscribe` is unchanged.

### Step 5 — Tests for visibility-filtered subscriptions

Include at minimum:

- `Entity(x)` subscriber does not receive events for `Private` predicates
  owned by another entity.
- `Entity(x)` subscriber receives events for `Shared` and `Public` predicates
  regardless of subject.
- `System` subscriber receives all events.
- Filter does not suppress events the subscriber owns (`Entity(x)` receives
  its own `Private` predicate events).
- An unfiltered backend subscription (passing `None`) receives all matching
  events regardless of visibility.

---

## Invariant — all existing tests pass

No behaviour change for any code that passes `None` as the filter argument.
The 113 tests from chunks 1–10 must remain green after both phases.

---

## Open Questions

### `Store::clone_handle()` — deferred

See the discussion at the end of chunk 10. The design question is whether
cloned handles share a registry (`Arc<RwLock<PredicateRegistry>>`) or receive
a frozen snapshot. This should be driven by a concrete multi-handle use case
(e.g. the interpreter spawning worker contexts) rather than speculation.

### Registry sharing model and dispatch-path performance

The current design clones the full `PredicateRegistry` once per `subscribe`
call. This has three compounding costs that may matter in a live programming
environment:

1. **O(N_subscribers) memory.** Each active subscription owns an independent
   registry snapshot. With hundreds of subscribers and thousands of predicates,
   this is non-trivial allocation.
2. **Stale-by-default.** A subscriber whose snapshot predates a new
   `declare` call silently suppresses events for that predicate. The only
   recovery is `unsubscribe` + `subscribe`, which is an awkward polling
   pattern — callers must know to listen for declaration events and then
   re-subscribe. This is not what subscribers will expect.
3. **Hot dispatch path.** Every event dispatched to every subscriber executes
   the filter closure, which does a metadata lookup against the snapshot. With
   many predicates and many concurrent subscriptions, this is a significant
   fraction of dispatch cost.

The natural upgrade is an **atomic registry handle** — `ArcSwap<PredicateRegistry>`
(from the `arc-swap` crate, or hand-rolled with the same seqlock semantics).
`Store` holds one `Arc<ArcSwap<PredicateRegistry>>`; filter closures clone the
`Arc` (not the registry). Each dispatch calls `.load()` — a lock-free seqlock
read returning the current snapshot — so all subscribers automatically see the
latest declarations with no `RwLock` on the hot path. `declare` pays one
clone-and-atomic-store. This also makes `clone_handle()` trivial: handles
share the same `Arc<ArcSwap<...>>`.

**Decision needed:** Is this worth doing before predicates and subscribers are
numerous enough to measure? At minimum, the current `Clone`-based snapshot
approach should not be treated as settled; the upgrade path to an atomic handle
should be kept clear. Defer implementation until there is a concrete workload
to benchmark against, but do not add new surface area (e.g. `clone_handle`)
that bakes in the snapshot model.

### Stale filter snapshot on re-declare

Subsumed by the registry sharing model question above. The `ConflictingDeclaration`
guard means metadata cannot silently change; the practical concern is new
predicates being invisible to existing subscribers, which is the staleness
scenario described there.

### Bounded-channel backpressure (`TrySendError::Full`)

`Inner::dispatch` currently silently drops events when the channel is full
(PRD §5.3 deferred). This interacts with the new filter: a filtered subscriber
that rarely consumes events will still cause its channel to fill. The drop
policy and its interaction with visibility filtering should be addressed when
§5.3 is scheduled.

---

## Expected Deliverables

**Phase 1:**
- `intern.rs` — `Clone` derived on `StringInterner`, `PredicateInterner`,
  `SymbolInterner`.
- `predicate_registry.rs` — `Clone` derived on `PredicateRegistry`.
- `store.rs` — `Store::now`, `Store::query_as_of`, `Store::begin`,
  `StoreTransaction<'store>` with six buffer methods, `commit`, `rollback`.
- Tests covering multi-op commit, partial denial, rollback, retract-not-found
  at commit, mixed assert+retract.

**Phase 2:**
- `change.rs` — `filter` field added to `Subscriber`.
- `in_memory_store.rs` — `dispatch` evaluates `filter`; `subscribe`
  signature updated.
- `triple_store.rs` — `subscribe` signature updated.
- `store.rs` — `Store::subscribe` signature updated; filter closure built
  from registry snapshot.
- Existing call sites updated to pass `None`.
- Tests covering per-visibility filtering for all three visibility levels and
  `System`.
- All 113+ prior tests pass.

---

## What This Sets Up (Chunk 12 Preview)

- **`DurableFileTripleStore`** — implements `TripleStore<StoredFact>` and
  `Applier<StoredFact>` on its inner state. Plugs into `Store::new(backend)`
  with zero changes to `Store` or any caller. The filter-callback design from
  this chunk carries forward unchanged.
- **`Store::clone_handle()`** — once a concrete multi-handle use case exists,
  the registry-sharing decision can be made and `clone_handle` added. Phase 1
  of this chunk adds `Clone` to the registry types, so the implementation will
  be mechanical once the semantics are decided.
- **Entity ID allocation** — the store does not allocate `EntityId`s (PRD
  §8.6). When an interpreter is introduced, it will need a monotonic generator
  or a well-known allocation predicate. This is outside the store layer but
  depends on `Store` being stable, which it will be after chunk 11.
