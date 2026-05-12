# Chunk 14: Registry Persistence

## Status

Chunk 13 shipped `Store::clone_handle()`, `alloc_entity()`, and the
`Arc<ArcSwap<PredicateRegistry>>` live-registry upgrade. 136 tests passing.

Chunk 14 closes the correctness gap identified in the chunk 13 open questions:
predicate declarations are not currently persisted to the WAL. On reopen of a
`DurableFileTripleStore`, the registry starts empty; callers must re-declare all
predicates even though the facts that depend on them survive. This chunk fixes
that by interleaving declaration records with fact-batch records in the WAL.

---

## Architectural Decisions

### WAL record as a tagged union — `WalRecord`

The WAL currently writes one record type: `bincode(Vec<CommitChange<StoredFact>>)`.
Adding declaration records means the WAL now carries two kinds of records, so the
natural representation is an enum:

```rust
#[derive(Serialize, Deserialize)]
enum WalRecord {
    FactBatch(Vec<CommitChange<StoredFact>>),
    Declaration {
        name: String,
        visibility: Visibility,
        cardinality: Cardinality,
    },
}
```

`bincode` encodes enum variants with a `u32` discriminant prefix, so the on-disk
format becomes:

```
[4-byte payload length, LE u32]
[bincode(WalRecord)]
```

This is a **WAL format break**: existing WAL files written under the old format
cannot be replayed by the new code (the old format wrote the inner
`Vec<CommitChange<StoredFact>>` without the `WalRecord` envelope). This is
acceptable at pre-1.0 because no production data exists; existing test WAL files
created via `tempfile` are discarded after each test run. No migration path is
provided.

### `Visibility` serde derives

`Cardinality` already derives `Serialize, Deserialize` (from chunk 3).
`Visibility` in `predicate_registry.rs` does not. Add
`#[derive(Serialize, Deserialize)]` to `Visibility`; the three variants are
unit-like so bincode serializes them as `u32` discriminants with no payload.

### `TripleStore` trait extension — `persist_declaration` and `recovered_registry`

`Store::declare` needs to write to the WAL; `Store::new` needs to seed its
`ArcSwap` with declarations replayed from a prior session.

Two new methods are added to `TripleStore<TFact>` with default implementations
so existing implementors (`InMemoryTripleStore`) require no changes:

```rust
/// Persist a declaration record so it survives process restart.
///
/// Requires: caller holds `declare_lock` (or equivalent serialisation).
/// Ensures: on `Ok(())`, the declaration is durable. Default impl is a no-op.
fn persist_declaration(
    &self,
    name: &str,
    visibility: Visibility,
    cardinality: Cardinality,
) -> io::Result<()> {
    Ok(())
}

/// Return the `PredicateRegistry` recovered from a prior session.
///
/// Ensures: returned registry contains exactly the declarations replayed from
///   the WAL; empty for non-durable backends.
fn recovered_registry(&self) -> PredicateRegistry {
    PredicateRegistry::new()
}
```

Both methods use only concrete types (`&str`, `Visibility`, `Cardinality`,
`PredicateRegistry`) and take `&self`, so they are dyn-compatible. Importing
`predicate_registry::{PredicateRegistry, Visibility}` and
`predicates::Cardinality` into `triple_store.rs` introduces a dependency on
domain types that `TripleStore` did not previously hold. This is acceptable
because `TripleStore` is already the "whole-backend" interface and those types
originate in the same crate. The alternative — opaque byte payloads with
`Store`-side serialization — would hide the contract without reducing coupling.

### `DurableFileTripleStore` stores a recovered registry

`DurableInner` gains a `recovered_registry: PredicateRegistry` field, populated
during `open` by replaying `WalRecord::Declaration` entries in log order.
`DurableFileTripleStore::recovered_registry()` returns a clone of it.

### `Store::declare` persist-before-publish ordering

The current `declare` sequence is: acquire lock → clone registry → call
`registry.declare` → store updated `Arc`. After this chunk, the sequence
becomes:

1. Acquire `declare_lock`.
2. Load current `Arc<PredicateRegistry>` from the `ArcSwap`.
3. Clone the registry; call `next.declare(name, visibility, cardinality)?` — fail
   fast on conflict *before* any I/O.
4. Call `self.backend.persist_declaration(name, visibility, cardinality)` — write
   WAL record.
5. Only on `Ok`: `self.registry.store(Arc::new(next))` — publish to readers.

If step 4 fails, the in-memory `ArcSwap` is **not** updated; the error
propagates to the caller. The declaration is not visible to reads and was not
written to the WAL, so the store remains consistent.

