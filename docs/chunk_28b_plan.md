### Chunk 28b: Cross-Entity HandlerContext Propagation

## Goal

Make `HandlerContext` non-unwinding handlers work across entity boundaries.
Without this, HC is useful only when all signaling happens in the same evaluator
(i.e., pure-Value-subclass call chains), which is an unacceptable restriction in an
async-everywhere actor system.

Prerequisite: C28a (Cons, frozen ValueObjects, corrected HC pairs).

---

## Background

### The per-evaluator handler_chain problem

Each entity has its own `Evaluator` with its own `handler_chain`. When entity A's HC
body sends a message to entity B, entity B processes it in its own evaluator. If entity
B signals a condition, `signal_condition` runs on entity B's evaluator — which has no
knowledge of entity A's registered handlers.

### Why blocks CAN run cross-entity

A `Block` is a Value. It carries its own captured `env` (the lexical environment at
the point of creation). Running `ev_b.eval_block(handler_block, [condition], dummy_env)`
in entity B's evaluator is correct: the block uses its own captured env, not entity B's.
Variables closed over from entity A are Values in that env and are read directly.

Entity B's evaluator provides only the execution engine, not the data. This is identical
to how blocks work in `on:do:` across different method frames today.

### Why `resume:` works correctly cross-entity

1. Entity B raises a condition. `signal_condition(ev_b, ...)` is on B's call stack.
2. An inherited handler block runs via `ev_b.eval_block(...)`. The signal site is still
   live — B's stack has not unwound.
3. Handler calls `e resume: v` → `_primResume: v` → `Err(EvalError::Resume(v))`.
4. `signal_condition` in B's evaluator catches `Resume(v)` → returns `Ok(v)`.
5. B's execution continues from the signal site with `v`.

The signal site is still on B's stack because non-unwinding handlers run before
`EvalError::Signal` is raised. This is precisely the invariant that HandlerContext is
designed to preserve.

### `retry` semantics — nearest scope

`EvalError::Retry` bubbles up the call stack of the **signaling entity** until it hits
a retry boundary. Boundaries, innermost to outermost:

1. An active `_primHCRun:` body → re-runs that body (already implemented via loop).
2. An active `on:do:` body → re-runs that body (already implemented via loop).
3. The entity message dispatch in `run_entity_loop` → re-runs the entire message handler.

The boundary is determined by the signaling entity's call stack, not by which entity
registered the handler. A handler from an inherited (cross-entity) HC chain that calls
`retry` targets entity B's nearest boundary. This is analogous to how `^` (non-local
return) targets the block's home method regardless of where the block was evaluated.

`retry` is therefore always well-defined and requires no special-casing for cross-entity.

### `return:` semantics at entity dispatch level

`EvalError::ReturnFromBlock(v)` that reaches entity B's `run_entity_loop` (i.e., not
caught by any `_primHCRun:` or `on:do:` in B's stack) is treated as `Ok(v)`: the
`return:` exits the handler context and the message returns with value `v`.

---

## Steps

### S1: `Envelope` carries `inherited_chain`

**`entity.rs`** — `Envelope` struct:

```rust
pub struct Envelope {
    pub selector:        Arc<str>,
    pub args:            Vec<Value>,
    pub reply:           FutureHandle,
    pub inherited_chain: Vec<(Arc<Class>, Arc<Block>)>,
}
```

Default for `inherited_chain` is an empty `Vec`. Existing `send_to_entity` helper
produces envelopes with empty chain (no behaviour change for ordinary sends).

Add a second constructor (or extend the helper):

```rust
pub fn send_to_entity_with_chain(
    data: &EntityData,
    selector: Arc<str>,
    args: Vec<Value>,
    chain: Vec<(Arc<Class>, Arc<Block>)>,
) -> FutureHandle
```

---

### S2: Attach handler_chain on cross-entity send

**`builtins.rs` — `dispatch_entity` and `dispatch_class`** (cross-entity send paths):

When building the envelope for a cross-entity send, snapshot `ev.handler_chain` if
non-empty:

```rust
let chain_snapshot = {
    let chain = ev.handler_chain.borrow();
    if chain.is_empty() { vec![] } else { chain.clone() }
};
let future = send_to_entity_with_chain(d, selector, args, chain_snapshot);
```

If `ev.handler_chain` is empty, the snapshot is a zero-allocation empty vec — no
overhead for ordinary sends. This applies to both entity-to-entity sends and
top-level-to-entity sends.

