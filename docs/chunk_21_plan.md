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
   to it. A Value _is_ an immutable Object that can be shared across entity
   boundaries. The compiler/interpreter disallows assignment to instance variables
   after construction.
2. `Value::ValueObject(Arc<Object>)` — distinguishes value object instances from
   actor entity references in the Rust `Value` enum (renamed from old
   `Value::Object`).
3. `Value::Object(EntityHandle)` — actor entity references as first-class values.
   `Object subclass:` now creates actor entities by default.
4. `Value::Future(FutureHandle)` — lazy future references; forwarded without
   resolution; resolved only when a message is dispatched to the value.
5. `EntityState` — per-entity isolated instance variables, a mailbox, and a
   green thread running the entity's message loop.
6. `Envelope` — a queued message: selector, arguments, reply sender.
7. `FutureState` — shared state between the entity that produces a result and
   any entity waiting to consume it. Uses `may`'s green-thread park/unpark
   mechanism (not OS-thread blocking).
8. `Object class >> new` — on actor classes creates an `EntityState`, spawns
   a green thread to run the mailbox loop, queues `initialize`, and returns
   `Value::Object(EntityHandle)`.
9. Async dispatch — sending any message to a `Value::Object(EntityHandle)`
   enqueues an `Envelope` and returns `Value::Future` immediately.
10. Future resolution — dispatching a message to a `Value::Future` receiver
    parks the calling entity's green thread until the future resolves, then
    dispatches to the resolved value.
11. Sync self-send — messages sent to `self` within an entity's own handler
    dispatch synchronously (no envelope, no future, no deadlock).
12. Block cross-entity semantics:
    - **Reads**: captured env frames use `Mutex` (changed from `RefCell`); blocks
      safely share live references to outer variables via `Arc::clone`. Reads see
      current values, not a snapshot.
    - **Writes**: compile-time error at method installation if a block assigns
      to a captured outer variable and is passed as an argument to a non-`self`/
      `super` message send. Error message directs to `inject:into:` (for
      collection accumulation) or `ValueHolder` (other shared-state cases).
    - **Non-local return `^`**: the entity message loop catches `EvalError::Return`
      at the top level and resolves the reply future with the return value,
      enabling `^` in a cross-entity block to short-circuit return from the
      enclosing message — exactly as `^` in a local block returns from its
      enclosing method.
    - `Block` gains `home_entity: Option<EntityId>` to identify which entity's
      frames it captured.
13. `_prim*` restricted to `self` — `_prim*` selectors may only be sent to
    `self` or `super`. A compile-time error is raised at method installation if
    a `_prim*` send targets any other receiver. This preserves the actor
    invariant: entity data is only ever touched by its own thread.
14. Kernel updates: `OrderedCollection` and `Dictionary` replace all
    `storage _primAt:`, `storage _primAt:put:`, `storage _primSize` calls with
    the public Array interface (`storage at:`, `storage at:put:`, `storage size`).
    Behavior is unchanged; those messages route through Array's mailbox to Array's
    own thread, where `Array >> at: i` executes `^self _primAt: i` as a self-send.
15. Error isolation — an unhandled error in an entity's message handler
    terminates that entity; all other entities continue running.
16. Multi-threaded execution via `may` — each entity runs on a green thread
    in a work-stealing OS thread pool; true multi-core parallelism from the
    first chunk.

**Explicitly deferred:**

- Promise pipelining (forwarding messages to unresolved futures without blocking).
- Sync optimization via escape analysis — D2 notes synchronous execution is an
  optimization; the runtime does not yet perform it.
- Triple store as entity state — chunk 22; entity state is instance variables here.
- Per-entity GC — chunk 23+; `Arc` reference counting is used now.
- Named entity registry — chunk 22.
- `Float64Array`, `species`, `ReadStream`, `WriteStream` — chunk 22.
- `ImmutableArray` (`Value subclass:`, truly immutable indexed storage) — deferred.
- Full `Exception` hierarchy, `ensure:`, `retry` — chunk 22.
- Preemptive scheduling, time-sliced execution — VM work.
- Mutation-queue approach for cross-entity block writes — a logically separate
  write queue to avoid deadlock could recover the traditional Smalltalk mutable-
  closure idiom across entity boundaries. Noted as a possible future optimization;
  deferred until the core actor model is stable.
