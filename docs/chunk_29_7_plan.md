### Chunk 29.7: Inline `syncValue` / `ensure:` / `on:do:` via Continuation Variants

## Goal

Eliminate the last nested `step_loop` calls from builtins by inlining `syncValue`,
`ensure:`, and `on:do:` into the iterative stack. After C29.6, these three are the
only builtins that still call `ev.eval_block_sync()` or `ev.eval_block()` — each of
which creates a fresh `step_loop` on the Rust call stack. After this chunk, the
iterative evaluator is fully iterative: no builtin creates a nested `step_loop` for
Smol-level block evaluation.

As a bonus, inline `ifNil:`, `ifNotNil:`, `ifNil:ifNotNil:`, `ifNotNil:ifNil:` —
these are simple block-evals like `ifTrue:` was in C29.6 and require no new machinery.

Deliverables:

- Six new `Cont` variants covering sync forcing, ensure guarding, signal catching,
  and unwind resumption (see S1).
- `sync: bool` field on `Cont::CallFrame`; `push_block_frame` gains a `sync` parameter.
- `handle_step_error` extended to scan for `EnsureGuard` and `OnDoCatch` before
  propagating any error, including `EvalError::Return`.
- `dispatch_to_stack` intercepts `syncValue`, `ensure:`, `on:do:`, and the `ifNil:`
  family before falling through to builtins.
- Corresponding arms removed from `builtins.rs`.
- All existing condition system, ensure, and syncValue tests pass unchanged.

---

## Background

### What `eval_block_sync` means in continuation terms

`eval_block_sync` evaluates each statement and forces any `Future` result before
running the next statement. In continuation terms: between every two consecutive
statements of a sync block, there is an implicit `force_value(acc)` step. This is
exactly what `Cont::ForceAcc` represents.

### Why `on:do:` and `ensure:` need error interception

`on:do:` catches `EvalError::Signal`s raised anywhere inside the body, including from
resolved `Future`s (which is why the body runs with sync semantics — so a Future that
resolves to a signal is noticed before the next statement runs). In the current
implementation, this catching happens in the Rust `match` around `eval_block_sync`.
After inlining, signal propagation goes through `handle_step_error`, which must scan
the stack for `OnDoCatch` before giving up.

`ensure:` must run its cleanup block regardless of whether the body succeeded, signalled,
or triggered a non-local `^`-return. This makes it the hardest of the three: the
`EvalError::Return` path currently bypasses normal continuation processing (it goes
through `unwind_stack_to_depth`, which skips non-`CallFrame` entries). `EnsureGuard`
must be visible to the unwind logic.

---

## Steps

### S1: New `Cont` variants

Add to the `Cont` enum in `eval_iterative.rs`:

```rust
/// Force acc via force_value before the next continuation fires.
/// Pushed after each statement dispatch in a sync-semantics frame.
ForceAcc,

/// Restore acc to a saved value (used by EnsureGuard on the success path to
/// discard the ensure block's own result and return the body's result).
RestoreAcc { value: Value },

/// Re-raise a saved error after an ensure block has finished running.
/// Pushed below the ensure block frame so the error propagates after cleanup.
Rethrow { error: EvalError },

/// Marks the bottom of an ensure: body on the stack.
///
/// Success path: when the body frame completes normally, EnsureGuard fires,
///   runs the ensure block, then RestoreAcc restores the body's result.
/// Error path: handle_step_error intercepts it, pushes Rethrow + ensure frame.
/// Return (^) path: unwind_stack_to_depth intercepts it, pushes ContinueUnwind + ensure frame.
EnsureGuard { ensure_blk: Arc<crate::value::Block> },

/// Marks the bottom of an on:do: body on the stack.
///
/// Success path: fires and simply returns acc (the body's result) unchanged.
/// Signal path: handle_step_error intercepts it, checks catch_class, runs handler or re-raises.
/// Retry path: handler returns EvalError::Retry; re-push body frame + OnDoCatch and continue.
/// Return (^) path: passes through (unwind continues past it without running handler).
OnDoCatch {
    catch_class: Arc<crate::class::Class>,
    handler_blk: Arc<crate::value::Block>,
    body_blk: Arc<crate::value::Block>,
},

/// Resume a ^-return unwind after an ensure block has run.
/// Holds the original return value and the target depth to resume unwinding toward.
ContinueUnwind { return_val: Value, target_depth: usize },
```

---

### S2: `sync: bool` on `CallFrame`; update `push_block_frame`

Add `sync: bool` to `Cont::CallFrame`. When `sync=true`, after dispatching each
statement, `ForceAcc` is pushed below the statement continuations:

```rust
// In CallFrame arm, dispatching next statement:
stack.push(frame_back_with_remaining_pending);
if sync {
    stack.push(Cont::ForceAcc);   // fires after stmt, before frame is revisited
}
dispatch_stmt(ev, stmt, ...)?;    // pushes stmt continuations on top
```

