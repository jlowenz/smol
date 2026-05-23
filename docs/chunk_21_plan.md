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

1. `Value subclass:` — new language-level declaration for passive, immutable 
   data objects (opt-out from actor semantics); existing kernel types migrated 
   to it. This should be considered a subclass of Object with special 
   evaluator/compiler/VM semantics: a Value _is_ an immutable Object that can 
   be shared across entity boundaries. This does imply that the compiler/interpreter 
   must (eventually) disallow mutation of instance variables after creation. 
2. `Value::ValueObject` to differentiate value objects from objects 
3. `Object subclass:` now creates actor entities by default - because a 
   non-Value Object IS an asynchronous actor. 
4. `Value::Object(EntityHandle)` — entity references as first-class values.
5. `Value::Future(FutureHandle)` — lazy future references; forwarded without
   resolution; resolved only when a message is dispatched to the value.
6. `EntityState` — per-entity isolated instance variables, a mailbox, and a
   green thread running the entity's message loop. 
7. `Envelope` — a queued message: selector, arguments, reply sender.
8.  `FutureState` — shared state between the entity that produces a result and
   any entity waiting to consume it (`Mutex<Option<Value>> + Condvar`).
9.  `Object class >> new` — on actor classes creates an `EntityState`, spawns
   a coroutine/green thread to run the mailbox loop, queues `initialize`, adds 
   the thread to the run queue, and returns `Value::Object`.
10. Async dispatch — sending any message to a `Value::Object` enqueues an
   `Envelope` and returns `Value::Future` immediately.
11. Future resolution — dispatching a message to a `Value::Future` receiver
    blocks the calling entity's thread until the future resolves, then dispatches 
    to the resolved value.
12. Sync self-send — messages sent to `self` within an entity's own handler
    dispatch synchronously (no envelope, no future, no deadlock).
13. Error isolation — an unhandled error in an entity's message handler
    terminates that entity; all other entities continue running.
14. Multi-threaded execution — each entity runs on its own green thread; scheduler 
    has a fixed work-stealing pool of OS threads to execute green threads; true 
    multi-core parallelism from the first chunk. Like BEAM VM in evaluator form. 

**Explicitly deferred:**

- Promise pipelining (forwarding messages to unresolved futures without
  blocking) — the current model resolves at point of dispatch; message
  forwarding/buffering to an unresolved future is a future chunk.
- Sync optimization via escape analysis — D2 notes synchronous execution is
  an optimization; the runtime does not yet perform it.
- Triple store as entity state — chunk 22; entity state is instance variables here.
- Per-entity GC — chunk 23+; `Arc` reference counting is used now.
- Named entity registry — chunk 22.
- `Float64Array`, `species`, `ReadStream`, `WriteStream` — chunk 22.
- `ImmutableArray` (`Value subclass:`, truly immutable indexed storage) — deferred; 
  requires traits to share indexed-access protocol with `Array`.
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

| Base class         | Kind          | Message send result | Thread                              |
| ------------------ | ------------- | ------------------- | ----------------------------------- |
| `Object subclass:` | Actor entity  | `Value::Future`     | One green thread per instance       |
| `Value subclass:`  | Passive value | `Value` (direct)    | None; dispatches on caller's thread |

This inverts the current opt-in model. Passive value semantics require an
explicit declaration via `Value subclass:`. Actors are the default because D2
states that sequential execution is an optimization of concurrent semantics,
not the other way around. An opt-in model would result in most code defaulting
to the familiar Smalltalk object model, making actors a specialty feature rather
than the foundation.

The correct cut for kernel types is **immutability**, not collection vs.
non-collection:

- **Value (truly immutable)**: `Association`, `Error`, `Boolean`/`True`/`False`,
  `String`. These migrate to `Value subclass:` in this chunk. Because they are
  immutable they can be shared across entity boundaries by `Arc::clone` with no
  copying. Existing test behavior is fully preserved since value dispatch is
  identical to today's synchronous dispatch.

