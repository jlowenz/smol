# Chunk 27: Triple Store Integration — Within-Entity State and History

## Goal

Wire the existing `triple-store` crate into the interpreter: entity ivar assignments
become assertions in the shared store; the entity instance remains the in-memory cache
for current state; `historyOf:` / `historyOf:from:to:` make temporal history queryable
from Smol. An initial `World` class provides a gateway for cross-entity queries.

Cross-entity transaction coordination (sagas) is explicitly deferred to C28.

---

## Background

### Shared store, not per-entity

All entities share one `Store` instance distributed as cheap handles via
`Store::clone_handle()`. The store is the temporal journal; the entity's in-memory
`Object` HashMap remains the fast-path read cache. Because actor isolation prevents
external writes, the cache is always coherent — no invalidation needed.

### Why ivar assignment is the integration point

An ivar setter `balance := newVal` is the natural write boundary. After integration:

1. Update the in-memory slot (unchanged — fast path for reads).
2. Assert `(entityId, #balance, newVal)` to the shared store (new — temporal record).

If `newVal` is nil, the store fact is retracted instead, closing the temporal interval.
Nil-assignment means "this entity no longer holds a value for this slot"; the history
of prior values is preserved.

### Transaction granularity

The natural transaction boundary is the message handler: the entity opens a
`StoreTransaction` before dispatching an envelope and commits on success (or rolls
back on error). All ivar assertions within a single handler are buffered and committed
atomically. `Transaction<TFact>` already provides this — buffer methods are infallible,
all storage effects happen at `commit`.

`Store::begin(ctx)` returns `StoreTransaction<'store>`, which borrows `&'store Store`.
This lifetime prevents storing it directly in `EntityData`. The solution is a small
addition to the triple-store crate: `OwnedStoreTransaction`, obtained via
`Store::begin_owned(ctx)`, which holds a `clone_handle()` of the store internally
rather than a borrow. This is `'static`, can be stored in
`RefCell<Option<OwnedStoreTransaction>>` on the `Evaluator`, and set/cleared by
`run_entity_loop` around each handler dispatch. The evaluator's ivar-assertion code
accesses it via the `RefCell` without any lifetime complications.

### EntityId unification

The interpreter currently has `type EntityId = u64` (type alias). The triple store has
`struct EntityId(pub u64)` (proper newtype). These unify in this chunk: the interpreter
re-exports `triple_store::id::EntityId` and retires its own alias. `Store::alloc_entity()`
becomes the sole mint point for entity IDs.

### Value ↔ Term bridge

Every Smol primitive type has a faithful `Term` representation. Non-storable values
(Future, Block, Namespace) are deferred with explicit design notes; they are not
silently discarded as "not domain data." Nil is distinguished from "skip" via an
explicit `BridgeResult` enum (see S4).

| Value | Term |
|-------|------|
| `Int(i64)` | `Term::Int(i64)` |
| `Float(f64)` | `Term::Float(f64)` |
| `Bool(bool)` | `Term::Bool(bool)` |
| `Str(Arc<str>)` | `Term::Str(Arc<str>)` |
| `Symbol(Arc<str>)` | `Term::Symbol(Arc<str>)` — new variant; distinct from Str |
| `LargeInt(Arc<BigInt>)` | `Term::LargeInt(Arc<str>)` — decimal encoding; new variant |
| `Nil` | *(retract the open fact)* |
| `Instance` (Entity/ClassEntity) | `Term::EntityRef(entity_id)` |
| `Instance` (ValueObject) | `Term::EntityRef(content_hash_id)` — see below |
| `Block` | *(deferred — requires AST serialization)* |
| `Future` | *(skip — transient, no meaningful persistent representation)* |
| `Namespace` | *(deferred — requires design for notebook/artifact storage)* |