If the process crashes after step 4 but before step 5, the declaration is in
the WAL; on replay it will be applied, and the next `Store::new` will see it in
`recovered_registry`. This is safe because `declare` is idempotent for identical
metadata.

### `Store::declare` error type change

Currently `declare` returns `Result<Predicate, PredicateError>`. After this
chunk it may also fail with an I/O error from `persist_declaration`. The cleanest
surface is to fold persistence failure into the existing `Store`-level error:
`declare` returns `Result<Predicate, StoreError>`. Callers that previously used
`PredicateError` directly get a slightly wider type; in practice, the only new
variant is `StoreError::Internal("declaration persist failed")`.

`Store::declare` is the only public method whose signature changes at the `Store`
layer; all `TripleStore`-layer signatures are unchanged.

### Replay is declaration-order-preserving

WAL replay processes records in file order. `WalRecord::Declaration` entries are
applied to the recovering `PredicateRegistry` via `registry.declare(...)`. If a
declaration replayed from the WAL conflicts with a prior one in the same WAL
(which would indicate WAL corruption), replay panics rather than returning an
error — a corrupted WAL is unrecoverable, and silent continuation would silently
produce wrong metadata.

Fact-batch records and declaration records can be interleaved in any order
without loss of correctness. Fact storage uses predicate IDs (not names);
`meta(id)` falls back to private/single defaults for undeclared IDs, but
declarations replayed later in the same pass will register the correct metadata
before any queries run against the recovered store.

### Entity ID counter recovery via fact scan

`alloc_entity` previously reset to 1 on each `Store::new`. This chunk fixes
that by scanning replayed facts: `replay_log` tracks the running maximum of
every `subject` and `asserter` value seen across all `FactBatch` records and
returns it alongside `max_tx` and the recovered `PredicateRegistry`. On reopen,
`next_entity` is initialized to `max_entity_seen + 1`.

No new WAL record type is required — the entity IDs are already embedded in
committed facts, so replay is the natural source of truth.

**Limitation:** an entity allocated via `alloc_entity` but never committed to a
fact will not appear in the scan; that ID may be reused after restart. This is
acceptable — an entity with no associated facts is semantically nonexistent.

---

## What Already Exists (unchanged by this chunk)

- `DurableFileTripleStore`, `InMemoryTripleStore`, `TripleStore<TFact>`,
  `Applier<TFact>`, `Transaction<TFact>` — internal logic not modified.
- `Store` mutation, query, subscription, transaction methods — unchanged.
- `ExecContext`, `AccessError` — not modified.
- `PredicateRegistry::declare` — not modified.
- WAL write path in `DurableInner::apply` — refactored to use `WalRecord`, but
  the commit record logic is identical.

---

## Phase 1 — `Visibility` serde derives

**File:** `predicate_registry.rs`

Add `Serialize, Deserialize` to the `Visibility` derive list. No logic changes.

---

## Phase 2 — `WalRecord` enum and WAL format change

**File:** `durable_file_store.rs`

### Step 1 — Define `WalRecord`

```rust
#[derive(Serialize, Deserialize)]
enum WalRecord {
    FactBatch(Vec<CommitChange<StoredFact>>),
    Declaration {
        name: String,
        visibility: Visibility,
        cardinality: Cardinality,
    },
}
```

Place this above `DurableInner`. Both `Visibility` and `Cardinality` are already
imported (or add the imports from `predicate_registry` and `predicates`).

### Step 2 — Update `DurableInner::apply`

Replace the existing `bincode::serialize(&changes)` block with:

```rust
let record = WalRecord::FactBatch(changes.clone());
write_wal_record(&mut *self.log.lock(), &record)
    .map_err(|_| TxError::Internal("log write failed"))?;
```

Extract the write-and-fsync logic into a helper:

```rust
fn write_wal_record(log: &mut BufWriter<File>, record: &WalRecord) -> io::Result<()> {
    let encoded = bincode::serialize(record)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let len = u32::try_from(encoded.len())
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    log.write_all(&len.to_le_bytes())?;
    log.write_all(&encoded)?;
    log.flush()?;
    log.get_mut().sync_data()
}
```

`persist_declaration_to_log` (used in Phase 3) reuses the same helper.

### Step 3 — Add `recovered_registry` and `next_entity_start` fields to `DurableInner`

```rust
pub(crate) struct DurableInner {
    storage: InMemoryStorage,
    recovered_registry: PredicateRegistry,   // ← new
    next_entity_start: u64,                  // ← new
    clock: Arc<dyn Clock>,
    next_tx: AtomicU64,
    next_subscription: AtomicU64,
    subscribers: RwLock<Vec<Subscriber>>,
    log: Mutex<BufWriter<File>>,
}
```