- **Entity (mutable, identity-bearing)**: `Array`, `ByteArray`,
  `OrderedCollection`, `Dictionary`. All four are mutable — `Array` and
  `ByteArray` via `at:put:` — and therefore cannot be values. They remain
  `Object subclass:` and become actor entities.

  `ImmutableArray` (a truly immutable `Value subclass:` for fixed indexed
  storage) is deferred to a later chunk; it is the natural companion to `Array`
  once traits exist to express shared indexed-access protocols.

**`_prim*` methods are synchronous primitives.** `_primAt:`, `_primAt:put:`,
`_primSize`, and related selectors bypass the mailbox and execute directly on
the calling thread. When the receiver is an actor entity, the entity's data
lock (ivar mutex) is acquired for the duration of the primitive, ensuring
correct serialization against the entity's own message loop. No `Future` is
returned from a `_prim*` dispatch. This is the mechanism that allows
`OrderedCollection` and `Dictionary` to use `Array`/`ByteArray` as backing
stores without incurring async round-trips on every element access. It's possible
this could be further optimized for "owned" entities. 

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

NOTE: The suggestion below is probably NOT the correct implementation for the 
Future, as we need to _suspend_ the current entity's green thread to wait for 
the future to resolve. I.e. we cannot block one of the scheduler's OS threads 
with a `wait()` - NEEDS UPDATE

```rust
pub struct FutureState {
    pub inner: Mutex<FutureInner>,
    pub condvar: Condvar,
}

pub struct FutureInner {
    pub value: Option<Value>,
}
```

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

Each entity instance spawns one green thread. The thread runs the entity's message
loop: pop envelope, dispatch synchronously on the entity's thread, resolve the
reply future with the return value, repeat. The thread yields when
the mailbox is empty or when waiting to resolve a future and moves to a 
`waiting` queue. A scheduler with a work-stealing OS thread pool executes entity 
threads in the `ready` queue. 

This gives true multi-core parallelism immediately. Two entities process
messages concurrently on separate cores with no additional configuration.

1. Use green threads (stackful coroutines via a library
   such as `may`). No changes to the evaluator required — green threads have
   their own Rust call stack, so `eval_expr` stays synchronous.
2. Introduce a work-stealing thread pool (N OS threads for M green-thread
   entities), equivalent to the BEAM scheduler model.
3. Waiting for a `Future` becomes a green-thread park rather than an
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

### Object (Entity) State

   **NOTE**: we may want to update the names here to reflect that entities are _just_ objects
   now... i.e. why rename the main thing in the interpreter from Object to Entity when an entity is just an Object now? Value objects are the exception here, as they need the 
   distinguished implementation of being immutable, synchronous, and shared. 

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
never dispatches a message to it — the future is allocated but no yield for 
resolution ever occurs. 

### Cross-Entity Value Safety

Arguments in an `Envelope` follow these rules at send time. Because `Value
subclass:` types are truly immutable and mutable types are actor entities,
deep-clone is never required.

| Value kind                                                   | Actions                                                   |
| ------------------------------------------------------------ | --------------------------------------------------------- |
| `Int`, `Float`, `Bool`, `Nil`, `Symbol`, `LargeInt`          | `Clone` (copy)                                            |
| `Str(Arc<str>)`                                              | `Arc::clone` (immutable string, safe to share)            |
| `ValueObject(Arc<ObjectData>)` where `is_value: true` (Value instance) | `Arc::clone` (immutable, safe to share)                   |
| `Object(EntityHandle)`                                       | `Arc::clone` (entity references are intentionally shared) |
| `Future(FutureHandle)`                                       | `Arc::clone` (forwarded by reference; no resolution)      |
| `Block`                                                      | Error: `'blocks may not cross entity boundaries'`         |

### Error Isolation

Inside the entity's message loop, any error that is not `EvalError::Return` is caught at the loop level. The entity is marked `Faulted`, the error is logged, and the loop exits. All other entities continue unaffected. A `Faulted` entity's reply future is resolved with a sentinel error value
(rather than left unresolved forever, which would deadlock any waiter).
Sending a message to a `Faulted` entity raises
`Error signal: 'entity has faulted'`. 

