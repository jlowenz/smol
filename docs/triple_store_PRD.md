# Product Requirements Document: Bitemporal Triple Store

**Version:** 0.2-draft  
**Status:** Pre-implementation  
**Implementation Language:** Rust  
**Role in System:** Foundation layer — domain state, transaction rollback, and live environment instrumentation

---

## 1. Purpose and Motivation

The triple store is the first component to be built and the ground truth for all state in the
programming system. Every entity's state lives here — not in mutable Rust struct fields. The store
serves three roles simultaneously:

- **Domain state**: the facts that constitute the running world model
- **Transaction substrate**: tentative writes, commit, and rollback
- **Live environment instrumentation**: the environment observes and annotates the running system
  by asserting external facts, without modifying entity internals

This triple duty is what motivates the specific design choices below. A simpler key-value store
would serve the first role but not the other two.

### 1.1 Relationship to the OCaml EVStore

An earlier design (EVStore, OCaml) addressed related problems in a standalone persistence context.
Several lessons from that work carry forward directly:

- **Record both temporal dimensions from day one.** EVStore deferred transaction time (`asserted_at`)
  from v1. Retrofitting it later required a store migration. Here both valid time and transaction
  time are recorded from the first commit, even if full query exposure on transaction time is
  deferred.
- **Backend abstraction must be a first-class interface, not a refactoring target.** EVStore used
  OCaml functors; here Rust traits play the same role. The in-memory backend must implement the
  full interface from the start, not a simplified subset.
- **Closed Term union, open predicate namespace.** The set of primitive value types is closed
  (enforced by enum); the set of predicates is open (any string). Extensible value types are
  explicitly deferred.
- **Per-instance registries, not global singletons.** EVStore's global concept and property
  registries made isolated testing awkward. Here the store instance owns its own predicate and
  partition metadata, enabling fully isolated test instances.
- **SQL is the wrong medium-term backend for this model.** EVStore's SQLite backend had significant
  impedance with the EAV triple model: schema mapping friction, query translation overhead, and
  awkward handling of interval types. The v0 in-memory backend here uses a hashmap + index
  approach that maps cleanly to the triple structure, with the backend trait designed to accommodate
  a future embedded store (e.g., redb or sled) without SQL impedance.

### 1.2 Guiding Principles

These are inherited from the system-level design principles (P1–P8) and applied here:

- **Immutable fact log.** Facts are never overwritten. Retraction closes a valid-time or
  transaction-time interval; it never deletes a record. This is enforced by the `FactStore` trait,
  not by convention.
- **Both temporal dimensions, always.** Every fact carries valid time (world truth) and transaction
  time (when recorded). Both are first-class, not optional metadata.
- **Two logical layers.** Internal facts are owned by an entity and not externally writable.
  External assertions are open-world, carrying asserter identity and timestamp as provenance.
  Conflicts are preserved, not resolved.
- **Backend substitutability.** The `FactStore` trait is the only interface the rest of the system
  sees. Swapping backends requires no changes above that boundary.
- **Change streams are first-class.** Subscriptions are part of the core interface, not a wrapper.
  The UI synchronization model and the live environment depend on them.
- **Testability without infrastructure.** The in-memory backend enables complete deterministic
  testing of all temporal and subscription behavior with no external dependencies.

---

## 2. Core Concepts

### 2.1 Triples

The atomic unit of storage is a triple:

```
(Subject, Predicate, Object)
```

- **Subject**: an `EntityId` — a stable, opaque 64-bit identifier for an entity
- **Predicate**: an interned string naming the relationship or property (e.g., `"account.balance"`,
  `"sys.type"`, `"env.last_observed_type"`)
- **Object**: a `Term` — a typed value from a closed primitive union (see §2.4)

### 2.2 Facts

A fact is a triple with full temporal and provenance metadata:

```rust
/// A half-open interval [start, end). end = None means "currently open" (no known end).
struct Interval {
    start: Timestamp,
    end:   Option<Timestamp>,
}

impl Interval {
    fn contains(&self, t: Timestamp) -> bool {
        t >= self.start && self.end.map_or(true, |e| t < e)
    }
    fn is_open(&self) -> bool { self.end.is_none() }
    fn close(&mut self, at: Timestamp) { self.end = Some(at); }
}

struct Fact {
    subject:   EntityId,
    predicate: Predicate,
    object:    Term,
    valid:     Interval,     // when the fact is true in the world
    asserted:  Interval,     // when the system knew it
    layer:     Layer,
    tx_id:     TxId,
}
```

`valid` is the **valid-time interval**: when the fact is true in the world.  
`asserted` is the **transaction-time interval**: when the system knew the fact.

**A note on interval mutation.** When a new fact supersedes an existing one — because the
entity's state was updated, or because a retraction was recorded — the *superseded* fact's
open interval must be closed: its `valid.end` or `asserted.end` is set to the timestamp of
the superseding event. This is a targeted, controlled write to interval metadata, not a
modification of the fact's identity (subject, predicate, object, tx_id). The fact itself —
what was true, and who asserted it — is unchanged. The backend enforces this narrow contract:
only `Interval::close` is callable on a committed fact; the SPO content and tx_id are frozen
at commit time. No new fact with the same (subject, predicate, tx_id) can be inserted.

This is distinct from "overwriting" a fact, which would change what was true. Closing an
interval records *when* it stopped being true or known — preserving the full audit trail.

### 2.3 Layers

Facts belong to one of two logical layers:

```rust
enum Layer {
    Internal,                    // owned by the entity; not externally writable
    External { asserter: EntityId },  // open-world; carries asserter provenance
}
```

**Internal facts** are the entity's own state. Only the owning entity (or the system on its behalf)
can assert or retract them.

**External assertions** are facts asserted by other entities or by the live environment about a
subject entity. They are stored in a logically separate partition, carry full provenance
(`asserter` + `asserted_at`), and do not overwrite or conflict-resolve with internal facts or with
each other. Conflicting external assertions are both retained and distinguished by asserter identity
at query time.

This design enables the live environment to record observations about running code (e.g., inferred
type information, runtime behavior annotations) without modifying entity internals.

### 2.4 Terms (the Object union)

```rust
enum Term {
    Int(i64),
    Float(f64),
    Bool(bool),
    Symbol(InternedString),
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
    Timestamp(Timestamp),
    EntityRef(EntityId),         // a reference to another entity
}
```

The set of term variants is **closed** in v0. Composite or user-defined term types are
explicitly out of scope. `EntityRef` is the mechanism for inter-entity relationships, and is
what enables cross-entity queries.

**Extensibility path for v1+.** Adding composite or user-defined terms is genuinely tricky.
A type parameter (`Term<T>`) is impractical: it propagates through `Fact`, `Pattern`,
`FactStore`, and every API surface, and precludes heterogeneous collections. The more
workable approach is a type-erased variant backed by a codec registry:

```rust
// v1+ sketch — not part of v0
trait OpaqueValue: Any + Debug + Send + Sync {
    fn type_tag(&self) -> TypeTag;          // interned string: "money.Money", "geo.Point"
    fn eq_dyn(&self, other: &dyn OpaqueValue) -> bool;
    fn hash_dyn(&self, state: &mut dyn Hasher);
    fn encode(&self) -> Bytes;             // for persistence / wire
}

enum Term {
    // ... all v0 variants ...
    Opaque(Arc<dyn OpaqueValue>),          // v1+: composite and user-defined values
}
```

The `type_tag` allows the store to handle values whose codec is not yet loaded (store as raw
`Bytes`, deserialize lazily when the codec is registered). Index equality and hashing delegate
to `eq_dyn`/`hash_dyn`, so the PO index still works for registered types. The open question
is what `Match<Term>` means for `Opaque` values — range queries and structural matching would
require codec-level support and are likely v2+. For v1, equality matching on `Opaque` is
sufficient. This area needs a dedicated design session before the query layer is finalized.

### 2.5 Predicates

A predicate is an interned string with optional declared metadata:

```rust
struct PredicateMeta {
    id:         Predicate,           // interned string handle
    visibility: Visibility,          // Private | Shared | Public
    // extensible: additional metadata added here without breaking the interface
}

enum Visibility {
    Private,   // only the owning entity can read or write; invisible to all others
    Shared,    // owning entity writes; others can read
    Public,    // open-world: any entity may externally assert
}
```

Predicates need not be pre-declared. An undeclared predicate defaults to `Visibility::Private`,
consistent with a behavior-first, information-hiding approach: entity internals are not
externally visible unless explicitly opened. Explicit declaration is required to grant `Shared`
or `Public` visibility. The predicate registry is per-store-instance (not global), enabling
isolated test instances.

### 2.6 Queries and Patterns

A query is a pattern match over triple fields, with optional temporal constraints:

```rust
struct Pattern {
    subject:   Match<EntityId>,
    predicate: Match<Predicate>,
    object:    Match<Term>,
    layer:     Option<Layer>,    // None = both layers
}

/// v0: simple exact-match or wildcard.
/// v1+: combinators (And, Or, Not, Range) for richer query expressions.
enum Match<T> {
    Any,
    Is(T),
}

struct AsOf {
    valid_time: Option<Timestamp>,    // None = now
    tx_time:    Option<Timestamp>,    // None = latest
}
```

All six permutations of bound/unbound SPO slots are valid. The backend is responsible for choosing
the most efficient index for each pattern shape; the query interface is uniform regardless.

### 2.7 Transactions

All mutations go through a transaction:

```rust
struct TxId(u64);  // monotonically increasing, per-store

// Obtained from the store:
fn begin_tx(&mut self) -> TxId;
fn commit(&mut self, tx: TxId) -> Result<CommitResult, TxError>;
fn rollback(&mut self, tx: TxId);
```

Within a transaction, asserted facts are **tentative**: visible to the transaction that wrote them
(for read-your-writes), but not to any other reader until commit. On rollback, tentative facts are
discarded without trace. On commit, facts are promoted to permanent, `asserted_at` is finalized, and
change events are delivered to matching subscribers.

Nested transactions are **not** supported in v0. This is a known gap; composition is deferred.

---

## 3. Architecture

### 3.1 Layers

```
┌────────────────────────────────────────────────────────────┐
│                   Interpreter / Runtime                     │
│        entity message handlers read/write via Store API     │
├────────────────────────────────────────────────────────────┤
│                      Store API                              │
│   assert · retract · query · begin_tx · commit · rollback   │
│   subscribe · unsubscribe · history · as_of                 │
├────────────────────────────────────────────────────────────┤
│                   FactStore trait                           │
│            (the only interface backends see)               │
├──────────────────────────┬─────────────────────────────────┤
│   InMemoryStore (v0)     │   EmbeddedStore (future)        │
│   HashMap + indexes      │   redb / sled / custom          │
└──────────────────────────┴─────────────────────────────────┘
```

The **Store API** is the consumer-facing layer. It enforces access control, manages the predicate
registry, coordinates transactions, and dispatches change events. It holds an `Arc<dyn FactStore>`,
allowing it to be cloned cheaply and distributed to entity tasks without copying the backend.

The **FactStore trait** is the storage backend boundary. It sees raw facts and knows nothing about
access control, subscriptions, or the predicate registry. All of that logic lives above the trait.
Implementations use interior mutability to support concurrent access.

### 3.2 The `FactStore` Trait

