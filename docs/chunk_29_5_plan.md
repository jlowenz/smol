### Chunk 29.5: Cooperative Scheduling + Loop Primitives

## Goal

Fix two problems that C29 leaves open:

1. **Thread starvation.** The `step_loop` never yields to the `may` scheduler during
   message processing. An entity running a tight loop monopolizes its OS thread and
   starves all other entities on that thread pool slot.

2. **Heap frame accumulation for loops.** `whileTrue:` etc. implemented as recursive
   Smol methods would push one `CallFrame` per iteration — O(N) heap growth for N
   iterations. Without general TCO this is prohibitive for any non-trivial loop.

Deliverables:

- Reduction counter in `step_loop`: every `YIELD_INTERVAL` steps, call
  `may::coroutine::yield_now()` to cooperatively yield to the scheduler.
- `_primWhileTrue:`, `_primWhileFalse:`, `_primTimesRepeat:` — Rust loop primitives
  that call `eval_block` sequentially (not recursively) and yield every N iterations.
- `kernel.smol` additions: `BlockClosure >> whileTrue:`, `whileFalse:`,
  `whileTrue`, `whileFalse`; `Number >> timesRepeat:` — all delegate to primitives.
- Dead Rust dispatch arms for these selectors removed from `builtins.rs`.
- `cargo test` green; `cargo fmt`; `cargo clippy --all-targets` clean.

---

## Background

### Why not pure Smol loops with general TCO?

A `whileTrue:` written as a recursive Smol method is the elegant choice:

```smalltalk
BlockClosure >> whileTrue: aBlock [
    self value ifTrue: [ aBlock value. self whileTrue: aBlock ]
]
```

With TCO, the tail-recursive call `self whileTrue: aBlock` would replace the current
`CallFrame` instead of pushing a new one — O(1) heap, correct cooperative scheduling
via the reduction counter. Without TCO, each iteration pushes a new `CallFrame` onto
the heap `Cont` stack: 10 million iterations = 10 million frames = ~2–5 GB. Unusable.

