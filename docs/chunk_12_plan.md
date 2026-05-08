# Chunk 12: DurableFileTripleStore

## Status

Chunk 11 shipped `StoreTransaction`, visibility-filtered subscriptions, `Clone`
on registry types, and `Store::now` / `Store::query_as_of`. 125 tests passing.

Chunk 12 adds one thing: a durable, file-backed `TripleStore<StoredFact>`
implementation that plugs into `Store::new(backend)` with zero changes to
`Store` or any caller above it.

`Store::clone_handle()` and entity ID allocation remain deferred (see Open
Questions).

---

## Architectural Decisions

### WAL-based append-only log

Each committed transaction appends one length-prefixed binary record to a
write-ahead log file. The record is a `Vec<CommitChange<StoredFact>>` — the
canonical post-apply change set. On `open(path)`, the file is replayed from
the beginning: each record's changes are applied directly to `InMemoryStorage`,
restoring the full in-memory state. This makes recovery simple and correct:

- No secondary index state needs to be serialised — it is derived by replay.
- A truncated last record (crash mid-write) is detected by a length mismatch
  and skipped with a warning; earlier records are intact.
- Full replay is O(total committed facts), which is acceptable until compaction
  becomes necessary.

Compaction (snapshot + shorter tail) is explicitly deferred.

### Binary serialization: serde + bincode

`serde` is added to the workspace. `bincode` (v1) is added to the crate as a
dependency. Core domain types derive `Serialize` / `Deserialize`. `bincode`
encodes into compact fixed-width integers, which is appropriate for a store
where records are queried by index rather than inspected as text.

`Term::Str(Arc<str>)` and `Term::Bytes(Arc<[u8]>)` require `serde`'s `rc`
feature. Both serialize as their content (`str` → string, `[u8]` → bytes);
deserialization reconstructs via `Arc::from`. Flag `features = ["derive", "rc"]`
on the `serde` workspace dependency.

`FloatKey` wraps `f64` with bit-equality semantics. Its on-disk form is the raw
`u64` bit pattern; a custom `Serialize` / `Deserialize` pair preserves this
without relying on `f64`'s `Serialize` (which uses IEEE semantics and could
represent NaN inconsistently). Same treatment for `OrderingKey`.

### Shared commit logic: `collapse_and_apply` + `dispatch_changes`

The commit path in `Inner::apply` has two purely shared phases — collapsing ops
and dispatching events — with the only divergence being the WAL write that
`DurableInner` inserts between them. These are extracted as `pub(crate)` free
functions in `in_memory_store.rs`:

```rust
pub(crate) fn collapse_and_apply(
    tx_id: TxId,
    ops: Vec<TentativeOp>,
    clock: &dyn Clock,
    storage: &InMemoryStorage,
) -> Result<Vec<CommitChange<StoredFact>>, TxError>

pub(crate) fn dispatch_changes(
    tx_id: TxId,
    changes: &[CommitChange<StoredFact>],
    subscribers: &RwLock<Vec<Subscriber>>,
)
```

`Inner::apply` becomes:
```rust
fn apply(&self, tx_id, ops) -> ... {
    let changes = collapse_and_apply(tx_id, ops, &*self.clock, &self.storage)?;
    dispatch_changes(tx_id, &changes, &self.subscribers);
    Ok(changes)
}
```

`DurableInner::apply` inserts its WAL write between the two shared calls:
```rust
fn apply(&self, tx_id, ops) -> ... {
    let changes = collapse_and_apply(tx_id, ops, &*self.clock, &self.storage)?;
    if !changes.is_empty() { /* WAL write */ }
    dispatch_changes(tx_id, &changes, &self.subscribers);
    Ok(changes)
}
```

No code duplication. The WAL write — ~8 lines — is the only divergence.

### Standalone implementation: `DurableFileTripleStore` + `DurableInner`

`DurableFileTripleStore` is a new type in a new file (`durable_file_store.rs`).
It does **not** wrap `InMemoryTripleStore` or `Inner` directly — those are
`pub(crate)` to `in_memory_store.rs`. Instead, `DurableInner` is a parallel
structure with the same fields as `Inner` (clock, `InMemoryStorage`,
`next_tx`, `next_subscription`, `subscribers`) plus a `Mutex<BufWriter<File>>`
for the log.

`DurableInner` implements `Applier<StoredFact>` via `collapse_and_apply` and
`dispatch_changes`. The call order is apply → persist → dispatch → return; the
in-memory state is always ahead of or equal to the durable state (standard WAL
guarantee). If the WAL write fails after storage has been updated, the
in-memory state diverges from the durable log for that session; this is
accepted at this stage and noted in the doc comment on `apply`.

