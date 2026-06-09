### Chunk 28: Condition System

## Goa

Replace the current stub error mechanism (`EvalError::Signal(Arc<str>)` + broken
`on:do:`) with a full condition system inspired by Common Lisp, embedded within
Smol's actor model.

Deliverables:

- `{ }` dynamic array literals and `->` Association shorthand
- `Condition` / `Warning` / `Error` value hierarchy (replacing the current `Error` stub)
- `on:do:` corrected: class hierarchy matching, handler receives condition object
- `ensure:` unconditional cleanup block
- `HandlerContext` — non-unwinding pre-registered handlers (the CL `handler-bind` analog)
- Named restarts carried on condition objects
- `signal` vs `error` semantics: unhandled `Condition`/`Warning` continues; unhandled
  `Error` terminates the entity
- Warning stderr logging with configurable `#suppress` policy (for tests)
- Entity death stub: unhandled `Error` logs and terminates; supervisor hook documented

Cross-entity transactions (sagas) and the supervisor registration mechanism are
deferred.

---

## Background

### Two handler forms, two distinct semantics

**`[...] on: SomeError do: [:e | ...]`** — unwinding handler. The signal propagates as
`EvalError::Signal(condition)` until `on:do:` catches it, at which point the stack has
unwound. The handler can `return:` a value or `retry` the protected block. `resume:` in
this context has the same observable effect as `return:` — the signal site is gone.

**`HandlerContext new on: SomeError do: [:e | ...]; run: [...]`** — non-unwinding handler.
Handlers are pushed onto the evaluator's `handler_chain` *before* the body executes.
When `_primSignalCondition:` fires, it walks `handler_chain` first — no `Err(...)` has
propagated yet, so the signal site is still on the Rust call stack. `resume: v` makes
`_primSignalCondition:` return `Ok(v)`, delivering `v` directly back to the signal
call site.

Only if no handler handles the signal does `_primSignalCondition:` raise
`Err(EvalError::Signal(condition))`, which then unwinds to the nearest `on:do:`.

The "before vs. after" ordering is the structural reason `HandlerContext` exists:
a message sent to a block executes after block construction, which is too late to
register non-unwinding handlers.

### `EvalError::Signal` carries a Value, not a String

The current `EvalError::Signal(Arc<str>)` is why `on:do:` discards the condition class
argument and passes a raw string to the handler. The fix — `EvalError::Signal(Value)`
where Value is the condition object — cascades to every match site in `eval.rs` and
`builtins.rs`. Internal sites that currently create `Signal(message_string)` (e.g.
`dispatch_primitive_fallback`, index-out-of-bounds guards) use a Rust-level helper
`make_error_value(registry, message)` that constructs an `Error` instance directly
without calling kernel, bypassing `initialize`. These internal errors are not
resume-able, so the absent `restarts` ivar (nil) is acceptable.

### `resume:` is only meaningful from non-unwinding handlers

Once `Err(EvalError::Signal(...))` has propagated through the call stack, the signal-site
frames are gone. True `resume:` (returning a value to the `signal` call site) is only
implementable by calling the handler before propagating the error. From an unwinding
`on:do:` handler, `e resume: v` and `e return: v` are identical: both make the `on:do:`
expression return `v`. This is a deliberate and honest constraint, not a limitation to
paper over.

### Restarts belong to the condition object

The signaling code builds a condition object with named restart blocks before calling
`signal`. The restart block closes over the signaling context, which is what allows it
to retry or recover the original operation. The handler invokes a restart by name;
`invokeRestart:` calls the block and pipes the result through `resume:`.

### signal vs error semantics

| Class              | If unhandled                                       |
| ------------------ | -------------------------------------------------- |
| `Condition` (base) | Return nil from signal site — silent continue      |
| `Warning`          | Log per policy, return nil — audible but non-fatal |
| `Error`            | Terminate entity, log; supervisor notified (stub)  |

