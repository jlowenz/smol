# Chunk 27.5: Atemporal Value Store

## Goal

Extend the `triple-store` crate with a content-addressed, atemporal value store
for composite value object structures. C27 established `Term::ValueRef(EntityId)`
as the reference type and `content_hash_id` as the identity scheme; C27.5 builds
the registry that makes those references resolvable and ensures they are never
committed to the triple store without a corresponding structural record.

---

## Background

### Why a separate store

The triple store answers "what temporal assertions exist about entities." Composite
value objects (e.g. `PhoneNumber`, `Money`, `Point`) are not temporal assertions —
they have no subject, no valid-time interval, no owner. The triple store correctly
records *references* to values (`Term::ValueRef(hash_id)`) but has no place for
the structure those references point to.

Storing value structure as temporal facts would raise unanswerable questions:
subject = ?, valid_from = ?, asserter = ?. Values are eternal and definitional:
`Point(1, 2)` is the same value forever. They need an orthogonal, atemporal
registry keyed by content hash.

### Relationship to the triple store

The two stores are complementary:
- Triple store: `(entity_id, #phone, Term::ValueRef(hash_id))` — "Person had this
  phone value starting at T"
- Value store: `hash_id → StoredValue { class: PhoneNumber, named_fields: [area:
  415, number: 5551234], indexed_fields: [] }` — "what hash_id actually is"

`Term::ValueRef` variants in historical ivar facts must always resolve. This
requires that value records are committed *at the same time as or before* the
facts that reference them.

### Commit ordering

`OwnedStoreTransaction::commit()` returns `Result<_, StoreError>` — it is not
irrefutable. The main failure path is `StoreError::FactNotFound` when a retract
targets a non-existent open fact.

Correct commit ordering in `OwnedStoreTransaction::commit()`:
1. Commit ivar facts to the triple store (`tx.commit()`). If this returns `Err`,
   drop the value buffer and propagate the error. No values are stored; no
   `Term::ValueRef` facts exist; no orphans.
2. Only on `Ok`: flush the value buffer to the permanent value store
   (`value_store.commit_value` for each entry).

For the in-memory backend the value flush is a `HashMap` insert and is
infallible, so no new error surface. A future persistent backend may need to
handle flush failures after fact-commit succeeds — a known deferred concern.

### Interpreter commit handling

The interpreter's `run_entity_loop` wraps each handler dispatch in a
`OwnedStoreTransaction`. It must treat `StoreError::FactNotFound` on commit as a
non-fatal warning, not an error signal to the handler. The specific case is a
nil-assignment to an ivar that was never set — a retract targeting a
non-existent open fact — which should be silently ignored: the desired state
(no value for this ivar) is already achieved.

### Orphaned values and glacier

The only way a value can be "orphaned" (no `Term::ValueRef` pointing to it) is if
a commit fails after the value buffer was already flushed — not possible with the
ordering above for in-memory backends.

Because the triple store never removes historical facts (only closes their
intervals), a value referenced in any historical ivar fact is always reachable.
True garbage collection (deletion) would require proving no historical fact will
ever be queried — an unreliable guarantee. The correct long-term model is
"glacier" storage: identify values not referenced by any *currently-open* fact and
move them to cold storage. This is a future concern; the design accommodates it
via `ValueStore::all_ids()`.

### Recursive values

Value objects can contain other value objects as ivars. A `LineItem` might have an
ivar `price: Money`. When storing a `LineItem`, its `price` field becomes
`Term::ValueRef(money_hash_id)`, and `money_hash_id` must itself be in the value
store. The bridge processes the sub-tree bottom-up (innermost values first),
collecting all `(id, StoredValue)` pairs into a flat buffer before any commit.

### Indexed slots on value objects

Value classes can use indexed slots (e.g. an immutable `Tuple` or
`ImmutableArray`). `StoredValue` carries both named and indexed fields:
- `named_fields: Vec<(Arc<str>, Term)>` — named ivars, ordered by class definition
- `indexed_fields: Vec<Term>` — position-indexed slots; position = index

---

## Design

### Types (in `triple-store`)