- `ValueHolder` class in kernel — chunk 22 (referenced in compile-time error
  messages this chunk).

---

## Design Decisions

### Actor-by-Default (D2, D3)

The fundamental realization of D2 ("asynchronous by default") is that
`Object subclass:` creates an actor entity, not a passive object. There is no
`Entity subclass:` syntax — the distinction comes from the base class:

| Base class         | Kind          | Message send result       | Thread                              |
| ------------------ | ------------- | ------------------------- | ----------------------------------- |
| `Object subclass:` | Actor entity  | `Value::Future`           | One green thread per instance       |
| `Value subclass:`  | Passive value | `Value` (direct)          | None; dispatches on caller's thread |

This inverts the current opt-in model. Passive value semantics require an
explicit declaration via `Value subclass:`. Actors are the default because D2
states that sequential execution is an optimization of concurrent semantics,
not the other way around.

The correct cut for kernel types is **immutability**:

- **Value (truly immutable)**: `Association`, `Error`, `Boolean`/`True`/`False`.
  These migrate to `Value subclass:` in this chunk. They can be shared across
  entity boundaries by `Arc::clone` with no copying. Existing test behavior is
  fully preserved since value dispatch is identical to today's synchronous dispatch.

- **Entity (mutable, identity-bearing)**: `Array`, `ByteArray`,
  `OrderedCollection`, `Dictionary`. All four are mutable and become actor
  entities. `ImmutableArray` (`Value subclass:`, truly immutable indexed storage)
  is deferred.

`String` remains `Value::Str(Arc<str>)` — a Rust-level primitive variant, not an
`Object subclass:` instance. It is already immutable and shareable.

**Behavioral transparency**: from the programmer's perspective, code written for
a value class and code written for an actor class look identical — both use
standard message-send syntax. The difference is in what the system does under
the hood: synchronous dispatch vs. enqueue + future. This is the expression of
P7 (minimal ceremony) and P4 (concurrency is structural).

### `_prim*` Is Self-Only

`_prim*` selectors are an object's implementation detail for reaching into its
own Rust-level storage. They may only be sent to `self` or `super`. Sending
`_prim*` to any other receiver is a compile-time error raised at method
installation:

```
CompileError: '_prim*' may only be sent to self (found receiver: storage).
```

This preserves the actor invariant cleanly: an entity's raw data is only ever
touched by its own mailbox thread. No cross-entity data lock is needed for
correctness.

**`OrderedCollection` and `Dictionary`** continue to use a private `Array` entity
as their backing store, but their kernel methods call the public Array interface
(`storage at: i`, `storage at: i put: v`, `storage size`) rather than `_prim*`
directly. Inside `Array`'s handlers, `at: i` executes `^self _primAt: i` as a
self-send. The actor invariant holds: Array's data is only touched by Array's
own thread; `OrderedCollection`'s green thread parks on the reply future and
resumes when Array resolves it.

Resize and GC are unaffected: `pvtGrow` still allocates `Array new: n`, copies
elements via `at:put:` message sends, replaces `storage`, and the old Array
entity is garbage-collected via Arc refcount drop — identical semantics to before.

### Block Cross-Entity Semantics

Blocks cross entity boundaries frequently and are a fundamental part of the
language. The key barriers and their resolutions:

**Env frame thread safety.** `Block` captures its defining `Env` via
`Arc<Frame>` references. Frames currently use `RefCell<HashMap<Arc<str>, Value>>`,
which is `!Sync` and therefore prevents `Value` from being `Send`. Resolution:
change all `Env` frames to `Mutex<HashMap<Arc<str>, Value>>`. `Mutex<T>` is
`Send + Sync` when `T: Send`, so `Arc<Frame>` becomes sendable across entity
threads. The block's captured frames are `Arc::clone`d into the `Envelope` — no
copy, live references preserved. Reads see current values; concurrent reads are
safe since Mutex serializes access. Frame locks are uncontended in the common
case: only the owning entity writes to its own frames (enforced below).

