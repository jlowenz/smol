# Chunk 13: `Store::clone_handle()` and Entity ID Allocation

## Status

Chunk 12 shipped `DurableFileTripleStore` with WAL-based persistence,
`collapse_and_apply` / `dispatch_changes` as shared free functions, serde
derives on all core types, and a backend-parity test suite. 133 tests passing.

Chunk 13 delivers two tightly related capabilities that prepare `Store` for the
interpreter model:

1. **`Store::clone_handle()`** — a cheap clone that shares the backend, the
   predicate registry, and the entity ID counter. Multiple handles to the same
   store are the v0 multi-entity concurrency model.
2. **`Store::alloc_entity() -> EntityId`** — monotonic entity ID allocation
   within the store, shared across all handles.

Both depend on moving from an owned `PredicateRegistry` to a live shared
`Arc<RwLock<PredicateRegistry>>`. That change also resolves the subscription
staleness open question from chunk 11.

---

## Architectural Decisions

### Shared live registry: `Arc<RwLock<PredicateRegistry>>`

`Store.registry` changes from `PredicateRegistry` (owned) to
`Arc<RwLock<PredicateRegistry>>` (shared). All handles produced by
`clone_handle` share one registry instance.

Consequences:

- `declare` becomes `&self` — the write lock provides interior mutability.
  `&mut self` is no longer required to declare predicates, which aligns with
  the rest of the `Store` API (every other method already takes `&self`).
- `subscribe` filter closures capture `Arc::clone(&registry_arc)` instead of a
  full `PredicateRegistry` clone. Each filter invocation acquires a read lock,
  calls `meta`, and releases. This fixes the chunk 11 open question:
  subscriptions now see live registry state rather than a snapshot taken at
  subscribe time. Predicates declared after `subscribe` are visible immediately.
- Memory: one `PredicateRegistry` per store group instead of one per
  subscriber. For a store with hundreds of subscriptions and thousands of
  predicates, this is a significant reduction.
- Locking order: all call sites acquire `subscribers` lock (for dispatch) then
  `registry` lock (for filter). `declare` acquires only the `registry` write
  lock. No circular acquisition; no deadlock risk.

### `StringInterner::resolve` returns `Option<Arc<str>>`

The interner already stores `Arc<str>` internally. Returning a cloned `Arc`
(an atomic increment) instead of `&str` lets callers use the result freely
without lifetime coupling to a lock guard. `Store::resolve` is updated to
return `Option<Arc<str>>` accordingly.

### Shared entity ID counter: `Arc<AtomicU64>`

`Store` gains `next_entity: Arc<AtomicU64>` (initial value 1). All handles
from `clone_handle` share this counter. `alloc_entity()` is `fetch_add(1,
Relaxed)` — fast and contention-free in practice since entity allocation is not
on a hot path.

Choosing `Relaxed` ordering: entity ID allocation only requires uniqueness, not
synchronisation of other memory. The `TxId` counter in `Inner` / `DurableInner`
already uses `Relaxed` for the same reason.

### `clone_handle()` is not `Clone`