---

### S3: Push/pop inherited chain in `run_entity_loop`

**`entity.rs` — `run_entity_loop`**: Before dispatching each envelope, push
`envelope.inherited_chain` onto `ev.handler_chain` beneath any handlers the entity
has already registered (i.e., push them first so entity-local handlers take priority):

```
Requires: ev.handler_chain is empty at message dispatch start (per-message invariant).
Ensures: inherited handlers are visible to signal_condition during this dispatch;
  they are popped after dispatch completes regardless of outcome.
```

Implementation:

```rust
let inherited_len = envelope.inherited_chain.len();
{
    let mut chain = ev.handler_chain.borrow_mut();
    for pair in envelope.inherited_chain.iter() {
        chain.push(pair.clone());
    }
}

let result = match &state.kind { ... dispatch ... };

ev.handler_chain.borrow_mut().truncate(
    ev.handler_chain.borrow().len().saturating_sub(inherited_len)
);
// equivalently: truncate to 0 if per-message invariant holds
ev.handler_chain.borrow_mut().clear();
```

The entity-local `handler_chain` entries (from `_primHCRun:` bodies within this message
handler) sit above the inherited entries: they are pushed later and popped by
`_primHCRun:` itself. When the message dispatch returns, only the inherited entries
remain; clearing them restores the empty-chain invariant.

---

### S4: `retry` and `return:` at entity dispatch level

**`entity.rs` — `run_entity_loop`**: Wrap the dispatch call in a retry loop and handle
`ReturnFromBlock`:

```rust
let result = loop {
    let r = match &state.kind {
        InstanceKind::Entity(_) =>
            dispatch_envelope(&ev, handle.clone(), &envelope.selector,
                              envelope.args.clone(), &mut env),
        InstanceKind::ClassEntity(_) =>
            dispatch_class_envelope(&ev, handle.clone(), &envelope.selector,
                                    envelope.args.clone(), &mut env),
        InstanceKind::ValueObject(_) => unreachable!(),
    };
    match r {
        Err(EvalError::Retry) => continue,                    // retry this message
        Err(EvalError::ReturnFromBlock(v)) => break Ok(v),    // return: exits to caller
        other => break other,
    }
};
```

The `Retry` loop at the entity dispatch level is the outermost retry boundary — it
re-runs the entire message handler. There is no retry count limit at this level; if the
condition that caused the retry is not resolved by the handler, the system will loop.
Application code is responsible for ensuring retry termination (e.g., via a restart
that resolves the underlying cause). This is consistent with CL's `retry` restart
semantics, which makes no guarantee of termination.

---

### S5: Tests

**`tests/eval.rs`** — cross-entity HC scenarios:

```
"Non-unwinding resume: across entity boundary"
| hc actor result |
actor := SomeActor new.
hc := HandlerContext new.
hc on: Error do: [:e | e resume: 99].
result := hc run: [actor doSomethingThatSignals].
result = 99.

"return: across entity boundary exits run:"
hc on: Error do: [:e | e return: 'done'].
(hc run: [actor doSomethingThatSignals]) = 'done'.

"retry at entity dispatch level re-runs the message"
"(Use a ValueHolder to count retries; verify handler fires, increments counter, resolves)"

"Nested HC: inner entity handler takes priority over outer inherited handler"
"outer registers 'outer'; inner entity has 'inner'; signal fires 'inner'"

"retry semantics: Retry bubbles to nearest _primHCRun: in signaling entity's stack"
"(entity has its own HC body; retry re-runs that body, not the cross-entity sender's)"
```

Tests require a `SomeActor` helper entity whose methods signal conditions on demand.
Add a minimal test-only entity class in the test module for this purpose.

`cargo test` green. `cargo fmt`. `cargo clippy --all-targets`. Commit C28b.

---

## Ordering note

After C28a and C28b, the original C28 plan (`chunk_28_plan.md`) steps S1–S9 are
either complete or need only test verification (S9 kernel audit). C28 commits after
C28b passes.

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/entity.rs` | `Envelope.inherited_chain`; `send_to_entity_with_chain`; `run_entity_loop` push/pop + retry/return loop |
| `crates/interpreter/src/builtins.rs` | `dispatch_entity`, `dispatch_class`: attach chain snapshot to envelopes |
| `crates/interpreter/src/tests/eval.rs` | Cross-entity HC tests |
