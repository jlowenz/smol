### Chunk 28a: Value Immutability and Cons

## Goal

Establish the correct foundation for immutable Value objects before committing the
condition system (C28). Two concrete deliverables:

1. `_primNewValue:` — the sole authorised construction path for Value subclass instances.
   Mutation is permitted only inside the initialization block; the object is frozen
   permanently when the block returns.
2. `Cons` — a properly named, properly typed immutable linked list node. Replaces the
   Association-as-cons-cell abuse in `HandlerContext` and `Condition`.

These are prerequisites to C28, which currently uses Association-nested-lists (wrong
terminology) and has no enforcement preventing post-construction mutation.

---

## Background

### Why `_primNew + initialize` is insufficient for Value objects

`_primNew` creates a mutable `ValueObject`. The existing `Object class >> new` then sends
`initialize` as a separate message. Between `_primNew` and the end of `initialize`, the
object's ivars may be set freely. But there is no point at which the runtime transitions
the object from "under construction" to "immutable" — so any method, at any time, can
write to any ivar of any ValueObject. This is particularly dangerous in an async system:
a ValueObject shared between coroutines must be guaranteed immutable once published.

The `pvt*` naming convention is not sufficient because:
- It is advisory, not enforced.
- It ties immutability enforcement to method naming, which is fragile.
- It does not compose correctly across async boundaries: a coroutine could hold a
  reference to a ValueObject being "initialized" by another coroutine.

### `_primNewValue:` — initialization block pattern

The construction primitive for Value subclasses takes a 1-arg block (the constructor
body). The instance is passed to the block as its argument. Inside the block, ivar
writes are permitted. When the block returns, the instance is frozen; any subsequent
ivar write raises `TypeError` regardless of method name or call site.

```smalltalk
Cons class >> with: head rest: tail [
    ^self _primNewValue: [:c |
        c setHead: head.
        c setTail: tail
    ]
]
```

This makes the authorization boundary explicit and structural, not nominal. The `pvt*`
prefix convention is no longer required for Value subclasses (but may be retained for
documentation clarity). `Object class >> new` is unchanged — entity classes (`Object
subclass:`) continue using `_primNew initialize` since entities are mutable by design.

### Association naming is correct; cons-cell nesting is not

`Association key: k value: v` correctly describes a key-value pairing.
`cls -> blk` as a handler pair (class associated with its handler block) is semantically
appropriate.

The abuse was nesting Associations to carry list structure:
`Association key: cls value: (Association key: blk value: rest)` — the outer Association
is simultaneously a "pair" and a "list node." These are distinct concepts.

`Cons` carries exactly two things with no imposed semantic: `head` (the data at this
position) and `tail` (the rest of the list, or nil for the empty case). `nil` is the
empty list. No singleton required.

---

## Steps

### S1: `Object.frozen` field and evaluator enforcement

**`object.rs`**: Add `frozen: AtomicBool` to `Object`. Default: `false`.
Add `fn freeze(&self)` that sets `frozen` to `true` (Ordering::Release).
`set_slot` is unchanged (enforcement is at the evaluator level, not the data layer, so
error reporting goes through the normal `EvalError` path).

**`eval.rs` — `assign_var`**: When the ivar write path is taken for a `ValueObject`,
check `obj.frozen.load(Ordering::Acquire)`. If true, return:

```rust
Err(EvalError::TypeError(format!(
    "cannot assign to ivar '{}': Value object is frozen", name
)))
```

No method-name inspection required. The frozen flag is the sole gate.

**`entity.rs` — `SelfBinding`**: No change needed. The frozen check happens on the
`Object`, not on the binding.

Tests: constructing a ValueObject and attempting ivar write after freeze raises TypeError;
write inside `_primNewValue:` block succeeds.

`cargo test` green.

---

### S2: `_primNewValue:` primitive

**`builtins.rs` — `dispatch_class_builtin`**: Add `"_primNewValue:"` case:

```
Requires: args[0] is a 1-arg Block; receiver class is a Value subclass
  (inherits_is_value() == true).
Ensures: creates a new ValueObject, evaluates the block with the instance as
  argument (mutation permitted during block), freezes the object on block return,
  returns the frozen instance. Block return value is ignored — the instance is
  always the result.
  If the class does not inherit is_value, raises TypeError.
```

Implementation:
1. Extract block from args.
2. Assert `class.inherits_is_value()`; error if not.
3. `let obj = Arc::new(Object::new(class.clone()))` — mutable by default (frozen=false).
4. `let inst = InstanceHandle(Arc::new(InstanceState { class, kind: InstanceKind::ValueObject(obj.clone()) }))`.
5. `ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?`.
6. `obj.freeze()`.
7. `Ok(Value::Instance(inst))`.

For indexed Value subclasses, add `"_primNewValueIndexed:init:"` taking `n: Int` and a
1-arg block; identical except `Object::new_with_indexed(class, n as usize)`.

**`kernel.smol` — `Value class >> new`**:

```smalltalk
Value class >> new [
    ^self _primNewValue: [:inst | inst initialize]
]
Value class >> new: n [
    ^self _primNewValueIndexed: n init: [:inst | inst initialize]
]
```