**Write to foreign env frame = compile-time error.** At method installation, the
compiler scans all blocks in the method body. A block that (1) assigns to a
variable from the enclosing method scope and (2) is passed as an argument to a
message send whose receiver is not `self` or `super` is a compile-time error:

```
CompileError: Block assigns to captured variable 'sum'.
  If accumulating over a collection, use inject:into: instead.
  Otherwise, wrap the shared value in a ValueHolder.
```

This is conservative: it flags any such block regardless of whether the receiver
is statically known to be a value or an actor. False positives (e.g.,
`3 timesRepeat: [count := count + 1]`) are acceptable for MVP; the error message
provides the correct idiom. Self-sends (`self do: [...]`) are never flagged.

**Non-local return `^` across entity boundaries.** `^` inside a cross-entity
block should short-circuit return from the enclosing message in the calling
entity. The mechanism is already natural: `^` raises `EvalError::Return(value, depth)`
which propagates up through the receiving entity's handler. The entity message
loop catches `EvalError::Return(v, _)` at the top level and resolves the reply
`FutureHandle` with `v` — the same resolution path as normal handler completion.
The calling entity, parked on that future, resumes with `v` as the result. From
the programmer's perspective, `^` in a cross-entity block exits the iteration and
returns from the enclosing method, exactly as in local Smalltalk semantics.

`Block` gains `home_entity: Option<EntityId>`, set at creation time from the
`CURRENT_ENTITY` thread-local.

### Value::Future — Lazy, Forwarded by Reference

`Value::Future(FutureHandle)` is returned immediately on any actor message send.
The rule:

> **Resolution is required only at the point where a message is dispatched to
> the future's value.** All other operations — assignment, passing as argument
> into another envelope — propagate the future reference by `Arc::clone`.

Concretely:

```smalltalk
r1 := entityA compute: 42.          "r1 = Value::Future; no blocking"
entityB transform: r1.              "r1 forwarded as-is into envelope; no blocking"
result := r1 + 1.                   "r1 must resolve before '+' dispatches; parks here"
```

`FutureHandle` is `Arc<FutureState>`:

```rust
pub struct FutureState {
    inner: Mutex<FutureInner>,
}

pub struct FutureInner {
    value: Option<Value>,
    waiters: Vec</* may coroutine handle */>,
}
```

`FutureHandle::wait()` registers the calling green thread in `waiters` and parks
it via `may`'s coroutine park mechanism. The OS thread is released to execute
other green threads. `FutureHandle::resolve(v)` sets `inner.value` and unparks
all registered waiters.

The upgrade path to promise pipelining (deferred): add a `Vec<Envelope>` buffer
to `FutureState`; drain it on resolve. The `Arc`-shared structure does not
preclude this.

### Multi-Threaded Execution via `may`

Each entity instance spawns one green thread (stackful coroutine via the `may`
crate). The green thread runs the entity's message loop: pop envelope, dispatch
synchronously on the entity's thread, resolve the reply future with the return
value, repeat. The thread parks when the mailbox is empty or when waiting on a
`FutureHandle`.

`may`'s built-in work-stealing scheduler maps M green threads onto N OS threads
(N defaults to CPU count). `eval_expr` stays synchronous throughout — green
threads have their own Rust call stack, so no async/await transformation is
required. When the custom VM arrives, per-entity green-thread stacks are replaced
by explicit VM stack data structures; the scheduler is unchanged.

### Sync Self-Send

Messages sent to `self` within an entity's own message handler must dispatch
synchronously. A thread-local `CURRENT_ENTITY: Option<EntityId>` is set before
dispatching an envelope. If the target entity's ID matches `CURRENT_ENTITY`,
dispatch falls through to direct synchronous invocation. No programmer annotation
required.

### Object (Entity) State

`EntityState` holds a `Mutex<Object>` rather than separate `ivars`/`indexed`
fields. In correct programs this mutex is never actually contended — the actor
invariant ensures only the entity's own green thread acquires it. It exists solely
to make `Arc<EntityState>: Send + Sync` in Rust's type system.

```rust
pub struct EntityState {
    pub id:              EntityId,
    pub class:           Arc<Class>,
    pub data:            Mutex<Object>,               // actor-private; uncontended in practice
    pub mailbox:         Mutex<VecDeque<Envelope>>,
    pub mailbox_condvar: may::sync::Condvar,
    pub status:          Mutex<EntityStatus>,
}

pub enum EntityStatus {
    Running,
    Faulted(String),
    Terminated,
}
```