General TCO (detecting tail positions in the step loop and replacing rather than
pushing `CallFrame`s) is the right long-term answer — it enables all recursive Smol
algorithms to be stack-safe, not just loops. But implementing it correctly requires
reasoning about which expressions are in tail position for both methods (implicit
`self` return) and blocks (last expression is the block's result), including the
interaction with `^`-returns and inherited HC handler chains. This is non-trivial and
is deferred.

**Rust loop primitives** are the pragmatic bridge: `_primWhileTrue:` is a Rust
`loop {}` that calls `ev.eval_block` twice per iteration (condition + body). Each
`eval_block` call runs to completion and returns before the next — no Rust stack
accumulation across iterations. The primitive also inserts `may::coroutine::yield_now()`
every `YIELD_INTERVAL` iterations to give other entities CPU time.

After C30 promotes `Evaluator::eval_block` to the iterative version, each
`eval_block` call in these primitives runs a step loop internally (O(1) Rust stack
per user method call), so the only Rust frames on the stack during a loop are the
primitive itself plus two `eval_block` entry frames — constant depth regardless of
loop count.

### Reduction counter

The step loop is the only place all Smol execution passes through. Adding a counter
here gives every code path — including Smol methods called from non-loop builtins —
a chance to yield. The counter is a `u32` local in `step_loop`; it increments on
each message dispatch step and resets on yield. No global state.

`YIELD_INTERVAL` default: 1024. This is coarse-grained enough to avoid overhead on
tight inner loops while still yielding frequently enough (at ~1M Smol messages/second
this is ~1ms between yields — comparable to BEAM's ~2ms time slice).

---

## Steps

### S1: Reduction counter in `step_loop`

In `eval_iterative.rs`, modify `step_loop` to maintain a `u32` reduction counter:

```rust
let mut reductions: u32 = 0;
// inside the dispatch loop, on each message send (CollectArgs → dispatch):
reductions += 1;
if reductions >= YIELD_INTERVAL {
    reductions = 0;
    may::coroutine::yield_now();
}
```

`YIELD_INTERVAL` is a module-level constant: `const YIELD_INTERVAL: u32 = 1024;`

Only message dispatch steps count as reductions (not literal evaluation, not
`CallFrame` push/pop bookkeeping). This matches BEAM's "one reduction per function
call" model.

**Test:** spawn two entities, each running a tight arithmetic loop of 10,000
iterations that never sends a message. Verify both entities complete and neither
starves the other. (Since the reduction counter fires at message dispatch, pure
arithmetic loops inside a method body will not yield within the loop body — the
yield occurs at the next top-level message send into the entity. This is acceptable:
the granularity is per-message-handler, not per-arithmetic-op.)

`cargo test` green.

---

### S2: `_primWhileTrue:` and `_primWhileFalse:` on `BlockClosure`

Add to `builtins.rs` in the `BlockClosure` dispatch section:

```
_primWhileTrue: bodyBlock
  Requires: self is a zero-arg Block (condition); bodyBlock is a zero-arg Block.
  Ensures: evaluates self; while result is Value::Bool(true), evaluates bodyBlock.
    Returns Value::Nil. Calls may::coroutine::yield_now() every YIELD_INTERVAL
    iterations so long loops do not monopolize the may thread pool.
```

Implementation:
```rust
let cond = recv_as_block(&recv, &selector)?;
let body = as_block(args.into_iter().next().unwrap(), &selector)?;
let mut iters: u32 = 0;
loop {
    match force_value(ev.eval_block(&cond, vec![], env)?)? {
        Value::Bool(true) => { ev.eval_block(&body, vec![], env)?; }
        _ => break,
    }
    iters += 1;
    if iters % YIELD_INTERVAL == 0 { may::coroutine::yield_now(); }
}
Ok(Value::Nil)
```

`YIELD_INTERVAL` here matches the step loop constant (or is a separate constant with
the same default). Import `may::coroutine` where needed.

`_primWhileFalse:` is identical with the condition inverted:
```rust
Value::Bool(false) => { ev.eval_block(&body, vec![], env)?; }
```

**Tests:**
- `| n | n := 0. [n < 100] whileTrue: [n := n + 1]. n` → 100
- `| n | n := 100. [n > 0] whileTrue: [n := n - 1]. n` → 0
- `| n | n := 0. [n >= 5] whileFalse: [n := n + 1]. n` → 5
- Condition that starts false: loop body never runs, returns nil.
- `_primWhileTrue:` with body that signals an error: error propagates correctly.

`cargo test` green.

---

### S3: `_primTimesRepeat:` on `Integer`

Add to the Integer dispatch section in `builtins.rs`:

```
_primTimesRepeat: aBlock
  Requires: self is a non-negative integer; aBlock is a zero-arg Block.
  Ensures: evaluates aBlock exactly self times. Returns Value::Nil.
    Calls may::coroutine::yield_now() every YIELD_INTERVAL iterations.
```

Implementation:
```rust
let n = recv_as_int(&recv, &selector)?;
let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
for i in 0..n.max(0) {
    ev.eval_block(&blk, vec![], env)?;
    if i as u32 % YIELD_INTERVAL == 0 && i > 0 { may::coroutine::yield_now(); }
}
Ok(Value::Nil)
```

**Tests:**
- `| n | n := 0. 5 timesRepeat: [n := n + 1]. n` → 5
- `0 timesRepeat: [Error signal: 'should not run']` → no error
- `-1 timesRepeat: [Error signal: 'should not run']` → no error (max(n, 0) = 0)

`cargo test` green.

---

### S4: `kernel.smol` — loop delegation

Add to `kernel.smol`, in the `BlockClosure` method section:

```smalltalk
BlockClosure >> whileTrue: aBlock  [ ^self _primWhileTrue: aBlock ]
BlockClosure >> whileFalse: aBlock [ ^self _primWhileFalse: aBlock ]
BlockClosure >> whileTrue          [ ^self _primWhileTrue: [] ]
BlockClosure >> whileFalse         [ ^self _primWhileFalse: [] ]
```

`[]` evaluates to a zero-arg block returning nil. Used as the body in the no-arg
forms (`whileTrue` / `whileFalse`), where the condition block IS the body — the loop
just pumps the condition until it returns false/true.

Add to the `Number` (or `Integer`) method section:

```smalltalk
Number >> timesRepeat: aBlock [ ^self _primTimesRepeat: aBlock ]
```

---

### S5: Remove dead Rust loop dispatch arms

In `builtins.rs`, remove the existing Rust dispatch arms for:
- `"whileTrue:"`, `"whileFalse:"`, `"whileTrue"`, `"whileFalse"` on `BlockClosure`
- `"timesRepeat:"` on `Integer` / `Number`

These are now handled by kernel.smol delegation to the `_prim*` primitives. Removing
the Rust arms ensures the Smol methods are exercised and the dead code is eliminated.

**Verify:** existing tests for these selectors still pass (they now go through the
Smol delegation → primitive path).

---

### S6: Tests and differential

- All existing `whileTrue:`, `whileFalse:`, `timesRepeat:` tests in `tests/eval.rs`
  and `tests/builtins.rs` pass through the new Smol→primitive path.
- C29 differential tests (`iterative_matches_recursive`) still pass — the Smol loop
  methods produce the same results on both evaluators.
- **Scheduling test:** spawn 4 entities each running `[true] whileTrue: [n := n + 1]`
  for a bounded count; confirm all complete and no single entity monopolizes the pool.

`cargo test` green. `cargo fmt`. `cargo clippy --all-targets` clean. Commit.

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/eval_iterative.rs` | Add reduction counter to `step_loop`; `YIELD_INTERVAL` constant |
| `crates/interpreter/src/builtins.rs` | Add `_primWhileTrue:`, `_primWhileFalse:`, `_primTimesRepeat:`; remove old Rust loop dispatch arms |
| `crates/interpreter/src/kernel.smol` | `BlockClosure >> whileTrue:`, `whileFalse:`, `whileTrue`, `whileFalse`; `Number >> timesRepeat:` |
| `crates/interpreter/src/tests/eval.rs` | Scheduling test; regression tests for loop methods |

---

## Deferred

- **General TCO** — tail-call optimization in `step_loop` to make *all* recursive
  Smol algorithms heap-safe (not just loops backed by Rust primitives). This is the
  right long-term fix. The loop primitives here are a bridge until TCO is implemented.
  Should be prioritized before introducing recursive algorithms in `kernel.smol`.

- **`to:do:` and `to:by:do:`** — common numeric iteration forms. Can be implemented
  as Smol methods using `whileTrue:`, or as their own Rust primitives following the
  same pattern. Deferred to C30 or a later chunk.

- **Async I/O yield** — when network/file I/O arrives, `eval_block` suspension will
  be needed (a block submitted to a scheduler, resumed on I/O completion). The loop
  primitive pattern here is synchronous; async I/O requires a different mechanism.
  The reduction counter handles cooperative CPU scheduling; I/O scheduling is future.
