# Chunk 21: Async Actor Model MVP

## Status

Chunk 20 delivers `EvalError::Signal`, `_primHash`, `Error`, `Association`, and
`Dictionary`. The interpreter has arrays, collections, blocks, non-local returns,
and a sufficient exception primitive. The triple store crate is complete and idle.

This chunk implements the first proof of the system's core design thesis:
**all objects are actors; all message sends are conceptually asynchronous** (D2,
D4). It is intentionally a minimum viable slice: enough to prove that the async-
everywhere model works as a Rust runtime, enough to catch design errors before
more infrastructure is layered on top, and narrow enough to complete in one chunk.

**Delivers:**

1. `Value subclass:` — new language-level declaration for passive data objects
   (opt-out from actor semantics); existing kernel types migrated to it.
2. `Object subclass:` now creates actor entities by default.
3. `Value::Entity(EntityHandle)` — entity references as first-class values.
4. `Value::Future(FutureHandle)` — lazy future references; forwarded without
   resolution; resolved only when a message is dispatched to the value.
5. `EntityState` — per-entity isolated instance variables, a mailbox, and an
   OS thread running the entity's message loop.
6. `Envelope` — a queued message: selector, arguments, reply sender.
7. `FutureState` — shared state between the entity that produces a result and
   any entity waiting to consume it (`Mutex<Option<Value>> + Condvar`).
8. `Object class >> new` — on actor classes creates an `EntityState`, spawns
   its OS thread, queues `initialize`, and returns `Value::Entity`.
9. Async dispatch — sending any message to a `Value::Entity` enqueues an
   `Envelope` and returns `Value::Future` immediately.
10. Future resolution — dispatching a message to a `Value::Future` receiver
    blocks the calling entity's OS thread (condvar wait) until the future
    resolves, then dispatches to the resolved value.
11. Sync self-send — messages sent to `self` within an entity's own handler
    dispatch synchronously (no envelope, no future, no deadlock).
12. Error isolation — an unhandled error in an entity's message handler
    terminates that entity; all other entities continue running.
13. Multi-threaded execution — each entity runs on its own OS thread; true
    multi-core parallelism from the first chunk.

**Explicitly deferred:**

- Promise pipelining (forwarding messages to unresolved futures without
  blocking) — the current model resolves at point of dispatch; message
  forwarding/buffering to an unresolved future is a future chunk.
- Green thread pool (N threads for M entities) — current model is one OS
  thread per entity; documented as the upgrade path to a work-stealing pool.
- Sync optimization via escape analysis — D2 notes synchronous execution is
  an optimization; the runtime does not yet perform it.
- Triple store as entity state — chunk 22; entity state is instance variables here.
- Per-entity GC — chunk 23+; `Arc` reference counting is used now.
- Named entity registry — chunk 22.
- `Float64Array`, `species`, `ReadStream`, `WriteStream` — chunk 22.
- Full `Exception` hierarchy, `ensure:`, `retry` — chunk 22.
- Preemptive scheduling, time-sliced execution — VM work.
- Blocks across entity boundaries — blocked with error for now (same as
  before; correct model deferred).

---

## Design Decisions

### Actor-by-Default (D2, D3)

The fundamental realization of D2 ("asynchronous by default") is that
`Object subclass:` creates an actor entity, not a passive object. There is no
`Entity subclass:` syntax — the distinction comes from the base class:

| Base class | Kind | Message send result | Thread |
|---|---|---|---|
| `Object subclass:` | Actor entity | `Value::Future` | One OS thread per instance |
| `Value subclass:` | Passive value | `Value` (direct) | None; dispatches on caller's thread |

This inverts the current opt-in model. Passive value semantics require an
explicit declaration via `Value subclass:`. Actors are the default because D2
states that sequential execution is an optimization of concurrent semantics,
not the other way around. An opt-in model would result in most code defaulting
to the familiar Smalltalk object model, making actors a specialty feature rather
than the foundation.

