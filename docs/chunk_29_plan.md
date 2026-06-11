### Chunk 29: Iterative Evaluator

## Goal

Replace the recursive tree-walking evaluator with an iterative one that keeps the
Smol execution stack on the heap rather than on the `may` coroutine's Rust call stack.
The recursive evaluator is retained unchanged as a reference implementation and test
oracle.

**Why this matters:** each Smol method call currently consumes ~1.5–2 KB of the `may`
coroutine's Rust stack. At 128 KB per coroutine, that caps practical recursion depth
at ~64–80 levels and makes each entity heavyweight. After this chunk, the dominant
stack cost is per *block-consuming builtin nesting level* (~0.5–1 KB), not per
user-defined method call. The next chunk (C30) then reduces `COROUTINE_STACK_SIZE`
to ~16–32 KB.

Deliverables:

- `crates/interpreter/src/eval_iterative.rs` — new iterative eval module
- `Cont` enum representing expression- and method-frame-level continuations
- Iterative `eval_expr`, `eval_block`, `invoke_method`, `invoke_class_method`,
  `eval_program` on a new `IEvaluator` type
- `builtins.rs` unchanged: builtins still call `eval_block` / `invoke_method`
  synchronously; those entry points now run a step loop internally
- Differential tests: every existing eval test exercised on both evaluators
- `cargo test` green; `cargo fmt`; `cargo clippy --all-targets` clean

---

## Background

### Why the hybrid approach

A full CPS transform of `builtins.rs` (making every `eval_block` call site a
continuation) would reduce the Rust stack to absolute minimum, but would require
rewriting ~40 call sites in `builtins.rs`. The hybrid approach here provides the
primary win with minimal disruption to builtins:

- Within any single method or block body, all message sends — including user-defined
  method calls — push a `CallFrame` onto a heap `Vec<Cont>` rather than adding Rust
  stack frames. This is where most of the depth comes from in practice.
- The only remaining source of Rust stack growth is `eval_block` / `invoke_method`
  called **from builtins** (e.g., `ifTrue:`, `do:`, `on:do:`). Each such call adds
  ~3–4 Rust frames (~0.5–1 KB) instead of the previous 6–8 frames (~1.5–2 KB).

Typical worst case after this chunk: `on:do:` containing `whileTrue:` containing
`do:` = 3 nested builtin block calls = ~2–3 KB Rust stack, regardless of how much
user-defined method depth exists within those blocks.

### Stack anatomy (before → after)

```
Before (per user-defined Smol method call on Rust stack):
  invoke_method        ~200 B
  eval_program         ~100 B
  eval_stmt            ~100 B
  eval_expr            ~400 B
  send / dispatch      ~300 B
  builtins::dispatch   ~600 B
  ── total ≈ 1.7 KB per level ──

After (per user-defined Smol method call — entirely on heap):
  CallFrame in Vec<Cont>   (heap-allocated, no Rust stack cost)
  ── Rust stack cost = 0 ──

Remaining Rust stack growth — only when a builtin calls eval_block/invoke_method:
  step_loop entry      ~100 B
  builtins::dispatch   ~600 B
  eval_block           ~200 B
  ── total ≈ 0.9 KB per builtin-initiated block level ──
```

### Dual evaluator design

The existing `Evaluator` in `eval.rs` is unchanged. `eval_iterative.rs` introduces
`IEvaluator`, a thin wrapper that holds:

- `inner: Arc<Evaluator>` — shared registry, store, entity tracking, handler chain,
  namespace, entity ID (all still needed and shared)
- The `Cont` stack is a **local variable** inside `step_loop`, not a field on
  `IEvaluator`. Each `eval_block` / `invoke_method` call creates a fresh `Vec<Cont>`.

`IEvaluator` exposes the same public API as `Evaluator` for eval purposes
(`eval_program`, `eval_block`, `invoke_method`, `invoke_class_method`, `dispatch`).
`builtins.rs` receives `&Evaluator`; for the iterative path, `IEvaluator::eval_block`
wraps the inner `Evaluator` reference during the step loop using a thin adapter so
re-entrant builtin calls reach the iterative `eval_block` / `invoke_method`. (See S5
for details.)