```rust
/// The storage backend interface. Implementations use interior mutability
/// (e.g., sharded locks, DashMap) to support concurrent access from many
/// entity processes without requiring external serialization.
///
/// All methods take &self (not &mut self) to allow the store to be wrapped
/// in Arc and shared across entity tasks without a top-level lock.
trait FactStore: Send + Sync {
    // Mutations (always within a transaction)
    fn assert_fact(&self, fact: &PendingFact, tx: TxId) -> Result<()>;
    fn retract_fact(&self, subject: EntityId, predicate: Predicate,
                    valid_at: Timestamp, tx: TxId) -> Result<()>;

    // Commit / rollback
    fn commit_tx(&self, tx: TxId) -> Result<Vec<CommittedChange>>;  // returns changes (for event dispatch)
    fn rollback_tx(&self, tx: TxId);

    // Queries (read committed facts only, unless in same tx)
    fn query(&self, pattern: &Pattern, as_of: &AsOf, tx: Option<TxId>) -> Result<Vec<Fact>>;
    fn history(&self, subject: EntityId, predicate: Predicate) -> Result<Vec<Fact>>;

    // Introspection
    fn entity_ids(&self) -> Vec<EntityId>;
    fn tx_log(&self) -> Vec<TxId>;  // ordered; for replay and audit
}
```

**Why `&self` with interior mutability rather than `&mut self`?** A `&mut self` trait forces
the store to be held behind a single `Mutex` at the call site, serializing all entity accesses
through one lock. The runtime will have many entity processes (potentially thousands) all
accessing the same store concurrently; a single outer lock is a bottleneck by design. Using
`&self` moves the locking responsibility inside the implementation, where it can be
fine-grained (sharded by entity, by predicate bucket, or by transaction). The in-memory v0
implementation will use a straightforward `RwLock<HashMap<...>>` per shard; future backends
can use lock-free structures (e.g., `DashMap`) or MVCC. The `Store` API wrapper holds
`Arc<dyn FactStore>` rather than `Box<dyn FactStore>`, allowing it to be cloned cheaply and
passed to entity tasks.

`CommittedChange` is a lightweight descriptor (subject, predicate, old/new object, kind)
returned by `commit_tx` and used by the `Store` layer to dispatch change events. Returning
the changed set rather than requiring a post-commit query avoids re-scanning committed state.

### 3.3 In-Memory Backend (v0)

Primary storage: a sharded structure of `RwLock<HashMap<(EntityId, Predicate), Vec<Fact>>>` —
the SP buckets. Sharding by `EntityId % N` (with N configurable, default 16) keeps lock
contention proportional to the number of entities accessing the same shard, not the total
number of concurrent entities.

Auxiliary indexes, built and maintained incrementally (each also sharded or behind their own lock):

- **PO index**: `HashMap<(Predicate, TermKey), Vec<EntityId>>` — for "who has predicate P with
  value V" queries
- **S index**: `HashMap<EntityId, Vec<Predicate>>` — for "all predicates of entity E"

`TermKey` is a hashable, equality-comparable wrapper over `Term` (noting that `f64` requires care:
NaN is excluded from equality; floats are compared by bit pattern for index purposes only).

Tentative (uncommitted) facts are held in a per-transaction `HashMap<TxId, Vec<PendingFact>>`
behind its own lock. On commit they are moved to primary storage and indexes updated atomically
per SP bucket (acquire bucket write lock, update, release). On rollback the `TxId` entry is
dropped.

The v0 implementation prioritizes correctness and interface fidelity over raw performance.
Sharding provides enough concurrency for early interpreter use; a lock-free backend (e.g.,
DashMap-based or purpose-built MVCC) is a v1+ concern.

### 3.4 Change Streams

Subscriptions are managed by the Store API layer (not the backend):

```rust
struct Subscription {
    id:      SubscriptionId,
    pattern: Pattern,
    sender:  mpsc::UnboundedSender<ChangeEvent>,
}

struct ChangeEvent {
    subject:   EntityId,
    predicate: Predicate,
    kind:      ChangeKind,
    tx_id:     TxId,
}

enum ChangeKind {
    /// A new fact was asserted with no prior open fact for this (subject, predicate).
    Asserted { new_object: Term },
    /// The object value changed: the prior fact's valid interval was closed and a new
    /// fact opened. Neither the old nor new fact is "retracted" in the audit sense —
    /// both remain in history. The old fact's valid.end has been set.
    Updated { old_object: Term, new_object: Term },
    /// The prior open fact's valid interval was explicitly closed with no replacement.
    Retracted { old_object: Term },
}
```

