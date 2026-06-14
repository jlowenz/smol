### Chunk 29.8: Restore recursive/iterative evaluator independence

## Problem

C29 built the iterative evaluator *inside* the recursive evaluator's modules rather than
*alongside* them. Specifically:

- `eval.rs` had `eval_program`, `eval_block`, `invoke_method`, `invoke_class_method` replaced
  with iterative (step_loop) versions; the recursive originals were saved with `_recursive`
  suffixes and are now orphaned.
- `builtins.rs` calls `ev.invoke_method()` and `ev.eval_block()`, which are now the iterative
  implementations. The `_recursive` variants are never reached from the dispatch path.
- The differential test in `test_helpers.rs` runs `eval_program` twice (both iterative),
  providing no comparison value.
- C29.5/29.6/29.7 stripped `builtins.rs` of `whileTrue:`, `ensure:`, `on:do:`, `ifNil:` etc.
  because those moved to `dispatch_to_stack`/`kernel.smol` for the *iterative* path — but
  this further broke the recursive path by removing implementations it still needs.
- C29.5/29.6 changed `kernel.smol` to use Smol-recursive `whileTrue:` etc., which would
  stack-overflow on the recursive Rust evaluator (no TCO at the Rust level).

## Goal

Two fully independent evaluator implementations with no runtime switching:

1. **Recursive evaluator** — original Rust-stack recursive implementation, restored from
   the pre-C29 (`f79b5d5`) state of `eval.rs` and `builtins.rs`, with its own kernel.
2. **Iterative evaluator** — the C29–C29.7 heap-stack implementation, in its own modules
   with its own slimmer builtins and the current `kernel.smol`.

Removing the recursive evaluator = deleting its modules + one line in `lib.rs`. No runtime
flag, no conditional dispatch, no shared mutable state that mixes the paths.

---

## Proposed module layout

```
crates/interpreter/src/
  # Shared state (used by both paths — no eval_program/eval_block/invoke_method here)
  eval.rs             — Evaluator struct + shared helpers: lookup_var, assign_var,
                        push_self/pop_self, eval_literal, eval_literal_as_value,
                        eval_namespace_ref, send_super (*), current_block_home, etc.
                        Also: EvalError, SelfBinding, eval_literal free fn, self_sym.

  # ── Recursive path (restored from f79b5d5) ──────────────────────────────────
  eval_recursive.rs   — RecursiveEvaluator { inner: Arc<Evaluator> }
                        eval_program, eval_block, eval_block_sync, invoke_method,
                        invoke_class_method — all purely recursive (no step_loop/Cont).
                        eval_stmt, eval_expr, send_message live here (call
                        builtins_recursive::dispatch, not builtins_iterative::dispatch).

  builtins_recursive.rs — dispatch(ev: &RecursiveEvaluator, ...) → full primitive dispatch
                          including whileTrue:, ensure:, on:do:, syncValue, ifNil: family,
                          calling ev.eval_block() / ev.invoke_method() (recursive).
                          Source: pre-C29 builtins.rs from git f79b5d5.

  kernel_recursive.smol — pre-C29.5 kernel: loop primitives use _primWhileTrue: etc.
                          (NOT Smol-recursive, which would stack-overflow).
                          Source: kernel.smol at f79b5d5 or just before C29.5.

  # ── Iterative path (C29–C29.7 implementation) ───────────────────────────────
  eval_iterative.rs   — IEvaluator { inner: Arc<Evaluator> } (existing)
                        Cont enum, step_loop, push_block_frame, push_method_call_frame,
                        dispatch_to_stack, drain_tail_frames.
                        All eval_program/eval_block/invoke_method methods on IEvaluator
                        use step_loop (existing).

  builtins_iterative.rs — dispatch(ev: &Evaluator, ...) → slimmer primitive dispatch
                           (no whileTrue:, ensure:, on:do:, ifNil: — those are in
                           dispatch_to_stack or kernel.smol).
                           Source: current builtins.rs.
                           NOTE: takes &Evaluator (shared state), not &IEvaluator,
                           because step_loop calls it with &Evaluator directly.

  kernel.smol          — current kernel (Smol-recursive loops, C29.5+ style).
```

(*) `send_super` is trickier — it uses `eval_expr` which calls dispatch. It may need to live
in each evaluator module, or be parameterised on a dispatch callback. Assess during S2.

---

## What `eval.rs` retains after the split

The `Evaluator` struct keeps only state + stateless helpers that neither path reimplements:

- Fields: `registry`, `store`, `method_stack`, `value_objects`, `current_tx`, `method_depth`,
  `current_block_home`, `current_entity_id`, `handler_stack`.