This maps to the actor model's "let it crash" philosophy: errors nobody handles surface
visibly (entity death is observable), while informational signals nobody observes are
silent.

### Dynamic array literals

`#(...)` array literals are static-only (compile-time constants). `{ expr. expr }`
dynamic array literals evaluate their elements at runtime. They enable:

```smalltalk
HandlerContext
    on: { NetworkError -> [:e | e invokeRestart: #retry].
          FileError    -> [:e | e return: nil] }
    run: [self processFiles]
```

`->` already lexes as `BinOp`; adding `Object >> -> aVal [ ^Association key: self value: aVal ]`
to kernel.smol is the only kernel change needed.

---

## Steps

### S1: Dynamic array literals `{ }` and `Object >> ->`

**Lexer** (`lexer.rs`): add `Token::LBrace` and `Token::RBrace` for `{` and `}`. Both
are currently unused in the grammar; no conflicts.

**AST** (`ast.rs`): add `Expr::DynamicArray(Vec<Expr>)`.

**Parser** (`parser.rs`): in `parse_primary`, when `Token::LBrace` is seen, consume it
and enter `parse_dynamic_array`. This production parses zero or more expressions
separated by `.`, ignoring a trailing `.`, until `Token::RBrace`. Returns
`Expr::DynamicArray(elements)`. Empty `{}` is valid and produces a zero-element Array.

**Eval** (`eval.rs`): `Expr::DynamicArray` — evaluate each element in order; allocate an
Array via the existing `_primNew:` path; fill slots with `_primAt:put:`.

**kernel.smol**: add one line:

```smalltalk
Object >> -> aVal [ ^Association key: self value: aVal ]
```

Tests: `{ 1 + 1. 3. 'hello' }` is a 3-element Array; `{}` is an empty Array;
`#foo -> 42` is an Association; `{ #a -> 1. #b -> 2 }` is a 2-element Array of
Associations; `{ #a -> 1. #b -> 2 } asDictionary` is a Dictionary.

`cargo test` green.

---

### S2: Condition hierarchy in kernel.smol + `EvalError` refactor

**`EvalError`** (`eval.rs`):

- Change `Signal(Arc<str>)` to `Signal(Value)`.
- Add three new variants:
  
  ```rust
  #[error("resume")]
  Resume(Value),
  #[error("return from block")]
  ReturnFromBlock(Value),
  #[error("retry")]
  Retry,
  ```
- Update `#[error(...)]` on `Signal` to display `{0}` (the condition's Display impl will
  show its message text).
- Update every match site in `eval.rs`, `builtins.rs`, and test helpers.

**`make_error_value` helper** (`builtins.rs`): a Rust-level function that constructs
an `Error` instance directly from the registry without calling kernel (no `initialize`,
no `pvtInit`). Sets the `message` ivar; leaves `restarts` nil. Used by
`dispatch_primitive_fallback` and all other signal sites that currently produce
`EvalError::Signal(string)`. Add `registry: &ClassRegistry` (or `ev: &Evaluator`)
parameter to `dispatch_primitive_fallback` to make the registry accessible; update all
call sites (all are within `builtins.rs` and already have `ev` in scope).

**kernel.smol** — replace the current `Error` stub entirely:

```smalltalk
Value subclass: #Condition
    instanceVariableNames: 'message restarts'
    classVariableNames: ''
    indexedSlots: #none.

Condition class >> withMessage: aString [
    | c |
    c := self _primNew.
    c pvtInit.
    c pvtSetMessage: aString.
    ^c
]
Condition class >> signal: aString [ ^(self withMessage: aString) signal ]

Condition >> pvtInit         [ message := ''. restarts := Dictionary new ]
Condition >> pvtSetMessage: s [ message := s ]
Condition >> message          [ ^message ]
Condition >> messageText      [ ^message ]
Condition >> addRestart: name block: aBlock [
    restarts at: name put: aBlock. ^self
]
Condition >> hasRestart: name     [ ^restarts includesKey: name ]
Condition >> availableRestarts    [ ^restarts keys ]
Condition >> invokeRestart: name [
    | blk |
    blk := restarts at: name ifAbsent: [self error: 'no restart named: ', name].
    ^self resume: blk value
]
Condition >> invokeRestart: name with: arg [
    | blk |
    blk := restarts at: name ifAbsent: [self error: 'no restart named: ', name].
    ^self resume: (blk value: arg)
]
Condition >> signal   [ ^self _primSignalCondition: self ]
Condition >> resume: v [ ^self _primResume: v ]
Condition >> return: v [ ^self _primReturn: v ]
Condition >> retry     [ ^self _primRetry ]

Condition subclass: #Warning
    instanceVariableNames: ''
    classVariableNames: 'LoggingPolicy'
    indexedSlots: #none.

Warning class >> initialize          [ LoggingPolicy := #stderr ]
Warning class >> loggingPolicy: sym  [ LoggingPolicy := sym ]
Warning class >> loggingPolicy       [ ^LoggingPolicy ]

Condition subclass: #Error
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
```

Update `Object >> error: aString` (no longer needs `await` since `signal` is now
synchronous within the kernel):

```smalltalk
Object >> error: aString [ Error signal: aString ]
```

Note: `Warning class >> initialize` requires class-side initialize to be called at
bootstrap. If the class entity `initialize` wiring does not cover this yet, default
`LoggingPolicy` to `#stderr` in the Rust unhandled-warning path directly as a fallback
(see S6).

Tests: `Condition withMessage: 'x' message` → `'x'`; `Error new isKindOf: Condition`
→ `true`; `Warning new isKindOf: Error` → `false`; `Warning new isKindOf: Condition`
→ `true`.

`cargo test` green (involves updating every `EvalError::Signal` match site).

---

### S3: `is_subclass_of` and `_primSignalCondition:`

**`class.rs`**: add:

```rust
/// Returns true if `child` is `parent` or a subclass of `parent` (by walking
/// the superclass chain using pointer equality).
///
/// Requires: both classes are live (not mid-definition).
/// Ensures: returns true iff child == parent or parent appears in child's
///   superclass chain.
pub fn is_subclass_of(child: &Arc<Class>, parent: &Arc<Class>) -> bool
```

**`eval.rs`**: add to `Evaluator`:

```rust
/// Handler chain for non-unwinding (HandlerContext) handlers.
/// Each entry is (condition_class, handler_block). Entries are pushed when a
/// HandlerContext begins execution and popped when it completes (success or error).
/// _primSignalCondition: walks this in reverse (most-recently-pushed first) before
/// raising EvalError::Signal.
pub handler_chain: RefCell<Vec<(Arc<Class>, Arc<Block>)>>,
```

Initialized to empty `Vec`.

**`builtins.rs`** — add `_primSignalCondition:` dispatched on any Condition instance.
It is also the new implementation behind `_primSignal:` (kept as a legacy shim that
wraps its string argument in a bare Error for backward compatibility during transition):

```
Requires: args[0] is a Value whose class is a subclass of Condition.
Ensures: if a matching non-unwinding handler is found, that handler's outcome
  determines the return:
    - handler returns Ok(v)         → Ok(v)
    - handler raises Resume(v)      → Ok(v)
    - handler raises ReturnFromBlock(v) → Err(ReturnFromBlock(v))  propagates
    - handler raises Retry          → Err(Retry)                   propagates
    - handler re-signals            → fall through to raise
  If no handler matches, raises Err(EvalError::Signal(condition)).
  (Unhandled policy for Warning/Condition applied in S6.)
```

Implementation: snapshot `ev.handler_chain.borrow().clone()`; iterate in reverse;
for each `(handler_class, handler_block)` where `is_subclass_of(cond_class, handler_class)`,
call `ev.eval_block(&handler_block, vec![condition.clone()], env)` and match the result
as above. A re-signal breaks out of the loop and falls through to raise.