`data` and `mailbox` use separate locks. The `data` lock is held only during
dispatch of a single envelope; it is never held while waiting on a `FutureHandle`.

### Object Struct: Remove RefCell

`Object.slots` and `Object.indexed` currently use `RefCell` for interior
mutability. `RefCell: !Sync` makes `Arc<Object>: !Send`, preventing
`Value::ValueObject(Arc<Object>)` from appearing in an `Envelope`. Resolution:
remove `RefCell` from `Object`; fields become plain `HashMap<Arc<str>, Value>`
and `Option<IndexedStorage>` with plain inner `Vec<T>`. Mutation requires
`&mut Object`:

- **Entity objects**: acquired via `entity_state.data.lock().unwrap()` in the
  entity's message handler dispatch path.
- **Value objects**: mutated only during `initialize` before the `Arc` is shared;
  `Arc::get_mut` is valid at this point (refcount is 1).

`IndexedStorage` inner `RefCell`s are removed for the same reason.

### Envelope

```rust
pub struct Envelope {
    pub selector: Arc<str>,
    pub args:     Vec<Value>,
    pub reply:    FutureHandle,
}
```

Every send allocates a `FutureHandle`. Fire-and-forget semantics emerge naturally
when the caller stores the future in a variable and never dispatches a message to
it — the future is allocated but the green thread never parks on it.

### Cross-Entity Value Safety

Arguments in an `Envelope` follow these rules at send time:

| Value kind | Action |
| ------------------------------------------------------------ | ------------------------------------------- |
| `Int`, `Float`, `Bool`, `Nil`, `Symbol`, `LargeInt`          | `Clone` (copy) |
| `Str(Arc<str>)`                                              | `Arc::clone` |
| `ValueObject(Arc<Object>)`                                   | `Arc::clone` (immutable, safe to share) |
| `Object(EntityHandle)`                                       | `Arc::clone` (entity references intentionally shared) |
| `Future(FutureHandle)`                                       | `Arc::clone` (forwarded by reference; no resolution) |
| `Block`                                                      | `Arc::clone` on code; `Arc::clone` on env frames (live references); compile-time guard prevents foreign-frame writes |

### Error Isolation

Inside the entity's message loop, errors are caught at the loop level:

- `EvalError::Return(v, _)` — non-local return from a cross-entity block;
  resolves reply future with `v`; loop continues normally.
- All other errors — entity is marked `Faulted`; reply future resolved with
  `Error signal: 'entity faulted: <message>'` (not left unresolved); loop exits.

Sending a message to a `Faulted` entity raises
`Error signal: 'entity has faulted'`.

---

## Class Hierarchy Change

`Value` is a new built-in class registered at interpreter startup as a subclass
of `Object` with `is_value: true`.

```
Object  (is_value: false)
  Value  (is_value: true)           ← truly immutable data; shareable without copying
    Association
    Error
    Boolean
      True
      False
  Array              (is_value: false)
  ByteArray          (is_value: false)
  OrderedCollection  (is_value: false)
  Dictionary         (is_value: false)
  Counter  (user-defined; inherits is_value: false from Object)
```

`is_value` is inherited: subclasses of `Object` are actors unless they or an
ancestor set `is_value: true` by subclassing `Value`. Subclassing `Value` within
`Value` also inherits `is_value: true`. Mixed-hierarchy subclassing (an actor
class subclassing a value class or vice versa) raises
`Error signal: 'cannot mix actor and value inheritance'` at class-definition time.

---

## New Rust Surface