### `method_stack` and `method_depth` synchronization

`Evaluator::method_stack` and `method_depth` are accessed by `eval.rs`,
`builtins.rs`, and `store_bridge.rs` (for ivar assertions, return-depth matching,
`current_defining_class`). The step loop must keep them in sync: push to
`method_stack` / increment `method_depth` when pushing a `CallFrame`, pop / decrement
when popping one. This preserves all existing consumers without changes.

### `current_block_home` discipline

Currently a `Cell<Option<usize>>` saved/restored around every `eval_block` and
`invoke_method` call. In the iterative version, each `CallFrame` carries:
- `saved_block_home: Option<usize>` — value of `current_block_home` on frame entry
- `frame_block_home: Option<usize>` — the home to set for the duration of this frame

The step loop restores `current_block_home` from `saved_block_home` when popping a
`CallFrame`. This replaces the manual save/restore in `eval_block` and `invoke_method`.

---

## `Cont` enum

```rust
pub(crate) enum Cont {
    /// Evaluate the receiver of a Send, then proceed to arg collection.
    ///
    /// After the receiver is evaluated, this continuation pops and is replaced
    /// by CollectArgs (or dispatches immediately for zero-arg messages).
    AfterReceiver {
        message: Message,
        env: Env,
    },

    /// Collect keyword/binary message arguments left-to-right.
    ///
    /// `pending` is the remaining (key, expr) pairs for keyword messages, or
    /// a single arg expr for binary messages. When `pending` is exhausted,
    /// dispatch is called.
    CollectArgs {
        receiver: Value,
        selector: Arc<str>,
        done: Vec<Value>,
        pending: VecDeque<Expr>,  // expressions yet to evaluate
        env: Env,
    },

    /// Cascade: receiver already evaluated, stepping through messages.
    CascadeStep {
        receiver: Value,
        pending: VecDeque<Message>,
        env: Env,
    },

    /// Dynamic array: accumulating evaluated elements.
    DynArrayElem {
        array_class: InstanceHandle,
        done: Vec<Value>,
        pending: VecDeque<Expr>,
        env: Env,
    },

    /// Assignment: after RHS evaluated, write to variable.
    Assign {
        name: Arc<str>,
        env: Env,
    },

    /// Non-local return target marker: the accumulated value becomes the return
    /// value of the enclosing method. Depth is implicit from the sibling CallFrame.
    ReturnUnwind,

    /// An active method or block invocation frame.
    ///
    /// Pushed by invoke_method / eval_block. Popped when the frame's statement
    /// sequence is exhausted or a matching ^-return arrives.
    CallFrame {
        /// SelfBinding pushed to Evaluator::method_stack for this frame.
        self_binding: SelfBinding,
        /// Depth of this frame (for matching EvalError::Return).
        depth: usize,
        /// Pending statements for this frame.
        pending: VecDeque<Stmt>,
        /// Environment for this frame.
        env: Env,
        /// Value to return if the frame exits without an explicit ^-return.
        /// For methods: Value::Instance(self); for blocks: last-expr value.
        implicit_return: Value,
        /// Whether implicit return is self (method) or last expression (block).
        is_method: bool,
        /// current_block_home to restore when this frame is popped.
        saved_block_home: Option<usize>,
        /// current_block_home to set for the duration of this frame.
        frame_block_home: Option<usize>,
    },
}
```

---

## Steps

### S0: New module skeleton + `Cont` type

Create `crates/interpreter/src/eval_iterative.rs`. Add `mod eval_iterative;` to
`lib.rs`. Define the `Cont` enum as above. Define the `IEvaluator` struct:

```rust
pub struct IEvaluator {
    pub inner: Arc<Evaluator>,
}
```

No logic yet — just types and a stub `impl IEvaluator` with `pub fn new(ev: Arc<Evaluator>) -> Self`.

Add `use crate::eval::SelfBinding` in the new module (it is `pub` on `eval.rs`; if not,
make it `pub(crate)`).