---

## Class Hierarchy Change

`Value` is a new built-in class registered at interpreter startup as a
subclass of `Object` with `is_value: true`. Its instances are `Value::Object`
in Rust (same as today's non-actor instances); there is no new Rust variant.

```
Object  (is_value: false)
  Value  (is_value: true)           ← truly immutable data; shareable without copying
    Association
    Error
    Boolean
      True
      False
  Array          (is_value: false)    ← mutable indexed storage; _prim* dispatch is sync
  ByteArray      (is_value: false)    ← mutable byte storage; _prim* dispatch is sync
  OrderedCollection  (is_value: false)  ← mutable, identity-bearing
  Dictionary         (is_value: false)  ← mutable, identity-bearing
  Counter  (user-defined; is_value: false — inherits from Object)
  ...
```

`String` is an `Arc<str>` (a separate `Value` variant in Rust, not an
`Object subclass:` instance); it is already immutable and shareable.
`ImmutableArray` (`Value subclass:`, truly immutable indexed storage) is
deferred.

The `is_value: false` flag is inherited: subclasses of `Object` are actors and not value objects unless
they or an ancestor explicitly set `is_value: true` by subclassing `Value`
(or `Value subclass:` in Smol). Subclassing `Value` within `Value` inherits
`is_value: true`.

Mixed-hierarchy subclassing (an actor class subclassing a value class or vice
versa) is an open question for the MVP. For chunk 21: detected at class-
definition time and raises `Error signal: 'cannot mix actor and value inheritance'`.

---

## New Rust Surface

| Addition                         | Location         | Notes                                                        |
| -------------------------------- | ---------------- | ------------------------------------------------------------ |
| `Value::Object(EntityHandle)`    | `value.rs`       | Actor references; pointer equality; extends existing Object implementation for actors. |
| `Value::ValueObject(ObjectData)` | `value.rs`       | New value object distinction                                 |
| `Value::Future(FutureHandle)`    | `value.rs`       | Lazy future; resolved only at message dispatch               |
| `EntityId`                       | `entity.rs`      | `u64`, assigned atomically at spawn                          |
| `EntityHandle`                   | `entity.rs`      | `Arc<EntityState>` newtype                                   |
| `EntityState`                    | `entity.rs`      | ivars, mailbox, mailbox condvar, status                      |
| `EntityStatus`                   | `entity.rs`      | `Running \| Faulted(String) \| Terminated`                   |
| `Envelope`                       | `entity.rs`      | selector, args, reply `FutureHandle`                         |
| `FutureHandle`                   | `entity.rs`      | `Arc<FutureState>` newtype                                   |
| `FutureState`                    | `entity.rs`      | `Mutex<FutureInner> + Condvar`  **NOTE** may need update here |
| `FutureHandle::wait()`           | `entity.rs`      | Yields caller's thread until resolved                        |
| `FutureHandle::resolve(v)`       | `entity.rs`      | Sets value; notifies waiters                                 |
| `ClassDef.is_value`              | `interpreter.rs` | Inherited flag; determines dispatch path                     |
| Thread-local `CURRENT_ENTITY`    | `entity.rs`      | Sync self-send detection                                     |

---

## Changes to Existing Source Files

> **NOTE**: there may be conflicting information in this section. Discussions and decisions from above should override the suggested changes here, or ASK the user for resolution or confirmation of a specific change.

### `src/value.rs`

- Add `Entity(EntityHandle)` and `Future(FutureHandle)` variants.
- `Display`: `'an EntityName'`, `'a Future'`.
- `PartialEq`: entity is pointer equality; future is pointer equality.
- `Clone`: both are `Arc::clone`.

### `src/builtins.rs`

- Add `Value::Entity(handle)` arm in `dispatch()`:
  - If selector starts with `_prim`: acquire entity's data lock (ivar mutex),
    execute primitive inline on caller's thread, return value directly (no
    `Future`).
  - If self-send (target ID matches `CURRENT_ENTITY`): synchronous direct
    dispatch; return value unwrapped.
  - Otherwise: calls `dispatch_entity_send`, enqueues `Envelope`, returns
    `Value::Future` immediately.
