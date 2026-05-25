# Chunk 22: Instance Unification

## Goal

Collapse the three object-carrying `Value` variants (`Value::Object`,
`Value::ValueObject`, `Value::Class`) into a single `Value::Instance(InstanceHandle)`.
This is a structural refactor with no behavior change: actors stay async, value
objects stay synchronous, class objects stay synchronous.

## New types (entity.rs)

```rust
/// Per-entity isolated data: instance variables, mailbox, status.
/// Extracted from old EntityState; `class` is lifted to InstanceState.
pub struct EntityData {
    pub id: EntityId,
    pub data: Arc<Object>,
    pub mailbox: Mutex<VecDeque<Envelope>>,
    pub mailbox_ready: Condvar,
    pub status: Mutex<EntityStatus>,
}

/// Discriminates instance semantics.
pub enum InstanceKind {
    /// Actor entity — async mailbox dispatch, dedicated coroutine.
    Entity(Box<EntityData>),
    /// Passive value object — sync dispatch, shared across threads.
    ValueObject(Arc<Object>),
    /// Class object — sync dispatch via class-side method table.
    /// Class-as-actor is deferred to a later chunk.
    ClassObject,
}

/// Shared state for every object instance regardless of kind.
pub struct InstanceState {
    pub class: Arc<Class>,
    pub kind: InstanceKind,
}

/// Reference to any object instance.  Replaces EntityHandle, Arc<Object>-in-
/// ValueObject, and Arc<Class>-in-Value.
#[derive(Clone)]
pub struct InstanceHandle(pub Arc<InstanceState>);
```

`Arc<Class>` is preserved as the immutable method-lookup descriptor. It lives in
`InstanceState.class` on every instance. Only `Value::Class(Arc<Class>)` (the env
binding) disappears; the descriptor itself does not.

## Value enum changes

Remove: `Value::Object(EntityHandle)`, `Value::ValueObject(Arc<Object>)`,
`Value::Class(Arc<Class>)`.

Add: `Value::Instance(InstanceHandle)`.

`PartialEq` / `Hash` use `Arc::ptr_eq` on the inner `Arc<InstanceState>`, giving
pointer identity for all instance variants.

## Namespace identity requirement

`populate_env` is called once per entity spawn. If it created a fresh
`InstanceHandle` per call, `Object = Object` would fail (pointer identity mismatch).
Therefore `Namespace.classes` stores canonical `InstanceHandle`s:

```
Namespace.classes: RwLock<HashMap<Arc<str>, InstanceHandle>>
register(InstanceHandle)        // callers supply the handle
lookup_class(&str) -> Option<InstanceHandle>
```

`Arc<Class>` is always accessible as `handle.0.class` for method lookup.

## Dispatch routing

`builtins::dispatch` matches on `Value::Instance(h)` and branches on `h.0.kind`:

| kind | route |
|------|-------|
| `Entity(data)` | existing async entity mailbox path |
| `ValueObject(obj)` | existing sync `dispatch_object_value` path |
| `ClassObject` | existing sync `dispatch_class` path (user methods + builtins) |

`dispatch_class` is kept as an internal fn; it now receives `InstanceHandle` so it
can reconstruct `Value::Instance(handle)` for `self` binding in
`invoke_class_method`.

## invoke_class_method signature change

`invoke_class_method` gains an `InstanceHandle` for the `self` value so `self` in
a class method body evaluates to `Value::Instance(handle)` (the class object
reference) rather than `Value::Class(arc)`. Implicit self-return also returns the
handle.

## Files changed

| File | Change |
|------|--------|
| `entity.rs` | Add EntityData, InstanceKind, InstanceState, InstanceHandle; old EntityState/EntityHandle removed |
| `value.rs` | Replace 3 variants with Value::Instance; PartialEq/Hash/Display/kind() updated |
| `namespace.rs` | .classes field type → InstanceHandle; register/lookup_class signatures updated |
| `class_registry.rs` | Construct InstanceHandles for Object/Value; populate_env emits Value::Instance |
| `builtins.rs` | dispatch routes on InstanceKind; _primNew/_primNew: return Value::Instance; identity comparisons use handle |
| `eval.rs` | All Value::Class/Object/ValueObject match arms → Value::Instance; invoke_class_method carries InstanceHandle; eval_namespace_ref, eval_array_literal, MethodDef handler updated |

## What does NOT change

- `Arc<Class>` type — method lookup, superclass chain, is_value check all unchanged
- `Object` struct — instance-variable store for Entity/ValueObject unchanged
- Dispatch semantics — actors async, value objects sync, class objects sync
- All test outcomes — tests compare Value::Int, Value::Bool, etc.; instance-level
  identity tests updated to new form

## Deferred

`InstanceKind::ClassObject(Box<EntityData>)` — making class objects full actors
with their own coroutine loop and async class-side dispatch. This requires all
class-side message sends to return `Value::Future`, which is a behavioral change
with broad test impact. Scheduled for a dedicated follow-on chunk.

## Steps

- **S1** Add EntityData, InstanceKind, InstanceState, InstanceHandle to entity.rs;
       add spawn_instance (replaces spawn_entity_bare) returning InstanceHandle;
       keep old types until S6.
- **S2** Update namespace.rs (.classes → InstanceHandle) and class_registry.rs
       (canonical handles for Object/Value; populate_env emits Value::Instance).
- **S3** Replace Value::Object/ValueObject/Class with Value::Instance in value.rs.
- **S4** Update builtins.rs (dispatch routing, _primNew/_primNew:, identity ops,
       handle_subclass).
- **S5** Update eval.rs (all match arms, invoke_class_method, eval_namespace_ref,
       eval_array_literal, MethodDef handler).
- **S6** Remove dead EntityHandle/EntityState; cargo fmt + cargo clippy; full test
       run; fix breakage; commit.