`cargo build` compiles with no warnings.

---

### S1: Step loop skeleton and leaf expressions

Add the private free function:

```rust
fn step_loop(
    ev: &Evaluator,
    initial_cont: Cont,
    env: Env,
) -> Result<Value, EvalError>
```

The loop maintains:
- `stack: Vec<Cont>` — starts as `vec![initial_cont]`
- `acc: Value` — the result of the last completed step, starts as `Value::Nil`

Each iteration pops from `stack` and dispatches on the variant. Loop exits when
`stack` is empty (returns `acc`) or a terminal error is hit.

Handle leaf `Cont::CallFrame` statement dispatch first:

When a `CallFrame` is at the top and `pending` is empty → the frame is done:
1. Pop `method_stack` / decrement `method_depth` on `ev`
2. Restore `ev.current_block_home` from `saved_block_home`
3. The result is `implicit_return` (or the last accumulated value for blocks)
4. Set `acc = result` and continue

When a `CallFrame` has pending statements → pop the next `Stmt` and process it:
- `Stmt::LocalDecl` → pre-bind names in frame env, continue (acc unchanged)
- `Stmt::Expr(e)` → push `Cont::EvalExpr(e, env.clone())` (see S2 for expression dispatch)
- `Stmt::Assign { name, value }` → push `Cont::Assign { name, env.clone() }`, then
  push expression evaluation for `value`
- `Stmt::Return(e)` → push `Cont::ReturnUnwind`, then expression evaluation for `e`
- `Stmt::MethodDef(def)` → handle inline (same logic as `eval.rs`); set acc = Nil

For `Cont::Assign { name, env }` with `acc` as the evaluated RHS:
- Call `ev.assign_var(&name, acc.clone(), &mut env)` and set `acc` to the result

For `Cont::ReturnUnwind` with `acc` as the return value:
- Construct `EvalError::Return(acc, depth)` where `depth` is from the innermost
  `CallFrame` on the stack
- Unwind: pop `Cont`s until a `CallFrame` with matching `depth` is found; pop
  `method_stack` for each `CallFrame` popped; restore `current_block_home`
- If found: set `acc = return_value`, continue
- If not found: propagate `Err(EvalError::Return(v, depth))`

Handle literal expressions (the easiest path through `Cont::CallFrame`):
Add a `EvalExpr` helper variant (not in the main `Cont` enum — or add it) that
evaluates an expression and sets `acc`. For now only `Expr::Lit` and `Expr::Var`
need to work.

Write one test: a method body containing `^42` returns 42 through the iterative path.

`cargo test` green (existing tests still use recursive evaluator; this is new test).

---

### S2: Expression evaluation — Send and dispatch

Handle `Expr::Send { receiver, message }` in the step loop:

1. Push `Cont::AfterReceiver { message: message.clone(), env: env.clone() }`
2. Push expression evaluation for `*receiver`

When `Cont::AfterReceiver { message, env }` fires (acc = receiver value):
- `Message::Unary(sel)` → call `ev.dispatch(acc, sel, vec![], &mut env)`:
  - If the result is a user-defined method invoke (see below), push `CallFrame`
  - Otherwise set `acc = result`
- `Message::Binary { op, arg }` → push `Cont::CollectArgs { receiver: acc, selector: op, done: vec![], pending: VecDeque::from([*arg]), env }`
- `Message::Keyword { parts }` → push `Cont::CollectArgs { receiver: acc, selector, done: vec![], pending: exprs_from_parts, env }`

When `Cont::CollectArgs { receiver, selector, done, pending, env }` fires (acc = last
evaluated arg):
- Append `acc` to `done`
- If `pending` is non-empty: push updated `CollectArgs` with smaller `pending`, then
  push expression eval for the next pending expr
- If `pending` is empty: call dispatch (see below)

**Dispatch at the method boundary:**

`builtins::dispatch` routes to either a builtin (returns `Ok(Value)`) or a
user-defined method via `dispatch_object_value` / `dispatch_entity` /
`dispatch_class_entity` which call `ev.invoke_method`. To avoid those calls
recursing into the Rust stack when called from the step loop, introduce a thin
dispatch hook:

In `builtins.rs`, extract the user-defined method lookup from `dispatch_object_value`
into a `lookup_user_method(class, selector) -> Option<(Arc<Method>, Arc<Class>)>`.
The step loop calls this lookup before calling `builtins::dispatch`. If a user method
is found, push a `CallFrame` directly; if not, call `builtins::dispatch` normally
(which will hit the builtin arm or `dispatch_object_primitive`).

This avoids changing `builtins::dispatch`'s signature and does not affect the
recursive evaluator path.

The same pattern applies for class entity dispatch (line 2052): extract a
`lookup_user_class_method` helper.

For self-send entity dispatch (line 1329): same — check `lookup_user_method` first.

Tests:
- `3 + 4` → 7 through iterative path
- `'hello' size` → 5
- A user-defined method call through the iterative `CallFrame` path

`cargo test` green.

---

### S3: Cascade, DynamicArray, remaining expression types

**`Expr::Cascade { receiver, messages }`:**

1. Push `Cont::CascadeStep { receiver: pending_recv, pending: messages.into() }`
2. Push expression eval for `*receiver`

When `Cont::CascadeStep` fires (acc = receiver after first step, or last message result):
- If `pending` is non-empty: pop next message, evaluate it as a send against
  `receiver` (push AfterReceiver or CollectArgs as appropriate), set `receiver` to
  the original cascade receiver — NOT the message result (Smalltalk cascade semantics:
  all messages to the same original receiver)
- If `pending` is empty: set `acc = last_message_result`

Note: cascade evaluates to the value of the **last** message send result.

**`Expr::DynamicArray(elements)`:**

1. Look up the Array class handle
2. Push `Cont::DynArrayElem { array_class, done: vec![], pending: elements.into() }`

When `Cont::DynArrayElem` fires:
- Append `acc` to `done`
- If `pending` non-empty: push updated frame, push next element expr
- If empty: allocate `Object::new_with_indexed(array_class, done.len())`, fill slots,
  return `Value::Instance(...)`

**`Expr::Block { params, locals, body }`:**
Closure creation is already non-recursive — construct `Value::Block(Arc::new(Block {...}))`
capturing the current env. No change from recursive version.

**`Expr::NamespaceRef { ... }`:**
Purely data lookup — call the existing `eval_namespace_ref` helper directly (it does
no recursion).

**`Expr::ArrayLiteral(elements)`:**
Same as `DynamicArray` but elements are `Literal`s, evaluated via `eval_literal_as_value`.

**`Expr::Super`:** returns `Err(EvalError::TypeError(...))` immediately (unchanged semantics).

`cargo test` green.

---

### S4: `IEvaluator` public entry points

Add `impl IEvaluator`:

```rust
/// Iterative replacement for Evaluator::eval_program.
///
/// Requires: env has at least one live frame.
/// Ensures: semantically identical to Evaluator::eval_program; Smol method calls
///   within the body use heap-allocated CallFrames rather than Rust stack frames.
pub fn eval_program(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError>
```

Implementation: pre-bind `LocalDecl` names (same as recursive version), then
construct a synthetic `CallFrame` for the statement sequence and call `step_loop`.
There is no `SelfBinding` pushed for the outermost program frame (it is not a method
invocation); `is_method = false`, `implicit_return = Value::Nil`.

```rust
/// Iterative replacement for Evaluator::eval_block.
///
/// Requires: args.len() == block.params.len().
/// Ensures: semantically identical to Evaluator::eval_block. eval_block called
///   from builtins runs a complete step_loop to completion and returns synchronously.
pub fn eval_block(&self, block: &Block, args: Vec<Value>, outer_env: &mut Env) -> Result<Value, EvalError>
```

Implementation: build `block_env` (clone of block.env, push_frame, bind params and
locals), construct a `CallFrame` with `frame_block_home = Some(block.home_depth)`,
`is_method = false`, `implicit_return = Value::Nil`. Call `step_loop`.