| Addition | Location | Notes |
| -------------------------------- | ---------------- | ---------------------------------------------------------- |
| `Value::Object(EntityHandle)` | `value.rs` | Actor entity references; pointer equality |
| `Value::ValueObject(Arc<Object>)` | `value.rs` | Value object instances; renamed from old `Value::Object` |
| `Value::Future(FutureHandle)` | `value.rs` | Lazy future; resolved only at message dispatch |
| `Block.home_entity` | `value.rs` | `Option<EntityId>`; set at block creation from thread-local |
| `EntityId` | `entity.rs` | `u64`, assigned atomically at spawn |
| `EntityHandle` | `entity.rs` | `Arc<EntityState>` newtype |
| `EntityState` | `entity.rs` | `data: Mutex<Object>`, mailbox, condvar, status |
| `EntityStatus` | `entity.rs` | `Running \| Faulted(String) \| Terminated` |
| `Envelope` | `entity.rs` | selector, args, reply `FutureHandle` |
| `FutureHandle` | `entity.rs` | `Arc<FutureState>` newtype |
| `FutureState` | `entity.rs` | `Mutex<FutureInner>` with green-thread waiter list |
| `FutureHandle::wait()` | `entity.rs` | Parks caller's green thread via `may` until resolved |
| `FutureHandle::resolve(v)` | `entity.rs` | Sets value; unparks all waiters |
| `Class.is_value` | `class.rs` | Inherited flag; determines dispatch path |
| Thread-local `CURRENT_ENTITY` | `entity.rs` | Sync self-send detection |

---

## Changes to Existing Source Files

### `src/value.rs`

- Rename `Value::Object(Arc<Object>)` → `Value::ValueObject(Arc<Object>)`.
- Add `Value::Object(EntityHandle)` for actor entity references.
- Add `Value::Future(FutureHandle)`.
- Add `home_entity: Option<EntityId>` field to `Block`.
- `Display`: `Value::Object` → `'a ClassName'`; `Value::Future` → `'a Future'`;
  `Value::ValueObject` → `'a ClassName'` (same as old `Value::Object`).
- `PartialEq`: entity is pointer equality; future is pointer equality.
- `Clone`: both are `Arc::clone`.

### `src/object.rs`

- Remove `RefCell` from `Object.slots`: `RefCell<HashMap<Arc<str>, Value>>`
  → `HashMap<Arc<str>, Value>`.
- Remove inner `RefCell` from each `IndexedStorage` variant
  (`RefCell<Vec<T>>` → `Vec<T>`).
- `Object::get_slot` keeps `&self`; `Object::set_slot` requires `&mut self`.
- `Object::new` and `Object::new_with_indexed` signatures unchanged.

### `src/env.rs`

- Change `type Frame = RefCell<HashMap<Arc<str>, Value>>`
  to `type Frame = Mutex<HashMap<Arc<str>, Value>>`.
- Update all `frame.borrow()` / `frame.borrow_mut()` calls to
  `frame.lock().unwrap()`.

### `src/class.rs`

- Add `is_value: bool` field to `Class`.
- `Class::is_value` inherits from the superclass chain: `true` if `self.is_value`
  or any ancestor has `is_value: true`.

### `src/builtins.rs`

- **`_prim*` self-check**: at method installation, scan the method body AST; if
  any `_prim*` message send has a receiver other than `self` or `super`, raise a
  compile-time error: `'_prim*' may only be sent to self`.
- **Cross-entity block write check**: at method installation, scan blocks in the
  method body; if a block (1) assigns to a variable from the enclosing method
  scope and (2) is passed as an argument to a message send whose receiver is not
  `self` or `super`, raise a compile-time error with `inject:into:` / `ValueHolder`
  guidance.
- **`Value::Object(EntityHandle)` arm** in `dispatch()`:
  - Self-send (target ID == `CURRENT_ENTITY`): synchronous direct dispatch;
    return value unwrapped.
  - Target status `Faulted`: raise `Error signal: 'entity has faulted'`.
  - Otherwise: allocate `FutureHandle`; enqueue `Envelope`; return `Value::Future`.
- **`Value::Future(FutureHandle)` arm** in `dispatch()`: call `handle.wait()`
  (parks green thread); when resolved, re-dispatch to the resolved value.
- **`Value::ValueObject` arm**: rename from old `Value::Object` arm; dispatch
  path unchanged (synchronous).
- **`Object class >> new`**: if `class.is_value` is `false`, spawn entity green
  thread and return `Value::Object(EntityHandle)`; else synchronous
  `Value::ValueObject` path.
- **`Value subclass:` guard**: set `is_value: true` on newly defined class.

### `src/eval.rs`

