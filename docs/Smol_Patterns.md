# Smol Patterns — Async-Safe Coding Idioms

This document describes the canonical patterns for writing correct Smol code
under the actor model. Each pattern addresses a recurring situation where the
naive Smalltalk idiom is unsound in Smol's concurrent execution model.

---

## Framing: the entity / block split

The execution model rests on a clean split between two kinds of computation:

**Entities** are the stateful units. Every `Object` subclass instance is an
actor with its own mailbox and coroutine. Only the entity's own coroutine
executes its method bodies. Mutation of an entity's instance variables is
therefore safe — no other thread races on them — as long as the mutation
happens at *method level*, not inside a block.

**Blocks** are value-level computations. A block is a closure that can be
passed to another entity and evaluated in that entity's coroutine. If the
block captures and assigns an outer variable, it reaches back into the
*creating* entity's state from the *evaluating* entity's context. That is a
cross-entity mutation and violates the single-writer guarantee.

The strict rule that follows from this:

> **A block may only assign to variables declared in its own params or `| locals |`.**

Assignment to method temporaries, instance variables, or any outer scope is
forbidden inside a block body. The parser enforces this statically as
`ParseError::NonLocalBlockAssignment`.

### Why sequential-looking code is often concurrent

A statement sequence looks synchronous but is not:

```smalltalk
self pvtRemoveAt: i.
^v
```

`self pvtRemoveAt:` is a message send to `self`. Even as a self-send it is
processed asynchronously — it is queued and a `Value::Future` is returned.
The `^v` return executes immediately, before `pvtRemoveAt:` has run.

The correct form uses `then:` to establish the ordering:

```smalltalk
(self pvtRemoveAt: i) then: [^v]
```

`then:` chains the continuation so `^v` fires only after the self-send has
resolved. The two forms look nearly identical but have fundamentally different
ordering guarantees. This is the central hazard of writing Smol code.

### Recursion vs ValueHolder

For bounded-depth accumulation where the collection size is known to be small,
recursion via sync self-sends can replace ValueHolder:

```smalltalk
MyObj >> sumFrom: i [
    i >= self size ifTrue: [^0].
    ^(self at: i) + (self sumFrom: i + 1)
]
```

Each self-send is processed synchronously in the same entity coroutine. No
actor mailbox overhead, no Future. Use recursion for dictionary probing,
tree traversal, and similar bounded-depth patterns.

For collection-scale iteration (variable or large `n`), use
**Pattern 3 (ValueHolder accumulator)** — recursion at that scale exhausts
the coroutine stack.

### Known open questions

- **Non-escaping block annotation** — a `{...}` or `#nonEscaping` pragma to
  mark blocks that cannot be passed to another entity, allowing the parser to
  relax the strict assignment rule for performance-critical inline control flow.
- **Language-level private** — `pvt`-prefixed methods are private by
  convention only. A `private` keyword or pragma is deferred.
- **Sync self-send optimisation** — routing `self pvtFoo` directly to
  `invoke_method` without a mailbox round-trip. The `then:` pattern remains
  correct whether or not this optimisation is applied.

---

## Pattern 1 — Index loops with `to:do:`

**Problem**: A loop needs a counter. The naive form assigns the counter inside
a block, which violates the strict rule.

**Forbidden**:
```smalltalk
| i |
i := 0.
[i < self _primSize] whileTrue: [
    aBlock value: (self _primAt: i).
    i := i + 1          "i is a method temp — illegal inside a block"
]
```

**Canonical form**:
```smalltalk
0 to: self _primSize - 1 do: [:i |
    aBlock value: (self _primAt: i)
]
```

`to:do:` is a built-in that provides the index as a block parameter. The
block parameter `i` belongs to the block's own scope — assignment is legal.
Use `to:by:do:` for step increments other than 1.

---

## Pattern 2 — Ivar mutation from a block

**Problem**: A conditional inside a block needs to mutate an instance variable
(e.g., decrement `size` after removal) and then return a value.

**Forbidden**:
```smalltalk
(storage at: i) = v ifTrue: [
    "...shift loop..."
    size := size - 1.   "ivar assignment inside a block — illegal"
    ^v
]
```

**Canonical form**:
```smalltalk
(storage at: i) = v ifTrue: [
    (self pvtRemoveAt: i) then: [^v]
]
```

Extract the mutation into a `pvt`-prefixed method that operates at method
level. Use `then:` to ensure the continuation (`^v`) fires only after the
mutating self-send resolves. The `pvt` prefix signals private-by-convention;
callers should treat it as an implementation detail.