- Methods: `new()`, `with_registry()`, `shutdown()`, `push_self()`, `pop_self()`,
  `lookup_var()`, `assign_var()`, `eval_literal_as_value()`, `eval_namespace_ref()`,
  `populate_env()` (via registry), `check_prim_sends_in_body()`, `check_prim_sends_in_expr()`.
- **Removed from `eval.rs`**: `eval_program()`, `eval_block()`, `eval_block_sync()`,
  `invoke_method()`, `invoke_class_method()`, `eval_stmt()`, `eval_expr()`, `dispatch()`,
  `send_message()`, `send_super()`, and all `_recursive` variants.

The iterative path's public entry points (`eval_program` etc.) are on `IEvaluator`.
The recursive path's entry points are on `RecursiveEvaluator`.

Entity loops and `with_registry` spawns continue to create `IEvaluator` instances.

---

## Class registry and kernel loading

Currently `ClassRegistry::new()` embeds `kernel.smol` at compile time. After the split:

```rust
// In class_registry.rs
pub(crate) const KERNEL_ITERATIVE: &str = include_str!("kernel.smol");
pub(crate) const KERNEL_RECURSIVE: &str = include_str!("kernel_recursive.smol");

impl ClassRegistry {
    pub fn new_iterative() -> Arc<Self> { /* loads KERNEL_ITERATIVE */ }
    pub fn new_recursive() -> Arc<Self> { /* loads KERNEL_RECURSIVE */ }
}
```

`Evaluator::new()` (and `IEvaluator::new()`) uses `new_iterative()`.
`RecursiveEvaluator::new()` uses `new_recursive()`.

The kernel sources can share most content (copy `kernel_recursive.smol` from the pre-C29.5
tag and keep it frozen; `kernel.smol` continues to evolve with the iterative path).

---

## `test_helpers.rs` fix

```rust
pub(crate) fn eval(src: &str) -> Value {
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");

        // Iterative evaluator
        let ev_iter = IEvaluator::new(Arc::new(Evaluator::new())); // loads kernel.smol
        let v_iter = ev_iter.eval_program(&prog.stmts, &mut env_iter)?;

        // Recursive evaluator (independent registry + kernel_recursive.smol)
        let ev_rec = RecursiveEvaluator::new();
        let v_rec = ev_rec.eval_program(&prog.stmts, &mut env_rec)?;

        assert!(values_equivalent(&v_iter, &v_rec), "evaluator divergence: ...");
        Ok(v_iter)
    });
}
```

The two evaluators are completely independent — no shared registry, no shared state.

---

## Benchmarking

With no runtime flag, each path is directly benchmarkable:

```rust
// benches/interpreter.rs
fn bench_iterative(c: &mut Criterion) {
    // IEvaluator::new() + eval_program
}
fn bench_recursive(c: &mut Criterion) {
    // RecursiveEvaluator::new() + eval_program
}
```

---

## Steps

### S1: Extract `kernel_recursive.smol`

Check out `kernel.smol` at commit just before C29.5 (`28f6a32~1`) as `kernel_recursive.smol`.
This preserves `_primWhileTrue:` etc. (Rust-implemented loop primitives, safe for recursive
Rust evaluator). Verify manually that it has no Smol-recursive loop implementations.

### S2: Add `ClassRegistry::new_recursive()` / `new_iterative()`

Add both `include_str!` constants and the two constructors. The existing `ClassRegistry::new()`
becomes an alias for `new_iterative()` (no behaviour change for existing code).

### S3: Create `eval_recursive.rs` — restore from git

`git show f79b5d5:crates/interpreter/src/eval.rs` is the source. Extract:
- `eval_program` (recursive loop) → `RecursiveEvaluator::eval_program`
- `eval_block`, `eval_block_sync` → `RecursiveEvaluator::eval_block`, `eval_block_sync`
- `invoke_method`, `invoke_class_method` → `RecursiveEvaluator::invoke_method`, etc.
- `eval_stmt`, `eval_expr`, `send_message` → private methods on `RecursiveEvaluator`
- `send_super` → private method on `RecursiveEvaluator`; uses
  `builtins_recursive::dispatch_class_builtin` for class-side super sends

All `self.dispatch(...)` calls in the recursive path become
`builtins_recursive::dispatch(self, ...)`.

### S4: Create `builtins_recursive.rs` — restore from git

`git show f79b5d5:crates/interpreter/src/builtins.rs` is the source.
- Change `ev: &Evaluator` → `ev: &RecursiveEvaluator` in `dispatch` signature.
- All `ev.eval_block(...)` and `ev.invoke_method(...)` now call the recursive
  implementations on `RecursiveEvaluator`.