On every `commit`, the Store API iterates the committed fact set and delivers matching events to
all registered subscribers. Delivery is **asynchronous**: the store sends to each subscriber's
channel and does not block waiting for the receiver to consume. The channel is unbounded in v0;
backpressure and debounce are deferred. Subscribers that fall behind accumulate events in their
channel buffer — the store is never stalled by a slow subscriber.

Subscriptions match against both layers. A subscriber can filter to a specific layer via the
`Pattern.layer` field.

### 3.5 Access Control

Enforced by the Store API layer before reaching the backend:

- An `assert` of an **Internal** fact for entity E is only accepted from entity E's context (the
  current execution context carries an `EntityId`). Violation returns `Err(AccessDenied)`.
- An `assert` of an **External** fact requires the predicate's visibility to be `Public`.
  `Private` and `Shared` predicates reject external assertions.
- A `query` for facts about entity E obeys predicate visibility:
  - `Private`: only E's own context can read. All other contexts receive no results for this
    predicate, as if it does not exist.
  - `Shared`: E's context can read and write; any other entity's context can read (but not
    externally assert). This is the standard "public state, private mutation" visibility.
  - `Public`: any entity can read, and any entity may externally assert into the External layer.

In the interpreter (v0), "current execution context" is the entity whose message handler is
currently running — tracked on the call stack. Before the interpreter exists, access control can
be bypassed via a privileged `SystemContext` (used in store-level tests only).

---

## 4. Store API Surface

```rust
// Store construction
impl Store {
    pub fn new(backend: Arc<dyn FactStore>) -> Self;
    pub fn in_memory() -> Self;  // convenience: wraps InMemoryStore in Arc
    pub fn clone_handle(&self) -> Self;  // cheap clone; shares the same backend
}

// Predicate registry
impl Store {
    pub fn declare_predicate(&mut self, name: &str, visibility: Visibility) -> Predicate;
    pub fn predicate(&self, name: &str) -> Option<Predicate>;  // lookup without declaring
}

// Transaction management
impl Store {
    pub fn begin(&mut self) -> TxId;
    pub fn commit(&mut self, tx: TxId) -> Result<CommitResult, TxError>;
    pub fn rollback(&mut self, tx: TxId);
}

// Mutations (require an active TxId)
impl Store {
    pub fn assert(&mut self, tx: TxId, ctx: &ExecContext,
                  subject: EntityId, predicate: Predicate,
                  object: Term, valid_from: Timestamp) -> Result<()>;

    pub fn retract(&mut self, tx: TxId, ctx: &ExecContext,
                   subject: EntityId, predicate: Predicate,
                   valid_at: Timestamp) -> Result<()>;

    pub fn assert_external(&mut self, tx: TxId, ctx: &ExecContext,
                           subject: EntityId, predicate: Predicate,
                           object: Term, valid_from: Timestamp) -> Result<()>;
}

// Queries (no TxId required; reads committed state + own tentative)
impl Store {
    pub fn query(&self, ctx: &ExecContext, pattern: &Pattern,
                 as_of: &AsOf) -> Result<Vec<Fact>>;

    pub fn current(&self, ctx: &ExecContext,
                   subject: EntityId, predicate: Predicate) -> Result<Option<Term>>;

    pub fn history(&self, ctx: &ExecContext,
                   subject: EntityId, predicate: Predicate) -> Result<Vec<Fact>>;

    pub fn as_of(&self, ctx: &ExecContext, subject: EntityId,
                 predicate: Predicate, valid_at: Timestamp) -> Result<Option<Term>>;

    pub fn entities_with(&self, ctx: &ExecContext,
                         predicate: Predicate, object: &Match<Term>) -> Result<Vec<EntityId>>;
}

// Change streams
impl Store {
    pub fn subscribe(&mut self, pattern: Pattern,
                     sender: mpsc::UnboundedSender<ChangeEvent>) -> SubscriptionId;
    pub fn unsubscribe(&mut self, id: SubscriptionId);
}
```