### fsync per commit

After appending a record, `DurableInner` calls `file.flush()` and
`file.sync_data()` (or `sync_all()` where metadata durability is required).
This serialises all committed transactions to stable storage before returning to
the caller. A future batching option (fsync every N commits) is not added now.

### TxId continuity after replay

During replay, `next_tx` is updated to `max(tx_id_seen) + 1`. This ensures
that transactions created after `open` do not collide with replayed ones. If no
records are found (empty or new file), `next_tx` starts at `1` as normal.

### Single-writer assumption

No file locking is used in this chunk. Two `DurableFileTripleStore` instances
opening the same path concurrently is undefined behaviour (they will corrupt
each other's log). This is noted in the doc comment on `open`. File locking via
`flock` or `LockFile` is deferred.

### Log record format

```
[4-byte record length, little-endian u32]
[`bincode`-encoded Vec<CommitChange<StoredFact>>, `length` bytes]
```

Empty commits (zero changes) do not write a record. Recovery skips records
whose payload cannot be fully read (length byte present but payload truncated).

---

## What Already Exists (unchanged by this chunk)

- All of `Store`, `StoreTransaction`, `ExecContext`, `AccessError` — not modified.
- `InMemoryTripleStore`, `Inner`, `InMemoryStorage` — not modified.
- `TripleStore<TFact>`, `Applier<TFact>`, `Transaction<TFact>` — not modified.
- `CommitChange`, `ChangeEvent`, `Subscriber`, `Subscription` — not modified.

---

## Phase 1 — Serialization Derives

**Files touched:** `Cargo.toml` (workspace), `crates/triple-store/Cargo.toml`,
`term.rs`, `id.rs`, `time.rs`, `predicates.rs`, `fact.rs`, `storage.rs`

### Step 0 — Workspace and crate dependencies

In `Cargo.toml` (workspace):
```toml
serde = { version = "1", features = ["derive", "rc"] }
bincode = "1"
```

In `crates/triple-store/Cargo.toml`:
```toml
[dependencies]
serde = { workspace = true }
bincode = { workspace = true }
```

### Step 1 — Derive on core types

Add `#[derive(Serialize, Deserialize)]` (via `serde::Serialize`, `serde::Deserialize`) to:

| Type | File | Notes |
|---|---|---|
| `EntityId`, `TxId`, `SubscriptionId`, `SymbolId` | `id.rs` | Newtype u64/u32; trivial |
| `Predicate` | `term.rs` | Newtype u32 |
| `Term` | `term.rs` | `Arc<str>` and `Arc<[u8]>` need serde `rc` feature |
| `SymbolId` | `id.rs` | Already listed above |
| `Timestamp` | `time.rs` | Newtype or struct over u64 |
| `Interval<T>` | `time.rs` | Generic; `T: Serialize` bound added |
| `FloatKey` | `term.rs` | Custom: serialise/deserialise as `u64` bit pattern |
| `Cardinality` | `predicates.rs` | Enum; trivial |
| `OrderingKey` | `predicates.rs` | Custom: same bit-pattern approach as `FloatKey` |
| `Fact` | `fact.rs` | All fields already covered |
| `StoredFact` | `fact.rs` | All fields covered |
| `CommitChange<TFact>` | `storage.rs` | Generic enum; `TFact: Serialize` bound |

`FloatKey` and `OrderingKey` custom impls:

```rust
impl Serialize for FloatKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0.to_bits())
    }
}
impl<'de> Deserialize<'de> for FloatKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bits = u64::deserialize(d)?;
        Ok(Self(f64::from_bits(bits)))
    }
}
```

`OrderingKey` uses the same pattern (it is a newtype over `FloatKey` or its own
`f64` — match the existing definition).

### Step 2 — Verify derive compilation

`cargo check` must pass. No new tests in this phase; the derives are verified by
the serialization round-trip tests in Phase 3.

---

## Phase 1.5 — Refactor `Inner` to expose shared commit logic

**File touched:** `in_memory_store.rs`

Before adding `DurableInner`, extract the two shared phases of `Inner::apply`
into `pub(crate)` free functions so `durable_file_store.rs` can call them.

1. Rename `Inner::dispatch` body → free function `dispatch_changes(tx_id,
   changes, subscribers: &RwLock<Vec<Subscriber>>)`.
2. Extract the collapse + apply body of `Inner::apply` → free function
   `collapse_and_apply(tx_id, ops, clock, storage: &InMemoryStorage)`.
3. Simplify `Applier<StoredFact> for Inner` to two lines: call
   `collapse_and_apply`, then `dispatch_changes`.
4. Make `InMemoryStorage::new` `pub(crate)` so `DurableInner` can construct one.

---

## Phase 2 — `DurableFileTripleStore`

**New file:** `crates/triple-store/src/durable_file_store.rs`
**Files touched:** `lib.rs` (module declaration + re-export)

### Step 1 — `DurableInner`

```rust
pub(crate) struct DurableInner {
    storage: InMemoryStorage,
    clock: Arc<dyn Clock>,
    next_tx: AtomicU64,
    next_subscription: AtomicU64,
    subscribers: RwLock<Vec<Subscriber>>,
    log: Mutex<BufWriter<File>>,
}
```

`DurableInner::new(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self>`:
creates or opens the file in append mode and constructs the struct with an empty
`InMemoryStorage`.

`DurableInner::open(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self>`:
opens an existing file for replay. Reads records sequentially; for each decoded
`Vec<CommitChange<StoredFact>>`, applies `Asserted`/`Updated`/`Retracted`
directly to `InMemoryStorage` and advances `next_tx` past the highest seen
`TxId`. After replay, reopens the file in append mode for new writes.

A helper `fn replay_storage(changes: &[CommitChange<StoredFact>], storage: &InMemoryStorage)` applies:
- `Asserted { fact }` → `storage.write(fact.subject, fact.predicate, fact)`
- `Updated { closed, opened }` → close the prior via `storage.supersede`, write `opened`
- `Retracted { closed }` → `storage.close(...)` at `closed.asserted.end.unwrap()`

The replay helper must be careful to use the stored timestamps directly rather than the clock.

`DurableInner` implements `Applier<StoredFact>` via the shared free functions,
inserting the WAL write between them:

```rust
fn apply(&self, tx_id: TxId, ops: Vec<TentativeOp>)
    -> Result<Vec<CommitChange<StoredFact>>, TxError>
{
    let changes = collapse_and_apply(tx_id, ops, &*self.clock, &self.storage)?;

    if !changes.is_empty() {
        let encoded = bincode::serialize(&changes)
            .map_err(|_| TxError::Internal("serialization failed"))?;
        let len = u32::try_from(encoded.len())
            .map_err(|_| TxError::Internal("record too large"))?;
        let mut log = self.log.lock();
        log.write_all(&len.to_le_bytes())
            .map_err(|_| TxError::Internal("log write failed"))?;
        log.write_all(&encoded)
            .map_err(|_| TxError::Internal("log write failed"))?;
        log.flush()
            .map_err(|_| TxError::Internal("log write failed"))?;
        log.get_mut().sync_data()
            .map_err(|_| TxError::Internal("fsync failed"))?;
    }

    dispatch_changes(tx_id, &changes, &self.subscribers);
    Ok(changes)
}
```

### Step 2 — `DurableFileTripleStore`

```rust
/// A `TripleStore<StoredFact>` backed by an append-only write-ahead log.
///
/// All committed facts survive process restart. On `open`, the log is replayed
/// in full to rebuild in-memory state before new transactions are accepted.
///
/// Requires: at most one `DurableFileTripleStore` may open a given path at a
///   time. Concurrent access from multiple instances is undefined behaviour.
pub struct DurableFileTripleStore {
    inner: Arc<DurableInner>,
}
```

Constructors:

```rust
impl DurableFileTripleStore {
    /// Create a new store at `path`, failing if the file already exists.
    ///
    /// Ensures: the log file is created empty; the store is ready for use.
    pub fn create(path: &Path) -> io::Result<Self> { ... }

    /// Open an existing store at `path`, replaying the log to restore state.
    ///
    /// Ensures: all facts committed in prior sessions are queryable.
    ///   `TxId`s issued after `open` do not collide with replayed ones.
    pub fn open(path: &Path) -> io::Result<Self> { ... }

    /// Open if `path` exists, otherwise create a new store.
    pub fn open_or_create(path: &Path) -> io::Result<Self> { ... }
}
```

`Queryable<StoredFact>` and `TripleStore<StoredFact>` implementations mirror
`InMemoryTripleStore`, substituting `self.inner` for `self.inner` (same
structure). The clock uses `SystemClock` by default; a `with_clock` variant is
added for tests.

### Step 3 — Module wiring

In `lib.rs`, add:
```rust
mod durable_file_store;
pub use durable_file_store::DurableFileTripleStore;
```

---

## Phase 3 — Test Suite Parity

**File:** `durable_file_store.rs` (test module) or a separate integration test
file under `tests/`.

The goal is to demonstrate that all behaviours verified for `InMemoryTripleStore`
also hold for `DurableFileTripleStore`. Rather than duplicating every test, a
generic test harness parametrised over a store constructor is preferred:

```rust
fn run_suite<F: Fn() -> Arc<dyn TripleStore<StoredFact>>>(make: F) {
    // all shared test bodies
}

#[test]
fn in_memory_suite() { run_suite(|| Arc::new(InMemoryTripleStore::new())); }

#[test]
fn durable_suite() {
    let dir = tempfile::tempdir().unwrap();
    run_suite(|| Arc::new(DurableFileTripleStore::create(dir.path().join("store.wal")).unwrap()));
}
```

This requires adding `tempfile` to `[dev-dependencies]` in the crate manifest.

Test suite parity means all of: transaction lifecycle, single/set/indexed
cardinality, retract, query_open / query_at / history / query_as_of,
subscriptions (with and without filter), multi-op commit, rollback.

### Durability-specific tests

Add these in the `durable_file_store` test module (not in the shared suite):

- **Persist across reopen**: assert facts, drop the store, `open` the same
  path, verify facts are queryable.
- **TxId continuity**: commit one transaction, record its `TxId`; reopen; the
  first new transaction has a strictly higher `TxId`.
- **Truncated last record is skipped**: write a valid record, then append a
  partial payload (simulated crash); `open` succeeds and the valid record's
  facts are present; the partial record is silently dropped.
- **Empty file is valid**: `create` then immediately `open` returns an empty
  store without error.

---

## Invariant — all existing tests pass

The 125 tests from chunks 1–11 must remain green. No changes to
`InMemoryTripleStore`, `Store`, or any caller.

---

## Open Questions

### `Store::clone_handle()` — still deferred

The registry-sharing model (snapshot vs. `Arc<ArcSwap<PredicateRegistry>>`) is
unresolved (see chunk 11 open questions). `clone_handle` is not added in this
chunk; the concrete use case (interpreter spawning worker contexts) has not yet
materialised.

### Log compaction / snapshots

Full replay is O(total committed facts). For a development environment that may
accumulate millions of facts over a long session, startup time will grow
linearly. A snapshot mechanism (write current `InMemoryStorage` state to a
separate file; only replay the tail after the snapshot) is the natural fix.
Deferred until replay latency is observed to be a problem.

### File locking

Two `DurableFileTripleStore` instances opening the same path will silently
corrupt each other's log. A cross-platform lock (via `flock` on Unix or
`LockFile` via the `fs2` crate) prevents this. Deferred; the single-writer
assumption is documented on the constructors.