- Update all `Value::Object(...)` match arms to `Value::ValueObject(...)`.
- Set `CURRENT_ENTITY` thread-local at entity message loop dispatch entry.
- Entity message loop: catch `EvalError::Return(v, _)` and resolve reply future
  with `v` (in addition to resolving on normal return and error-sentinel on
  `Signal`/`TypeError`).
- At `Block` creation: capture `CURRENT_ENTITY` into `block.home_entity`.

### `src/interpreter.rs`

- Register `Value` as a built-in class (`is_value: true`) at startup, as a
  subclass of `Object`.
- Set `is_value: true` on `Association`, `Error`, `True`, `False`, `Boolean` at
  initialization.
- `Array`, `ByteArray`, `OrderedCollection`, `Dictionary` retain `is_value: false`.

### `src/kernel.smol`

- Add `Value` class declaration (`Object subclass: #Value ...`).
- Change `Association` and `Error` from `Object subclass:` to `Value subclass:`.
- `OrderedCollection`: replace all `storage _primAt: i` → `storage at: i`,
  `storage _primAt: i put: v` → `storage at: i put: v`,
  `storage _primSize` → `storage size`.
- `Dictionary`: same replacement throughout `pvtSlotFor:`, `at:put:`,
  `at:ifAbsent:`, `pvtRehash`, and all other methods using `storage _prim*`.

---

## Protocol Tables

### Object (actor base class — Smol-visible)

| Selector | Side  | Requires | Ensures |
| -------- | ----- | -------- | ------- |
| `new` | class | `is_value` is `false` | Allocates `EntityState`, spawns green thread, queues `initialize`, returns `Value::Object(EntityHandle)` |
| `new` | class | `is_value` is `true`  | Allocates `Object`, calls `initialize` synchronously, returns `Value::ValueObject` |

### Value (passive base class — Smol-visible)

No new methods beyond `Object` inheritance. The class exists to declare
`is_value: true` for the subhierarchy.

### Value::Object (entity) dispatch semantics (Rust)

| Condition | Behavior |
| --------- | -------- |
| Target ID equals `CURRENT_ENTITY` (self-send) | Synchronous direct dispatch; return value unwrapped |
| Target status `Running`, not self-send | Allocate `FutureHandle`; enqueue `Envelope`; return `Value::Future` |
| Target status `Faulted` | Raise `Error signal: 'entity has faulted'` |

### Value::Future dispatch semantics (Rust)

| Condition | Behavior |
| --------- | -------- |
| Future already resolved | Return resolved value; re-dispatch message synchronously |
| Future not yet resolved | `handle.wait()` (parks green thread via `may`); when resolved, re-dispatch |

### FutureHandle

| Method | Requires | Ensures |
| ------ | -------- | ------- |
| `wait() -> Value` | Called from entity's green thread | Parks green thread until `resolve()` is called; OS thread remains free; returns resolved value |
| `resolve(v: Value)` | Called at most once | Sets value; unparks all waiters; `debug_assert!` on double-resolve |

---

## Implementation Order

1. **Env frame Mutex** — change `RefCell<HashMap>` to `Mutex<HashMap>` in
   `env.rs`; update all borrow calls.
   Tests: all existing env tests pass unchanged.

2. **`Object` RefCell removal** — strip `RefCell` from `Object.slots` and
   `IndexedStorage` inner vecs; update `set_slot` to `&mut self`; update call
   sites in `eval.rs`/`builtins.rs` to acquire `&mut Object` via entity data
   lock or `Arc::get_mut` during value construction.
   Tests: all existing object tests pass.

3. **`Class.is_value` flag** — add field to `Class`; set on built-in classes at
   interpreter init; inherited via superclass chain.
   Tests: `Object.is_value` false; `Array.is_value` false; `Association.is_value`
   true; user `Object subclass:` inherits false; user `Value subclass:` inherits
   true; mixed hierarchy raises.

4. **`Value` variant rename** — `Value::Object(Arc<Object>)` → `Value::ValueObject`;
   add `Value::Object(EntityHandle)` and `Value::Future(FutureHandle)` stubs.
   Update all match arms throughout the codebase.
   Tests: all existing tests pass with renamed variant.