`ExecContext` carries the identity of the calling entity (or `SystemContext` for privileged
internal operations). It is threaded through all mutation and query calls to enforce access control.
It is a lightweight value type, not a capability object — capability semantics are a future concern.

---

## 5. Functional Requirements

### 5.1 Core (P0 — v0)

| Requirement | Notes |
|---|---|
| Assert internal facts within a transaction | Access-controlled to owning entity |
| Retract internal facts | Closes valid-time interval; does not delete |
| Assert external facts with provenance | Requires `Public` predicate visibility |
| Commit transaction | Promotes tentative facts; dispatches change events |
| Rollback transaction | Discards tentative facts without trace |
| Query by SPO pattern, any combination bound/unbound | Returns all matching committed facts |
| Query current value of a predicate for an entity | `current()` shorthand |
| Query history of a predicate for an entity | Full fact log, ordered by valid_from |
| Point-in-time query (`as_of`) | On valid time; tx time recorded but not queried in v0 |
| In-memory backend | HashMap + auxiliary indexes |
| Predicate registry (per-store-instance) | Declaration with default visibility |
| Change stream subscriptions | Pattern-matched, delivered on commit |
| Access control enforcement | Private / Shared / Public per predicate |
| Both temporal dimensions recorded | `asserted_at` recorded at commit even if not queried |

### 5.2 Extended Query (P1 — v0 or early v1)

| Requirement | Notes |
|---|---|
| Cross-entity queries by predicate + object value | `entities_with(predicate, Match)` |
| Query combining internal + external layer | Layer filter in Pattern |
| Provenance-filtered query | "All external assertions about E made by asserter A" |
| Transaction time query (`as_of` on tx time) | Full bitemporal query |

### 5.3 Future (P2 — v1+)

| Requirement | Notes |
|---|---|
| Embedded persistent backend | redb or sled; same `FactStore` trait |
| Subscription debounce / rate limiting | Per-subscription configurable policy |
| Nested transaction composition | Not designed yet; deferred |
| Store compaction / archival | Age-out of superseded facts |
| Schema / predicate evolution tooling | Renaming, merging predicates across history |

---

## 6. Non-Functional Requirements

**Correctness of temporal semantics.** The `FactStore` trait must make arbitrary fact mutation
impossible. Only interval-closing (`valid.end` or `asserted.end`) is permitted on committed facts;
SPO content and `tx_id` are frozen at commit. This is enforced by the type system: committed facts
are returned as owned `Fact` values; the backend holds its own copies and exposes no `&mut Fact`
references. The only mutation path for intervals is through the backend's internal commit logic.

**Concurrent access without a global lock.** The `FactStore` trait uses `&self` throughout,
requiring implementations to use interior mutability. The in-memory backend must not serialize all
entity accesses through a single `Mutex`. Sharded locking (by entity or predicate bucket) is the
minimum acceptable approach for v0. This is validated by a concurrency test: N entity tasks
performing concurrent reads and writes must complete without deadlock, livelock, or data races
(verified by running tests under `cargo test -- --test-threads=N` and under `loom` if feasible).

**No `unsafe` in the store layer.** The store is safe Rust throughout. `unsafe` is reserved for the
memory subsystem of the future VM. Strategic use of `Arc` and `RwLock` is acceptable and expected.

**Full testability with in-memory backend.** All temporal query semantics, transaction behavior,
access control, and subscription delivery must be testable without an embedded database or
external process. The in-memory backend must implement the full `FactStore` trait, not a stub.

**Per-instance isolation.** Two `Store` instances that do not share a backend must share no mutable
state. The predicate registry, subscription list, and transaction log are per-`Store`. This is
required for parallel test execution. (Two `Store::clone_handle()` instances *do* share a backend
by design — that is the multi-entity concurrency model.)

**Deterministic test behavior.** Timestamps are injectable (the store accepts a clock function or
a `Timestamp` directly from the caller in tests) so that test scenarios are reproducible without
wall-clock dependency.

**Backend swap requires no API changes above the trait.** Any code that uses `Store` (not
`InMemoryStore` directly) must continue to compile and behave correctly when the backend is
replaced. This is verified by running the full test suite against both the in-memory backend and
any future backend.

