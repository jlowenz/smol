# Chunk 27.6: Value Store — Architectural Correction

## Goal

Relocate value object storage from `Store` (the façade) into the `TripleStore`
backends, following the same structural pattern as fact storage. C27.5 placed
`ValueStore` as a free-standing `Arc<ValueStore>` field on `Store`, decoupled
from the backend. This means:

- `DurableFileTripleStore` never writes value records to the WAL — after restart
  every `Term::ValueRef(id)` in the triple store resolves to `None`.
- `InMemoryTripleStore` owns no part of its own value storage, which is
  architecturally inconsistent with how it owns fact storage.

The correct structure: each backend owns and manages its own value storage.
The write path (buffering + committing values) belongs on `Transaction`, like
`commit`/`rollback`. The read path (`lookup_value`) belongs on `TripleStore`,
like `query_open`/`query_at`.

---

## Design

### Write path

`Applier<TFact>::apply` gains a `values` parameter so facts and values commit
atomically:

```rust
fn apply(
    &self,
    tx_id: TxId,
    ops: Vec<TentativeOp>,
    values: Vec<(EntityId, StoredValue)>,
) -> Result<Vec<CommitChange<TFact>>, TxError>;
```

`Transaction<TFact>` gains a value buffer and a staging method:

```rust
pub struct Transaction<TFact> {
    // existing fields...
    value_buf: Vec<(EntityId, StoredValue)>,
}

impl<TFact> Transaction<TFact> {
    /// Stage a value for atomic commit with the buffered facts.
    ///
    /// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
    /// Ensures: if `commit()` returns `Ok`, the value is durable in the backend.
    ///   If `commit()` returns `Err`, the value is discarded with no side effects.
    pub fn buffer_value(&mut self, id: EntityId, value: StoredValue);
}
```

`Transaction::commit()` passes `value_buf` to `Applier::apply`. `rollback()`
drops both ops and the value buffer.

`OwnedStoreTransaction` is simplified:
- Its `value_buf` field and manual flush in `commit()` are removed.
- `OwnedStoreTransaction::buffer_value` delegates to `self.tx.buffer_value`.
- `OwnedStoreTransaction::commit()` is just `self.tx.commit()`.

### Read path

`TripleStore<TFact>` gains one method (no default — both backends must implement):

```rust
/// Look up a previously committed value object by its content-hash id.
///
/// Requires: `id.0 & 0x8000_0000_0000_0000 != 0`.
/// Ensures: `Some(v)` iff the value was committed in this session or recovered
///   from a prior session; `None` otherwise.
fn lookup_value(&self, id: EntityId) -> Option<Arc<StoredValue>>;
```

`Store::lookup_value` delegates to `self.backend.lookup_value()`.
`Store` drops the `value_store: Arc<ValueStore>` field entirely.

### Backend changes

**`InMemoryTripleStore`** (via its inner struct / applier):
- Embeds `ValueStore` directly.
- `apply`: stores values in its `ValueStore`.
- Implements `lookup_value`.

**`DurableFileTripleStore`** (`DurableInner`):
- Embeds `ValueStore` directly.
- `WalRecord::FactBatch` is replaced by `WalRecord::Commit { facts, values }`,
  committing both atomically in a single record. The old `FactBatch` variant is
  kept as a legacy replay variant (reads of old WAL files still work).
- `DurableInner::apply`: writes `Commit { facts, values }` WAL record; stores
  values in its `ValueStore`.
- `replay_log`: handles both `FactBatch` (legacy, values empty) and `Commit`
  (populates `DurableInner.value_store`).
- `DurableInner.create` / `DurableInner.open` initialise the embedded
  `ValueStore`; `open` repopulates it from `Commit` records during replay.
- Implements `lookup_value`.

### `StoredValue` Serde

`StoredValue` needs `#[derive(Serialize, Deserialize)]` so it can be encoded in
WAL records. `Term`, `EntityId`, and `Arc<str>` already derive Serde.

---

## Steps

- **S1** Add `#[derive(Serialize, Deserialize)]` to `StoredValue`.
  `cargo test` green.

- **S2** Embed `ValueStore` in both backends. Add `lookup_value` to
  `TripleStore<TFact>` trait; implement on both backends.
  Remove `value_store` field from `Store`; `Store::lookup_value` delegates to
  backend.
  Unit tests: `lookup_value` returns `None` on a fresh store; returns a value
  after a direct backend insert (via a test helper).
  `cargo test` green.

- **S3** Extend `Applier<TFact>::apply` with `values` parameter.
  Update both applier implementations (in-memory: store in `ValueStore`;
  durable: store in `ValueStore`, WAL write added in S4).
  Move `value_buf` and `buffer_value` into `Transaction<TFact>`;
  `Transaction::commit()` passes `value_buf` to `Applier::apply`.
  `OwnedStoreTransaction`: delegate `buffer_value` to `self.tx`, drop manual
  value flush from `commit()`, drop `value_buf` field.
  Unit tests: `buffer_value` + `commit` round-trips via `lookup_value`; `rollback`
  leaves value store empty; fact-commit failure discards values.
  `cargo test` green.

- **S4** WAL persistence for values in `DurableFileTripleStore`.
  Add `WalRecord::Commit { facts, values }`. Keep `WalRecord::FactBatch` as a
  legacy read-only replay variant.
  `DurableInner::apply`: emit `Commit` record (replaces `FactBatch` write).
  `replay_log`: handle both `FactBatch` (no values) and `Commit` (replay facts
  + populate `value_store`).
  Durable integration test: commit a value-object ivar, reopen the store,
  `lookup_value` returns the correct `StoredValue`.
  `cargo test` green including parity suite.

- **S5** `cargo fmt`, `cargo clippy --all-targets`, full `cargo test`, commit.

---

## Files changed

| File | Change |
|---|---|
| `crates/triple-store/src/value_store.rs` | Add `Serialize`/`Deserialize` to `StoredValue` |
| `crates/triple-store/src/triple_store.rs` | Add `lookup_value` to trait |
| `crates/triple-store/src/transaction.rs` | Add `value_buf`, `buffer_value`; pass to `Applier::apply` in `commit` |
| `crates/triple-store/src/in_memory_store.rs` | Embed `ValueStore`; implement `lookup_value`; handle `values` in applier |
| `crates/triple-store/src/durable_file_store.rs` | Embed `ValueStore` in `DurableInner`; `WalRecord::Commit`; replay; implement `lookup_value` |
| `crates/triple-store/src/store.rs` | Drop `value_store` field; delegate `lookup_value`; simplify `OwnedStoreTransaction` |
| `crates/interpreter/src/store_bridge.rs` | `buffer_value` call site unchanged (delegates through `OwnedStoreTransaction` → `Transaction`) |

`entity.rs` and `store_bridge.rs` call sites need no substantive changes — `OwnedStoreTransaction::buffer_value` still exists, just delegates one level deeper.

---

## Invariant preserved

The C27.5 commit-ordering invariant is maintained: the `Applier` only stores
values after facts are committed (they arrive together in the same `apply` call).
A `TxError` from ops processing returns before values are stored, so no orphaned
`ValueRef` facts can exist.