5. **`Block.home_entity`** — add `Option<EntityId>` field to `Block`; set from
   `CURRENT_ENTITY` thread-local at block creation in `eval.rs`.

6. **`_prim*` self-check** — at method installation, scan method body AST; raise
   compile-time error if `_prim*` is sent to a non-`self`/`super` receiver.
   Tests: `self _primAt: 0` compiles; `storage _primAt: 0` raises compile error.

7. **Cross-entity block write check** — at method installation, scan blocks for
   outer-variable assignment passed as a non-self message argument; raise
   compile-time error with `inject:into:` / `ValueHolder` guidance.
   Tests: accumulation via `inject:into:` compiles; outer-assignment variant
   raises.

8. **`FutureState`, `FutureHandle`** — new in `entity.rs`; green-thread
   park/unpark via `may`.
   Tests: `resolve` + `wait` across green threads; wait-before-resolve parks then
   returns; double-resolve triggers `debug_assert`.

9. **`EntityState`, `EntityHandle`** — new in `entity.rs`; entity green-thread
   message loop (pop envelope, dispatch, resolve reply future, park when empty).
   Tests: entity with no methods starts and parks without panic.

10. **`Object class >> new` for actor classes** — spawn entity green thread;
    return `Value::Object(EntityHandle)`.
    Tests: `Counter new` returns `Value::Object`; `Association new` returns
    `Value::ValueObject`.

11. **`Value::Object` dispatch** — self-send detection via `CURRENT_ENTITY`;
    faulted check; enqueue envelope; return `Value::Future`.

12. **`Value::Future` dispatch** — `wait()` + re-dispatch.
    Tests: `result := entity compute: 42. result + 1` returns `43`.

13. **Sync self-send** — `CURRENT_ENTITY` thread-local; bypass enqueue when
    target entity ID matches.
    Tests: entity calling a method on `self` within a handler returns correct
    value and does not deadlock.

14. **Error isolation + `EvalError::Return` handling** — entity loop catches
    both; `Return(v, _)` resolves future with `v` and continues; all other
    errors resolve future with error sentinel and mark entity `Faulted`.
    Tests: `^` in cross-entity block short-circuits and resolves future; faulted
    entity alongside healthy entity; healthy entity continues.

15. **kernel.smol migration** — `Value subclass:` for `Association` and `Error`;
    `storage _prim*` → public Array interface in `OrderedCollection` and
    `Dictionary`. Run full test suite; all existing tests must pass.

16. **Multi-entity test** — two entities; one sends a message to the other and
    uses the result; both run on separate OS threads; correct result and no data
    races.

17. **Error isolation test** — faulty entity alongside healthy entity; healthy
    entity continues; waiter on faulty entity's future receives error sentinel
    rather than blocking forever.

18. **Coverage check** — `just cov`; no file below 80%.

---

## Open Questions

### `pvtRehash` Scratch Buffer

`Dictionary >> pvtRehash` uses a temporary `Array` as a scratch buffer to hold
old entries during rehash. Under the `_prim*` self-only rule, this temporary
Array must be accessed via `at:`/`at:put:` message sends (not `_prim*`). The
temporary Array is a locally-created entity used only within the Dictionary's
own handler — no other entity touches it — so the actor invariant holds. The
async round-trips per element are acceptable for MVP. In-place rehashing without
a scratch buffer could be explored during implementation if performance warrants.

### `initialize` Timing

`new` queues `initialize` as the first envelope. A caller that immediately sends
more messages before the entity's thread has run its first envelope will queue
those after `initialize` — `initialize` still runs first (FIFO). This is a
contract of `new`.

### Triple Store Persistence (Chunk 22 Preview)

Chunk 22 integrates the triple store as entity state: `EntityState.data` becomes
entries in a `TripleStore`. This is a contained change to `EntityState` and the
ivar read/write path; the entity thread loop, mailbox, `FutureHandle`, and Smol
surface are unchanged. The chunk 21 design keeps entity state simple specifically
to make this swap clean.

**Open**: should all entities be independently persistent, or should persistence
be opt-in? The triple store was intended to replace image-based persistence, but
may add overhead where unnecessary. Deferred to chunk 22 for experimentation.