For `historyOf:` results, the reverse bridge resolves `EntityRef` terms back to live
`InstanceHandle`s via a unified entity registry (see S3). If an ID is not found (e.g.
the entity was terminated or from a prior session not yet resumed), the fallback is
`Value::Int(id.0 as i64)` with a runtime warning.

### Value object storage — content-hash identity

Value objects (immutable, identity-free) are stored using content-hash identity:
a deterministic `u64` hash of the class name and all ivar values (recursively).
The high bit marks the ID as a value-storage ID rather than a live entity ID:
`EntityId(hash | 0x8000_0000_0000_0000)`, preventing collision with `alloc_entity`.

When a value object appears as an ivar value:
1. Compute `content_hash_id` from class name + ordered ivar values (recursively).
2. Buffer the value's own ivar facts into the current transaction under that ID
   using `ExecContext::System`.
3. Return `Term::EntityRef(content_hash_id)` for the referencing entity's ivar.

`assert_single` is idempotent for already-stored facts (supersedes with the same
value), so no "already stored?" guard is needed for correctness. Value facts
participate in the handler's transaction: if the handler rolls back, value storage
rolls back too.

The hash function must be deterministic across runs for persistence. FNV-1a (64-bit)
is implemented inline — no new dependency — and is stable across Rust versions.

Reference counting / structural sharing optimisation (store each unique value only
once) is deferred; the idempotent re-assert approach is correct if slightly wasteful.

### Class variables

`class_var_storage` on `ClassEntity` instances is deferred. The ExecContext for
class-side assertions is slightly different and is a small follow-on once instance
ivars are solid.

---

## Steps

- **S1** Add `triple-store` dependency to `crates/interpreter/Cargo.toml`. Retire
  `pub type EntityId = u64` in `entity.rs`; re-export
  `pub use triple_store::id::EntityId`. Update all call sites:
  - The monotonic counter now produces `EntityId(fetch_add(...))` rather than a bare `u64`.
  - `Block.home_entity: Option<EntityId>` picks up the new type automatically.
  - Comparisons and `From`/`Into` uses throughout `eval.rs`, `builtins.rs`, etc.
  `cargo build` and `cargo test` green.

- **S2** Extend `Term` and add `OwnedStoreTransaction` to the triple-store crate.

  **Term extension** (`term.rs`): add two new variants to ensure complete Smol round-trip:
  - `Term::Symbol(Arc<str>)` — distinct from `Term::Str`; required for correct Symbol
    reconstruction on read-back.
  - `Term::LargeInt(Arc<str>)` — decimal string encoding of arbitrary-precision integers,
    replacing the lossy `Str(to_string())` workaround in the bridge.
  Update `PartialEq`, `Hash`, and any match exhaustiveness sites in `term.rs` and
  callers. `cargo test` for triple-store green.

  **`OwnedStoreTransaction`** (`store.rs`): new type alongside `StoreTransaction`:
  ```rust
  pub struct OwnedStoreTransaction {
      store: Store,   // clone_handle() — not a borrow
      ctx: ExecContext,
      tx: Transaction<StoredFact>,
  }
  ```
  `Store::begin_owned(ctx: ExecContext) -> OwnedStoreTransaction` calls
  `self.clone_handle()` internally; mirrors `begin` but without the `'store` lifetime.
  Exposes `assert_single`, `assert_set`, `retract_single` (access-checked eagerly,
  buffered) and `commit(self)` / `rollback(self)`.
  `cargo test` for the triple-store crate green.

