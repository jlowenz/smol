# Chunk 21: Async Actor Model MVP

## Status

Chunk 20 delivers `EvalError::Signal`, `_primHash`, `Error`, `Association`, and
`Dictionary`. The interpreter has arrays, collections, blocks, non-local returns,
and a sufficient exception primitive. The triple store crate is complete and idle.

This chunk implements the first proof of the system's core design thesis:
**entities as isolated, asynchronously-messaged processes**. It is intentionally
a minimum viable slice: enough to prove that sync-inside/async-outside works as a
Rust runtime model, enough to catch design errors before more infrastructure is
layered on top, and narrow enough to complete in one chunk.

**Delivers:**

1. `Value::Entity(EntityHandle)` — entity references as first-class values,
   distinct from passive `Value::Object`.
2. `EntityState` — per-entity isolated instance variables and a mailbox.
3. `Envelope` — a queued message: selector, arguments, optional reply channel.
4. `Scheduler` — cooperative round-robin driver; `run_until_quiescent` for tests,
   `run` event loop for programs.
5. `Entity subclass:` syntax — declares an entity class; instances are created with
   `spawn` rather than `new`.
6. Async dispatch — sending any message to a `Value::Entity` queues an `Envelope`
   rather than dispatching directly.
7. Sync self-send — messages sent within an entity's own handler context dispatch
   synchronously (no queuing, no deadlock).
8. Error isolation — an unhandled `EvalError::Signal` (or any non-Return error)
   inside an entity's message handler terminates that entity and logs the fault;
   all other entities continue running.
9. `_primSpawn` — Rust primitive on entity classes that creates an `EntityState`,
   registers it with the scheduler, and returns the `Value::Entity` handle.
10. `_primSelf` — allows an entity method to obtain its own `Value::Entity` handle
    for passing as a callback reference.

**Explicitly deferred:**

- Implicit futures / `await` — chunk 23+; async sends are fire-and-forget for now.
- Request-response pattern — the `reply:` mechanism is stubbed (the optional
  `oneshot` channel in `Envelope` is allocated but not exposed to Smol yet).
- Triple store as entity state — chunk 22; entity state is instance variables here.
- Per-entity garbage collection — chunk 23+; `Arc` reference counting is used now.
- Ownership / escape analysis for cross-entity value sharing — chunk 23+.
- Named entity registry (global lookup by symbol) — chunk 22; uses `Dictionary`.
- `Float64Array`, `species`, `ReadStream`, `WriteStream` — chunk 22 (from chunk 20).
- Full `Exception` hierarchy, `ensure:`, `retry` — chunk 22.
- Preemptive scheduling, time-sliced execution — future VM work.

---

## Design decisions

### The Value / Entity distinction

The fundamental divide in the system is:

| | Value | Entity |
|---|---|---|
| Identity | Structural (content equality) | Nominal (pointer / handle identity) |
| Mutability | Immutable or locally mutated | Mutable, history-bearing (eventually) |
| Concurrency | Synchronous always | Asynchronous by default |
| Shareable | Freely; copy is identity | By handle; no shared ivar access |

In Rust, `Value` grows one new variant:

```rust
Value::Entity(EntityHandle)
```

where `EntityHandle` is:

```rust
#[derive(Clone)]
pub struct EntityHandle(Arc<EntityState>);
```

`EntityHandle::ptr_eq` compares `Arc` pointer addresses; this is entity identity.
`Value::Entity` equality is pointer equality (two entity refs are `=` iff they
wrap the same `Arc`).

### EntityState

```rust
pub struct EntityState {
    pub id: EntityId,                       // monotonic u64, assigned at spawn
    pub class: Arc<ClassDef>,               // the entity's class
    pub ivars: Mutex<Vec<Value>>,           // instance variable slots
    pub mailbox: Mutex<VecDeque<Envelope>>, // pending messages
    pub status: Mutex<EntityStatus>,        // Running | Faulted(String) | Terminated
}

pub enum EntityStatus {
    Running,
    Faulted(String),   // last error message; entity will accept no further messages
    Terminated,        // clean shutdown (future)
}
```