A helper `get_condition_class(ev, value) -> Option<Arc<Class>>` extracts the class from
a `Value::Instance` InstanceState; returns `None` for primitive values.

Tests: `[Error signal: 'x'] on: Error do: [:e | e message]` → `'x'`;
`[Error signal: 'x'] on: Condition do: [:e | 'caught']` → `'caught'` (hierarchy match);
`[Warning signal: 'w'] on: Error do: [:e | 'caught']` propagates (Warning ≢ Error).

`cargo test` green.

---

### S4: Fixed `on:do:`, `ensure:`

**`on:do:` in builtins.rs** (Block dispatch) — complete rewrite of the existing stub:

```
Requires: args[0] is a class (subclass of Condition); args[1] is a Block.
Ensures: evaluates body; if body raises EvalError::Signal(cond) where cond's class
  is a subclass of args[0], calls handler with cond:
    - handler returns Ok(v)         → Ok(v)
    - handler raises Resume(v)      → Ok(v)   (resume = return in unwinding context)
    - handler raises ReturnFromBlock(v) → Ok(v)
    - handler raises Retry          → re-execute body (loop)
    - handler raises other error    → propagate
  If cond's class does not match, propagate EvalError::Signal(cond) unchanged.
  EvalError::Return propagates unchanged (non-local method return must not be caught).
```

The body eval is wrapped in a `loop` to support `Retry`.

**`ensure:` in builtins.rs** (Block dispatch):

```
Requires: args[0] is a Block (the ensure block).
Ensures: body block is evaluated; ensure block is always evaluated regardless of
  outcome; body's result (Ok or Err) is returned. Ensure block result is discarded.
  EvalError::Return propagates after ensure runs.
```

Tests: `[Error signal: 'x'] on: Error do: [:e | e message]` → `'x'`;
retry: `| n | n := 0. [n := n + 1. n < 3 ifTrue: [Error signal: 'x']. n] on: Error do: [:e | e retry]` → `3`;
ensure: a ValueHolder accumulator confirms ensure block runs on both success and error paths.

`cargo test` green.

---

### S5: Condition control-flow primitives

**builtins.rs** — three new `_prim*` methods dispatched on Condition instances:

- `_primResume: v` → `Err(EvalError::Resume(v))`
- `_primReturn: v` → `Err(EvalError::ReturnFromBlock(v))`
- `_primRetry`     → `Err(EvalError::Retry)`

These raise non-local control-flow errors that are caught by `_primSignalCondition:`
(for Resume) or by `on:do:` / `HandlerContext >> run:` (for ReturnFromBlock and Retry).
kernel.smol already has `Condition >> resume:`, `return:`, `retry` delegating to these
(added in S2).

Tests: confirm Resume propagates through `on:do:` and HandlerContext; ReturnFromBlock
and Retry are caught at the right boundary.

`cargo test` green.

---

### S6: HandlerContext

**kernel.smol**:

```smalltalk
Object subclass: #HandlerContext
    instanceVariableNames: 'pairs'
    classVariableNames: ''
    indexedSlots: #none.

HandlerContext >> initialize        [ pairs := OrderedCollection new ]
HandlerContext >> on: cls do: blk   [ pairs add: (cls -> blk). ^self ]
HandlerContext >> on: anArray run: body [
    anArray do: [:assoc | pairs add: assoc].
    ^self run: body
]
HandlerContext >> pairs             [ ^pairs ]
HandlerContext >> run: body         [ ^self _primHCRun: body ]
```

**`_primHCRun:` in builtins.rs** (dispatched on HandlerContext instances):

```
Requires: args[0] is a Block (body). self's `pairs` is an OrderedCollection of
  Associations (key = condition class, value = handler block).
Ensures: handler pairs are pushed onto ev.handler_chain for the duration of body's
  execution and are popped when body completes, whether normally or via error.
  Body is re-executed on Retry. ReturnFromBlock(v) → Ok(v).
  EvalError::Return propagates unchanged.
```