```rust
/// Iterative replacement for Evaluator::invoke_method.
///
/// Requires: args.len() == method.pattern.params().len().
/// Ensures: semantically identical to Evaluator::invoke_method. Method calls within
///   the body are heap-allocated; calling invoke_method from builtins adds only a
///   fixed number of Rust frames.
pub fn invoke_method(&self, handle: InstanceHandle, defining_class: Arc<Class>,
    method: Arc<Method>, args: Vec<Value>, env: &mut Env) -> Result<Value, EvalError>
```

Implementation: build `method_env` (clone env, push_frame, bind self, params,
locals). Construct `CallFrame` with `self_binding = SelfBinding::Instance { handle, defining_class }`,
`is_method = true`, `implicit_return = Value::Instance(handle)`,
`frame_block_home = None`. Call `step_loop`.

Same pattern for `invoke_class_method`.

**Re-entrant builtins:** `builtins.rs` takes `&Evaluator`. When the step loop calls
`builtins::dispatch(ev_inner, ...)` and a builtin calls `ev.eval_block(...)`, it
reaches `Evaluator::eval_block` — the **recursive** version. This is intentional
for C29. The Rust stack still grows when builtins call `eval_block`, but only at
the depth of *nested builtin block calls* (e.g., `on:do:` containing `whileTrue:`),
not at the depth of user-defined Smol method calls. The main win — user-defined
methods going to heap `CallFrame`s — is already in place.

In C30, `Evaluator::eval_block` is *replaced* by the iterative version (promoted
from `IEvaluator`). At that point builtins automatically use the iterative path
without any signature changes.

`cargo test` green.

---

### S5: Non-local returns and condition unwinding

**`EvalError::Return(v, depth)` propagated from builtins or block bodies:**

When `step_loop` receives `Err(EvalError::Return(v, depth))` (either from a builtin
call or from `Cont::ReturnUnwind`):
1. Pop `Cont` frames, calling cleanup for each `CallFrame` popped:
   - Pop matching entry from `ev.method_stack` / decrement `method_depth`
   - Restore `ev.current_block_home` from `saved_block_home`
2. When a `CallFrame` with `depth == target_depth` is found: return `Ok(v)`
3. If the stack is exhausted: propagate `Err(EvalError::Return(v, depth))` to the
   Rust caller (a builtin that called `eval_block`)

**`EvalError::Signal(cond)`, `Resume(v)`, `ReturnFromBlock(v)`, `Retry`:**

These are raised by builtins (`_primSignalCondition:`, `_primResume:`, etc.) and must
propagate through the step loop correctly. The step loop treats them as terminal
errors that escape the current `step_loop` invocation:
- Clean up any `CallFrame`s on the current loop's stack (pop method_stack, restore
  block_home for each)
- Propagate the error to the Rust caller

Rationale: `on:do:` and `HandlerContext run:` are implemented in builtins, which call
`eval_block` synchronously. These builtins sit on the Rust call stack and catch the
errors via `match eval_block(...)` — the existing mechanism works unchanged.

`EvalError::Retry` from within the step loop triggers a re-run of the current step
loop from the top (for `on:do:` retry semantics, this is handled in the builtin,
not in the step loop itself).

Tests:
- Non-local `^` return from a block inside a method (depth targeting)
- `on:do:` catching an error raised within the iterative eval path
- `ensure:` running cleanup when a non-local return passes through

`cargo test` green.

---

### S6: `super` dispatch

`Expr::Super` as a receiver in `Cont::AfterReceiver`: instead of the normal
`dispatch`, walk the superclass chain. Replicate the logic of
`Evaluator::send_super` but push a `CallFrame` instead of recursing into
`invoke_method`.

Tests: `super someMethod` through iterative path, using an existing test that already
exercises super dispatch.

`cargo test` green.

---

### S7: `eval_block_sync`

`IEvaluator::eval_block_sync` — same as `eval_block` but wraps each statement result
with `force_value` (same as the recursive `eval_block_sync`). Used by `on:do:` and
`ensure:` in builtins. Since builtins call `Evaluator::eval_block_sync` directly
(via `&Evaluator`), this entry point on `IEvaluator` is only needed if entity
dispatch is wired through `IEvaluator` in C30. For now, add a stub that delegates to
`eval_block` (the sync force-value loop is in builtins, not here).