---

## 7. Key Design Decisions

**Why triples (SPO) rather than EAV with a typed property registry?**  
The SPO model is uniform: domain facts, system metadata, and environment observations all fit
the same structure. A typed property registry (as in EVStore) adds ceremony and a type-resolution
step that is valuable in a standalone persistence library but adds friction in a runtime context
where the language's own type system is the primary type-safety mechanism. Predicates here are
interned strings with visibility metadata; type safety of `Term` values is enforced at the
language level, not the store level.

**Why two layers (Internal / External) rather than a single namespace?**  
The live environment must be able to annotate running entities without modifying their internal
state. Without a layer distinction, there is no way to query "what does entity E believe about
itself" vs. "what has the environment observed about E." Provenance on external assertions enables
conflict preservation (multiple asserters can hold conflicting beliefs; the query layer surfaces
them all).

**Why not SQL for the medium-term backend?**  
The OCaml EVStore used SQLite. The impedance between a triple model with interval types and SQL
schemas was a persistent source of friction: storing `valid_from`/`valid_to` as nullable
timestamps required NULL sentinel handling in every query; pattern-matching over arbitrary SPO
combinations required query generation rather than straightforward index lookup; and the
transaction-time dimension added a second pair of nullable columns. A native embedded store (redb,
sled) with a binary key encoding maps more cleanly to the triple structure and avoids the SQL
translation layer entirely. For v0, the in-memory hashmap approach is cleaner still.

**Why record `asserted_at` in v0 even though tx-time queries are deferred?**  
Retrofitting transaction time later requires either a store migration or accepting that historical
data has incomplete temporal metadata. Recording it silently from day one has zero query-layer
cost and zero API surface cost: it is just another field on `Fact`. Exposing tx-time queries
in v1 is then purely additive.

**Why per-instance predicate registry rather than a global?**  
EVStore's global registries made isolated parallel test execution impossible without process
forking. A per-instance registry makes `Store::new()` a complete, isolated unit. The mild cost —
predicates must be declared on the specific store instance — is acceptable given that the runtime
will have at most one production store per process.

**Why is the `FactStore` trait split from the `Store` API?**  
Access control, subscription dispatch, the predicate registry, and transaction bookkeeping are
all cross-cutting concerns that belong above the storage layer. Putting them in the trait would
require every backend to re-implement them. The trait is narrow: assert, retract, query, commit,
rollback. Everything else is in `Store`, which is not replaceable but is not meant to be.

**Why `&self` (interior mutability) rather than `&mut self` in `FactStore`?**  
The runtime will have many entity processes — potentially thousands — all accessing the same store
concurrently. `&mut self` forces a single outer `Mutex` at every call site, serializing all entity
accesses through one lock: a bottleneck by design. Using `&self` with interior mutability moves
locking inside the implementation, where it can be fine-grained (sharded by entity bucket, by
predicate, or by transaction). This also allows `Store` to be cloned cheaply as
`Arc<dyn FactStore>` and handed to each entity task, rather than requiring the runtime to route
all store access through a single synchronization point. The cost is that the implementation must
reason carefully about lock ordering to avoid deadlocks — a known complexity that is accepted
in exchange for the concurrency headroom.

---

## 8. Open Questions

1. **`ExecContext` threading.** The current design threads `ExecContext` through every mutation and
   query call. An alternative is a thread-local or async-task-local context set when entering an
   entity's message handler. The explicit parameter is more transparent but adds noise to call
   sites. Decision deferred until the interpreter's call stack model is clearer.

2. **`Term` equality for floats.** The index uses bit-pattern equality for `f64`. This is
   consistent and predictable but means `0.0 != -0.0` and `NaN != NaN` in index lookups. Whether
   this is the right semantic for domain queries (vs. index internals) needs explicit decision.