- Add `Value::Future(handle)` arm in `dispatch()`: calls `handle.wait()` to
  block until resolved, then re-dispatches to the resolved value.
- `Object class >> new` dispatch: if `class.is_actor`, spawn entity thread
  and return `Value::Entity`; else existing synchronous `Value::Object` path.
- Guard: `Value subclass:` sets `is_value: true` on the new class.

### `src/eval.rs`

No structural changes required. The evaluator stays synchronous. Async
behavior is handled entirely in `builtins.rs` dispatch and entity threads.

### `src/interpreter.rs`

- Register `Value` as a built-in class (`is_value: true`) at startup,
  as a subclass of `Object`.
- Set `is_value: true` on truly immutable built-in classes
  (`Association`, `Error`, `True`, `False`, `Boolean`)
  — initialization-time equivalent of `Value subclass:`.
- `Array`, `ByteArray`, `OrderedCollection`, and `Dictionary` retain
  `is_value: false` (inherited from `Object`); all four become actor entities
  in this chunk. `Array` and `ByteArray` are mutable and therefore not values.

### `src/kernel.smol`

- Add `Value` class declaration (no instance variables; `is_value: true`
  class flag set by Rust init, not Smol source):

  ```smalltalk
  "Value — base class for passive data objects (no actor semantics)."
  Object subclass: #Value
      instanceVariableNames: ''
      classVariableNames: ''
      indexedSlots: #none.
  ```

- Change truly immutable kernel types from `Object subclass:` to
  `Value subclass:`. This is a mechanical change; no behavioral change:

  ```smalltalk
  Value subclass: #Association ... .
  Value subclass: #Error ... .
  ```

  (`Boolean`/`True`/`False` are handled at the Rust `interpreter.rs` init
  level, same as before.)

- `Array`, `ByteArray`, `OrderedCollection`, and `Dictionary` remain
  `Object subclass:` and become actor entities. Existing tests that exercise
  these types now depend on entity threads running and futures resolving;
  behavioral contracts are unchanged.

---

## Protocol Tables

### Object (actor base class — Smol-visible)

| Selector | Side  | Requires                         | Ensures                                                      |
| -------- | ----- | -------------------------------- | ------------------------------------------------------------ |
| `new`    | class | receiver's `is_value` is `false` | Allocates `EntityState`, spawns green thread, queues `initialize`, adds to ready queue, returns `Value::Entity` |
| `new`    | class | receiver's `is_value` is `true`  | Allocates `Value::Object` synchronously, calls `initialize`, returns value |

### Value (passive base class — Smol-visible)

No new methods beyond `Object` inheritance. The class exists to declare
`is_value: true` for the subhierarchy.

### Value::Object (entity) dispatch semantics (Rust)

| Condition                                     | Behavior                                                     |
| --------------------------------------------- | ------------------------------------------------------------ |
| Selector starts with `_prim`                  | Acquire entity's data lock; execute primitive inline on caller's thread; return value directly (no `Future`) |
| Target ID equals `CURRENT_ENTITY` (self-send) | Synchronous direct dispatch; return value unwrapped          |
| Target status `Running`, not self-send        | Allocate `FutureHandle`; enqueue `Envelope`; return `Value::Future` |
| Target status `Faulted`                       | Raise `Error signal: 'entity has faulted'`                   |
| Any arg is `Value::Block`                     | Raise `Error signal: 'blocks may not cross entity boundaries`<br />Actually, this will be a problem for everything that _needs_ a block to operate. We MAY need to reconsider this. Ask the user for resolution on Block and entity boundaries... did we just encounter a hiccup in "everything is an asynchronous object?" |
|                                               |                                                              |