- **S3** Build the entity registry and thread `Store` through the interpreter.

  **Entity registry**: the interpreter has two existing collections that need upgrading
  from `Vec` to `HashMap<EntityId, Arc<InstanceState>>`:
  - `Evaluator::instance_entities: RefCell<Vec<...>>` →
    `RefCell<HashMap<EntityId, Arc<InstanceState>>>`.
  - `ClassRegistry::class_entities: RwLock<Vec<...>>` →
    `RwLock<HashMap<EntityId, Arc<InstanceState>>>`.

  Add a third registry for content-hash-addressed value objects:
  - `Evaluator::value_objects: RefCell<HashMap<EntityId, Arc<InstanceState>>>`.

  Insertion points: `spawn_entity_bare` registers into `instance_entities`;
  `spawn_class_entity` registers into `class_entities`; `ensure_value_buffered`
  (S4) registers into `value_objects`. All insertions key on the instance's `EntityId`
  (or content-hash ID for value objects).

  **Store threading**:
  - Add `store: Store` to `EntityData`.
  - Add `store: Store` parameter to `spawn_entity_bare`, `spawn_class_entity`, and
    `ClassRegistry::new`.
  - Add `current_tx: RefCell<Option<OwnedStoreTransaction>>` to `Evaluator`.
  - `run_entity_loop`: before calling `dispatch_envelope`, open
    `store.begin_owned(ExecContext::Entity(entity_id))` and place it in
    `ev.current_tx`. After dispatch: on `Ok` commit; on `Err` rollback. Clear
    `ev.current_tx` in both branches.
  - `main.rs`: initialize with `Store::in_memory()` by default; pass to registry
    and evaluator setup.
  - Test setup in `eval.rs` tests: initialize a store and inject it.
  `cargo test` green.

  Note: `ClassRegistry::new()` currently spawns bootstrap class entities (Object, Value)
  inline. Pass `store: Store` to `new()` and thread through to
  `new_entity_data`/`spawn_class_entity` calls inside.

- **S4** Add the `Value ↔ Term` bridge.
  - New `store_bridge.rs` in the interpreter crate.
  - `enum BridgeResult { Assert(Term), Retract, Skip }` — distinguishes nil-retract
    from non-storable-skip.
  - `fn value_to_bridge(v: &Value) -> BridgeResult` per the bridge table above.
    For `Instance(ValueObject)`: calls `content_hash_id(v)` and returns
    `Assert(Term::EntityRef(hash_id))`. For `Future`: `Skip`. For `Block`,
    `Namespace`: `Skip` with a `#[allow(unreachable_patterns)]`-free match arm that
    documents the deferral explicitly.
  - `fn content_hash_id(v: &Value) -> EntityId` — FNV-1a over a canonical byte
    sequence: class name length + class name bytes + for each ivar in definition
    order: name bytes + recursive `content_hash_id` or primitive bytes. Sets the
    high bit. Panics (debug_assert) if called on a non-ValueObject.
  - `fn ensure_value_buffered(tx: &mut OwnedStoreTransaction, hash_id: EntityId, handle: &InstanceHandle)`
    — buffers the value object's ivar facts into `tx` under `hash_id` using
    `ExecContext::System`. Recurses for nested ValueObject ivars. Only touches
    facts, never commits — the surrounding handler transaction handles that.
  - `fn term_to_primitive(t: &Term) -> Value` in `store_bridge.rs` — handles all
    non-EntityRef terms: `Symbol(s)` → `Value::Symbol(s)`, `LargeInt(s)` → parse,
    `Timestamp` → `Value::Int(epoch_millis)`, etc. `EntityRef` is deliberately
    excluded; callers that need full resolution use `Evaluator::resolve_term`.
  - `fn resolve_term(&self, t: &Term) -> Value` on `Evaluator` — calls
    `term_to_primitive` for non-EntityRef terms. For `EntityRef(id)`:
    1. If high bit set → look up in `self.value_objects` → return
       `Value::Instance(handle)` if found.
    2. Else → look up in `self.instance_entities`, then `self.registry.class_entities`
       → return `Value::Instance(handle)` if found.
    3. Fallback → `Value::Int(id.0 as i64)` with a `eprintln!` debug warning
       (entity not found in live registry — terminated or from prior session).
  - Unit tests: all primitive round-trips; nil (Retract); Block/Namespace (Skip);
    ValueObject hash stability (same object hashes identically on two calls);
    nested ValueObject produces correct EntityRef chain; EntityRef resolves to
    Instance when the handle is registered.