`ivars` and `mailbox` use separate `Mutex` locks to avoid deadlock: the ivar lock
is held only during method dispatch (inside the entity's own scheduler turn),
never while sending to another entity.

### Envelope

```rust
pub struct Envelope {
    pub selector: Arc<str>,
    pub args: Vec<Value>,
    pub reply: Option<oneshot::Sender<Value>>, // unused in this chunk; allocated for future
}
```

Arguments are deep-cloned at send time (see "Cross-entity value safety" below).

### Scheduler

The scheduler is a simple cooperative, single-threaded driver for this chunk.
Multi-threaded work-stealing is a later concern (once the design is validated).

```rust
pub struct Scheduler {
    entities: Vec<EntityHandle>,   // registration order; round-robin
    next: usize,
}

impl Scheduler {
    /// Process all pending messages across all entities, round-robin,
    /// until no entity has any pending messages.
    /// Requires: called from a context that holds no entity ivar locks.
    /// Ensures: on return, all mailboxes are empty (or all remaining entities
    ///          are Faulted).
    pub fn run_until_quiescent(&mut self, interp: &mut Interpreter);

    /// Run indefinitely, blocking until no work is available, then sleeping.
    /// Used by the program entry point.
    pub fn run(&mut self, interp: &mut Interpreter);

    /// Register a newly spawned entity.
    pub fn register(&mut self, handle: EntityHandle);
}
```

Each turn: pick the next entity with a non-empty mailbox, pop one `Envelope`,
dispatch synchronously using the entity's `class` and `ivars`. Any error other
than `EvalError::Return` sets the entity's status to `Faulted` and logs the fault;
the turn ends and the scheduler moves to the next entity.

`run_until_quiescent` is the test harness entry point. It loops until a full
round-trip through all registered entities finds no pending messages.

### Entity class declaration

In Smol, an entity class is declared by subclassing `Entity` rather than `Object`:

```smalltalk
Entity subclass: #Counter
    instanceVariableNames: 'count'
    classVariableNames: ''
    indexedSlots: #none.

Counter >> initialize [ count := 0 ]
Counter >> increment  [ count := count + 1 ]
Counter >> decrement  [ count := count - 1 ]
Counter >> count      [ ^count ]
```

`Entity` is a new built-in class registered in the interpreter's class table at
startup, alongside `Object`. It carries a class-level flag `isEntityClass: true`
that `_primSpawn` checks.

`new` on an entity class raises `Error signal: 'use spawn to create entities'`.
This enforces the distinction at the language level.

### Spawning

```smalltalk
Counter class >> spawn [
    ^self _primSpawn
]
```

`_primSpawn` in Rust:

1. Verifies the receiver class has `isEntityClass: true`.
2. Allocates `EntityState` with zeroed ivars (one slot per declared instance
   variable name).
3. Wraps it in `EntityHandle(Arc::new(state))`.
4. Registers the handle with the thread-local `Scheduler`.
5. Queues an `initialize` message in the new entity's mailbox (so `initialize`
   runs on the first scheduler turn, not synchronously in the spawning context).
6. Returns `Value::Entity(handle)`.

The thread-local scheduler pattern allows `_primSpawn` to register without an
explicit scheduler argument threaded through every dispatch call.

### Async dispatch

The dispatch table in `builtins.rs` already routes on `Value::*`. A new top-level
branch:

```rust
Value::Entity(handle) => dispatch_entity_send(handle, sel, args, scheduler),
```

`dispatch_entity_send`:

```rust
fn dispatch_entity_send(
    handle: EntityHandle,
    sel: &str,
    args: Vec<Value>,
    scheduler: &mut Scheduler,
) -> Result<Value, EvalError> {
    let args_cloned = deep_clone_args(args); // cross-entity value safety
    let envelope = Envelope { selector: sel.into(), args: args_cloned, reply: None };
    handle.enqueue(envelope);
    Ok(Value::Entity(handle)) // async send returns the receiver (fire-and-forget)
}
```

The return value of an async send is the entity handle itself. This is a
deliberate design choice for this chunk: no futures yet, so the caller cannot
block on a result. Returning the handle allows chaining sends.

### Sync self-send

When a method dispatch occurs *within an entity's scheduler turn* (i.e., the
interpreter is already inside `dispatch_entity_turn`), sends to `self` must be
synchronous — otherwise an entity could deadlock waiting for its own message to be
processed by the same scheduler turn.

Detection: the scheduler sets a thread-local `current_entity: Option<EntityHandle>`
before calling dispatch. If `dispatch_entity_send` is called and the target handle
is `ptr_eq` to `current_entity`, dispatch falls through to direct synchronous
invocation (same path as `Value::Object` dispatch) rather than queuing.

This implements sync-inside/async-outside at the call level with no programmer
annotation.

### Cross-entity value safety

When values are enqueued in an `Envelope`, all arguments are deep-cloned:

- `Int`, `Float`, `Bool`, `Nil`, `Symbol`, `LargeInt` — `Clone` is a no-op copy.
- `Str(Arc<str>)` — `Arc::clone` is O(1); strings are immutable; safe to share.
- `Object(Arc<ObjectData>)` — deep-cloned: a new `ObjectData` is allocated with
  recursively cloned ivar slots. This gives true isolation.