### Structured log iteration / inspection

An append-only log is opaque without tooling. A `dump` utility (reads and
pretty-prints records) would help debug and validate the format. Out of scope
for this chunk; noted for tooling phase.

---

## Expected Deliverables

- `Cargo.toml` — `serde` (with `derive` + `rc` features) added to workspace;
  `bincode` added; `tempfile` added to dev-dependencies.
- `id.rs`, `term.rs`, `time.rs`, `predicates.rs`, `fact.rs`, `storage.rs` —
  `Serialize` / `Deserialize` derived or manually implemented on all types
  needed to encode `CommitChange<StoredFact>`.
- `durable_file_store.rs` (new) — `DurableInner`, `DurableFileTripleStore`
  with `create` / `open` / `open_or_create` constructors, `Applier`,
  `Queryable`, and `TripleStore` implementations.
- `lib.rs` — module declaration and public re-export of `DurableFileTripleStore`.
- Tests: shared parity suite covering all `InMemoryTripleStore` behaviours plus
  four durability-specific tests.
- All 125+ prior tests pass.

---

## What This Sets Up (Chunk 13 Preview)

- **`Store::clone_handle()`** — now that a concrete persistent backend exists,
  the multi-handle use case becomes more tangible (e.g., one handle for the
  interpreter, one for the UI). The registry-sharing decision can be revisited
  with a real workload in hand.
- **Interpreter bootstrap** — with a stable, durable `Store`, the next major
  component (a bytecode interpreter or AST evaluator for the programming
  environment) can begin. Entity ID allocation, the `ExecContext` threading
  model, and read-your-writes semantics (PRD §8.5) will need resolution before
  the first interpreter operation.
- **Log compaction** — if replay latency proves to be a problem during early
  interpreter sessions, the snapshot mechanism described above becomes the next
  storage chunk.