`Store` does not implement the standard `Clone` trait. `clone_handle()` is an
explicit, named operation so that call sites communicate intent ("I am creating
a shared handle", not "I am copying a value"). A `Clone` impl would be
misleading because the clone shares mutable state.

### Read-your-writes

`clone_handle` shares the same backend `Arc<dyn TripleStore<StoredFact>>`.
Writes committed through any handle are immediately visible to reads on any
other handle (they hit the same `InMemoryStorage` or `DurableInner`). No
additional mechanism is needed for read-your-writes semantics in v0.

---

## What Already Exists (unchanged by this chunk)

- All of `DurableFileTripleStore`, `InMemoryTripleStore`, `TripleStore<TFact>`,
  `Applier<TFact>`, `Transaction<TFact>` — not modified.
- `Store` mutation and query methods (`assert_*`, `retract_*`, `query_*`,
  `history`, `subscribe`, `unsubscribe`, `begin`, `now`) — signatures unchanged;
  internal registry access updated to use the read lock.
- `ExecContext`, `AccessError`, `StoreError` — not modified.
- `PredicateRegistry` — not modified (only its wrapper in `Store` changes).

---

## Phase 1 — `StringInterner::resolve` → `Option<Arc<str>>`

**File:** `intern.rs`

Change `StringInterner::resolve(id: u32) -> Option<&str>` to return
`Option<Arc<str>>`:

```rust
pub fn resolve(&self, id: u32) -> Option<Arc<str>> {
    self.by_id.get(id as usize).cloned()
}
```

`by_id` stores `Arc<str>`, so `cloned()` is a cheap atomic increment. Update
the typed wrappers `PredicateInterner` and `SymbolInterner` in the same file;
update `PredicateRegistry::resolve` to return `Option<Arc<str>>`.

Update `Store::resolve` signature and any call sites / tests.

---

## Phase 2 — Shared Registry in `Store`

**File:** `store.rs`

### Step 1 — Change `Store` struct

```rust
pub struct Store {
    backend: Arc<dyn TripleStore<StoredFact>>,
    registry: Arc<RwLock<PredicateRegistry>>,
    next_entity: Arc<AtomicU64>,
}
```

Add `use parking_lot::RwLock;` and `use std::sync::atomic::{AtomicU64, Ordering};`.

### Step 2 — Update constructors

```rust
impl Store {
    pub fn new(backend: Arc<dyn TripleStore<StoredFact>>) -> Self {
        Store {
            backend,
            registry: Arc::new(RwLock::new(PredicateRegistry::new())),
            next_entity: Arc::new(AtomicU64::new(1)),
        }
    }
}
```

### Step 3 — Update registry methods

`declare` changes from `&mut self` to `&self`:

```rust
pub fn declare(&self, name: &str, visibility: Visibility, cardinality: Cardinality)
    -> Result<Predicate, PredicateError>
{
    self.registry.write().declare(name, visibility, cardinality)
}
```

`lookup`, `meta` use `self.registry.read()`. `resolve` returns `Option<Arc<str>>`.

### Step 4 — Update `subscribe` filter closure

```rust
pub fn subscribe(&self, ctx: ExecContext, pattern: Pattern) -> Subscription {
    let registry = Arc::clone(&self.registry);
    let filter: Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync> =
        Box::new(move |event: &ChangeEvent| {
            let meta = registry.read().meta(event.predicate);
            ctx.check_read(event.subject, meta).is_ok()
        });
    self.backend.subscribe(pattern, Some(filter))
}
```

The lock is acquired and released per event — no guard escapes the closure.

### Step 5 — Update `StoreTransaction`

`StoreTransaction` holds `&'_ Store`. Its mutation methods call
`self.store.registry.read().meta(...)`. Update all five method bodies and the
`commit` path accordingly.

### Step 6 — Update query methods

`query_open`, `query_at`, `query_as_of`, `history` each call
`self.registry.meta(predicate)` — update to `self.registry.read().meta(predicate)`.

---

## Phase 3 — `alloc_entity` and `clone_handle`

**File:** `store.rs`

```rust
impl Store {
    /// Allocate a fresh, unique `EntityId` within this store group.
    ///
    /// Ensures: the returned id is distinct from all ids previously returned
    ///   by any handle sharing this store's entity counter.
    pub fn alloc_entity(&self) -> EntityId {
        EntityId(self.next_entity.fetch_add(1, Ordering::Relaxed))
    }

    /// Return a cheap handle to the same store.
    ///
    /// The returned handle shares the backend, predicate registry, and entity
    /// ID counter with `self`. Writes through either handle are immediately
    /// visible through the other.
    ///
    /// Ensures: `clone_handle().alloc_entity()` returns an id distinct from
    ///   all ids previously allocated through any handle in this group.
    pub fn clone_handle(&self) -> Store {
        Store {
            backend: Arc::clone(&self.backend),
            registry: Arc::clone(&self.registry),
            next_entity: Arc::clone(&self.next_entity),
        }
    }
}
```

---

## Phase 4 — Test Suite

**File:** `store.rs` test module (existing tests updated + new tests added)

### Existing tests

All existing `store::tests` must remain green with the updated method signatures.
The primary impact:
- `store.declare(...)` no longer requires a `mut` binding — update bindings in
  test setup.
- `store.resolve(id)` now returns `Option<Arc<str>>` — update assertions to
  use `as_deref()` or compare `Arc<str>` directly.

### New tests for `clone_handle`

```
clone_handle_shares_backend
  Assert through handle_a; query through handle_b — fact is visible.

clone_handle_shares_predicate_registry
  Declare predicate through handle_a; lookup through handle_b returns Some.

clone_handle_shares_entity_counter
  Allocate 3 entities from handle_a and 3 from handle_b (interleaved);
  all 6 ids are distinct.

declare_after_subscribe_is_visible_to_filter
  Subscribe, then declare a new Public predicate, then assert a fact on that
  predicate — the subscription delivers the event (proving the filter uses
  the live registry, not a snapshot).

clone_handle_write_visible_immediately
  clone_handle returns a new handle; a fact committed through it is visible
  via query_open on the original before any synchronisation.
```

---

## Invariant — all existing tests pass

The 133 tests from chunks 1–12 must remain green. The only breaking change to
the public API is `Store::resolve` returning `Option<Arc<str>>` and `declare`
dropping `&mut self` — both are strictly API improvements.

---

## Open Questions

### `ExecContext` threading model

The PRD describes `ExecContext` as the "current execution context" tracked on
the interpreter call stack. Today it is passed explicitly to every `Store`
method. When the interpreter exists, `ExecContext` may be a thread-local or
carried through a stack frame rather than passed as an argument. This decision
is deferred; the current explicit-parameter style is compatible with both
approaches.

### Entity ID allocation strategy

`alloc_entity` uses a simple monotonic counter. This is correct for v0 but does
not support:
- IDs that survive process restart (the counter resets to 1 on each `Store::new`).
- Distributed allocation across multiple machines.

For durability: the next session should start the counter above the highest
`EntityId` seen in the WAL. This requires either persisting the counter or
scanning replayed facts for the max id. Deferred; noted for when entity IDs
need to survive restart.

### Registry persistence

Predicate declarations are not persisted to the WAL. On reopen of a
`DurableFileTripleStore`, the registry is empty; callers must re-declare
predicates. This is inconsistent: facts survive, but the metadata needed to
interpret them does not. Fixing this requires either serialising the registry
to the WAL (as a special record type) or deriving it from the facts themselves.
Deferred; noted as a correctness gap.

### Log compaction

Still deferred from chunk 12. Full replay is O(total committed facts). Until
early interpreter sessions show measurable startup latency, no action needed.

---

## What This Sets Up (Chunk 14 Preview)

With shared handles and entity allocation in place, the next component can
begin: **the interpreter bootstrap**. The minimum viable interpreter needs:

- An entity to represent each "running program" (an `ExecContext` sourced from
  `alloc_entity`).
- A way to encode operations (assertions, queries, subscriptions) as values in
  the store — likely as facts on a well-known predicate scheme.
- A dispatch loop: listen on a subscription, decode the operation term,
  execute it, write the result back as a fact.

Before the dispatch loop can run, registry persistence (above) needs a decision,
because the predicate scheme the interpreter relies on must survive restart.
That makes registry persistence the likely first sub-task of chunk 14.