**Implementation**:
```smalltalk
MyCollection >> pvtRemoveAt: idx [
    idx + 1 to: size - 1 do: [:j |
        storage at: j - 1 put: (storage at: j)
    ].
    size := size - 1
]
```

All mutation happens at method level — no block assignments.

---

## Pattern 3 — ValueHolder accumulator

**Problem**: A loop needs to accumulate state across iterations, but the
accumulator cannot be a method temporary (that would require assignment from
inside a block).

**Canonical form**:
```smalltalk
inject: init into: aBlock [
    | holder |
    holder := ValueHolder initialValue: init.
    ^[self do: [:each |
        holder update: [:acc | aBlock value: acc value: each]
    ]] then: [holder get]
]
```

**How it works**:

1. `ValueHolder` is an actor. `initialValue:` creates it with `val = init`.
2. The receiver block `[self do: [...]]` is evaluated first by `then:`. For
   each element, `holder update: [...]` queues a message in the holder's
   mailbox. `self do:` is a self-send; it completes synchronously.
3. `then:` evaluates the receiver block; since the result is not a Future,
   it runs the continuation `[holder get]` immediately after.
4. `holder get` is a cross-entity send and arrives in the holder's mailbox
   *after* all `update:` messages. Actor FIFO ordering guarantees `get` sees
   the final accumulated value.
5. The method returns the Future from `holder get`, which resolves to the
   accumulated value.

**`ValueHolder` interface**:
```smalltalk
ValueHolder class >> initialValue: aVal   "create with val = aVal"
ValueHolder >> set: aVal                  "val := aVal"
ValueHolder >> get                        "^val"
ValueHolder >> update: aBlock             "self set: (aBlock value: self get)"
```

`update:` runs `get` and `set:` as synchronous self-sends within the holder's
own coroutine — no races.

**Caller responsibility**: The result of any method using this pattern is a
`Value::Future`. Callers must `await` it or chain with `then:`:

```smalltalk
(myArray inject: 0 into: [:acc :e | acc + e]) then: [:sum | ... ]
```

---

## Pattern 4 — Non-local return as early-exit accumulator

**Problem**: A loop checks a condition over a collection and wants to return
`false` at the first failure, `true` if all pass. The naive form uses a
`found` or `ok` accumulator variable.

**Forbidden**:
```smalltalk
| ok |
ok := true.
self do: [:each |
    (condition value: each) ifFalse: [ok := false]   "ok is a method temp"
].
^ok
```

**Canonical form**:
```smalltalk
self do: [:each |
    (condition value: each) ifFalse: [^false]
].
^true
```

`^false` is a non-local return from inside the block; it exits the *enclosing
method* immediately. This eliminates the accumulator entirely. The method
returns `true` only if the loop body completes without triggering `^false`.

Non-local returns require an enclosing method (the parser rejects `^` inside
a bare block with no method). They work at arbitrary nesting depth — `^` always
targets the method frame, not just the immediately enclosing block.

---

## Pattern 5 — Recursive helper for non-range loops

**Problem**: Some loops cannot be expressed with `to:do:` because the index
arithmetic is modular or otherwise irregular (e.g., open-addressing hash
probing). A `whileTrue:` with a counter mutation would violate the strict rule.

**Canonical form**:
```smalltalk
Dictionary >> pvtSlotFor: key [
    | cap h i |
    cap := table size.
    h := key hash \\ cap.
    i := h < 0 ifTrue: [h + cap] ifFalse: [h].
    i to: (i + table size - 1) do: [:i |
        | slot j |
        j := i \\ cap.
        slot := table at: j.
        (slot isNil or: [slot key = key]) ifTrue: [^j]
    ]
]
```

The probe index `i` is a block parameter of the `to:do:` range. All variables
(`slot`, `j`) are block-local. Non-local return `^j` exits the method when the
slot is found.

**Alternative — tail-recursive helper**:
```smalltalk
Dictionary >> pvtSlotFor: key at: i [
    | slot |
    slot := table at: i.
    (slot isNil or: [slot key = key]) ifTrue: [^i].
    ^self pvtSlotFor: key at: (i + 1) \\ table size
]
```

`slot` is a method local — legal to have at method level. The recursive
self-send is synchronous. Each frame is shallow. Use whichever form reads
more clearly for the specific loop shape.

---

## Applicability summary

| Situation | Pattern |
|-----------|---------|
| Indexed iteration with counter | 1 — `to:do:` |
| Ivar mutation triggered from block condition | 2 — `pvt` method + `then:` |
| Cross-iteration accumulation (variable size) | 3 — ValueHolder |
| Early exit / membership test / equality check | 4 — non-local return |
| Modular or irregular index arithmetic | 5 — recursive helper or `to:do:` with `\\` |
