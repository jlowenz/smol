# Chunk 31.5: Complete the iterative evaluator (small per-entity stacks)

**Status**: Planned — **prerequisite to completing C31** (nested `atomic:` /
HandlerContext-as-Value rely on it).
**Raised**: 2026-06-21, during C31 S5. The async `Transaction` design required
making `HandlerContext` a Value (synchronous `run:`); that exposed a latent
property of the "iterative" evaluator: it still grows the Rust coroutine stack
per nested block-taking builtin.

## Problem (measured, not theorized)

A trivial 2-level nested `HandlerContext` —

```smalltalk
outer := HandlerContext new on: Error do: [:e | 'outer'].
outer run: [ (HandlerContext new on: Error do: [:e | 'inner']) run: [Error signal: 'x'] ]
```

— reaches **`step_loop` recursion depth 6** and **overflows the 256 KB coroutine
stack** (`TEST_COROUTINE_STACK` / `COROUTINE_STACK_SIZE` = `0x8000` *words* =
256 KB). Instrumentation showed: max depth 6, `size_of::<Cont>()` = 176 bytes —
so ~**40 KB of real stack per `step_loop` level**. It is correct in the REPL
(larger main-thread stack), so it is **not a logic bug and not unbounded
recursion** — it is frame size × recursion count.

Two compounding causes:

1. **`step_loop`'s frame is huge.** In debug, the function holds the `Vec<Cont>`,
   `acc`, and a ~30-arm `match` whose branch locals are laid out
   **non-overlapping** → the frame is the *sum* of all arms (~40 KB), paid on
   every recursion.
2. **Block-taking builtins still recurse `step_loop`.** Only Smol *method* calls
   and `value`/`syncValue`/`ensure:`/`on:do:`/`then:` (the last fixed in C31 S5)
   are flattened onto the `Cont` stack. `_primHCRun`, `_primNewValue`, the
   signal-handler eval in `signal_condition`, `do:`/`to:do:`, `namespace do:`,
   `_primInstancesOf:` blocks, etc. all call `ev.eval_block(...)` → a fresh
   recursive `step_loop`.

**Why C31 surfaced it:** `HandlerContext`-as-entity hid the recursion — nested
`run:` was a cross-entity send, so each level got its *own* coroutine with a
fresh stack. `HandlerContext`-as-Value (required to keep the tx coordinator from
auto-enrolling in its own transaction) makes nested `run:` synchronous, stacking
~6 huge frames in one coroutine.

**Why it matters beyond C31:** the iterative evaluator exists to keep per-entity
coroutine stacks small so many entities can coexist. A ceiling of ~6 nested
synchronous block-builtins per 256 KB defeats that. "Iterative" is only half-true
until this is fixed.

## Goals / Ensures

- **Ensures:** nested synchronous block-taking builtins do not grow the Rust
  stack unboundedly — depth is bounded by a small constant regardless of Smol
  nesting.
- **Ensures:** the 3 `#[ignore]`'d nested-HC tests
  (`handler_context_most_recent_handler_matches_first`,
  `cross_entity_hc_inner_handler_takes_priority`,
  `cross_entity_retry_targets_nearest_hcrun_boundary`) pass at 256 KB.
- **Ensures:** `step_loop`'s per-call frame is small (target: low single-digit
  KB debug), so any residual recursion is cheap.
- **Ensures:** `Cont::CallFrame` is split into clear, non-overloaded variants.

## Workstreams

### A. Shrink the `step_loop` frame
- Extract each large `match` arm body into a dedicated `#[inline(never)]` helper
  fn (`fn handle_<arm>(ev, &mut stack, &mut acc, ...) -> ...`). The arm locals
  then live in the helper's frame (created and freed within one loop iteration),
  not summed into `step_loop`'s frame. `step_loop` keeps only the loop, `stack`,
  `acc`, and the dispatch.
- Box large `Cont` variants (notably `CallFrame`) so `size_of::<Cont>()` and the
  `Vec<Cont>` element size shrink. Measure `size_of::<Cont>()` before/after.

### B. Refactor `Cont::CallFrame` (de-overload)
`CallFrame` currently multiplexes method-frame / block-frame / program-frame via
`is_method`, `pops_env`, `sync`, `frame_block_home`, `block_home_self`,
`self_binding`, `defining_class` — bools + callsite-specific handling that is
bug-prone and inflates the variant. Split into distinct variants (e.g.
`MethodFrame`, `BlockFrame`, `ProgramFrame`) carrying only their relevant fields,
so the per-arm logic is explicit and the enum shrinks. Coordinate with A (smaller
variants help frame size).

### C. Flatten the remaining block-taking builtins
Convert recursive `ev.eval_block(...)` call sites to push step-loop frames /
`Cont`s instead, so nesting accumulates on the heap `Cont` stack, not the Rust
stack. Priority order (by how much they nest in practice):
1. `_primHCRun:` — the direct cause of the ignored tests. Needs a `Cont` that
   pushes/pops the handler_chain around the body and supports the retry loop
   (analogous to `OnDoCatch`/`HandleOnDoResult`).
2. `signal_condition` handler eval — running a handler block while signaling.
3. `_primNewValue:` / `_primNewValueIndexed:init:` init blocks.
4. `do:` / `to:do:` iteration blocks; `namespace do:`; `_primInstancesOf:`.

Some of these (signal/HCRun) interact with error unwinding (`handle_step_error`,
`unwind_stack_to_depth`) — those paths must learn any new `Cont` variants.

## Steps (suggested ordering)
- **S0**: Add a debug-only `step_loop` depth/headroom probe (feature-gated) to
  measure before/after; record `size_of::<Cont>()`.
- **S1**: Workstream B — split `CallFrame` into distinct `Cont` variants; update
  `eval_block`/`invoke_method`/unwind paths. Suite stays green.
- **S2**: Workstream A — extract `match` arms into `#[inline(never)]` helpers; box
  remaining large variants. Re-measure frame size + depth-6 headroom.
- **S3**: Workstream C(1) — flatten `_primHCRun:`; un-`#[ignore]` the 3 nested-HC
  tests; confirm they pass at 256 KB.
- **S4**: Workstream C(2–4) — flatten `signal_condition` handler eval,
  `_primNewValue:`, iteration blocks. Re-measure.
- **S5**: `cargo fmt` + `cargo clippy --all-targets`; full suite green (no ignores
  for this cause); document the final per-level stack cost.

## Verification
- The 3 nested-HC tests pass at 256 KB (un-ignored).
- A stress test: deeply nested synchronous `run:`/`on:do:`/`ensure:` (e.g. depth
  20+) completes within 256 KB.
- `size_of::<Cont>()` and measured max `step_loop` depth recorded before/after.
- Full interpreter suite green.

## After this chunk — resume C31
- **S5b**: nested `atomic:` integration tests (Child path already coded).
- **S5c**: deadlock detection (`Timer`-based, graph keyed on `TxCtxId`).
- **S6**: full C31 test matrix (incl. parallel children, sibling serialization).

## Notes / open
- Release builds already "use noticeably less" (local overlap + inlining), so the
  overflow is worst in debug; the fix must hold in debug (tests run debug).
- The recursive evaluator (`eval_recursive`/`kernel_recursive.smol`) is being
  retired and is out of scope; C28 HC tests were switched to iterative-only
  `eval` because the recursive kernel still has `HandlerContext` as an entity.
