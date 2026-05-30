# Chunk 26: Strict Block Assignment + Async-Safe Kernel Patterns

## Goal

Enforce a static rule — blocks may only assign to their own declared params/locals —
and rewrite `kernel.smol` to comply, using a set of canonical async-safe patterns.
Unify `then:` to handle both `Value::Future` and `Value::Block` receivers.

---

## Background and Motivation

### The paradigm gap

Smol's actor model makes every state mutation an async operation. Traditional
Smalltalk idioms (loop counters, accumulator variables, `inject:into:` via a
captured `acc`) mutate variables across block boundaries, which creates latent
cross-entity mutation hazards: a block created in entity A may be passed to entity
B, where it would mutate A's state from B's coroutine.

Because we cannot verify non-escape statically without full escape analysis, the
rule must be **strict**: within a block, assignment (`x := value`) is only
permitted if `x` is declared in that block's own params or `| locals |`. No
exceptions — not for method temporaries, not for instance variables.

This is a significant paradigm shift from typical Smalltalk. The pattern doc
(deliverable of this chunk) is a first mitigation; language-level tooling to
guide programmers is a future concern noted in Deferred.

### Why `then:` needs a Block arm

`then:` currently handles `Value::Future` (spawns async continuation) and any
other value (runs the argument block synchronously, treating the receiver as
already-resolved). A `Value::Block` receiver hits the catch-all and runs the
argument block immediately — the receiver block is never evaluated. This makes
`[loop] then: [holder get]` silently wrong.

With a Block arm, `then:` becomes the unified "do this, then do that" combinator:
evaluate the receiver block, then — if the result is a Future, chain; if not,
run the next block synchronously. `syncValue` remains for "evaluate this block
right now and give me the result."

---

## New canonical patterns (full treatment in `docs/Smol_Patterns.md`)

### 1. Index loops — replace `whileTrue:` with `to:do:`

Before (forbidden — `i := i + 1` in block):
```
| i |
i := 0.
[i < self _primSize] whileTrue: [
    aBlock value: (self _primAt: i).
    i := i + 1
]
```

After:
```
0 to: self _primSize - 1 do: [:i |
    aBlock value: (self _primAt: i)
]
```

`to:do:` is already a builtin. The index is a block parameter — legal to assign
to within the block (it is block-local).

### 2. Ivar mutation from a block — extract to private method + `then:`

Before (forbidden — `size := size - 1` is an ivar assignment inside a block):
```
(storage at: i) = v ifTrue: [
    "...shift loop..."
    size := size - 1.
    ^v
]
```

After:
```
(storage at: i) = v ifTrue: [
    (self pvtRemoveAt: i) then: [^v]
]
```

The `pvtRemoveAt:` method body does the shift and decrements `size` at method
level (not inside a block). `then:` ensures `^v` fires only after the mutation
completes. Without `then:`, `^v` could race with the async self-send.

`pvt`-prefixed methods are private by convention (self-send only); no language
enforcement in this chunk (see Deferred).

### 3. ValueHolder accumulator — `[loop] then: [holder get]`

For cross-iteration state (cannot be expressed as block-local):
```
inject: init into: aBlock [
    | holder |
    holder := ValueHolder initialValue: init.
    [self do: [:each |
        holder update: [:acc | aBlock value: acc value: each]
    ]] then: [holder get]
]
```

- The loop block queues `update:` messages to the holder actor.
- `then:` evaluates the loop block first (because `recv` is a Block), then runs
  `[holder get]`.
- By the time `holder get` is sent, all `update:` messages are already in the
  holder's mailbox. Actor message ordering ensures `get` sees the final value.
- Result is a `Value::Future` resolving to the accumulated value.

`update:` already exists: `ValueHolder >> update: aBlock [ self set: (aBlock value: self get) ]`.
`self get` and `self set:` inside `update:` are sync self-sends (same entity).

### 4. Non-local return as early-exit accumulator

`ok := false` accumulator patterns can often be replaced with `^false` non-local
return from within a block, eliminating the captured variable entirely:

```
"Before"
ok := true.
self associationsDo: [:assoc |
    (other at: assoc key ifAbsent: [ok := false. nil]) = assoc value
        ifFalse: [ok := false]
].
^ok

"After"
self associationsDo: [:assoc |
    (other at: assoc key ifAbsent: [^false]) = assoc value
        ifFalse: [^false]
].
^true
```

### 5. Recursive helper for non-range loops

Loops with modular counter arithmetic (e.g., open-address hash probing) that
cannot be expressed with `to:do:` use a recursive private method that passes the
current index as a parameter:

```
Dictionary >> pvtSlotFor: key [
    | cap h |
    cap := table size.
    h := key hash \\ cap.
    h < 0 ifTrue: [h := h + cap].
    ^self pvtSlotFor: key at: h
]

Dictionary >> pvtSlotFor: key at: i [
    | slot |
    slot := table at: i.
    (slot isNil or: [slot key = key]) ifTrue: [^i].
    ^self pvtSlotFor: key at: (i + 1) \\ table size
]
```

`slot` and the index `i` are method-level — no block assignment. `[slot key = key]`
only reads `slot` (read of a captured variable is fine). Sync self-sends keep
this efficient.

---

## Steps

- **S1** Fix `then:` in `builtins.rs` to handle `Value::Block` receivers:
  evaluate the receiver block, then — if result is Future, chain; otherwise run
  argument block synchronously. Add tests for `[loop] then: [continuation]` and
  `Future then: [block]` (existing behaviour unchanged).

