### Chunk 29.6: TCO + Inline Block Dispatch for Pure Smol Loops

## Goal

Enable pure Smol recursive methods (e.g. `whileTrue:`) that run without O(N) Rust stack
or heap frame accumulation. This chunk inserts between C29.5 and C30 S2 because it is
a prerequisite for any serious Smol kernel work that relies on recursion.

Deliverables:

- `dispatch_to_stack` inlines `Block >> value` family and `Bool >> ifTrue:` family by
  pushing `CallFrame`s onto the running stack instead of creating nested `step_loop` calls.
- `lookup_user_method` handles `Value::Block` and `Value::Int` via their registered
  primitive classes; `push_method_call_frame` is called instead of `invoke_primitive_method`
  (which otherwise creates a nested `step_loop`).
- Two TCO optimisations in `dispatch_to_stack` before `push_method_call_frame`:
  - **Block TCO**: drain empty `is_method=false` frames from the stack top (safe —
    block frames return last `acc`, so the tail callee's result IS the block's result).
  - **ReturnUnwind TCO**: drain a `ReturnUnwind(D)` + empty `is_method=true` frame at
    depth D from the stack top (safe — the `^`-return would have immediately overridden
    the method's implicit `self` return anyway).
- `kernel.smol` gains proper recursive Smol implementations of `whileTrue:`,
  `whileFalse:`, `whileTrue`, `whileFalse`, `timesRepeat:`; the `_prim*` builtins remain
  as fallback-only paths (reachable only if no user method is defined).
- `syncValue`, `on:do:`, and `ensure:` are **not** inlined — they require
  `eval_block_sync` to force `Future`s between statements so that condition signals
  propagate correctly. Their semantics are fully preserved.
- `cargo test` green; `cargo fmt`; `cargo clippy --all-targets` clean.

---

## Background

### Why builtins created nested `step_loop` calls (and why that breaks recursion)

`dispatch_to_stack` dispatches in two ways:
1. User method → `push_method_call_frame` → frame pushed onto **current** stack (good).
2. Builtin → `builtins::dispatch` → e.g. `ifTrue:` → `ev.eval_block()` → **new `step_loop`** (bad).

A new `step_loop` is a new Rust call frame. A recursive Smol `whileTrue:` that goes
through `self value` (block eval) and `ifTrue:` each create one nested `step_loop` per
iteration. 1000 iterations = 2000 Rust stack frames = stack overflow.

The fix is to handle the common block-evaluation selectors directly in
`dispatch_to_stack`, bypassing `builtins::dispatch` and pushing `CallFrame`s instead.

### Why `lookup_user_method` must cover Block and Int

`dispatch_block` (builtins) checks user methods then calls `invoke_primitive_method`.
After C30 S1, `invoke_primitive_method` calls `ev.eval_program()` → `step_loop` → new
nested `step_loop`. This means user-defined `Block >> whileTrue:` is treated like any
other primitive dispatch and creates nested evaluations.

Moving the Block/Int user-method lookup upstream to `lookup_user_method` means
`dispatch_to_stack` finds them and calls `push_method_call_frame`, which pushes onto the
**current** stack. No nesting.

### Why TCO requires two distinct patterns

**Block TCO (drain empty block frames)**

A block frame in tail position means: the block's last expression IS the value the block
returns. Draining an empty `is_method=false` `CallFrame` before pushing the callee is
safe because:
- The (empty) block frame would immediately set `acc = last_acc` and return — i.e., it
  passes `acc` through unchanged.
- After draining, the callee's result becomes `acc`, which IS what the block frame would
  have returned. No semantic difference.

**ReturnUnwind TCO (drain `^` return + empty method frame)**

Pattern: top-of-stack is `Cont::ReturnUnwind { target_depth: D }` with an empty
`is_method=true` `CallFrame(depth: D)` immediately below it.

Meaning: the method at depth D was going to `^`-return some value. The `ReturnUnwind`
would have: (a) set `acc = that_value`, then (b) drained the method frame (pop_self,
pop_env). The callee's result replaces `that_value` exactly — the frame would have been
drained either way. Safe to drain proactively.

**Together**: for the canonical recursive `whileTrue:` implementation shown below, each
iteration leaves zero extra frames on the stack.

### `syncValue` / `on:do:` / `ensure:` — why they stay in builtins

`eval_block_sync` forces each statement's `Future` result before executing the next
statement. This is what makes signal propagation through `on:do:` correct: if a body
statement produces a `Future` that resolves to a signal, the sync loop detects it before
the next statement runs.

Inlining these as plain `CallFrame`s (without inter-statement force) would break
`on:do:` for async bodies. They must remain in `builtins::dispatch` calling
`eval_block_sync`. This is a hard semantic invariant.

---

## Steps

### S1: Update `lookup_user_method` to cover Block and Int

In `builtins.rs`, `lookup_user_method`:

```
Requires: `ev` has Block and Int registered as primitive classes (done in C29.5).
Ensures: for Value::Block and Value::Int, looks up the selector in the Block / Int
  class respectively and returns UserMethodFound::Primitive if found. Returns None
  for all builtins (value, ifTrue:, etc.) that are not user-defined overrides.
```

New arms (before the catch-all `_ => None`):

```rust
Value::Block(blk) => {
    let blk_cls = ev.registry.kernel().lookup_class("Block")?;
    let (method, def_class) = blk_cls.0.class.lookup(selector)?;
    Some(UserMethodFound::Primitive {
        self_value: Value::Block(blk.clone()),
        method,
        def_class,
    })
}
Value::Int(n) => {
    let int_cls = ev.registry.kernel().lookup_class("Int")?;
    let (method, def_class) = int_cls.0.class.lookup(selector)?;
    Some(UserMethodFound::Primitive {
        self_value: Value::Int(*n),
        method,
        def_class,
    })
}
```

After this change, remove the user-method-lookup preamble from `dispatch_block` and
`dispatch_int` (those blocks that called `ev.invoke_primitive_method`). They are no
longer reachable from `dispatch_to_stack` for user-defined methods.

**Verify**: existing tests for `Block >> whileTrue:` delegation (C29.5) still pass —
the user method is now found by `lookup_user_method` → `push_method_call_frame` rather
than `dispatch_block` → `invoke_primitive_method`. Semantics identical.

---

### S2: Add `push_block_frame` to `eval_iterative.rs`

```rust
/// Push an async-semantics block evaluation frame onto the iterative stack.
///
/// Requires: `args.len() == block.params.len()`.
/// Ensures: equivalent to `Evaluator::eval_block` but reuses the caller's continuation
///   stack; no nested `step_loop` call is made. Suitable for `value`, `value:`,
///   `ifTrue:`, `ifFalse:` and other block-evaluation sites where a `Future` block
///   result is acceptable (caller will force if needed). NOT suitable for
///   `syncValue`, `on:do:`, or `ensure:`, which require inter-statement future forcing.
pub(crate) fn push_block_frame(
    ev: &Evaluator,
    block: &Arc<crate::value::Block>,
    args: Vec<Value>,
    stack: &mut Vec<Cont>,
) -> Result<(), EvalError> {
    use crate::eval::EvalError;
    if args.len() != block.params.len() {
        return Err(EvalError::ArityMismatch {
            expected: block.params.len(),
            got: args.len(),
        });
    }
    let mut block_env = block.env.clone();
    block_env.push_frame();
    for name in &block.locals {
        block_env.define(name.clone(), Value::Nil);
    }
    for (name, val) in block.params.iter().zip(args) {
        block_env.define(name.clone(), val);
    }
    for stmt in &block.body {
        if let crate::ast::Stmt::LocalDecl(names) = stmt {
            for name in names { block_env.define(name.clone(), Value::Nil); }
        }
    }
    let saved_home = ev.current_block_home.get();
    ev.current_block_home.set(Some(block.home_depth));
    stack.push(Cont::CallFrame {
        self_binding: None,
        depth: 0,
        pending: block.body.iter().cloned().collect(),
        env: block_env,
        implicit_return: Value::Nil,
        is_method: false,
        pops_env: true,
        saved_block_home: saved_home,
        frame_block_home: Some(block.home_depth),
        defining_class: None,
    });
    Ok(())
}
```

Key differences from `eval_block`:
- Does not call `step_loop` — pushes onto the **caller's** stack.
- Sets `ev.current_block_home` before pushing (the frame's `saved_block_home` will
  restore it when the frame is popped, same as `eval_block`).

---

### S3: Inline block and bool dispatch in `dispatch_to_stack`

Modify `dispatch_to_stack` in `eval_iterative.rs`. New structure:

```
Requires: receiver, selector, args are fully evaluated.
Ensures:
  (1) User-defined methods for Block/Int/Nil/ValueObject found via lookup_user_method
      → push_method_call_frame (TCO draining in S4 applies here).
  (2) Block value/value:/value:value:/value:value:value: → push_block_frame (no nested
      step_loop; async-ok semantics).
  (3) Bool ifTrue:/ifFalse:/ifTrue:ifFalse:/ifFalse:ifTrue: → push_block_frame for the
      selected branch, or set acc=Value::Nil for the unchosen branch.
  (4) All other selectors → builtins::dispatch (includes syncValue, on:do:, ensure:,
      _primWhileTrue:, etc. — their sync semantics are preserved intact).
```

Implementation details:

**Block value family (step 2)**:

```rust
if let Value::Block(ref blk) = receiver {
    let arity: Option<usize> = match selector.as_ref() {
        "value"            => Some(0),
        "value:"           => Some(1),
        "value:value:"     => Some(2),
        "value:value:value:" => Some(3),
        _ => None,
    };
    if let Some(expected) = arity {
        if args.len() != expected {
            return Err(EvalError::ArityMismatch { expected, got: args.len() });
        }
        push_block_frame(ev, blk, args, stack)?;
        return Ok(());
    }
}
```

**Bool dispatch (step 3)**:

```rust
if let Value::Bool(b) = receiver {
    match selector.as_ref() {
        "ifTrue:" => {
            expect_arity(&selector, 1, args.len())?;
            let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
            if b { push_block_frame(ev, &blk, vec![], stack)?; }
            else { *acc = Value::Nil; }
            return Ok(());
        }
        "ifFalse:" => { /* symmetric */ }
        "ifTrue:ifFalse:" => {
            expect_arity(&selector, 2, args.len())?;
            let mut iter = args.into_iter();
            let t_blk = as_block(iter.next().unwrap(), &selector)?;
            let f_blk = as_block(iter.next().unwrap(), &selector)?;
            if b { push_block_frame(ev, &t_blk, vec![], stack)?; }
            else { push_block_frame(ev, &f_blk, vec![], stack)?; }
            return Ok(());
        }
        "ifFalse:ifTrue:" => { /* symmetric */ }
        _ => {}
    }
}
```

`and:` and `or:` are NOT inlined in this chunk. They require type-checking the block's
return value (must be Bool). Left in builtins for correctness.

`as_block` can be copied/imported from `builtins.rs` or inlined. It calls `force_value`
then checks for `Value::Block`. This future-forcing is correct and necessary: a block
argument that is a Future must be resolved before it can be pushed as a frame.

---

### S4: TCO — drain tail frames before `push_method_call_frame`

In `dispatch_to_stack`, in the `if let Some(found) = lookup_user_method(...)` arm,
**before** calling `push_method_call_frame`, call a single combined drain:

```rust
drain_tail_frames(ev, stack);
push_method_call_frame(ev, found, args, env, stack)?;
```

**Why a single combined drain matters**

Two natural Smol styles produce different stack shapes at the tail-call dispatch point:

*Split form* (`^` in method body directly):
```smalltalk
Block >> whileTrue: aBlock [
    self value ifFalse: [ ^nil ].
    aBlock value.
    ^self whileTrue: aBlock      "← ^ is in the METHOD body"
]
```
Stack at dispatch: `[..., whileTrue-method(D, empty), ReturnUnwind(D)]`
→ Simple ReturnUnwind(D) + method(D) match. ✓

*Combined form* (`^` inside an ifTrue: block):
```smalltalk
Block >> whileTrue: aBlock [
    self value
        ifTrue: [ aBlock value. ^self whileTrue: aBlock ]
        ifFalse: [ ^nil ]
]
```
The `^` is inside the true-block. Stack at dispatch:
`[..., whileTrue-method(D, empty), true-block(depth=0, empty), ReturnUnwind(D)]`
→ ReturnUnwind(D) does NOT sit directly above a method frame at depth D. A naïve
two-entry check fails and no drain happens — O(N) frame accumulation results.

The extended drain handles both by scanning past any empty block frames between the
`ReturnUnwind` and the target method frame. Empty block frames are safe to skip because
they pass `acc` through unchanged; they contribute nothing to the result.

```rust
/// Drain the tail-call frame pattern from the stack top before pushing a new callee.
///
/// Requires: none (safe to call on any stack state).
/// Ensures:
///   (1) All empty block frames (is_method=false, pending empty) at the top are drained.
///   (2) If the top is then ReturnUnwind(D) and, scanning past any further empty block
///       frames, the next is an empty method frame at depth D, the entire sequence
///       (ReturnUnwind + intervening block frames + method frame) is drained.
///   (3) The process repeats until no further pattern applies.
///   Safe: empty block frames return acc unchanged; ReturnUnwind(D) would have overridden
///   the method's implicit self-return with the same value the new callee will produce.
fn drain_tail_frames(ev: &Evaluator, stack: &mut Vec<Cont>) {
    loop {
        // Phase 1: drain leading empty block frames.
        while let Some(Cont::CallFrame { pending, is_method: false, .. }) = stack.last()
            && pending.is_empty()
        {
            if let Some(Cont::CallFrame { saved_block_home, pops_env, mut env, .. }) = stack.pop() {
                ev.current_block_home.set(saved_block_home);
                if pops_env { env.pop_frame(); }
            }
        }

        // Phase 2: look for ReturnUnwind(D) followed (past empty block frames) by
        // an empty method frame at depth D.
        let target = match stack.last() {
            Some(Cont::ReturnUnwind { target_depth }) => *target_depth,
            _ => break, // no ReturnUnwind — nothing more to drain
        };

        // Scan downward past empty block frames to find the matching method frame.
        let mut scan = stack.len().wrapping_sub(2); // index below the ReturnUnwind
        let mut intermediate = 0usize;
        let found_method = loop {
            match stack.get(scan) {
                Some(Cont::CallFrame { is_method: false, pending, .. }) if pending.is_empty() => {
                    intermediate += 1;
                    scan = scan.wrapping_sub(1);
                }
                Some(Cont::CallFrame { is_method: true, depth, pending, .. })
                    if *depth == target && pending.is_empty() =>
                {
                    break true;
                }
                _ => break false,
            }
        };

        if !found_method {
            break;
        }

        // Drain: ReturnUnwind, then intermediate block frames, then the method frame.
        stack.pop(); // ReturnUnwind
        for _ in 0..intermediate {
            if let Some(Cont::CallFrame { saved_block_home, pops_env, mut env, .. }) = stack.pop() {
                ev.current_block_home.set(saved_block_home);
                if pops_env { env.pop_frame(); }
            }
        }
        if let Some(Cont::CallFrame {
            saved_block_home, pops_env, mut env, is_method: true, ..
        }) = stack.pop()
        {
            ev.pop_self();
            ev.current_block_home.set(saved_block_home);
            if pops_env { env.pop_frame(); }
        }
        // Loop: check again — there may be another layer above.
    }
}
```

---

### S5: Update `kernel.smol` with pure recursive Smol loop methods

Replace the C29.5 delegation bodies with recursive implementations for `whileTrue:`
and `whileFalse:`. The `_prim*` builtins remain in `builtins.rs` as fallbacks.

```smalltalk
"Loop — pure Smol implementations enabled by inline block dispatch + TCO."

Block >> whileTrue: aBlock [
    self value ifFalse: [ ^nil ].
    aBlock value.
    ^self whileTrue: aBlock
]

Block >> whileFalse: aBlock [
    self value ifTrue: [ ^nil ].
    aBlock value.
    ^self whileFalse: aBlock
]

Block >> whileTrue [
    self value ifFalse: [ ^nil ].
    ^self whileTrue
]

Block >> whileFalse [
    self value ifTrue: [ ^nil ].
    ^self whileFalse
]

Int >> timesRepeat: aBlock [
    self > 0 ifFalse: [ ^nil ].
    aBlock value.
    ^(self - 1) timesRepeat: aBlock
]
```

`timesRepeat:` avoids `NonLocalBlockAssignment` by decrementing the receiver on each
recursive call rather than mutating a counter variable. `self - 1` produces a new Int;
each recursive call uses a different receiver. No assignment to outer locals required.

**Why `^nil` as base case, not fall-through**: Without an explicit `^nil`, the method
would return `self` (implicit Smalltalk self-return). Standard Smalltalk `whileTrue:`
returns `nil`. The explicit `^nil` is required for correct return semantics.

**Why `^self whileTrue: aBlock` rather than fall-through**: The explicit `^` is what
triggers `ReturnUnwind TCO`. Without `^`, the method would return `self` at the end
(after the recursive call completes), not the recursive call's result. With `^`, the
`ReturnUnwind(D) + empty-method-frame(D)` pattern forms, which `drain_tail_return_frame`
eliminates in O(1).

**Body result is fire-and-forget (no implicit await)**: `aBlock value` may return a
`Value::Future` when the body sends to an entity. The loop does NOT await this future.
This is intentional — the async-everywhere principle is that concurrency is explicit,
not implicit. The body's future is discarded; the next iteration starts immediately.

**Causal safety without await**: In the actor model, messages from the same coroutine
arrive at the same entity in FIFO order. For the common case
`[ e value < 10 ] whileTrue: [ e increment ]`, the sequence sent from this coroutine is
`value?` → `increment` → `value?` → … — the entity processes them in that order.
When the second `value?` is handled, `increment` has already been processed. Causal
ordering is preserved by the mailbox, not by explicit await. For cross-entity cases
(body affects entity A, condition reads entity B), there is no dependency and no await
is required.

**When the caller needs to synchronize**: use explicit `await` in the body block:
`[ cond ] whileTrue: [ someEntitySend await ]`, or `[ cond ] whileTrue: [ blk syncValue ]`.
No new loop primitives are needed; `await` and `syncValue` are the explicit
synchronization idioms.

**Why `self value ifFalse:` does not need explicit `await`**: When `self value` returns
a `Value::Future`, the `ifFalse:` dispatch falls through to `builtins::dispatch` (since
the inline path checks `if let Value::Bool(b) = receiver` and doesn't match a Future).
The builtin `dispatch` forces Futures transparently before re-dispatching to `dispatch_bool`.
The condition is always forced before the branch decision — this is unavoidable since
the Bool value is required to decide which branch to take.

**`whileTrue` / `whileFalse` (no-arg)**: no body block, so no `await` needed. These
evaluate `self` as the combined condition-and-body block.

**`timesRepeat:`**: uses `whileTrue:` (which is itself Smol-recursive). Requires
`NonLocalBlockAssignment` — but `i` is a local of `timesRepeat:`, not an outer closure
variable, so assignment is valid here.

---

### S6: Verify semantics of `syncValue`, `on:do:`, `ensure:` are unchanged

These three remain in `builtins.rs` calling `eval_block_sync`. The inlining in S3 only
intercepts `value`, `value:`, `value:value:`, `value:value:value:`, `ifTrue:`,
`ifFalse:`, `ifTrue:ifFalse:`, `ifFalse:ifTrue:`. All other selectors fall through to
`builtins::dispatch`.

Required tests (add to `tests/builtins.rs` or `tests/eval.rs`):

```
- on_do_catches_signal_from_future:
    A block that sends to an entity (producing a Future that resolves to a signal)
    is wrapped in on:do:. Signal must be caught. Verifies eval_block_sync still forces
    the Future before the next statement.

- ensure_runs_after_signal:
    [Error signal: 'x'] ensure: [cleanup-ran := true].
    Cleanup runs even when body signals.

- sync_value_forces_future:
    | v | v := [ anEntitySend ] syncValue. v isNil not.
    (If entity send returns a Future, syncValue forces it before returning.)
```

Also regression-test all existing C29.5 loop tests pass with the new Smol implementations.

---

### S7: Non-local return (`^`) from within inline block frames

The existing `Cont::ReturnUnwind` mechanism handles this correctly. When an inline block
frame is on the stack and evaluates `^expr`, `dispatch_stmt` pushes
`Cont::ReturnUnwind { target_depth }` (using `current_block_home` which was set by
`push_block_frame` to `block.home_depth`). The unwind logic is identical to the
`eval_block` path. No changes needed here; only a regression test:

```
- nonlocal_return_through_inline_iftrue:
    A method contains `cond ifTrue: [ ^42 ]. ^0`. When cond is true, method returns 42.
    This tests that ^-return unwinds correctly through an inlined ifTrue: block frame.
```

---

### S8: Tests for O(1) stack behaviour

Add to `tests/eval.rs`:

```rust
#[test]
fn while_true_1000_iterations() {
    // Uses the Smol recursive whileTrue:. Would overflow without TCO.
    let v = eval("
        | c |
        c := OrderedCollection new.
        [ c size < 1000 ] whileTrue: [ c add: 1 ].
        c size
    ");
    assert_eq!(v, Value::Int(1000));
}

#[test]
fn times_repeat_1000() {
    let v = eval("
        | c |
        c := OrderedCollection new.
        1000 timesRepeat: [ c add: 1 ].
        c size
    ");
    assert_eq!(v, Value::Int(1000));
}
```

Also a deeper recursion test using a user-defined recursive method:

```rust
#[test]
fn user_recursive_count_down() {
    // Verifies block TCO + ReturnUnwind TCO together.
    let src = "
        Value subclass: #Counter instanceVariableNames: ''.
        Counter >> countFrom: n [
            n = 0 ifTrue: [ ^0 ].
            ^self countFrom: n - 1
        ].
        Counter new countFrom: 1000
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(0));
}
```

---

### S9: fmt, clippy, commit

- `cargo fmt`
- `cargo clippy --all-targets` clean
- All existing tests pass
- Commit: "C29.6: inline block dispatch + TCO → pure Smol recursive loops"

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/builtins.rs` | `lookup_user_method`: add Block/Int arms; remove user-method preamble from `dispatch_block`/`dispatch_int` |
| `crates/interpreter/src/eval_iterative.rs` | `push_block_frame`; updated `dispatch_to_stack` with inline dispatch + TCO drain helpers |
| `crates/interpreter/src/kernel.smol` | Loop methods: recursive Smol replacing delegation stubs |
| `crates/interpreter/src/tests/builtins.rs` | Loop correctness tests (1000 iterations) |
| `crates/interpreter/src/tests/eval.rs` | Recursive user method test; syncValue/on:do:/ensure: regression tests |

---

## Invariants preserved

| Invariant | Mechanism |
|---|---|
| `syncValue` forces futures between statements | Not inlined; goes to `builtins::dispatch` → `eval_block_sync` |
| `on:do:` body forces futures for signal propagation | Not inlined; `eval_block_sync` in builtin `on:do:` arm |
| `ensure:` body forces futures | Not inlined; `eval_block_sync` in builtin `ensure:` arm |
| `^`-return from inline block frame unwinds correctly | `push_block_frame` sets `current_block_home`; `dispatch_stmt` for `Stmt::Return` produces `ReturnUnwind` with correct target depth; existing unwind logic unchanged |
| User-defined `Block >> value` override takes priority | `lookup_user_method` checks Block class methods FIRST; inline block dispatch only fires when no user method is found |
| `Bool >> and:` / `or:` type-check block return value | Not inlined; remain in `builtins::dispatch_bool` where the Bool type-check is enforced |

---

## Deferred

- **`and:` / `or:` inlining** — requires propagating a "must be Bool" constraint onto
  the block frame result. Possible via a new `Cont::AssertBool` variant. Not needed for
  loops; deferred.
- **Collection iteration inlining** (`do:`, `collect:`, etc.) — these have Rust loops
  calling `eval_block`; converting them requires either Smol recursion or explicit
  iterative Cont variants. Deferred to a dedicated chunk.
- **Super-send inlining** — currently uses the recursive path. Deferred; low priority.
- **`and:` / `or:` Smol migration** — once inlining is supported or collection
  methods are in Smol, these become natural follow-ons.