Implementation:

1. Extract pairs from HandlerContext ivar; build `Vec<(Arc<Class>, Arc<Block>)>`.
2. Push entries onto `ev.handler_chain`.
3. Install a RAII `HandlerChainGuard` that truncates `ev.handler_chain` by the pushed
   count on drop — ensures cleanup on all exit paths.
4. Execute body in a loop (for Retry):
   
   - `Ok(v)` → return `Ok(v)` (guard drops, handlers popped).
   
   - `Err(EvalError::ReturnFromBlock(v))` → return `Ok(v)`.
   
   - `Err(EvalError::Retry)` → continue loop.
   
   - `Err(EvalError::Return(v, d))` → return `Err(Return(v, d))` (non-local method return).
   
   - `Err(other)` → return `Err(other)`.

Tests: non-unwinding `resume:` delivers value to signal site; `return: v` exits `run:`
returning `v`; `retry` re-executes body; handler cleanup on propagated error (verify
`handler_chain` is empty after unhandled propagation); multi-handler ordering (most
recently pushed matches first).

`cargo test` green.

---

### S7: Named restarts

kernel.smol already contains `addRestart:block:`, `invokeRestart:`, and
`invokeRestart:with:` from S2. These are purely Smol-level — no new interpreter
primitives are required.

Tests:

```smalltalk
"invokeRestart: delivers value to signal site via resume:"
| result |
result := HandlerContext new
    on: Error do: [:e | e invokeRestart: #useDefault];
    run: [
        Error new
            addRestart: #useDefault block: ['fallback'];
            signal
    ].
"result = 'fallback'"

"invokeRestart:with: passes argument to restart block"
HandlerContext new
    on: Error do: [:e | e invokeRestart: #useValue: with: 99];
    run: [
        Error new
            addRestart: #useValue: block: [:v | v];
            signal
    ].
"→ 99"
```

`cargo test` green.

---

### S8: Unhandled policy — `signal` vs `error` semantics

**`_primSignalCondition:` unhandled path** (completing the stub left in S3):

When no handler in `handler_chain` matches and `EvalError::Signal` is about to be
raised, first apply the unhandled policy based on the condition's class:

- **Condition (base, not Warning or Error)**: return `Ok(Value::Nil)` — silent continue.
- **Warning**: check `Warning loggingPolicy` (class variable; if nil/unreachable,
  default to `#stderr`):
  - `#stderr`: `eprintln!("WARNING: {message}")` then `Ok(Value::Nil)`.
  - `#stdout`: `println!(...)` then `Ok(Value::Nil)`.
  - `#suppress`: `Ok(Value::Nil)` silently.
- **Error** (and any other subclass not caught above): raise
  `Err(EvalError::Signal(condition))` to unwind toward `on:do:` or the entity loop.

**`run_entity_loop` in entity.rs** — unhandled Error termination:

When an entity's handler dispatch returns `Err(EvalError::Signal(condition))` (i.e.,
no `on:do:` caught it), extract the message and:

```rust
eprintln!("Entity {} ({}) terminated: {}", data.id.0, class_name, condition_message);
data.signal_shutdown();
// TODO C29: send crash notification to supervisor entity (EntityData.supervisor field
// not yet present; see open_questions.md Q2 and supervisor design notes).
break;
```

Tests:

- `Warning signal: 'w'` with `Warning loggingPolicy: #suppress` completes without error.
- Unhandled `Error signal: 'x'` inside an entity handler terminates the entity (verify
  via lifecycle state check after join).
- `[Error signal: 'x'] on: Condition do: [:e | 'caught']` still catches (Error ⊆ Condition).

`cargo test` green.

---

### S9: Kernel audit and full test coverage

Audit every method in `kernel.smol` that calls `self error:` or uses `on:do:`:

