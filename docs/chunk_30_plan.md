### Chunk 30: Evaluator Promotion + Boolean Smol Migration + Stack Reduction

## Goal

Complete the iterative evaluator transition by making it the default path for all
code, not just the `IEvaluator` wrapper. Move the Boolean protocol to `kernel.smol`
as the first major builtin-to-Smol migration. Reduce the `may` coroutine stack size
to reflect the dramatically lower Rust stack usage.

Deliverables:

- `Evaluator::eval_block`, `invoke_method`, `invoke_class_method`, `eval_program`
  replaced by the iterative (step-loop) versions; old recursive implementations
  renamed to `eval_block_recursive` etc. as reference implementations.
- `True`, `False`, `Boolean` classes in `kernel.smol` with the full Boolean
  dispatch protocol (`ifTrue:`, `ifFalse:`, `and:`, `or:`, `not`, etc.).
- Rust bool dispatch in `builtins.rs` routes through `True`/`False` class lookup;
  dead Rust arms for the above selectors removed.
- Entity spawn uses the updated `Evaluator` directly (no `IEvaluator` indirection).
- `COROUTINE_STACK_SIZE` reduced to a profiled safe target (≤ 32 KB expected).
- `cargo test` green; `cargo fmt`; `cargo clippy --all-targets` clean.

---

## Background

### Why promote rather than keep two types?

C29 introduced `IEvaluator` as a wrapper around `Evaluator` to avoid changing
`builtins.rs`. Builtins call `ev.eval_block(...)` via `&Evaluator`; as long as
`Evaluator::eval_block` is the recursive version, builtins still recurse into the
Rust stack.

Promotion (replacing `Evaluator::eval_block` with the iterative version) is the
clean resolution: builtins continue to call `ev.eval_block` with no signature
changes, but those calls now run step loops instead of recursing. The Rust stack
depth per builtin that calls `eval_block` drops from ~6–8 frames to ~3–4 frames,
and within each resulting step loop, user-defined method calls use heap `CallFrame`s.

The dual-evaluator learning value is preserved via `eval_block_recursive` — the old
recursive implementation stays in the file, renamed, callable for differential tests.

### Boolean protocol in Smol

`Value::Bool` currently dispatches via a Rust match in `dispatch_bool`. The
`ifTrue:`, `ifFalse:`, `and:`, `or:` etc. arms are effectively Smol-level methods
written in Rust — they receive a block argument and call `ev.eval_block`. After
promotion, `eval_block` is iterative, so these can move to `kernel.smol` with no
loss of performance or correctness.

Moving them establishes the pattern for future builtin-to-Smol migrations:
- Define the class hierarchy in `kernel.smol`
- Add the methods using existing primitives and block evaluation
- Update the Rust dispatch to route through class lookup first
- Remove the dead Rust arms

`True` and `False` as real kernel classes also makes `isKindOf:`, `isNil`, and
class-hierarchy queries work correctly on booleans — something the current
`Value::Bool` approach makes awkward.

### Stack size target

After promotion, the Rust stack depth during a `may` entity message handler is
bounded by:
- The depth of *nested builtin calls that invoke `eval_block`* — typically 3–5 for
  real programs (e.g. `on:do:` → body contains `do:` → element handler contains
  `ensure:`)
- Each such level adds ~3–4 Rust frames × ~300–600 B ≈ ~1–2 KB

Peak realistic depth: ~5–10 KB. Add headroom for `may` internal machinery (~4 KB)
and edge cases. Target: 16–32 KB. Profile first; do not guess.

---

## Steps

### S1: Promote iterative methods to `Evaluator`

In `eval.rs`:

1. Rename the current `eval_block` to `eval_block_recursive`. Update its doc comment:
   `/// Reference implementation retained for differential testing. Production code uses eval_block.`
   Same renames for `invoke_method → invoke_method_recursive`,
   `invoke_class_method → invoke_class_method_recursive`,
   `eval_program → eval_program_recursive`.

2. Add new `eval_block`, `invoke_method`, `invoke_class_method`, `eval_program`
   methods on `Evaluator` that call the corresponding `step_loop` from
   `eval_iterative.rs`. These are the same implementations as on `IEvaluator`; the
   `step_loop` function takes `&Evaluator` so no refactoring of `step_loop` is needed
   — `self` is already a `&Evaluator`.