```rust
/// Structural record for a content-addressed value object.
///
/// Both named and indexed fields are stored as `Term` values. Nested value
/// objects appear as `Term::ValueRef(hash_id)`, establishing recursive structure
/// by reference rather than nesting.
pub struct StoredValue {
    pub class_name: Arc<str>,
    pub named_fields: Vec<(Arc<str>, Term)>,  // ordered by class definition
    pub indexed_fields: Vec<Term>,             // position = index
}

/// Content-addressed, atemporal registry for value object structures.
///
/// `pub(crate)` — not exposed in the `Store` public API. Accessed only via
/// `OwnedStoreTransaction::buffer_value` (write path) and
/// `Store::lookup_value` (read path).
pub(crate) struct ValueStore {
    permanent: RwLock<HashMap<EntityId, Arc<StoredValue>>>,
    // future: glacier: RwLock<HashMap<EntityId, Arc<StoredValue>>>
}

impl ValueStore {
    /// Insert `value` at `id` if not already present. Idempotent.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0` (content-hash space).
    /// Ensures: `lookup(id)` returns `Some(_)` after this call.
    pub(crate) fn commit_value(&self, id: EntityId, value: StoredValue);

    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
    /// Ensures: `Some(v)` iff `id` was previously committed.
    pub(crate) fn lookup(&self, id: EntityId) -> Option<Arc<StoredValue>>;

    /// All stored value IDs. Entry point for a future glacier pass.
    pub(crate) fn all_ids(&self) -> Vec<EntityId>;
}
```

### Embedding in `Store`

`Store` gains a `value_store: Arc<ValueStore>` field, shared across all
`clone_handle()` instances alongside the triple-store backend. No public
`value_store()` accessor is exposed. The two public surfaces are:

- `OwnedStoreTransaction::buffer_value(id: EntityId, value: StoredValue)` — called
  by the interpreter bridge when converting a value object to `Term::ValueRef`
- `Store::lookup_value(id: EntityId) -> Option<Arc<StoredValue>>` — called by
  `resolve_term` when expanding `Term::ValueRef` back to a `Value`

### `OwnedStoreTransaction` changes

```rust
pub struct OwnedStoreTransaction {
    store: Store,
    ctx: ExecContext,
    tx: Transaction<StoredFact>,
    value_buf: Vec<(EntityId, StoredValue)>,   // new
}

impl OwnedStoreTransaction {
    /// Stage a value for commit. Infallible; the value is not yet in the store.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
    /// Ensures: if `commit()` returns `Ok`, `store.lookup_value(id)` returns `Some`.
    ///   If `commit()` returns `Err`, the value is discarded with no side effects.
    pub fn buffer_value(&mut self, id: EntityId, value: StoredValue);

    /// Commit all buffered ivar facts and staged values.
    ///
    /// Ordering:
    ///   1. Commit ivar facts via `tx.commit()`. On `Err`, drop value buffer,
    ///      return error. No values are stored; no Term::ValueRef facts exist.
    ///   2. On `Ok`, flush value buffer: call `value_store.commit_value` for each
    ///      entry. For in-memory backends this is infallible.
    ///
    /// Ensures: every `Term::ValueRef(id)` in a committed fact resolves via
    ///   `store.lookup_value(id)` immediately after this call returns `Ok`.
    pub fn commit(self) -> Result<Vec<CommitChange<StoredFact>>, StoreError>;
}
```

### Bridge changes (interpreter)

```rust
// store_bridge.rs

/// Collect the full value sub-tree rooted at `v` into `buf`, bottom-up.
///
/// Requires: `v` is a `Value::Instance` with `InstanceKind::ValueObject`.
/// Ensures: `buf` contains one entry per distinct value in the sub-tree,
///   innermost values first. The returned `EntityId` is the root's hash id.
fn collect_value_subtree(
    v: &Value,
    buf: &mut Vec<(EntityId, StoredValue)>,
) -> EntityId;