`push_block_frame` gains `sync: bool`. All existing call sites pass `false`; the new
`syncValue`/`ensure:`/`on:do:` dispatch sites pass `true`.

`Cont::ForceAcc` arm in `step_loop`:

```rust
Cont::ForceAcc => {
    acc = force_value(std::mem::replace(&mut acc, Value::Nil))?;
}
```

`Cont::RestoreAcc` arm:

```rust
Cont::RestoreAcc { value } => { acc = value; }
```

---

### S3: `EnsureGuard` — success and error paths

**Success path** (`EnsureGuard` fires normally from the loop):

```rust
Cont::EnsureGuard { ensure_blk } => {
    let body_result = acc.clone();
    // Push RestoreAcc below, then the ensure block frame above it.
    stack.push(Cont::RestoreAcc { value: body_result });
    push_block_frame(ev, &ensure_blk, vec![], false, stack)?;
}
```

**Error path** — extend `handle_step_error`:

When any error other than `Return` propagates, scan the stack for the nearest
`EnsureGuard`, cleaning up intervening `CallFrame`s:

```rust
// In handle_step_error, for non-Return errors:
while let Some(cont) = stack.pop() {
    match cont {
        Cont::CallFrame { ... } => { /* cleanup: pop_self, pop_env */ }
        Cont::EnsureGuard { ensure_blk } => {
            // Run ensure block, then re-raise.
            stack.push(Cont::Rethrow { error: other.clone() });
            push_block_frame(ev, &ensure_blk, vec![], false, stack)?;
            return Ok(()); // let step_loop run the ensure block
        }
        Cont::OnDoCatch { .. } => { /* pass through — ensure beats on:do: for cleanup */ }
        _ => { /* other conts discarded */ }
    }
}
// Stack exhausted without finding EnsureGuard — propagate.
Err(other)
```

`Cont::Rethrow` arm:

```rust
Cont::Rethrow { error } => {
    handle_step_error(error, ev, &mut stack, &mut acc)?;
}
```

**Return (^) path** — extend `unwind_stack_to_depth`:

When `unwind_stack_to_depth` encounters `EnsureGuard` before reaching the target depth:

```rust
Cont::EnsureGuard { ensure_blk } => {
    // Can't run the ensure block here (not in step_loop context).
    // Push ContinueUnwind to resume the unwind after the ensure block runs.
    stack.push(Cont::ContinueUnwind {
        return_val: return_val.clone(),
        target_depth,
    });
    push_block_frame(ev, &ensure_blk, vec![], false, stack)?;
    return InterruptedByEnsure; // new enum or bool return
}
```

`handle_step_error` for `EvalError::Return` checks the return value from
`unwind_stack_to_depth`: if interrupted, return `Ok(())` (let step_loop continue).

`Cont::ContinueUnwind` arm:

```rust
Cont::ContinueUnwind { return_val, target_depth } => {
    // ensure block just ran; acc = ensure result (discard it).
    // Resume unwinding from wherever the stack currently is.
    if unwind_stack_to_depth(ev, &mut stack, return_val.clone(), target_depth) == Completed {
        acc = return_val;
    } else {
        // Another EnsureGuard or exhausted — handle_step_error will manage.
        return Err(EvalError::Return(return_val, target_depth));
    }
}
```

---

### S4: `OnDoCatch` — success, signal, and retry paths

**Success path** (`OnDoCatch` fires normally):

```rust
Cont::OnDoCatch { .. } => {
    // Body completed without signal; pass acc through.
    // (Nothing to do — acc is already the body result.)
}
```

**Signal path** — in `handle_step_error`, after scanning past `EnsureGuard`s:

```rust
Cont::OnDoCatch { catch_class, handler_blk, body_blk } => {
    if let EvalError::Signal(ref cond) = other {
        let signal_class = /* extract class from cond */;
        if Class::is_subclass_of(&signal_class, &catch_class) {
            // Push handler invocation. Handler result becomes the on:do: result.
            push_block_frame(ev, &handler_blk, vec![cond.clone()], false, stack)?;
            // If handler raises Retry, push a Cont::RetryOnDo to re-run body.
            stack.push(Cont::HandleOnDoResult { catch_class, handler_blk, body_blk });
            return Ok(());
        }
    }
    // Signal doesn't match (or non-signal error): keep scanning deeper.
}
```

**Retry path** — `Cont::HandleOnDoResult` checks the handler's outcome:

```rust
Cont::HandleOnDoResult { catch_class, handler_blk, body_blk } => {
    // Normal completion: acc = handler result; done.
    // (Retry and Resume are raised as EvalErrors from the handler block.)
}
// In handle_step_error for HandleOnDoResult:
Cont::HandleOnDoResult { catch_class, handler_blk, body_blk } if matches!(other, EvalError::Retry) => {
    stack.push(Cont::OnDoCatch { catch_class, handler_blk, body_blk: body_blk.clone() });
    push_block_frame(ev, &body_blk, vec![], true /* sync */, stack)?;
    return Ok(());
}
Cont::HandleOnDoResult { .. } if matches!(other, EvalError::Resume(v)) => {
    acc = v; return Ok(());
}
Cont::HandleOnDoResult { .. } if matches!(other, EvalError::ReturnFromBlock(v)) => {
    acc = v; return Ok(());
}
```