3. `IEvaluator` in `eval_iterative.rs` can now be a trivial one-line wrapper if it
   still needs to exist for differential test infrastructure; otherwise remove it and
   update tests to call `Evaluator` directly.

**Verify:** `cargo test` passes. All tests that previously called the recursive
path now exercise the iterative path. Differential tests in `tests/eval.rs` compare
`eval_block` vs. `eval_block_recursive` on the same programs and confirm equal output.

---

### S2: Boolean class hierarchy in `kernel.smol`

Add to `kernel.smol`, before any code that references booleans:

```smalltalk
Object subclass: #Boolean
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.

Boolean subclass: #True
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.

Boolean subclass: #False
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
```

These are class-entity singletons (same as any other class). `Value::Bool(true)`
and `Value::Bool(false)` continue to be the runtime representation; they are NOT
converted to `Value::Instance`. Instead, the Rust bool dispatch is updated in S3
to route through the `True` / `False` class lookup.

---

### S3: Boolean protocol in `kernel.smol`

Add instance methods to `True` and `False`. The complete minimal protocol:

```smalltalk
True >> ifTrue: aBlock              [ ^aBlock value ]
True >> ifFalse: aBlock             [ ^nil ]
True >> ifTrue: t ifFalse: f        [ ^t value ]
True >> ifFalse: f ifTrue: t        [ ^t value ]
True >> and: aBlock                 [ ^aBlock value ]
True >> or: aBlock                  [ ^true ]
True >> not                         [ ^false ]
True >> xor: aBool                  [ ^aBool not ]
True >> printString                 [ ^'true' ]
True >> = aBool                     [ ^aBool ]

False >> ifTrue: aBlock             [ ^nil ]
False >> ifFalse: aBlock            [ ^aBlock value ]
False >> ifTrue: t ifFalse: f       [ ^f value ]
False >> ifFalse: f ifTrue: t       [ ^f value ]
False >> and: aBlock                [ ^false ]
False >> or: aBlock                 [ ^aBlock value ]
False >> not                        [ ^true ]
False >> xor: aBool                 [ ^aBool not ]
False >> printString                [ ^'false' ]
False >> = aBool                    [ ^aBool not ]
```

**Note on `true` and `false` literals:** the kernel.smol methods use `true` and
`false` as variable references. These must be bound in the bootstrap environment.
`ClassRegistry::populate_env` should bind `true → Value::Bool(true)` and
`false → Value::Bool(false)` alongside class names. Verify this is already the case;
add if not.

---

### S4: Update Rust bool dispatch to route through class lookup

In `builtins.rs`, `dispatch_bool` (or wherever `Value::Bool` dispatch lives):

```
Requires: recv is Value::Bool(b); selector and args are the message.
Ensures: if the True (b=true) or False (b=false) class in the registry defines a
  user method for selector, invoke it. Otherwise fall through to Rust primitives for
  selectors that are not yet in kernel.smol (e.g. comparison operators, hash).
```

Implementation: look up `"True"` or `"False"` class from `ev.registry.kernel()`;
call `class.lookup(&selector)`. If found, call `ev.invoke_method(...)`. Otherwise,
fall through to the existing Rust match for remaining primitives.

**Remove** the following dead Rust arms after verifying the Smol methods cover them:
`ifTrue:`, `ifFalse:`, `ifTrue:ifFalse:`, `ifFalse:ifTrue:`, `and:`, `or:`, `not`,
`xor:`, `printString` for booleans.

**Retain** in Rust: `=` (between bool and non-bool types), `hash`, `_primHash`,
comparison to nil, and any selector not defined in kernel.smol.

Tests:
- `true ifTrue: ['yes']` → `'yes'`
- `false ifFalse: ['no']` → `'no'`
- `true ifTrue: ['t'] ifFalse: ['f']` → `'t'`
- `(true and: [false])` → `false`
- `true not` → `false`
- `true isKindOf: Boolean` → `true` (class hierarchy now queryable)
- `true isKindOf: True` → `true`
- `true isKindOf: False` → `false`
- `true printString` → `'true'`

`cargo test` green.

---

### S5: Wire entities to updated `Evaluator`

`entity.rs` creates `Evaluator::with_registry(registry)` for each entity's message
loop. Since `Evaluator::eval_block` is now iterative, entities automatically use the
iterative path. No changes to `entity.rs` are needed unless `IEvaluator` was
explicitly introduced there during C29 experimentation.