3. **Subscription delivery backpressure.** Delivery is non-blocking (fire-and-forget into an
   unbounded channel), so the store is never stalled by a slow subscriber. The risk is unbounded
   memory growth for a lagging subscriber. Debounce, bounded channels with drop policy, or
   explicit flow control are all options; the interface does not preclude any of them. Deferred
   to when real subscriber patterns are understood.

4. **Predicate declaration: required or optional?**  
   Currently, undeclared predicates default to `Visibility::Private`. An alternative is to require
   explicit declaration, catching typos at declaration time. Given that `Private` is now the
   default, an undeclared predicate is at least not accidentally exposed, so the ceremony tradeoff
   is less acute. A lint/warning mode for undeclared predicates used in cross-entity queries may
   be the right middle ground.

5. **Tentative fact visibility within a transaction.** The current spec says a transaction can
   read its own tentative facts ("read-your-writes"). The implementation cost in the sharded
   backend requires a secondary scan of the pending buffer per query. Whether this semantic is
   worth the complexity should be validated against early interpreter use cases.

6. **Entity ID generation.** `EntityId` is a `u64`. The store currently does not generate entity
   IDs — that is the runtime's responsibility. This is correct for the long term but means the
   store's own tests need a test ID generator. A simple monotonic counter in the test harness is
   sufficient; no design decision needed, just noting the boundary.

7. **`Match<T>` combinators.** The v0 `Match<T>` supports only `Any` and `Is(T)`. Real query
   patterns will want range queries (for timestamps and numeric values), negation (`IsNot`), and
   set membership (`IsOneOf`). These can be added in v1 without changing the `FactStore` trait
   interface, but the index structures should be designed with range queries in mind (e.g., a
   `BTreeMap` secondary index on numeric predicates) rather than hash-only.

8. **Shard count and strategy for in-memory backend.** A fixed shard count of 16 (by `EntityId
   % 16`) is a placeholder. The right count depends on the expected number of concurrent entity
   tasks and the read/write ratio. An alternative is sharding by predicate (better for
   cross-entity predicate scans) rather than by entity. This is an implementation detail that
   does not affect the `FactStore` interface, but it affects which query patterns are fast.

9. **`Opaque` term support: v1 scope.** The `OpaqueValue` trait sketch (§2.4) needs a concrete
   design before the query layer is finalized. Key decisions: how codecs are registered (global
   registry vs. per-store), how unrecognized type tags are handled (error vs. opaque bytes), and
   whether `Match::Is(Term::Opaque(...))` equality is supported in v1 or deferred.

---

## 9. Out of Scope (v0)

- Persistent / durable backend (hashmap is in-memory only; persistence deferred to embedded store)
- Nested or composable transactions
- Subscription debounce, rate limiting, or backpressure
- Transaction-time queries (recorded, not queried)
- Distributed or multi-writer scenarios
- Store compaction or archival
- Schema/predicate migration tooling
- Full capability-based access control (ExecContext is identity, not a capability object)
- Query optimization beyond index selection by pattern shape

---

## 10. Implementation Phases

| Phase | Focus | Key Deliverables |
|---|---|---|
| 0 — Core Types | Data model | `EntityId`, `Predicate`, `Term`, `Fact`, `PendingFact`, `Layer`, `Pattern`, `AsOf`, `TxId`; intern table for strings |
| 1 — In-Memory Backend | `FactStore` impl | `InMemoryStore`: SP hashmap, PO and S indexes; `assert_fact`, `retract_fact`, `commit_tx`, `rollback_tx`, `query` |
| 2 — Store API | Consumer facade | `Store`: predicate registry, access control, tx management, `ExecContext`; full mutation and query API |
| 3 — Change Streams | Subscriptions | Pattern-matched subscription registration; commit-time event dispatch; `ChangeEvent` type |
| 4 — Extended Queries | Cross-entity | `entities_with`, provenance-filtered query, layer-filtered query |
| 5 — Tx Time Queries | Full bitemporality | Expose `as_of` on transaction time; validate against recorded `asserted_at`/`retracted_at` |
| 6 — Persistent Backend | Durability | `FactStore` impl over redb or sled; key encoding for SPO + temporal fields; test suite parity |