The correct cut for kernel types is **mutability under identity**, not
collection vs. non-collection:

- **Value (immutable or identity-free)**: `Array` (fixed-size indexed storage),
  `ByteArray`, `Association`, `Error`, `Boolean`/`True`/`False`, `String`.
  These migrate to `Value subclass:` in this chunk; existing test behavior is
  fully preserved since value dispatch is identical to today's synchronous
  dispatch.

- **Entity (mutable, identity-bearing)**: `OrderedCollection`, `Dictionary`.
  These remain `Object subclass:` and become actor entities: an
  `OrderedCollection` is "the same collection" as elements are added and
  removed over time, which is exactly the definition of persistent identity.
  Existing tests that use these types will require the entity thread machinery
  running; their behavioral contracts (correct sizes, correct contents) are
  preserved — only the execution mechanism changes.

**Behavioral transparency**: from the programmer's perspective, code written
for a value class and code written for an actor class look identical — both use
standard message-send syntax. The difference is in what the system does under
the hood: synchronous dispatch vs. enqueue + future. This is the expression
of P7 (minimal ceremony) and P4 (concurrency is structural).

### Value::Future — Lazy, Forwarded by Reference

`Value::Future(FutureHandle)` is returned immediately on any actor message send.
It is not resolved eagerly. The rule is:

> **Resolution is required only at the point where a message is dispatched to
> the future's value.** All other operations — assignment, passing as argument
> into another envelope — propagate the future reference by `Arc::clone`.

Concretely:

```smalltalk
r1 := entityA compute: 42.          "r1 = Value::Future; no blocking"
entityB transform: r1.              "r1 forwarded as-is into envelope; no blocking"
result := r1 + 1.                   "r1 must resolve before '+' dispatches; blocks here"
```

This enables natural fan-out: multiple async sends can be fired before any
suspension occurs. Resolution happens only when the caller needs to inspect
or use the value.

`FutureHandle` is `Arc<FutureState>`:

```rust
pub struct FutureState {
    pub inner: Mutex<FutureInner>,
    pub condvar: Condvar,
}

pub struct FutureInner {
    pub value: Option<Value>,
}
```

`FutureHandle::wait()` acquires the lock and condvar-waits until `value` is
`Some`. The producing entity calls `FutureHandle::resolve(value)` at the end
of its message handler, which sets `value` and calls `condvar.notify_all()`.

**Forwarding across entity boundaries**: when `Value::Future` appears in the
arguments of an `Envelope`, it is `Arc::clone`d — the receiving entity gets a
live reference to the same `FutureState`. If the receiving entity eventually
dispatches a message to that argument, it will block at that point if not yet
resolved. This is the first step toward promise pipelining; full message
forwarding/buffering is deferred.