### Step 4 — Update `replay_log`

Change return type to `(u64, PredicateRegistry, u64)` — the third value is
`max_entity_seen`, the largest `subject` or `asserter` observed across all
replayed facts:

```rust
fn replay_log(path: &Path, storage: &InMemoryStorage) -> io::Result<(u64, PredicateRegistry, u64)> {
    let mut file = File::open(path)?;
    let mut max_tx = 0u64;
    let mut max_entity = 0u64;
    let mut registry = PredicateRegistry::new();
    loop {
        // read length prefix — unchanged
        let record: WalRecord = bincode::deserialize(&payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        match record {
            WalRecord::FactBatch(changes) => {
                for change in &changes {
                    max_tx = max_tx.max(replay_change(&change, storage));
                    let fact = change.fact();
                    max_entity = max_entity.max(fact.subject).max(fact.asserter);
                }
            }
            WalRecord::Declaration { name, visibility, cardinality } => {
                registry.declare(&name, visibility, cardinality)
                    .unwrap_or_else(|_| panic!("WAL corruption: conflicting declaration for {name}"));
            }
        }
    }
    Ok((max_tx, registry, max_entity))
}
```

`change.fact()` returns a reference to the inner `StoredFact` regardless of
whether the change is an assert or retract — both carry an entity-bearing fact.

### Step 5 — Update `DurableInner::open`

```rust
fn open(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self> {
    let storage = InMemoryStorage::new(DEFAULT_SHARD_COUNT);
    let (max_tx, recovered_registry, max_entity) = replay_log(path, &storage)?;
    let file = OpenOptions::new().append(true).open(path)?;
    Ok(Self {
        storage,
        recovered_registry,
        next_entity_start: max_entity + 1,
        clock,
        next_tx: AtomicU64::new(max_tx + 1),
        next_subscription: AtomicU64::new(1),
        subscribers: RwLock::new(Vec::new()),
        log: Mutex::new(BufWriter::new(file)),
    })
}
```

`DurableInner::create` initialises both `recovered_registry: PredicateRegistry::new()`
and `next_entity_start: 1`.

---

## Phase 3 — `TripleStore` trait extension

**Files:** `triple_store.rs`, `durable_file_store.rs`, `in_memory_store.rs`

### Step 1 — Add default methods to `TripleStore`

In `triple_store.rs`, add imports and three default methods (see Architectural
Decisions above). `InMemoryTripleStore` inherits all three defaults unchanged.

```rust
/// Return the first entity ID to use after recovering from a prior session.
///
/// Ensures: returned value is greater than every entity ID committed to the
///   store in any prior session; returns 1 for non-durable backends.
fn recovered_next_entity(&self) -> u64 {
    1
}
```

### Step 2 — Implement for `DurableFileTripleStore`

```rust
impl TripleStore<StoredFact> for DurableFileTripleStore {
    // existing impls unchanged …

    fn persist_declaration(
        &self,
        name: &str,
        visibility: Visibility,
        cardinality: Cardinality,
    ) -> io::Result<()> {
        let record = WalRecord::Declaration {
            name: name.to_owned(),
            visibility,
            cardinality,
        };
        write_wal_record(&mut *self.inner.log.lock(), &record)
    }

    fn recovered_registry(&self) -> PredicateRegistry {
        self.inner.recovered_registry.clone()
    }

    fn recovered_next_entity(&self) -> u64 {
        self.inner.next_entity_start
    }
}
```

`persist_declaration` acquires the log `Mutex` internally. The caller (`Store`)
already holds `declare_lock`, so the two locks are never acquired in the opposite
order — no deadlock risk.

---

## Phase 4 — `Store` plumbing

**File:** `store.rs`

### Step 1 — `Store::new` seeds from `recovered_registry` and `recovered_next_entity`

```rust
pub fn new(backend: Arc<dyn TripleStore<StoredFact>>) -> Self {
    let initial = backend.recovered_registry();
    let next_entity = backend.recovered_next_entity();
    Store {
        backend,
        registry: Arc::new(ArcSwap::from_pointee(initial)),
        declare_lock: Arc::new(Mutex::new(())),
        next_entity: Arc::new(AtomicU64::new(next_entity)),
    }
}
```

### Step 2 — `Store::declare` persist-before-publish