**Return (^) path**: `OnDoCatch` is NOT intercepted during `^`-return unwind — the
unwind passes through it. If the `^` targets a depth outside the `on:do:` body, the
unwind pops `OnDoCatch` as if it were just another non-frame entry.

---

### S5: `dispatch_to_stack` — intercept before builtins

Add before the `builtins::dispatch` fallthrough:

**`syncValue`**:
```
Value::Block: "syncValue" → push_block_frame(ev, blk, vec![], sync=true, stack)
```

**`ensure:`**:
```
Value::Block: "ensure:" → {
    ensure_blk = as_block(args[0])?;
    stack.push(Cont::EnsureGuard { ensure_blk });
    push_block_frame(ev, blk, vec![], sync=true, stack)  // body is sync
}
```

**`on:do:`**:
```
Value::Block: "on:do:" → {
    catch_class = /* from args[0] */;
    handler_blk = as_block(args[1])?;
    stack.push(Cont::OnDoCatch { catch_class, handler_blk, body_blk: blk.clone() });
    push_block_frame(ev, blk, vec![], sync=true, stack)  // body is sync
}
```

**`ifNil:` family** (same pattern as C29.6's `ifTrue:` inlining):
```
Any receiver: "ifNil:" → { force acc; if nil push nil_blk frame, else acc=Nil }
"ifNotNil:"  → { force acc; if not-nil push blk frame with forced recv, else acc=Nil }
"ifNil:ifNotNil:" / "ifNotNil:ifNil:" → symmetric
```

---

### S6: Remove inlined arms from `builtins.rs`

Remove `syncValue`, `ensure:`, `on:do:`, `ifNil:`, `ifNotNil:`, `ifNil:ifNotNil:`,
`ifNotNil:ifNil:` from `builtins::dispatch` and `dispatch_block`. These are now handled
entirely by `dispatch_to_stack`. The `then:` arm stays (genuinely spawns a new
coroutine — not a block eval).

---

### S7: Tests

Regression: all existing C28/C28a/C28b condition system tests pass.

New tests exercising the inlined paths:

- `ensure_runs_after_normal_completion` — body returns value; ensure block runs; body result returned.
- `ensure_runs_after_signal` — body signals; ensure block runs; signal re-raised.
- `ensure_runs_after_nonlocal_return` — `^` from inside body; ensure block runs; return continues.
- `on_do_catches_signal` — body signals matching class; handler runs; handler result returned.
- `on_do_passes_non_matching_signal` — body signals; class doesn't match; signal propagates.
- `on_do_retry_reruns_body` — handler returns Retry; body runs again.
- `on_do_resume_returns_value` — handler resumes with value.
- `sync_value_forces_future` — body returns Future; syncValue blocks until resolved.
- `ifnil_family` — standard nil-guard tests (regression).
- `nested_ensure_on_do` — `[[body] ensure: [e1]] on: Error do: [h]`: verify both run correctly.

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/eval_iterative.rs` | New `Cont` variants; `sync` field on `CallFrame`; extended `handle_step_error`; extended `unwind_stack_to_depth`; new `dispatch_to_stack` arms; `push_block_frame` gains `sync` param |
| `crates/interpreter/src/builtins.rs` | Remove `syncValue`, `ensure:`, `on:do:`, `ifNil:` family arms |
| `crates/interpreter/src/tests/eval.rs` | New tests for inlined paths |

---

## Ordering invariants preserved

| Invariant | Mechanism |
|---|---|
| `syncValue` forces futures between statements | `sync=true` CallFrame pushes `ForceAcc` after each statement |
| `on:do:` body forces futures (so async signals are noticed) | body frame is `sync=true` |
| `ensure:` body forces futures | body frame is `sync=true` |
| ensure runs for all outcomes (success, signal, ^-return) | EnsureGuard on success path; handle_step_error intercepts errors; unwind_stack_to_depth intercepts ^-returns |
| ^-return target depth matches across nested ensure | ContinueUnwind resumes unwind at correct target_depth after ensure block runs |
| on:do: handler does NOT run on ^-return | unwind passes through OnDoCatch without invoking handler |
| Retry re-runs the body with fresh OnDoCatch | HandleOnDoResult pushes body frame + new OnDoCatch |
| `then:` still spawns a new coroutine | stays in builtins; not affected |

---

## Deferred

- **Collection `do:`, `collect:`, `select:`, `detect:`, `inject:into:`** — these use
  Rust loops calling `eval_block`. Inlining them requires either Smol recursion or a
  dedicated `Cont::CollectionIterate` variant with a stored iterator. Deferred to a
  later chunk once the collection class hierarchy is stabilised.
- **`then:` inlining** — genuinely spawns a new coroutine; inlining it would require
  a coroutine-resumption continuation, which is a different problem.