**Note — precluding nothing**: the eventual model (D2, "synchronous is an
optimization") may include futures that buffer messages sent to them before
resolution, dispatching to the resolved value upon resolution. The current
`FutureState` design does not support this yet, but the `Arc`-shared structure
does not preclude it. The upgrade path is to add a `Vec<Envelope>` buffer to
`FutureState` and drain it on resolve.

### Multi-Threaded Execution from the Start (P4, D10)

Each entity instance spawns one OS thread. The thread runs the entity's message
loop: pop envelope, dispatch synchronously on the entity's thread, resolve the
reply future with the return value, repeat. The thread parks on a condvar when
the mailbox is empty.

This gives true multi-core parallelism immediately. Two entities process
messages concurrently on separate cores with no additional configuration.

**Why not a cooperative single-threaded scheduler**: a cooperative scheduler
would prevent entity A from progressing while entity B runs. With implicit
futures and blocking resolution (`FutureHandle::wait()`), entity A's thread
can block waiting for entity B's result while entity B's thread makes progress.
A single-threaded cooperative scheduler cannot express this without coroutine
infrastructure (explicit continuations or green threads).

**Why not a green-thread pool immediately**: for the MVP with small entity
counts (tens in tests), one OS thread per entity is correct, simple to
implement, and does not preclude the upgrade. The documented upgrade path is:

1. Replace OS threads with green threads (stackful coroutines via a library
   such as `may`). No changes to the evaluator required — green threads have
   their own Rust call stack, so `eval_expr` stays synchronous.
2. Introduce a work-stealing thread pool (N OS threads for M green-thread
   entities), equivalent to the BEAM scheduler model.
3. The `FutureHandle::wait()` call becomes a green-thread park rather than an
   OS-thread block; the evaluator code is unchanged.

The Rust evaluator (`eval_expr`) stays synchronous throughout this evolution.
When the custom VM arrives, per-entity green-thread stacks are replaced by
explicit VM stack data structures; the thread-pool scheduler is unchanged.

### Sync Self-Send

Messages sent to `self` within an entity's own message handler must dispatch
synchronously. Queuing a self-send would require the entity's own thread to
wait on a future while also being the thread that would process it —
deadlock. A thread-local `CURRENT_ENTITY: Option<EntityId>` is set before
dispatching an envelope. If the target entity's ID matches `CURRENT_ENTITY`,
dispatch falls through to direct synchronous invocation (same path as value
dispatch). No programmer annotation required.

### Entity State

```rust
pub struct EntityState {
    pub id: EntityId,                       // monotonic u64, assigned at spawn
    pub class: Arc<ClassDef>,
    pub ivars: Mutex<Vec<Value>>,           // locked during handler dispatch only
    pub mailbox: Mutex<VecDeque<Envelope>>,
    pub mailbox_condvar: Condvar,           // entity thread parks here when empty
    pub status: Mutex<EntityStatus>,
}

pub enum EntityStatus {
    Running,
    Faulted(String),
    Terminated,
}
```

`ivars` and `mailbox` use separate locks. The ivar lock is held only during
dispatch of a single envelope; it is never held while waiting on a `FutureHandle`.

### Envelope

```rust
pub struct Envelope {
    pub selector: Arc<str>,
    pub args: Vec<Value>,
    pub reply: FutureHandle,  // always present; entity resolves it at handler end
}
```

Every send allocates a `FutureHandle` pair. The sender holds the `FutureHandle`
for the caller; the entity holds the same `Arc` to resolve it. Fire-and-forget
semantics emerge naturally when the caller stores the future in a variable and
never dispatches a message to it — the future is allocated but `wait()` is
never called.

### Cross-Entity Value Safety

Arguments in an `Envelope` follow these rules at send time:

| Value kind | Action |
|---|---|
| `Int`, `Float`, `Bool`, `Nil`, `Symbol`, `LargeInt` | `Clone` (copy) |
| `Str(Arc<str>)` | `Arc::clone` (immutable, safe to share) |
| `Object(Arc<ObjectData>)` where `is_actor: false` | Deep-clone (new `ObjectData` with recursively cloned ivars) |
| `Array`, `ByteArray`, `OrderedCollection` (value class instances) | Deep-clone |
| `Entity(EntityHandle)` | `Arc::clone` (entity references are intentionally shared) |
| `Future(FutureHandle)` | `Arc::clone` (forwarded by reference; no resolution) |
| `Block` | Error: `'blocks may not cross entity boundaries'` |

### Error Isolation

Inside the entity's message loop, any error that is not `EvalError::Return`
is caught at the loop level. The entity is marked `Faulted`, the error is
logged, and the loop exits. All other entities continue unaffected. A
`Faulted` entity's reply future is resolved with a sentinel error value
(rather than left unresolved forever, which would deadlock any waiter).
Sending a message to a `Faulted` entity raises
`Error signal: 'entity has faulted'`.

---

## Class Hierarchy Change

`Value` is a new built-in class registered at interpreter startup as a
subclass of `Object` with `is_actor: false`. Its instances are `Value::Object`
in Rust (same as today's non-actor instances); there is no new Rust variant.

```
Object  (is_actor: true)
  Value  (is_actor: false)           ← immutable / identity-free data
    Array
    ByteArray
    Association
    Error
    Boolean
      True
      False
  OrderedCollection  (is_actor: true)  ← mutable, identity-bearing
  Dictionary         (is_actor: true)  ← mutable, identity-bearing
  Counter  (user-defined; is_actor: true — inherits from Object)
  ...
```

The `is_actor` flag is inherited: subclasses of `Object` are actors unless
they or an ancestor explicitly set `is_actor: false` by subclassing `Value`
(or `Value subclass:` in Smol). Subclassing `Value` within `Value` inherits
`is_actor: false`.

Mixed-hierarchy subclassing (an actor class subclassing a value class or vice
versa) is an open question for the MVP. For chunk 21: detected at class-
definition time and raises `Error signal: 'cannot mix actor and value inheritance'`.

---

## New Rust Surface

| Addition | Location | Notes |
|---|---|---|
| `Value::Entity(EntityHandle)` | `value.rs` | Actor references; pointer equality |
| `Value::Future(FutureHandle)` | `value.rs` | Lazy future; resolved only at dispatch |
| `EntityId` | `entity.rs` | `u64`, assigned atomically at spawn |
| `EntityHandle` | `entity.rs` | `Arc<EntityState>` newtype |
| `EntityState` | `entity.rs` | ivars, mailbox, mailbox condvar, status |
| `EntityStatus` | `entity.rs` | `Running \| Faulted(String) \| Terminated` |
| `Envelope` | `entity.rs` | selector, args, reply `FutureHandle` |
| `FutureHandle` | `entity.rs` | `Arc<FutureState>` newtype |
| `FutureState` | `entity.rs` | `Mutex<FutureInner> + Condvar` |
| `FutureHandle::wait()` | `entity.rs` | Blocks caller's thread until resolved |
| `FutureHandle::resolve(v)` | `entity.rs` | Sets value; notifies waiters |
| `deep_clone_args` | `entity.rs` | Cross-entity value cloning |
| `ClassDef.is_actor` | `interpreter.rs` | Inherited flag; determines dispatch path |
| Thread-local `CURRENT_ENTITY` | `entity.rs` | Sync self-send detection |

---

## Changes to Existing Source Files

### `src/value.rs`

- Add `Entity(EntityHandle)` and `Future(FutureHandle)` variants.
- `Display`: `'an EntityName'`, `'a Future'`.
- `PartialEq`: entity is pointer equality; future is pointer equality.
- `Clone`: both are `Arc::clone`.

### `src/builtins.rs`

- Add `Value::Entity(handle)` arm in `dispatch()`: calls `dispatch_entity_send`,
  returning `Value::Future` immediately (unless it is a self-send, in which
  case dispatches synchronously and wraps result in an already-resolved
  `Value::Future` or returns directly).
- Add `Value::Future(handle)` arm in `dispatch()`: calls `handle.wait()` to
  block until resolved, then re-dispatches to the resolved value.
- `Object class >> new` dispatch: if `class.is_actor`, spawn entity thread
  and return `Value::Entity`; else existing synchronous `Value::Object` path.
- Guard: `Value subclass:` sets `is_actor: false` on the new class.

### `src/eval.rs`

No structural changes required. The evaluator stays synchronous. Async
behavior is handled entirely in `builtins.rs` dispatch and entity threads.

### `src/interpreter.rs`

- Register `Value` as a built-in class (`is_actor: false`) at startup,
  as a subclass of `Object`.
- Set `is_actor: false` on immutable/identity-free built-in classes
  (`Array`, `ByteArray`, `Association`, `Error`, `True`, `False`, `Boolean`)
  — these are value types; initialization-time equivalent of `Value subclass:`.
- `OrderedCollection` and `Dictionary` retain `is_actor: true` (inherited
  from `Object`); they become actor entities in this chunk.

### `src/kernel.smol`

- Add `Value` class declaration (no instance variables; `is_actor: false`
  class flag set by Rust init, not Smol source):

  ```smalltalk
  "Value — base class for passive data objects (no actor semantics)."
  Object subclass: #Value
      instanceVariableNames: ''
      classVariableNames: ''
      indexedSlots: #none.
  ```

- Change immutable/identity-free kernel types from `Object subclass:` to
  `Value subclass:`. This is a mechanical change; no behavioral change:

  ```smalltalk
  Value subclass: #Array ... .
  Value subclass: #ByteArray ... .
  Value subclass: #Association ... .
  Value subclass: #Error ... .
  ```

- `OrderedCollection` and `Dictionary` remain `Object subclass:` and become
  actor entities. Existing tests that exercise these types now depend on
  entity threads running and futures resolving; behavioral contracts
  (correct element counts, correct values) are unchanged.

---

## Protocol Tables

### Object (actor base class — Smol-visible)

| Selector | Side | Requires | Ensures |
|---|---|---|---|
| `new` | class | receiver's `is_actor` is `true` | Allocates `EntityState`, spawns OS thread, queues `initialize`, returns `Value::Entity` |
| `new` | class | receiver's `is_actor` is `false` | Allocates `Value::Object` synchronously, calls `initialize`, returns value |

### Value (passive base class — Smol-visible)

No new methods beyond `Object` inheritance. The class exists to declare
`is_actor: false` for the subhierarchy.

### Value::Entity dispatch semantics (Rust)

| Condition | Behavior |
|---|---|
| Target ID equals `CURRENT_ENTITY` (self-send) | Synchronous direct dispatch; return value unwrapped |
| Target status `Running`, not self-send | Allocate `FutureHandle`; enqueue `Envelope`; return `Value::Future` |
| Target status `Faulted` | Raise `Error signal: 'entity has faulted'` |
| Any arg is `Value::Block` | Raise `Error signal: 'blocks may not cross entity boundaries'` |

### Value::Future dispatch semantics (Rust)

| Condition | Behavior |
|---|---|
| Future already resolved | Return resolved value; re-dispatch message synchronously |
| Future not yet resolved | `handle.wait()` (blocks OS thread); when resolved, re-dispatch |

### FutureHandle

| Method | Requires | Ensures |
|---|---|---|
| `wait() -> Value` | Called from entity's OS thread | Blocks until `resolve()` is called; returns resolved value |
| `resolve(v: Value)` | Called at most once | Sets value; wakes all waiters via `condvar.notify_all()` |

---

## Implementation Order

1. **`ClassDef.is_actor` flag** — add to `ClassDef`; set to `true` by default;
   set `false` on all current built-in kernel classes at interpreter init.
   Tests: `Object.is_actor` is `true`; `Array.is_actor` is `false`; a user-
   defined `Object subclass:` inherits `true`.

2. **`Value subclass:` syntax** — detect in class-definition dispatch; set
   `is_actor: false` on new class.
   Tests: `Value subclass: #Point ...` produces a class with `is_actor: false`;
   subclassing it also inherits `false`; mixed-hierarchy raises.

3. **`Value::Entity` and `Value::Future` variants** — add to `value.rs`; stub
   `Display`, `PartialEq`, `Clone`.
   Tests: entity identity (two handles from same `Arc` are `ptr_eq`; two
   distinct spawns are not); future equality is pointer equality.

4. **`FutureState`, `FutureHandle`** — new in `entity.rs`.
   Tests: `resolve` + `wait` in different threads; `wait` before `resolve`
   blocks and then returns; double-resolve is handled gracefully (second call
   is a no-op or panics — document the choice).

5. **`EntityState`, `EntityHandle`** — new in `entity.rs`; entity thread loop
   (pop envelope, dispatch, resolve reply future, park when empty).
   Tests: entity with no methods starts and parks without panic.

6. **`Object class >> new` for actor classes** — wire up in `builtins.rs`;
   migrates to `Value::Entity` path when `is_actor: true`.
   Tests: `Counter new` (actor class) returns `Value::Entity`; `Association new`
   (value class) returns `Value::Object`.

7. **`dispatch_entity_send`** — `Value::Entity` arm; deep-clone args;
   allocate `FutureHandle` pair; enqueue `Envelope`; return `Value::Future`.
   Tests: send to entity enqueues envelope; `Value::Block` arg raises;
   `Value::Object` arg deep-cloned (mutating original after send does not
   affect enqueued copy); `Value::Future` arg forwarded as `Arc::clone`.

8. **`dispatch_future`** — `Value::Future` arm; calls `wait()`; re-dispatches.
   Tests: `result := entity compute: 42. result + 1` returns `43` (entity's
   `compute:` method returns `42`).

9. **Sync self-send** — `CURRENT_ENTITY` thread-local; bypass enqueue when
   target ID matches.
   Tests: entity calling a method on `self` within a handler does not deadlock
   and returns the correct value.

10. **kernel.smol migration** — change existing `Object subclass:` to
    `Value subclass:` for all data types. Run full test suite; all existing
    tests must pass.

11. **Multi-entity test** — spawn two entities; one sends a message to the
    other and uses the result. Both run on separate OS threads. Verify
    correct result and no data races (`cargo test` with `MIRIFLAGS` or at
    minimum ThreadSanitizer pass).

12. **Error isolation test** — faulty entity (raises `Signal`) alongside healthy
    entity; verify healthy entity continues; waiter on faulty entity's future
    receives the sentinel error value rather than blocking forever.

13. **Coverage check** — `just cov`; no file below 80%.

---

## Open Questions

### One OS Thread Per Entity — Scale Ceiling

The MVP creates one OS thread per entity. This is correct but does not scale
beyond hundreds of entities (OS thread overhead is ~1–8 MB stack by default,
plus scheduling overhead). The documented upgrade path (green-thread pool,
see Design Decisions above) is the right next step after the model is
validated. Nothing in the chunk 21 design forecloses it; the entity thread
loop and `FutureHandle::wait()` are written to be drop-in compatible with
a coroutine-based implementation.

### Faulted Entity Future Resolution

When an entity faults mid-handler, its reply future must be resolved or
waiters will block forever. Options:

- Resolve with a sentinel `Value::Object` representing the fault (e.g., an
  `Error` instance).
- Resolve with a special `Value::Fault(String)` variant (adds a variant).
- Leave unresolved; document that callers must use a timeout (future chunk).

For chunk 21: resolve with an `Error signal:` value (reuse the existing
`Error` class). Any waiter receives the error value; if they dispatch a
message to it expecting a different type, the error will propagate naturally.

### `initialize` Timing

`new` queues `initialize` as the first envelope. A caller that immediately
sends more messages before the entity's thread has run its first envelope
will queue those after `initialize` — `initialize` still runs first (FIFO).
This is a contract of `new` and is documented in the protocol table above.

### Double-Resolve of FutureHandle

`resolve()` should be called exactly once per `Envelope`. The entity message
loop always resolves the reply future at the end of a handler turn (with the
return value). If the handler signals an error, the loop resolves with the
error sentinel. A `debug_assert!` in `resolve()` can catch double-resolve in
development.

### Relation to Triple Store (Chunk 22 Preview)

Chunk 22 integrates the triple store as entity state: `EntityState.ivars`
becomes a per-entity `TripleStore`. This is a contained change to
`EntityState` and the ivar read/write path; the entity thread loop, mailbox,
`FutureHandle`, and Smol surface are unchanged. The chunk 21 design keeps
entity state simple specifically to make this swap clean.