`Object class >> new` is unchanged (entity path).

Tests: `(Condition withMessage: 'x') message` = `'x'`; attempt to set ivar after
construction raises TypeError.

`cargo test` green.

---

### S3: `Cons` value class

**`kernel.smol`**: Define `Cons` as `Value subclass:`. Construction via `_primNewValue:`.
`nil` is the empty list; `isNil` is the emptiness check.

```smalltalk
Value subclass: #Cons
    instanceVariableNames: 'head tail'
    classVariableNames: ''
    indexedSlots: #none.

Cons class >> with: anObject rest: aList [
    ^self _primNewValue: [:c |
        c setHead: anObject.
        c setTail: aList
    ]
]

Cons >> setHead: h  [ head := h ]
Cons >> setTail: t  [ tail := t ]
Cons >> head        [ ^head ]
Cons >> tail        [ ^tail ]
Cons >> isEmpty     [ ^false ]
Cons >> addFirst: x [ ^Cons with: x rest: self ]

Cons >> do: aBlock [
    aBlock value: head.
    tail isNil ifFalse: [tail do: aBlock]
]

Cons >> size [
    ^1 + (tail isNil ifTrue: [0] ifFalse: [tail size])
]
```

`Object >> isEmpty [ ^false ]` (already implied; add if not present).
`nil` responds to `isEmpty` via `UndefinedObject >> isEmpty [ ^true ]` or via the
existing nil dispatch in builtins (`"isEmpty"` → `Ok(Value::Bool(true))`).

Tests: `Cons with: 1 rest: nil` has head=1, tail=nil, isEmpty=false; nil isEmpty=true;
`do:` visits all elements; `addFirst:` returns new Cons; frozen after construction.

`cargo test` green.

---

### S4: Migrate `Condition.restarts` to `Cons`

**`kernel.smol`**: `restarts` ivar is now a `Cons`-of-Associations (or nil).
`addRestart:block:` becomes a functional update returning a **new** Condition.

```smalltalk
Condition >> addRestart: name block: aBlock [
    ^self class _primNewValue: [:c |
        c setMessage: message.
        c setRestarts: (Cons with: (name -> aBlock) rest: restarts)
    ]
]
```

`pvtFindRestart:in:` (currently Smol-recursive) stays structurally similar but walks
`Cons` nodes: `node head key` = restart name, `node head value` = block, `node tail` =
rest.

```smalltalk
Condition >> pvtFindRestart: name in: node [
    node isNil ifTrue: [^nil].
    node head key = name ifTrue: [^node head value].
    ^self pvtFindRestart: name in: node tail
]
```

**Callers**: `e addRestart: #foo block: [...]` now returns a NEW condition. Signal
patterns must be updated to chain: `e := e addRestart: #foo block: [...]` before `e signal`.
Existing tests for restarts updated accordingly.

Tests: restart lookup works; `addRestart:block:` returns new Condition; original
unchanged; restarts survive through signal dispatch.

`cargo test` green.

---

### S5: Migrate `HandlerContext.pairs` to `Cons`

**`kernel.smol`**: `pairs` ivar is a `Cons`-of-Associations (or nil). Construction via
functional prepend — no OC, no cross-entity calls.

```smalltalk
HandlerContext >> initialize       [ pairs := nil ]
HandlerContext >> on: cls do: blk  [
    pairs := Cons with: (cls -> blk) rest: pairs.
    ^self
]
HandlerContext >> on: anArray run: body [
    anArray do: [:assoc | self on: assoc key do: assoc value].
    ^self run: body
]
```

**`builtins.rs` — `_primHCRun:`**: Walk `Cons` list. Each node's `head` is an
Association; `head key` is the class object (`InstanceHandle` with ClassEntity kind →
`h.0.class`); `head value` is the handler block. `node tail` is rest.

```
Requires: self's `pairs` ivar is a Cons-of-Association or nil.
Ensures: handler pairs are extracted (oldest first after reverse), pushed onto
  ev.handler_chain, body is run. Chain is truncated on all exit paths.
```

Implementation: walk `Cons` nodes via `head`/`tail` slots (read from the Association's
ValueObject storage); collect `(Arc<Class>, Arc<Block>)` pairs; reverse; push to chain.
This is the same walking code already written in the current branch, updated to read
`head`/`tail` slot names instead of `key`/`value` at the outer level.

Tests: all existing C28 HC tests pass.

`cargo test` green. `cargo fmt`. `cargo clippy --all-targets`. Commit C28a.

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/object.rs` | `frozen: AtomicBool`; `freeze()`; `new_mutable` alias (or rename) |
| `crates/interpreter/src/eval.rs` | `assign_var`: frozen check for ValueObject ivar writes |
| `crates/interpreter/src/builtins.rs` | `_primNewValue:`, `_primNewValueIndexed:init:`; `_primHCRun:` walks Cons |
| `crates/interpreter/src/kernel.smol` | `Value class >> new`/`new:`; `Cons` class; `Condition` restarts → Cons; `HandlerContext` pairs → Cons |
| `crates/interpreter/src/tests/eval.rs` | Tests for all of the above |