```rust
pub fn declare(
    &self,
    name: &str,
    visibility: Visibility,
    cardinality: Cardinality,
) -> Result<Predicate, StoreError> {
    let _guard = self.declare_lock.lock();
    let current = self.registry.load_full();
    let mut next = (*current).clone();
    // Validate metadata before any I/O.
    let id = next.declare(name, visibility, cardinality)
        .map_err(|e| StoreError::DeclarationConflict(e))?;
    // Persist; on failure the ArcSwap is NOT updated.
    self.backend.persist_declaration(name, visibility, cardinality)
        .map_err(|_| StoreError::Internal("declaration persist failed"))?;
    self.registry.store(Arc::new(next));
    Ok(id)
}
```

### Step 3 — `StoreError` gains `DeclarationConflict`

```rust
pub enum StoreError {
    AccessDenied { predicate: PredicateMeta, reason: &'static str },
    DeclarationConflict(PredicateError),
    FactNotFound,
    Internal(&'static str),
}
```

Existing call sites that matched on `PredicateError` are updated to match on
`StoreError::DeclarationConflict(e)`. In the existing test suite, `store_3()`
calls `store.declare(...).unwrap()` — the only change is unwrapping a
`StoreError` instead of a `PredicateError`, which is `.unwrap()`-compatible
without test edits.

---

## Phase 5 — Test Suite

**File:** `durable_file_store.rs` test module (new tests) and `store.rs` test
module (updated error type on `declare`).

### New tests in `durable_file_store.rs`

```
declare_persists_across_reopen
  Create store. Declare "account.balance" as Public/Single.
  Reopen the store. recovered_registry().lookup("account.balance") returns Some.
  meta(id) == (Public, Single).

two_declarations_both_persist_across_reopen
  Declare "p" and "q" with distinct visibility/cardinality.
  Reopen. Verify both are present with their exact metadata.

declaration_interleaved_with_facts_survives_reopen
  Declare "score". Assert a fact on it. Declare "label".
  Reopen. Both predicates in registry; the fact is queryable.

redeclare_identical_after_reopen_is_idempotent
  Declare "p" as Shared/Set. Reopen. Declare "p" as Shared/Set again —
  must return Ok with the same Predicate id, no error.

redeclare_conflicting_after_reopen_errors
  Declare "p" as Private/Single. Reopen.
  Declare "p" as Public/Single — must return DeclarationConflict.

entity_ids_do_not_collide_after_reopen
  Declare a predicate. Allocate two entities via Store::alloc_entity; assert
  facts with each as subject. Reopen the store via a new Store::new. Allocate
  one more entity — its ID must be strictly greater than both prior IDs. Verify
  that querying by the prior entity IDs still returns their original facts.
```

### Store-layer test updates

`store_3()` calls `store.declare(...).unwrap()`. The unwrap succeeds as before;
no assertion on the error type is made, so no test body changes are required
beyond compiling against `StoreError` instead of `PredicateError`.

Any test that explicitly checks for a `PredicateError` return (currently none in
`store::tests`) would need updating.

---

## Invariant — all existing tests pass

The 136 tests from chunks 1–13 must remain green. The only breaking change
visible to test code is `Store::declare` returning `Result<Predicate, StoreError>`
instead of `Result<Predicate, PredicateError>`. All existing call sites use
`.unwrap()` or pattern-match on the `Ok` arm only, so no test body changes are
required.

---

## Open Questions

### Log compaction

Full replay is O(total committed facts + total declarations). Until interpreter
sessions show measurable startup latency, no compaction mechanism is needed.
When added, the compactor must emit a contiguous prefix of declaration records
(to reconstruct the registry) before any fact records in the compacted segment.

### `Store` initialization without registry seeding for `in_memory` paths

`Store::new` now calls `backend.recovered_registry()`, which returns a fresh
empty registry for `InMemoryTripleStore`. This is a no-cost extra call but is
slightly surprising in the in-memory case. If it becomes a friction point in
tests, `Store::in_memory()` can be optimised to skip the call — but this is not
needed now.

---

## What This Sets Up (Chunk 15 Preview)

With the registry surviving restart, the interpreter bootstrap can begin:

- A well-known predicate scheme (e.g., `"smol.op.kind"`, `"smol.op.args"`,
  `"smol.result"`) can be declared once at program startup and relied upon across
  sessions.
- The dispatch loop subscribes to `"smol.op.kind"` events; each event encodes an
  operation to execute.
- Results are written back as facts on `"smol.result"`.

The minimum viable bootstrap needs: stable predicate names (this chunk),
allocated entity IDs for each "process" (chunk 13), and a subscription-driven
dispatch loop (chunk 15).