---

### S8: Differential tests

Modify `test_helpers::eval` and `test_helpers::eval_err` to run both evaluators
internally and assert they produce equal results before returning. Every existing
test automatically becomes a differential test with no per-test changes required.

```rust
pub(crate) fn eval(src: &str) -> Value {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");

        let ev_rec = Evaluator::new();
        let mut env_rec = Env::new();
        ev_rec.registry.populate_env(&mut env_rec);
        let v_rec = ev_rec.eval_program(&prog.stmts, &mut env_rec).expect("recursive eval");
        let v_rec = resolve(v_rec).expect("recursive future resolution");
        ev_rec.shutdown();

        let ev_iter = IEvaluator::new(Arc::new(Evaluator::new()));
        let mut env_iter = Env::new();
        ev_iter.inner.registry.populate_env(&mut env_iter);
        let v_iter = ev_iter.eval_program(&prog.stmts, &mut env_iter).expect("iterative eval");
        let v_iter = resolve(v_iter).expect("iterative future resolution");
        ev_iter.inner.shutdown();

        assert_eq!(v_rec, v_iter, "evaluator divergence on: {src}");
        tx.send(v_rec).unwrap();
    });
    rx.recv().expect("eval coroutine panicked")
}
```

`eval_err` follows the same pattern: both evaluators must return the same error.

No `both_eval` helper or `ALL_TEST_PROGRAMS` list is needed. All differentials pass.
`cargo test` green. `cargo fmt`. `cargo clippy --all-targets` clean. Commit.

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/eval_iterative.rs` | New: `Cont` enum, `IEvaluator`, `step_loop`, all iterative eval entry points |
| `crates/interpreter/src/lib.rs` | Add `pub(crate) mod eval_iterative;` |
| `crates/interpreter/src/eval.rs` | `SelfBinding` made `pub(crate)` if not already |
| `crates/interpreter/src/builtins.rs` | Extract `lookup_user_method` and `lookup_user_class_method` helpers (3 call sites); no other changes |
| `crates/interpreter/src/test_helpers.rs` | `eval` and `eval_err` run both evaluators and assert equal results |

---

## Deferred

- **Full CPS transform of builtins** — NOT needed. After C30, `Evaluator::eval_block`
  is iterative, so builtins calling `eval_block` automatically use the step loop. The
  only Rust stack growth remaining is the depth of *nested* builtin→eval_block calls
  (typically 3–5 levels for real programs, ~1–3 KB). CPS would only become relevant
  for async I/O primitives (suspend a block, resume later) — a future concern.

- **Cooperative scheduling + loop primitives (C29.5)** — the step loop never yields
  to the `may` scheduler during message processing. Without a yield point, long loops
  starve other entities. C29.5 adds a reduction counter to `step_loop` and provides
  `_primWhileTrue:`, `_primWhileFalse:`, `_primTimesRepeat:` as Rust loop primitives
  that insert `may::coroutine::yield_now()` every N iterations.

- **General TCO for user-defined recursive Smol** — the step loop does not yet
  optimize tail calls. Without TCO, recursive Smol methods accumulate heap `CallFrame`s
  linearly. This is acceptable for typical call depths but prohibitive for
  iteration-as-recursion (e.g., a naive `whileTrue:` written in pure Smol). C29.5
  avoids the issue for loops via Rust loop primitives. General TCO (detect tail
  position in step loop, replace rather than push `CallFrame`) is deferred.

- **Evaluator promotion + entity wiring (C30)** — replace `Evaluator::eval_block`
  with the iterative version; wire entities to use the updated `Evaluator`; reduce
  `COROUTINE_STACK_SIZE`; move Boolean protocol to kernel.smol.

- **Supervisor entities (C29 TODO)** — entity death notification to a supervisor.
  The stub comment in `run_entity_loop` remains.

- **Span tracking for coverage** — noted in CLAUDE.md; unaffected by this chunk.