- `Array`, `ByteArray`, `OrderedCollection` indexed storage — deep-cloned.
- `Entity(EntityHandle)` — `Arc::clone` only; entity handles are references, not
  values. Sending an entity handle to another entity is safe and intentional
  (it lets entities hold references to each other for later messaging).
- `Block` — **not** sent across entity boundaries in this chunk. Sending a block
  raises `Error signal: 'blocks may not cross entity boundaries'`. Blocks close
  over the spawning entity's ivar frame; cross-entity block sharing is a complex
  capability question deferred to a later chunk.

### Error isolation

Inside `dispatch_entity_turn`, any `EvalError` that is not `EvalError::Return` is
caught at the scheduler level:

```rust
match dispatch_entity_turn(&handle, &mut interp) {
    Ok(_) => {}
    Err(EvalError::Return(_)) => {} // non-local return escaped handler; treat as ok
    Err(e) => {
        let msg = e.to_string();
        eprintln!("[entity {}] fault: {}", handle.id(), msg);
        *handle.status.lock().unwrap() = EntityStatus::Faulted(msg);
    }
}
```

A faulted entity accepts no further messages. Its handle remains alive (other
entities may still hold it) but `dispatch_entity_send` checks status and raises
`Error signal: 'entity has faulted'` on attempted send.

### `_primSelf`

An entity method often needs to pass itself as a callback or reply address. Within
a synchronous method body, `self` is the raw value on the Smol receiver stack —
for entity methods, this must be the `Value::Entity(handle)` rather than a
`Value::Object`. The scheduler passes the entity handle as the Smol `self` when
dispatching a turn.

No additional primitive is needed if dispatch is wired correctly; this is a
correctness requirement on the dispatch path rather than a new primitive.

---

## New Rust surface

| Addition | Location | Notes |
|---|---|---|
| `Value::Entity(EntityHandle)` | `value.rs` | New variant; entity references |
| `EntityId` | `entity.rs` | `u64`, assigned atomically at spawn |
| `EntityHandle` | `entity.rs` | `Arc<EntityState>` newtype |
| `EntityState` | `entity.rs` | ivars, mailbox, status |
| `EntityStatus` | `entity.rs` | `Running \| Faulted(String) \| Terminated` |
| `Envelope` | `entity.rs` | selector, args, optional reply |
| `Scheduler` | `scheduler.rs` | `run_until_quiescent`, `run`, `register` |
| `_primSpawn` | `builtins.rs` | Entity class primitive |
| `deep_clone_args` | `entity.rs` | Cross-entity value cloning |
| Thread-local `CURRENT_ENTITY` | `scheduler.rs` | Sync-self-send detection |

---

## Changes to existing source files

### `src/value.rs`

- Add `Entity(EntityHandle)` to the `Value` enum.
- Add `Value::Entity` arms to `Display`, `PartialEq` (pointer equality),
  `Clone` (Arc clone), and any other derived impls.
- `Value::Entity` `printString` renders as `'an EntityName'`.

### `src/builtins.rs`

- Add top-level `Value::Entity(handle) =>` arm in `dispatch()` routing to
  `dispatch_entity_send`.
- Add `_primSpawn` to entity class dispatch (class-side).
- Add guard in `Object::new` dispatch: if the class has `isEntityClass`, redirect
  to `Error signal:`.

### `src/eval.rs`

- No structural changes required; `EvalError` already has `Signal` after chunk 20.
- `eval_block` and `invoke_method` remain synchronous; the scheduler drives
  entity-level async, not the evaluator.

### `src/interpreter.rs` (or equivalent top-level setup)

- Register `Entity` as a built-in class at interpreter startup.
- Initialize the thread-local `Scheduler`.

### `src/kernel.smol`

- Add `Entity` class declaration (no instance variables at the base level).
- Add `Entity class >> spawn [ ^self _primSpawn ]`.
- Add `Entity class >> new [ Error signal: 'use spawn to create entities' ]`.

---

## Protocol tables

### Entity (Smol-visible)

| Selector | Side | Requires | Ensures |
|---|---|---|---|
| `spawn` | class | receiver is an entity class | Returns `Value::Entity`; queues `initialize` |
| `new` | class | — | Always signals: 'use spawn to create entities' |
| `_primSpawn` | class | receiver's class flag `isEntityClass` is true | Allocates `EntityState`, registers with scheduler, returns handle |

### Value::Entity dispatch semantics (Rust)

| Condition | Behavior |
|---|---|
| Target is `current_entity` (self-send) | Synchronous direct dispatch |
| Target is any other entity, status `Running` | Enqueue `Envelope`; return handle |
| Target is any other entity, status `Faulted` | Raise `Error signal: 'entity has faulted'` |
| Arg is `Value::Block` | Raise `Error signal: 'blocks may not cross entity boundaries'` |

