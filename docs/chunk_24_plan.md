# Chunk 24: Class as Singleton Instance Entity

## Goal

Make every class object a running actor entity — a singleton `ClassActor` with its
own mailbox and coroutine. Class-side message sends become async, returning
`Value::Future`. Callers are unaffected because `dispatch` already resolves Futures
transparently (lines 181-184 of `builtins.rs`).

This completes the "everything is an actor" principle: instances are actors, and
now class objects are actors too.

NOTE: not sure I like the `ClassActor` terminology, why not just `Class` or if that
might cause confusion, `ClassInstance` or `ClassEntity`?

---

## New InstanceKind variant

```rust
pub enum InstanceKind {
    Entity(Box<EntityData>),       // user instance actor (unchanged)
    ValueObject(Arc<Object>),      // passive value object (unchanged)
    ClassActor(Box<EntityData>),   // class-side singleton actor (new)
}
```

`ClassObject` is replaced by `ClassActor`. The distinction from `Entity` is that
dispatch uses `class.lookup_class_method` instead of `class.lookup`, and the entity
has no named instance variable slots (class variables live in `Class.class_var_storage`
as before — no `Arc<Object>` needed for ivars).

Because `ClassActor` has no `Arc<Object>` for ivar storage, `ivar_storage()` on
`InstanceHandle` panics for `ClassActor` (as it already does for `ClassObject`).

---

## Entity loop changes (`entity.rs`)

`run_entity_loop` currently calls `dispatch_envelope` which does instance-method
lookup. With `ClassActor`, the loop must use class-method lookup and class-side
builtin fallback.

Add a `dispatch_class_envelope` function alongside `dispatch_envelope`:

```rust
fn dispatch_class_envelope(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: &Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    if let Some((method, def_class)) = handle.0.class.lookup_class_method(selector) {
        return ev.invoke_class_method(handle.clone(), def_class, method, args, env);
    }
    crate::builtins::dispatch_class_builtin(ev, handle, selector, args, env)
}
```

`run_entity_loop` branches on `InstanceKind`:
```rust
InstanceKind::Entity(_) => dispatch_envelope(&ev, handle, selector, args, &mut env),
InstanceKind::ClassActor(_) => dispatch_class_envelope(&ev, handle, selector, args, &mut env),
```

---

## Spawning class actors

Add a new function in `entity.rs`:

```rust
/// Spawn a class-side actor entity for `class`. The coroutine starts and
/// immediately waits for messages; the global env is populated lazily on
/// first message so bootstrap classes can be spawned before kernel loading.
pub fn spawn_class_entity(
    class: Arc<Class>,
    registry: Arc<ClassRegistry>,
) -> InstanceHandle
```

Key difference from `spawn_entity_bare`: there is no `Arc<Object>` for ivars;
`EntityData.data` is a dummy (empty Object, just to satisfy the type). The global
env is built inside the coroutine, not passed in, so the class actor can be spawned
during `ClassRegistry::new()` before the kernel is loaded.

WAIT: why can't we reuse the data mechanism for the class instance and have the class vars be 
accessible as instance vars, instead of a separate, parallel mechanism? They are defined from 
a different source, but otherwise operate the same way, right?

```rust
may::go!(move || {
    let mut env = Env::new();
    registry_clone.populate_env(&mut env);  // runs inside coroutine, after caller finishes
    run_entity_loop(state_clone, registry_clone, env);
});
```

Because `may::go!` spawns but the coroutine blocks immediately on the mailbox wait,
the `populate_env` call runs only when the coroutine first gets scheduled — which
is after the caller has returned and the registry is complete.

Wait, actually this is incorrect: `populate_env` is called before the loop (before
any message wait). There is a race: if the first message arrives before
`populate_env` runs, it would be processed with an empty env. But coroutines are
cooperative (green threads), so the spawning coroutine runs to completion before
the new coroutine gets scheduled. This means the registry is fully initialized
before `populate_env` runs. The race does not exist in cooperative scheduling.

For `may` (stackful coroutines with work-stealing), we should verify this holds
or add explicit synchronization. **Add a note in the implementation to verify
ordering with a test that dispatches to a class immediately after registry creation.**

---

## Bootstrap: `ClassRegistry::new()`

Replace the passive `ClassObject` with spawned `ClassActor` handles:

```rust
let object_state = spawn_class_entity(object_class.clone(), Arc::clone(&registry_arc));
object_class.class_object.set(Arc::downgrade(&object_state.0)).ok();
kernel.register(object_state);
```

`ClassRegistry::new()` must use an `Arc<ClassRegistry>` internally to pass to
`spawn_class_entity`. This requires restructuring `new()` slightly:

```rust
pub fn new() -> Self {
    let reg = Arc::new(ClassRegistryInner { ... });
    // spawn using Arc<ClassRegistryInner>
    Self(reg)
}
```