- Any shared pure utilities (`force_value`, `make_error_value`, `hash_value`,
  `as_block`, `expect_arity`, `condition_message`, etc.) can be pub(crate) in
  `builtins_iterative.rs` and used from `builtins_recursive.rs` via `use super::...`
  or moved to a shared `builtins_common.rs`.

### S5: Rename `builtins.rs` → `builtins_iterative.rs`

Update all `use crate::builtins::` references in `eval.rs`, `eval_iterative.rs`,
`entity.rs`, `store_bridge.rs` etc. to `use crate::builtins_iterative::`.
The `dispatch` signature stays `ev: &Evaluator` (step_loop passes `&Evaluator` directly).

### S6: Strip `eval.rs` down to shared state

Remove from `eval.rs`:
- `eval_program`, `eval_block`, `eval_block_sync`, `eval_program_sync` → gone (iterative
  path uses `IEvaluator`; these are now dead)
- `invoke_method`, `invoke_class_method` → moved to `IEvaluator` in `eval_iterative.rs`
  (they already exist there; the `Evaluator` versions are now dead)
- `eval_stmt`, `eval_expr`, `send_message`, `send_super`, `dispatch` → move to
  `eval_recursive.rs` (recursive path) or `eval_iterative.rs` (iterative path uses
  `dispatch_to_stack` + `builtins_iterative::dispatch` directly)
- All `_recursive` suffix variants → move to `eval_recursive.rs` and drop the suffix

What remains in `eval.rs`: the struct, its fields, `new()`, `with_registry()`,
`shutdown()`, `push_self()`, `pop_self()`, `lookup_var()`, `assign_var()`,
`eval_literal_as_value()`, `eval_namespace_ref()` (pure, no dispatch),
`check_prim_sends_*`, `SelfBinding`, `EvalError`.

### S7: Update `eval_iterative.rs`

- `IEvaluator::invoke_method` and `invoke_class_method` replace the (now-removed)
  `Evaluator::invoke_method` etc. as the canonical entry points.
- `step_loop` and `dispatch_to_stack` call `builtins_iterative::dispatch` (renamed import).
- `Evaluator` fields accessed as before via `ev.registry`, `ev.push_self()`, etc.

### S8: Update `entity.rs`

`run_entity_loop` creates evaluators for actor message processing. It should continue to
use `IEvaluator` (or `Evaluator` with iterative path). `RecursiveEvaluator` is never used
in entity loops.

### S9: Fix `test_helpers.rs`

Implement the differential test as described above. Both evaluators run the same source
independently. `eval_err` similarly.

### S10: Update `lib.rs` and all `use` paths

Add module declarations for `eval_recursive`, `builtins_recursive`, `builtins_iterative`.
Update imports throughout. Run `cargo build` to catch all broken paths.

### S11: Tests pass, differential coverage confirmed

All existing tests pass. The differential test now meaningfully compares two independent
implementations. Add a note in `CLAUDE.md` about the two-path architecture.

---

## Ordering notes

- S1–S2 are safe preconditions that do not break existing code.
- S3–S4 create new files; nothing calls them yet.
- S5 renames the module and updates imports — first point of breakage risk.
- S6 is the destructive step; do it last once S3–S5 compile cleanly.
- S7–S10 are mechanical follow-on fixes.

## Risk areas

- `send_super` in `eval_recursive.rs` needs `builtins_recursive::dispatch_class_builtin`;
  verify the class-side super-send path works correctly.
- `store_bridge.rs` and `entity.rs` import from `builtins`; these switch to
  `builtins_iterative` (they are iterative-path callers).
- Shared utilities in builtins (`force_value`, `make_error_value`, etc.) need to be
  accessible from `builtins_recursive`; a thin `builtins_common.rs` or re-export
  from `builtins_iterative` works.
- After S6, `Evaluator` no longer has `eval_program` as a method. Any code calling
  `ev.eval_program(...)` directly (e.g. in `with_registry`-spawned entity evaluators
  in `entity.rs`) must switch to `IEvaluator::new(Arc::new(ev)).eval_program(...)`.

## Deferred

- Benchmarks: simple to add once the split is done; not needed to unblock C30.
- Whether to expose `RecursiveEvaluator` in the crate's public API (currently internal).
- Long-term: when ready to retire the recursive evaluator, `git rm eval_recursive.rs
  builtins_recursive.rs kernel_recursive.smol` + remove from `lib.rs` + fix
  `test_helpers.rs` to run `eval_program` once. One session of work.