- **S5** Register predicates at class definition time.
  - In `ClassRegistry`: when a class is installed (bootstrap or user-defined via
    `handle_subclass`), iterate `class.own_instance_vars` and call
    `store.declare(ivar_name, Visibility::Private, Cardinality::Single)` for each.
    Use `ExecContext::System` for all bootstrap declarations. Idempotency means
    re-declaration with identical metadata is a safe no-op.
  - Add `ivar_predicates: HashMap<Arc<str>, Predicate>` to `Class` — populated at
    definition time so `set_named_slot` assertions need no `store.lookup()` per call.
  - `cargo test` green.

  Note: inherited vars were registered when the superclass was installed; no
  duplicate declarations needed. Schema evolution (renaming/removing ivars) is a
  known hard problem deferred to a later design chunk.

- **S6** Hook ivar-set to buffer into the current transaction.
  - In `eval.rs`, at the `Stmt::Assign` site where a named ivar is written (the point
    that calls `object.set_named_slot`), buffer a store operation when the receiver is
    an `Entity` (not a `ValueObject` — those are immutable).
  - Helper `fn buffer_ivar(ev: &Evaluator, entity_id: EntityId, predicate: Predicate, value: &Value)`:
    - Borrows `ev.current_tx` via `RefCell::borrow_mut()`.
    - If `None` (called outside a handler context, e.g. from a test): no-op.
    - Calls `value_to_bridge(value)`:
      - `Assert(Term::EntityRef(hash_id))` where value is a ValueObject: first calls
        `ensure_value_buffered(tx, hash_id, instance_handle)` to buffer the value's
        own ivar facts, then asserts the `EntityRef` for the entity's ivar.
      - `Assert(other_term)`: calls `tx.assert_single(Fact { subject: entity_id, predicate, object: other_term, asserter: entity_id, valid: None })`.
      - `Retract`: calls `tx.retract_single(entity_id, predicate, entity_id)`.
      - `Skip`: no-op.
  - `cargo test` green.

  Note: `object.rs` itself is not modified — it stays a pure data structure.
  The buffer call lives in `eval.rs` alongside the in-memory slot update, keeping
  the two operations visually paired.

- **S7** Add `historyOf:` and `historyOf:from:to:` primitives in `builtins.rs`.
  - Both are instance-side primitives on `Object`, dispatched when the receiver is
    an Entity.
  - `historyOf: slotName` — queries `(self_entity_id, predicate, *)` with
    `AsOf::AllTime`. Returns a `Future` resolving to an `OrderedCollection` of
    `Association`s (key = `Value::Int(epoch_millis)`, value = `ev.resolve_term(object)`).
  - `historyOf: slotName from: t1 to: t2` — same query bounded by valid-time range.
  - Both return `Value::Future` immediately; the query executes in the entity's
    coroutine (already async-safe).
  - `cargo test` green with tests verifying: history is empty before first set; first
    set produces one entry; reassignment produces two entries; nil-assignment closes
    the second entry.

- **S8** Add `World` class entity.
  - `kernel.smol`: declare `World` as a class (subclass of Object). The `World`
    singleton class entity is bootstrapped with a reference to the shared store.
  - At entity spawn time, assert a class-membership fact:
    `(entity_id, #instanceOf, class_entity_id)` using `ExecContext::System`. This
    requires a dedicated `#instanceOf` predicate declared at startup as
    `(Visibility::Shared, Cardinality::Single)`.
  - `World class >> instancesOf: aClass` — queries all open facts with predicate
    `#instanceOf` and object matching the class entity's ID. Returns a `Future`
    resolving to a collection of entity IDs (`Value::Int`).
  - `World class >> entity: anId historyOf: aSlotName` — resolves the predicate for
    `aSlotName`, constructs `ExecContext::Entity(world_entity_id)`, queries with
    `AsOf::AllTime`, and returns a `Future`. Access is checked by the store's
    `check_read`; `Private` predicates on other entities will be denied. For C27,
    ivars are all `Private`; the access model for shared queries is a design item
    deferred to C28.
  - `cargo test` green.