- Verify it reaches the new condition hierarchy correctly.
- Confirm both branches of every conditional in condition-related code are exercised.
- Update any test that checks `EvalError::Signal` by string content to check the
  condition object's `message` instead.

Existing error-path tests (`detect: no element`, `at: out of bounds`, `key not found`,
etc.) must still pass with the new `EvalError::Signal(Value)` type. If
`make_error_value` is used for these internal signals, confirm `on:do:` catches them
via the class hierarchy (Error ⊆ Condition).

`cargo fmt`, `cargo clippy --all-targets`, `cargo test` all green. Commit.

---

## Files changed

| File                                   | Change                                                                                                                                                                                                                                 |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/interpreter/src/lexer.rs`      | Add `Token::LBrace`, `Token::RBrace`                                                                                                                                                                                                   |
| `crates/interpreter/src/ast.rs`        | Add `Expr::DynamicArray(Vec<Expr>)`                                                                                                                                                                                                    |
| `crates/interpreter/src/parser.rs`     | `parse_dynamic_array`; `Expr::DynamicArray` in `parse_primary`                                                                                                                                                                         |
| `crates/interpreter/src/eval.rs`       | `EvalError::Signal(Value)` (was `Arc<str>`); add `Resume(Value)`, `ReturnFromBlock(Value)`, `Retry` variants; add `handler_chain` field; `Expr::DynamicArray` eval; update all `Signal` match sites                                    |
| `crates/interpreter/src/class.rs`      | Add `is_subclass_of(child, parent) -> bool`                                                                                                                                                                                            |
| `crates/interpreter/src/builtins.rs`   | `make_error_value` helper; `_primSignalCondition:`; `_primSignal:` shim; `_primResume:`, `_primReturn:`, `_primRetry`; fixed `on:do:`; `ensure:`; `_primHCRun:` + `HandlerChainGuard`; add `ev` param to `dispatch_primitive_fallback` |
| `crates/interpreter/src/entity.rs`     | `run_entity_loop`: catch unhandled `EvalError::Signal`, log, terminate entity; supervisor stub comment                                                                                                                                 |
| `crates/interpreter/src/kernel.smol`   | `Condition`, `Warning`, `Error` hierarchy; `HandlerContext`; `Object >> ->`; remove old `Error` stub; update `Object >> error:`                                                                                                        |
| `crates/interpreter/src/tests/eval.rs` | Tests for all items above                                                                                                                                                                                                              |

---

## Deferred

- **Supervisor registration** — `EntityData.supervisor: Option<EntityId>` and
  `supervisedBy:` Smol method. The entity death path has a `TODO C29` stub comment.
  How much of supervision lives in Smol vs. the interpreter is a design question for
  a later chunk.
- **`isKindOf:` / `isMemberOf:` in kernel.smol** — natural companions to the condition
  hierarchy; the interpreter-level `is_subclass_of` check is sufficient internally.
  Small follow-on.
- **`ifCurtailed:`** — cleanup block that runs only on abnormal exit (complement of
  `ensure:`). Easy addition; not urgent.
- **`outer` / `pass`** — re-raise to the next outer handler. Needed for handler chains
  that want to observe without fully handling; deferred.
- **`Warning` class variable bootstrap** — if class-side `initialize` is not called
  automatically for kernel classes, the `LoggingPolicy` class variable may not be
  initialized. The Rust unhandled-warning path defaults to `#stderr` as a fallback,
  which is correct. Properly wiring class-side `initialize` at bootstrap is a
  separate item.
- **Condition history in the store** — unhandled errors as store external assertions
  would make crash history queryable (aligns with P3, P5). Deferred pending the
  visibility model (Q5) and supervisor design.
- **`on:do:on:do:` multi-clause cascade on Block** — a convenience shorthand for
  catching multiple classes in sequence without HandlerContext. Pure Smol, easy to add
  once HandlerContext is solid.