- **S2** Write `docs/Smol_Patterns.md` with the five patterns above and worked
  examples for each. Lead with a **framing section** covering:
  - The core split: entities are the stateful units; blocks and value computations
    are pure. Mutation is legitimate in method bodies (the entity's own state);
    everything outside must be free of captured-variable side effects.
  - Why sequential-looking code is often concurrent: a statement sequence
    `self pvtFoo. ^v` and `(self pvtFoo) then: [^v]` look similar but have
    fundamentally different ordering guarantees.
  - Recursion as an alternative to ValueHolder for bounded-depth accumulation
    (sync self-sends make it practical; use ValueHolder for collection-scale
    iteration where stack depth is a concern).
  - Known open questions: non-escaping block annotation, language-level private,
    sync self-send optimisation.
  Also update `docs/Design_Principles_and_Key_Decisions.md` with a "Concurrency
  and mutation model" section capturing the entity/block split principle and the
  paradigm-shift tension (looks sequential, executes concurrently).

- **S3** Rewrite `kernel.smol` — `to:do:` loop replacements:
  `ByteArray >> do:`, `ByteArray >> copyFrom:to:`, `Array >> copyFrom:to:`,
  `OrderedCollection >> addFirst:`, `OrderedCollection >> removeFirst`,
  `OrderedCollection >> do:`, `OrderedCollection >> collect:`,
  `OrderedCollection >> copyFrom:to:`, `OrderedCollection >> = other`,
  `OrderedCollection >> pvtGrow`,
  `Dictionary >> do:`, `Dictionary >> keysDo:`, `Dictionary >> associationsDo:`,
  `Dictionary >> removeKey:ifAbsent:` (rehash loop), `Dictionary >> pvtRehash`.
  Cargo test green after this step.

- **S4** Rewrite `kernel.smol` — ivar-mutation and early-exit patterns:
  `OrderedCollection >> remove:ifAbsent:` → extract `pvtRemoveAt:`, use
  `(self pvtRemoveAt: i) then: [^v]`.
  `Dictionary >> = other` → replace `ok` accumulator with `^false` non-local
  returns.
  `Dictionary >> pvtSlotFor:` → recursive two-method form.
  Cargo test green after this step.

- **S5** Rewrite `kernel.smol` — ValueHolder accumulator patterns:
  `Array >> inject:into:`, `OrderedCollection >> inject:into:` → canonical
  ValueHolder + `then:` form.
  `Array >> printString`, `ByteArray >> printString`,
  `OrderedCollection >> printString`, `Dictionary >> printString` → ValueHolder
  string accumulation.
  Cargo test green after this step.

- **S6** Add parser strict block assignment check:
  - Add `block_scope_stack: Vec<HashSet<Arc<str>>>` to `Parser`.
  - In `parse_block`: push `(params ∪ locals)` before body, pop after.
  - In `parse_stmt` Assign branch: if `!block_scope_stack.is_empty()` and name
    not in `block_scope_stack.last()`, return
    `ParseError::NonLocalBlockAssignment(name)`.
  - Add the error variant to `ParseError`.
  - Cargo test: existing kernel tests should still be green (kernel was already
    rewritten in S3–S5). Parser tests for the new error.

- **S7** Audit `eval.rs` tests and `tests/eval.rs` — add tests for
  `inject:into:` (returns Future, resolves correctly), `printString` on
  collections (returns Future, resolves correctly), and the non-local-block-
  assignment parse error.

- **S8** `cargo fmt`, `cargo clippy --all-targets`, full test run, commit.

---

## Files changed

| File | Change |
|------|--------|
| `builtins.rs` | Add `Value::Block` arm to universal `then:` handler |
| `docs/Smol_Patterns.md` | New patterns reference doc |
| `kernel.smol` | Rewrites per S3–S5 |
| `parser.rs` | Add `block_scope_stack`; `NonLocalBlockAssignment` parse error |
| `ast.rs` | No change expected |
| `tests/builtins.rs` | Tests for `then:` on Block receiver |
| `tests/eval.rs` | Tests for new patterns; parser error tests |

---

## Deferred

- **Language-level private methods** — `pvt` prefix is convention only; no
  enforced visibility. Design of `private` keyword or pragma deferred.
- **Non-escaping block annotation** — a `{...}` or pragma form to mark a block
  as non-escaping, allowing the parser to relax the strict assignment rule for
  performance-critical inline control flow. Requires design work.
- **Sync self-send optimisation** — routing `self pvtFoo` directly to
  `invoke_method` without a round-trip through the actor mailbox. Noted in the
  pattern doc as a planned optimisation; the `then:` pattern remains correct
  with or without it.
- **Paradigm tooling** — three tiers identified:
  1. *Static*: warn when a message-send result (Future) is silently discarded at
     a statement boundary — catches the `self pvtFoo. ^v` race pattern at
     parse/lint time. Feasible as a parser-level lint once the strict block rule
     is in place.
  2. *Dynamic/observability*: the AST span tracker (planned later chunk) can
     annotate which expressions produced un-awaited Futures, surfacing implicit
     concurrency in a trace. Directly serves the "observable" project goal.
  3. *Syntax*: a `!` or `sync` keyword on sends to make async intent explicit
     (Rust `await` analogue) — forces the programmer to declare ordering intent;
     absence on a Future-producing send becomes a warning. Design deferred.
- **`printString` async ripple** — `printString` now returns a Future on
  collections, which ripples into any code that uses it expecting a String. Full
  audit of call sites deferred; the test suite will surface concrete failures.