- **S9** `cargo fmt`, `cargo clippy --all-targets`, full `cargo test`, commit.

---

## Files changed

| File | Change |
|------|--------|
| `crates/triple-store/src/term.rs` | Add `Term::Symbol(Arc<str>)` and `Term::LargeInt(Arc<str>)` variants |
| `crates/triple-store/src/store.rs` | Add `OwnedStoreTransaction`; `Store::begin_owned` |
| `crates/interpreter/Cargo.toml` | Add `triple-store` path dependency |
| `crates/interpreter/src/entity.rs` | Re-export `EntityId`; add `store: Store` to `EntityData`; update spawn fns |
| `crates/interpreter/src/class.rs` | Add `ivar_predicates: HashMap<Arc<str>, Predicate>` |
| `crates/interpreter/src/class_registry.rs` | `class_entities` Vec→HashMap; add `store: Store` to `new()`; register predicates at class install |
| `crates/interpreter/src/store_bridge.rs` | New: `BridgeResult`, `value_to_bridge`, `term_to_value` |
| `crates/interpreter/src/eval.rs` | `instance_entities` Vec→HashMap; add `value_objects` registry; `current_tx`; `resolve_term`; `buffer_ivar`; ivar-set hook; store init in tests |
| `crates/interpreter/src/entity.rs` | `run_entity_loop`: open/commit/rollback transaction around each dispatch |
| `crates/interpreter/src/builtins.rs` | `historyOf:` / `historyOf:from:to:` primitives; `#instanceOf` assert at spawn |
| `crates/interpreter/src/main.rs` | `Store::in_memory()` default; thread to registry and evaluator |
| `crates/interpreter/src/kernel.smol` | `World` class definition; `instancesOf:` / `entity:historyOf:` methods |
| `crates/interpreter/tests/eval.rs` | Tests for ivar assertions, history queries, World methods |

---

## Deferred

- **Class variable store assertions** — `class_var_storage` on class entities. Small
  follow-on once instance ivars are solid; ExecContext is the class-side EntityId.
- **Shared/Public ivar predicates** — all ivars are `Visibility::Private` for now.
  A `shared:` or `public:` annotation in class definitions is a future language feature.
- **Resume from durable store** — on startup with a `DurableFileTripleStore`, entity
  and value-object state could be reconstructed from persisted facts, restoring the
  live system from a prior session (Smalltalk image analogue). This requires: replaying
  entity-spawn facts to re-create actors, reconstructing value objects from their stored
  ivars, and re-seeding the entity registry. Targeted for C27.5 or C28.
- **Value object structural sharing** — content-hash identity is correct but each
  handler re-asserts value facts (idempotent). An optimisation pass (check if
  hash_id already has facts before asserting) is deferred.
- **Value object GC / reference tracking** — value facts accumulate indefinitely
  once no entity references them. Reference counting or mark-and-sweep over value
  IDs is a future concern.
- **`World` cross-entity `historyOf:` access** — currently blocked by `Private`
  visibility on all ivar predicates. Needs a visibility model for shared query access
  (C28 or a dedicated design chunk).
- **Schema evolution** — renaming or removing ivars leaves orphan predicates and facts.
  Known hard open question in the Project Summary; not addressed here.
- **Block storage** — blocks (AST + captured env) are storable in principle and
  relevant to the "code as facts" vision. Requires adding `serde::Serialize/Deserialize`
  to `Expr`/`Stmt` in `ast.rs`. Deferred to a dedicated chunk.
- **Namespace storage** — namespaces (class bindings, sub-namespaces) are storable
  and support the notebook/literate programming capability. Requires design for how
  namespace facts relate to class definition facts. Deferred.