If `IEvaluator` references exist in `entity.rs`, replace them with `Evaluator`.
Remove any `IEvaluator`-specific scaffolding that is no longer needed.

`cargo test` green (entity tests).

---

### S6: Profile and reduce `COROUTINE_STACK_SIZE`

Write a measurement test that approximates peak stack usage:

```rust
#[test]
fn measure_entity_stack_depth() {
    // Program that exercises: nested on:do:, ensure:, whileTrue:, do:, super send
    // at the same time — the "maximally nested builtins" scenario.
    let src = r#"
        | result |
        [
            [ [ [ 1 to: 10 ] do: [:i |
                ensure: [result := result + i] ensure: [nil]
            ] ] on: Error do: [:e | nil]
        ] on: Condition do: [:c | nil]
    "#;
    // Spawn this in an entity and measure the stack pointer delta.
    // Use a Rust stack probe or compare addresses of local variables at entry vs. deepest point.
}
```

After measuring, update `entity.rs`:

```rust
/// Coroutine stack size per entity.
///
/// Sized to accommodate the maximum observed Rust stack depth during entity
/// message processing. The dominant cost is nested builtin block-evaluation calls
/// (e.g. on:do: containing ensure: containing do:). Measured peak: ~N KB; this
/// constant provides 2× headroom.
///
/// With the iterative evaluator, user-defined Smol method calls do not add Rust
/// stack frames — only builtin→eval_block nesting levels do.
const COROUTINE_STACK_SIZE: usize = N * 1024; // replace N based on measurement
```

Expected: 16–32 KB. If measurement shows < 8 KB peak, set to 16 KB. If > 16 KB
peak, investigate before proceeding.

**Stress test:** spawn 100 entities each exchanging 1000 messages; confirm no stack
overflow and total memory usage is proportional to entity count at the new stack size.

Update the `CLAUDE.md` note (currently says 128 KB with a concern about depth).

---

### S7: Final tests and cleanup

- All existing eval, entity, and kernel tests pass.
- `eval_block_recursive` is called only from differential test helpers.
- No warnings from `cargo clippy --all-targets`.
- `COROUTINE_STACK_SIZE` constant has a doc comment explaining the measurement basis.

`cargo fmt`. `cargo clippy --all-targets`. `cargo test`. Commit.

---

## Files changed

| File | Change |
|---|---|
| `crates/interpreter/src/eval.rs` | `eval_block` → `eval_block_recursive`; new iterative `eval_block` etc.; `true`/`false` bootstrap binding if missing |
| `crates/interpreter/src/eval_iterative.rs` | `IEvaluator` reduced to thin wrapper or removed; step_loop unchanged |
| `crates/interpreter/src/builtins.rs` | `dispatch_bool`: route through class lookup; remove dead `ifTrue:` etc. arms |
| `crates/interpreter/src/kernel.smol` | `Boolean`, `True`, `False` class hierarchy + full boolean protocol |
| `crates/interpreter/src/entity.rs` | `COROUTINE_STACK_SIZE` reduced + doc comment; any `IEvaluator` references removed |
| `crates/interpreter/src/tests/eval.rs` | Differential tests updated; stack measurement test; 100-entity stress test |
| `CLAUDE.md` | Update 128 KB coroutine note to reflect new measured constant |

---

## Deferred

- **General TCO** — tail-call optimization for recursive Smol user methods.
  Still deferred after this chunk. The loop primitives from C29.5 handle `whileTrue:`
  etc. General TCO should be prioritized before kernel.smol gains recursive algorithms
  (tree traversal, mutual recursion).

- **`to:do:`, `to:by:do:`** — numeric iteration forms. Can be Smol methods using
  `whileTrue:` after this chunk, or dedicated Rust loop primitives following the C29.5
  pattern. Easy to add as a small follow-on.

- **Additional builtin-to-Smol migrations** — collection protocol (`do:`, `collect:`,
  `select:`, `detect:`, `inject:into:`). These currently call `eval_block` in Rust
  loops; after this chunk those loops are correct and efficient. Migration to Smol is
  possible but not urgent — it would need either general TCO or dedicated loop
  primitives per collection type. Deferred.

- **Supervisor entities (C29 TODO)** — entity death notification to a supervisor.
  The stub comment in `run_entity_loop` remains.

- **Async I/O** — cooperative scheduling for I/O blocking; not addressed by the
  reduction counter (which handles CPU-bound scheduling only).