OR: use a two-phase init — construct the registry with passive ClassObjects first,
then promote them to ClassActors once the Arc is available. This is less invasive:

```rust
pub fn new() -> Self {
    let reg = Self { ... };          // passive ClassObjects
    let arc = Arc::new(reg);
    arc.promote_class_actors();      // convert to ClassActor, set class_object back-refs
    arc
}
```

`promote_class_actors()` would be called from `ClassRegistry::new_arc()` which
returns `Arc<ClassRegistry>`. This is the recommended approach since it avoids
changing the type of `Self`.

---

## `dispatch` routing update (`builtins.rs`)

```rust
Value::Instance(handle) => match &handle.0.kind {
    InstanceKind::ValueObject(_) => dispatch_object_value(ev, handle, selector, args, env),
    InstanceKind::Entity(_) => dispatch_entity(ev, handle, selector, args, env),
    InstanceKind::ClassActor(_) => dispatch_class_entity(ev, handle, selector, args, env),
},
```

Add `dispatch_class_entity` — mirrors `dispatch_entity` but uses the class-side path.
The self-send check (`current_entity_id == entity_id`) still applies: if the class
is sending to itself (e.g., from a class-side method body), dispatch synchronously.

NOTE: this is where unifying the class instance structure could pay off, right? e.g.
class_vars -> instance_vars and class_methids -> instance_methods - maybe we don't 
need parallel dispatch mechanisms?

---

## `handle_subclass` change

When a new class is created via `subclass:`, spawn a `ClassActor` for it immediately:

```rust
let handle = spawn_class_entity(new_class.clone(), ev.registry.clone());
new_class.class_object.set(Arc::downgrade(&handle.0)).ok();
```

---

## Behavioral change: class sends are now async

`MyClass new` now returns `Value::Future(f)` (cross-actor send). Callers that
dispatch to the result get transparent Future resolution for free. But:

- Code that passes `MyClass new` as an ARGUMENT to a builtin that doesn't call
  `force_value` on it may receive a Future instead of an instance. This implies 
  the builtin should be fixed (it is ok to force at point of need, but we should
  always double-check the need).
- Most builtins already call `force_value` on args. Audit dispatch arms for
  any that don't and fix them.

The kernel's `Object class >> new` uses `syncValue` to wait for `initialize`, so
`new` resolves to a fully initialized instance (not a partially initialized Future).
Callers that do `x := MyClass new` and then immediately dispatch to `x` are fine
via transparent Future resolution. Classes can also have `initialize` methods on 
the class side, though many may not use this (Pharo has this support, i think).

---

## ivar_storage() update

```rust
pub fn ivar_storage(&self) -> &Arc<Object> {
    match &self.0.kind {
        InstanceKind::Entity(d) => &d.data,
        InstanceKind::ValueObject(o) => o,
        InstanceKind::ClassActor(_) | InstanceKind::ClassObject => {
            panic!("ClassActor has no instance variable storage")
        }
    }
}
```

---

## Files changed

| File | Change |
|------|--------|
| `entity.rs` | Add `ClassActor(Box<EntityData>)` variant; add `spawn_class_entity`; `run_entity_loop` branches on kind; add `dispatch_class_envelope`; update `ivar_storage` |
| `class_registry.rs` | Two-phase init: build registry, then promote ClassObjects to ClassActors; keep `Arc<ClassRegistry>` available for spawn |
| `builtins.rs` | Replace `ClassObject` arm with `ClassActor`; add `dispatch_class_entity`; audit args for missing `force_value` calls |
| `eval.rs` | Update `InstanceKind::ClassObject` match arms to `ClassActor`; `Debug` impl updated |
| `tests/builtins.rs` | Tests: class send returns Future; class send result is correct value; class-side self-send is synchronous |
| `tests/eval.rs` | Tests: `MyClass new` resolves correctly in compound expressions |

---

## Deferred

- Metaclasses (each class has a metaclass — deferred until reflection chunk).
- `perform:` / `on:do:` class filtering — separate primitives chunk.
- AST span tracking — separate chunk.
- SUnit testing framework — after primitives chunk.

---

## Steps

- **S1** Add `ClassActor(Box<EntityData>)` to `InstanceKind`; update all match arms
         (treat it like `ClassObject` for now — no behavior change yet); cargo test green.
- **S2** Add `spawn_class_entity`; add `dispatch_class_envelope` in entity.rs.
- **S3** Two-phase `ClassRegistry::new()`: build with passive ClassObjects, promote
         to ClassActors after Arc is available. Verify bootstrap test.
- **S4** `dispatch_class_entity` in builtins.rs (mirrors `dispatch_entity` but class path).
         Update routing. Audit args for `force_value` gaps.
- **S5** `handle_subclass` spawns `ClassActor` for new classes.
- **S6** Full test run; fix any breakage from async class sends; commit.