### Scheduler

| Method | Requires | Ensures |
|---|---|---|
| `run_until_quiescent` | No entity ivar locks held by caller | Returns when all mailboxes are empty or all entities are Faulted |
| `run` | — | Blocks indefinitely; returns only on process signal |
| `register(handle)` | — | Entity included in future scheduling rounds |

---

## Implementation order

1. **`Value::Entity` variant** — add to `value.rs`, stub `Display` / `PartialEq` /
   `Clone`. Tests: entity handles are not equal to each other by default (identity);
   two clones of the same handle are `ptr_eq`.

2. **`EntityState`, `EntityHandle`, `Envelope`** — new `entity.rs` module. Tests:
   `enqueue` adds to mailbox; `pop_message` removes in FIFO order; `EntityStatus`
   transitions correctly.

3. **`_primSpawn` and entity class flag** — wire up `Entity` built-in class; add
   `spawn` and guarded `new` to kernel.smol. Tests: `Counter spawn` returns a
   `Value::Entity`; `Counter new` signals.

4. **`dispatch_entity_send` and `deep_clone_args`** — route entity sends to mailbox.
   Tests: send to entity queues envelope; `Value::Block` send raises; `Value::Object`
   arg is deep-cloned (verify that mutating the original after send does not affect
   the enqueued copy).

5. **`Scheduler::run_until_quiescent`** with sync-self-send detection — cooperative
   turn-based dispatch. Tests (single entity): spawn a counter, send `increment`
   three times, run to quiescent, send `count`, run to quiescent, verify side-effects
   via a shared `Value::Int` result channel (or `Transcript` spy).

6. **Two-entity test** — spawn two entities; one sends messages to the other.
   Verify that neither entity accesses the other's ivars directly and that both
   process their queues correctly.

7. **Error isolation test** — spawn a faulty entity (raises `Signal` in handler)
   alongside a healthy entity; verify that the healthy entity continues processing
   after the faulty one is marked `Faulted`.

8. **Sync-self-send test** — verify that an entity method calling another method
   on `self` dispatches synchronously (no deadlock, correct ivar access).

9. **Coverage check** — `just cov`; no file below 80%.

---

## Open questions

### Scheduler ownership and thread-local approach

Using a thread-local `Scheduler` makes `_primSpawn` registration simple but
constrains the scheduler to a single thread. This is intentional for chunk 21 —
multi-threaded scheduling (e.g., one OS thread per entity, or a work-stealing pool
of OS threads each running a scheduler partition) is deferred. The thread-local
approach does not foreclose any of those options; it just makes them opt-in
migrations rather than requirements now.

### Deep clone cost

Deep-cloning every `Value::Object` on entity send is correct but potentially
expensive for large object graphs. The principled fix — copying only truly mutable
values, sharing immutable ones — requires the Value/Entity type distinction to be
visible at the type system level, which is a later chunk concern. For now, deep
clone is conservative and verifiably correct.

### `Block` across entity boundaries

Blocks are excluded from cross-entity sends in this chunk because a block closes
over its lexical environment, which is owned by the spawning entity's ivar frame.
Sharing that closure with another entity would violate isolation. The correct long-
term model (blocks as capability-bounded functions, with explicit captured-value
lists) is a design-chunk question; the error is raised now to fail-fast rather than
silently share mutable state.

### `initialize` timing

`_primSpawn` queues `initialize` rather than calling it synchronously. This means
an entity's first message may arrive before `initialize` runs if the caller
immediately sends more messages before the scheduler runs. The scheduler processes
the mailbox in FIFO order, so `initialize` always runs first. This is documented
here as a contract of `_primSpawn`.

### Entity class inheritance

Can an entity class subclass another entity class? For MVP: yes, the standard
subclassing machinery works. The `isEntityClass` flag is inherited. Mixed hierarchies
(an entity class subclassing a value class or vice versa) raise `Error signal:`
at class-definition time; the interpreter checks the superclass flag on
`subclass:instanceVariableNames:classVariableNames:indexedSlots:`.

### Relation to triple store (chunk 22 preview)

The triple store crate is complete. Chunk 22 will integrate it as the authoritative
state store for each entity: instead of `ivars: Mutex<Vec<Value>>`, each entity will
own a `TripleStore` instance, and ivar reads/writes will become triple queries and
assertions. This is a contained change to `EntityState` and the dispatch layer;
the scheduler, messaging, and Smol surface stay unchanged. The chunk 21 design
deliberately keeps entity state simple so this substitution is a clean swap.