### Value::Future dispatch semantics (Rust)

| Condition               | Behavior                                                       |
| ----------------------- | -------------------------------------------------------------- |
| Future already resolved | Return resolved value; re-dispatch message synchronously       |
| Future not yet resolved | `handle.wait()` (blocks OS thread); when resolved, re-dispatch |

### FutureHandle

| Method              | Requires                          | Ensures                                                      |
| ------------------- | --------------------------------- | ------------------------------------------------------------ |
| `wait() -> Value`   | Called from entity's green thread | Yields and moves entity to waiting queue/set until `resolve()` is called; returns resolved value |
| `resolve(v: Value)` | Called at most once               | Sets value; wakes all waiters via `condvar.notify_all()`     |

---

## Implementation Order

> **NOTE**: review this section to make sure the order is consistent with everything above.

1. **`ClassDef.is_actor` flag** — add to `ClassDef`; set to `true` by default;
   set `false` on truly immutable built-in classes at interpreter init.
   Tests: `Object.is_actor` is `true`; `Array.is_actor` is `true`;
   `Association.is_actor` is `false`; a user-defined `Object subclass:`
   inherits `true`; a user-defined `Value subclass:` inherits `false`.

2. **`Value subclass:` syntax** — detect in class-definition dispatch; set
   `is_value: true` on new class.
   Tests: `Value subclass: #Point ...` produces a class with `is_value: true`;
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
   migrates to `Value::Entity` path when `is_value: false`.
   Tests: `Counter new` (actor class) returns `Value::Entity`; `Association new`
   (value class) returns `Value::Object`.

7. **`_prim*` sync dispatch for entity receivers** — in `builtins.rs`
   dispatch, check selector prefix before routing to mailbox; acquire data
   lock and execute primitive inline. Tests: `_primAt:` on an `Array` entity
   from a different entity's thread returns the value synchronously (no
   `Future`); concurrent primitive call and mailbox dispatch serialize
   correctly via the data lock.

8. **`dispatch_entity_send`** — `Value::Entity` arm; `Arc::clone` args
   (values are immutable; entity handles are shared by reference); allocate
   `FutureHandle` pair; enqueue `Envelope`; return `Value::Future`.
   Tests: send to entity enqueues envelope; `Value::Block` arg raises;
   `Value::Future` arg forwarded as `Arc::clone`.

9. **`dispatch_future`** — `Value::Future` arm; calls `wait()`; re-dispatches.
   Tests: `result := entity compute: 42. result + 1` returns `43` (entity's
   `compute:` method returns `42`).

10. **Sync self-send** — `CURRENT_ENTITY` thread-local; bypass enqueue when
    target ID matches.
    Tests: entity calling a method on `self` within a handler does not deadlock
    and returns the correct value.

11. **kernel.smol migration** — change `Object subclass:` to `Value subclass:`
    for `Association` and `Error` only. Run full test suite; all existing
    tests must pass.

12. **Multi-entity test** — spawn two entities; one sends a message to the
    other and uses the result. Both run on separate OS threads. Verify
    correct result and no data races (`cargo test` with `MIRIFLAGS` or at
    minimum ThreadSanitizer pass).

13. **Error isolation test** — faulty entity (raises `Signal`) alongside healthy
    entity; verify healthy entity continues; waiter on faulty entity's future
    receives the sentinel error value rather than blocking forever.

14. **Coverage check** — `just cov`; no file below 80%.

---

## Open Questions

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
becomes entries in a `TripleStore`. This is a contained change to
`EntityState` and the ivar read/write path; the entity thread loop, mailbox,
`FutureHandle`, and Smol surface are unchanged. The chunk 21 design keeps
entity state simple specifically to make this swap clean.

**Question**: Should _ALL_ entities be independently persistent, or should entity persistence be opt-in in some way? The challenge here is that the triple store was intended to replace the image-based persistence of traditional Smalltalk, but the triple store may add significant overhead where unnecessary. May need some experimentation to understand the issues in more depth. 