// Caller pattern in `buffer_ivar` and `buffer_indexed_slot`:
//   let mut val_buf = Vec::new();
//   let hash_id = collect_value_subtree(v, &mut val_buf);
//   for (id, sv) in val_buf {
//       tx.buffer_value(id, sv);
//   }
//   tx.assert_single(Fact { ..., object: Term::ValueRef(hash_id) });
```

---

## Steps

- **S1** Add `StoredValue` (pub) and `ValueStore` (pub(crate)) to
  `crates/triple-store/src/value_store.rs`. `StoredValue`: `class_name`,
  `named_fields`, `indexed_fields` as above. `ValueStore`: `permanent:
  RwLock<HashMap<EntityId, Arc<StoredValue>>>` with `commit_value`, `lookup`,
  `all_ids`. No `TxId` tracking; idempotent insert.
  `cargo test` for triple-store green.

- **S2** Embed `ValueStore` in `Store`. Add `value_store: Arc<ValueStore>` to
  `Store` struct. Update `Store::new` to initialise it; update `clone_handle` to
  `Arc::clone` it. Add `pub fn lookup_value(&self, id: EntityId) ->
  Option<Arc<StoredValue>>` on `Store`. No other public API changes.
  `cargo test` green.

- **S3** Add `value_buf` to `OwnedStoreTransaction`. Implement `buffer_value`
  (infallible push). Modify `commit` to: (a) call `tx.commit()` first; on `Err`
  propagate immediately (value buffer dropped); on `Ok` (b) call
  `value_store.commit_value` for each entry in `value_buf`.
  Add `StoreTransaction` parity: a `StoreTransaction::buffer_value` is not needed
  (the interpreter only uses `OwnedStoreTransaction`), but note the asymmetry.
  Unit test: a `buffer_value` + `commit` succeeds and `lookup_value` returns the
  record; a `buffer_value` + `rollback` leaves the store empty.
  `cargo test` green.

- **S4** Implement `collect_value_subtree` in `store_bridge.rs`. Handles named
  ivars and indexed slots. Recursively processes nested value objects before the
  container (bottom-up). Returns the root `EntityId`. Unit tests: flat value
  (primitive ivars only); nested value (one level deep); value with indexed slots;
  shared nested value (same inner value referenced twice produces one entry in buf).
  `cargo test` green.

- **S5** Wire `collect_value_subtree` into `buffer_ivar` and `buffer_indexed_slot`
  (replacing the C27 `Assert(Term::ValueRef(_))` stub that deferred structural
  storage). The bridge now calls `collect_value_subtree` before building the
  `Term::ValueRef` assertion.
  Integration test: assign a value-object ivar, commit, then `lookup_value` on the
  resulting hash id returns the correct `StoredValue`. Reassign with a different
  value object produces a new hash id and a new record; old record still present.
  `cargo test` green.

- **S6** Handle `StoreError::FactNotFound` in `run_entity_loop`. Change the commit
  arm from a hard error to a logged warning when the error is `FactNotFound`
  (nil-assignment of a never-set ivar). All other errors remain fatal to the
  handler.
  `cargo test` green.

- **S7** `cargo fmt`, `cargo clippy --all-targets`, full `cargo test`, commit.

---

## Files changed

| File | Change |
|---|---|
| `crates/triple-store/src/value_store.rs` | New: `StoredValue`, `ValueStore` |
| `crates/triple-store/src/lib.rs` | Re-export `StoredValue` (pub); keep `ValueStore` pub(crate) |
| `crates/triple-store/src/store.rs` | Add `value_store` field; `lookup_value`; `OwnedStoreTransaction::buffer_value`; commit ordering |
| `crates/interpreter/src/store_bridge.rs` | `collect_value_subtree`; wire into `buffer_ivar` / `buffer_indexed_slot` |
| `crates/interpreter/src/entity.rs` | `run_entity_loop`: `FactNotFound` → warning, not error |
| `crates/interpreter/tests/eval.rs` | Integration tests: value ivar commit, `lookup_value`, rollback isolation |

---

## Deferred

- **Glacier storage** — values unreferenced by any currently-open fact could be
  archived to cold storage. `ValueStore::all_ids()` is the traversal entry point.
  Requires a full scan of open triple-store facts to identify live references.
  Not urgent while the system is in-memory only.
- **Persistent backend flush error handling** — for a durable backend, the value
  flush step (S3) could fail after facts are committed. The correct recovery
  (retry, compensating retraction, or write-ahead log) depends on the backend's
  durability model. Deferred to the persistent backend chunk.
- **`StoreTransaction::buffer_value`** — only `OwnedStoreTransaction` is used by
  the interpreter. If a use case for `StoreTransaction`-based value buffering
  arises, add it then.
- **Value store recovery on session resume** — on startup with a durable store,
  value records must be re-populated. Because `content_hash_id` is deterministic,
  the value store could be reconstructed by replaying all historical `ValueRef`
  terms and re-running `collect_value_subtree` on live instances, or by persisting
  the value store itself. Design deferred to the session-resume chunk.
