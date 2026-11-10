# Known Issues

Defects and suspect behavior found during design or implementation work that are
not yet fixed. Design questions belong in `open_questions.md`; this file tracks
concrete problems in the current implementation. Mark an issue **Fixed** (with the
commit) rather than deleting it.

Entry shape: status, where found, what happens, why it matters, related
questions.

---

## I1: `then:orElse:` continuations run on an anonymous coroutine

**Status**: Open
**Found**: 2026-11-01, Q23 glossary discussion
**Where**: `crates/interpreter/src/builtins_iterative.rs:222` (`dispatch_then_or_else`)

The future handles `then:orElse:` itself: it copies the sender's handler chain and
transaction context (`Evaluator::fork_context`), starts a fresh `may` coroutine that
waits on the future, runs the matching arm there, and returns a new future for the
arm's result. The continuation therefore runs in no entity's turn.

**Why it matters:** the causal model (glossary: turn, causal graph, registration)
says a continuation is caused by its registration, and nodes of the causal graph
are turns of an entity. An ownerless continuation has no place in that model, and
`fork_context` is expedient handling, not a principle (Q22, 2026-10-29..31).
`then:` and timer callbacks (`builtins_iterative.rs:3138`) share the pattern.

**Direction** (Q23 decision B2, 2026-11-03): the continuation runs as its own
short-lived continuation entity, with the registering turn's context. The fix is to
make today's anonymous coroutine that entity, not to move the continuation into the
registrant. Ivar reads from the block: I4.

**Related**: Q23 (decision B2), I4.

## I2: Forcing inside a turn can deadlock two entities, undetected

**Status**: Open; reproduced 2026-11-05 (table in I6); direction leaning, see below
**Found**: 2026-11-01, Q23 glossary discussion
**Where**: `crates/interpreter/src/entity.rs:1063` (`run_entity_loop`)

A turn runs to completion on its entity's coroutine; the mailbox is not serviced
while the turn is parked on `FutureHandle::wait`. If a turn of A forces a future
from B, and B's turn sends to A and forces that reply, both park forever. Without
forcing, A↔B mutual recursion is fine: each send is a new turn and replies settle
with still-pending futures.

Forcing points include `syncValue`, `ensure:`, `on:do:`, and `new` (I3). The
deadlock detector (`entity.rs:1089`) covers only transaction lock admission, not
future waits.

**Why it matters:** E avoids this class of deadlock by never blocking inside a
turn; Smol allows blocking, so either it must be detected/diagnosed or the forcing
points need rethinking.

**Mitigations** (Q23 decision A keeps blocking and exclusive turns):

- *Detection*: each Future records the process that will settle it, so a Future
  wait is a wait-for edge and the existing detector can report the cycle as a
  condition instead of a hang (`vm_compiler_plan.md` §5).
- *Fewer blocking points*: I3; and pipelining sends to a future. Today
  `Future >> doesNotUnderstand: aMessage [ ^aMessage sendTo: handle await ]`
  (`kernel.smol:1933`) blocks the turn. A pipelined form registers the send and
  returns a future for its reply, roughly `^handle then: [:v | aMessage sendTo: v]`:
  the target is not needed until the future settles (E's promise pipelining).
  The turn would then block only where a primitive needs the actual value, or on
  an explicit `await`/`syncValue`. Not decided.

**Direction** (2026-11-06, leaning; Q23 "Positions 2026-11-06"): a forced cycle
within a turn's own wait chain re-enters the parked turn; any other cycle is
detected and signals a condition. Detection must extend to ordinary future waits
either way.

**Related**: Q23 (decision A; glossary: Turn, Dependency); I3, I6.

## I3: `new` forces `initialize`; the stated reason is obsolete

**Status**: Open
**Found**: 2026-11-02, Q23 glossary discussion
**Where**: `crates/interpreter/src/kernel.smol:463` (`Object class >> new`, `new:`)

`new` is `[self _primNew initialize] syncValue`, so the creating turn blocks until
the new entity's `initialize` turn completes. Commit `c6f6123` (2026-05-25) gives the
reason: to eliminate "the cross-entity class-variable race that the removed
Initializing wait previously serialized". Class variables were removed by decision
on 2026-10-26 (`namespace_design_exploration.md` §9).

**Why it matters:** this is eager future resolution, which the project rules out
as a default (CLAUDE.md, "Futures"); it is also a forcing point for I2 (an
`initialize` that sends to its creator and forces deadlocks). Whether anything
else now relies on `new` returning a resolved instance is unverified.

**Related**: I2.

## I4: Blocks read ivars live, but a Block is a Value

**Status**: Open — direction leaning; capture syntax undecided
**Found**: 2026-11-03, Q23 discussion
**Where**: `crates/interpreter/src/eval.rs:454` (`lookup_receiver_scoped`, reached
from `eval_iterative.rs:654` for free names in a block)

A free name in a block inside a method resolves to the home receiver's ivar *at
the time the block runs*, through `block.home_self` (transaction-aware; it can
lazily read-enroll the home entity). Writes are a parse error (D13,
`NonLocalBlockAssignment`); reads are not restricted.

Two records disagree:

- `perf_env_frame_redesign.md` §6 item 2: "ivars are NOT snapshotted — they are
  `Dynamic`, read live from the instance at block-run time. This is correct and
  must stay: a block sees the *current* ivar value, not a stale one."
- A Block is a Value (`chunk_28b_plan.md`, Q22 2026-10-26), and the user's
  understanding: ivar references are copied at block creation, so a block cannot
  change over time.

**Provenance of the first statement** (traced 2026-11-04): added in commit
`113bbb1` (2026-08-09), the first version of the env-frame redesign doc. It sits
in §6, "Interaction points that must be preserved": a list of *existing behavior*
the performance refactor must not change, since §2.2 promises "no behavioral
change on valid code". No design discussion or rationale for live ivar reads is
recorded there or elsewhere in `docs/`; the 2026-08-09 session predates the
transcript exports (earliest 2026-10-07) and its session file is gone. Most likely
it is a preservation constraint phrased as a design claim, not a decision. The same
doc's §2.1 principle ("a block mutates nothing outside itself, and nothing outside
mutates what a block has closed over. Blocks become genuinely value-like
closures") is itself contradicted by live ivar reads.

**When would a snapshot be "stale"?** Unanswered by the record. Hypothesis, not
verified: a block re-run within the same turn after a synchronous self-send has
changed an ivar, e.g. a loop condition `[size > 0] whileTrue: [self removeFirst]`.
`kernel.smol` has no such loop today.

**Why it matters:**

- A Value whose result depends on another entity's current state is not
  immutable in any useful sense.
- Under Q23 decision B2 a continuation's block runs in its own entity, so a live
  ivar read is one entity reading another's state from another process, which
  breaks "an entity is single-threaded" for reads.

**Direction** (2026-11-06, leaning): forbid ivar references in blocks without an
explicit capture, which would probably be a shortcut for a temp assigned outside
the block. Live values are read with an explicit accessor send. This removes
out-of-turn reads, and with them the reader-side locking and lazy read-enrollment
from another entity's coroutine.

The rule depends on re-entry (I2, I6). Run on the current tree 2026-11-06, with
`Pricer` having `rate` 2 and `base` 5, and `lines` a shared `OrderedCollection`
holding 10:

| `Pricer >> totalFor: lines [ ... ]` | Result |
|---|---|
| `^(lines inject: 0 into: [:s :l \| s + (l * rate)]) + base` | 25 (live ivar read) |
| `\| r \| r := rate. ^(lines inject: 0 into: [:s :l \| s + (l * r)]) + base` | 25 (capture) |
| `^(lines inject: 0 into: [:s :l \| s + (l * self rate)]) + base` | **hangs** |
| `^lines inject: base into: [:s :l \| s + (l * self rate)]` | 25 (home does not wait) |

The block runs on `lines`' coroutine while `Pricer` is parked at the `+`, so the
accessor waits on a parked entity. An owned `lines` would run inline and not hang.

**Trap to design for:** a temp captured outside `Transaction stm:` is read once, so
a retry reuses the old value and the read does not enroll in the transaction
(from reasoning; not run). Inside those blocks the accessor is the right choice.

**Related**: Q23 (decision B2), I1, I2, I6, D13.

## I5: `^` in a block run by another entity does not return from the enclosing method

**Status**: Open — root cause identified (return target is a stack-depth
integer, not the home activation). The `Dictionary >> =` fallout is fixed in the
kernel (2026-11-06); the general fix is blocked on the "missing target" decision.
**Found**: 2026-11-05, I4 discussion
**Where**: the return *target* is computed and matched by depth in
`crates/interpreter/src/eval_iterative.rs` (block creation `:672`, `^` fires
`:1317`, depth match in `unwind_stack_to_depth` `:487`); on a miss the escaping
`EvalError::Return` is caught by the entity loop in
`crates/interpreter/src/entity.rs:1228` and settled as the receiver's reply.
**Tests**: `crates/interpreter/src/tests/nonlocal_return.rs` (E-labelled; 3 `red_*`
assert delivering `^` and are `#[ignore]`d pending Q24, 4 `ignored_*` characterize
the undecided cases). Runnable
companion: `docs/experiments/i5_nonlocal_return.smol`.

`^` in a block should return from the method that lexically encloses the block
(`Smol_Patterns.md` Pattern 4: "`^` always targets the method frame"). That holds
only when the block runs on the home entity's coroutine. When another entity
evaluates the block, `^` ends *that entity's* current message, the value becomes
its reply, and the enclosing method carries on:

```smalltalk
Object subclass: #Home instanceVariableNames: ''.
Home >> allPositive: arr [ arr do: [:x | x > 0 ifFalse: [^false]]. ^true ]
Home >> firstNeg: arr [ arr do: [:x | x < 0 ifTrue: [^x]]. ^nil ]

| neg | neg := Array new: 2.
neg at: 0 put: 1.
neg at: 1 put: -1.
(Home new allPositive: neg) await    "answers true; should be false"
(Home new firstNeg: neg) await       "answers nil; should be -1"
```

`Array` and the other collections are entities, so this is Pattern 4's idiom with
any receiver other than `self`.

**Provenance:** `chunk_21_plan.md` ("Non-local return `^` across entity boundaries")
says the receiver resolves its reply with the value, and then says `^` "exits the
iteration and returns from the enclosing method". The mechanism only does the
first half.

**Mechanism (traced from the code, 2026-11-06).** `^` identifies its home method
by a *stack-depth integer*, not by the identity of the home activation. A block
records the depth of its enclosing frame when it is created
(`eval_iterative.rs:672`):

```rust
let home_depth = ev.current_block_home.get()
    .unwrap_or_else(|| ev.method_depth.get());
```

When `^expr` runs it unwinds to that number (`eval_iterative.rs:1317`, with
`current_block_home` set to the running block's `home_depth` at `:1417`), and the
unwind matches purely on the integer (`eval_iterative.rs:487`):

```rust
if let Some(depth) = teardown_frame(ev, other) && depth == target_depth {
    return Ok(UnwindOutcome::Reached);
}
```

A depth is only meaningful on the coroutine that created the block. On any other
coroutine the number lands in one of two ways:

- *No frame at that depth* → `Exhausted` → `EvalError::Return(v, depth)` escapes
  and the entity loop settles it as the receiver's reply
  (`entity.rs:1228`, "Non-local return from a cross-entity block: resolve with v").
  `^` silently becomes "the value this foreign message answers"; the home method
  is untouched. This is E1, E5, E7, E11.
- *An unrelated frame sits at that depth* → `Reached` on the wrong frame → return
  from a method that has nothing to do with the block. In E8/E12 `^42` (home_depth
  2) matched `Worker>>helper:` at depth 2, giving 1042.

The existing `cross_entity_nonlocal_return_resolves_reply` test documents this
depth-collision path as if it were correct; it is encoding the bug.

**Why it matters:**

- No error is raised; the program just computes the wrong result.
- The idiom cannot work as written on a real foreign receiver:
  `arr do: [...]. ^true` does not wait for `do:`, so `^true` is reached regardless.
- Under VD2 the block would run on another process, where the enclosing method may
  be parked, running, or already returned.

**Kernel impact (resolved 2026-11-06).** A lexical scan of `kernel.smol` for `^`
inside a block found exactly one site that passes such a block to a receiver other
than `self`: `Dictionary >> =` does
`(other at: assoc key ifAbsent: [^false]) = assoc value`. The `[^false]` runs on
`other`'s coroutine, so `at:ifAbsent:` merely answers `false`; when the stored
value is also `false` the comparison `false = false` passes and the loop
continues, so two dictionaries with different keys and value `false` compared
*equal* (`green_e7`). Fixed 2026-11-06 by rewriting `Dictionary >> =` to query
`other` with `includesKey:` / `at:` whose `^false` stays on this coroutine
(`ifFalse:` sent to the future reply implicitly forces it); this removes the
kernel's only dependence on cross-entity `^` without touching the evaluator.
Every other kernel `^`-in-block either runs on `self`'s own coroutine (self-sends
such as `associationsDo:`, `to:do:`, `ifTrue:`) or is inside a `then:` whose
receiver is `self` — none depend on the broken cross-entity path.
`then:` on a *genuine* Future (E5): the `^` in the then-block settles the
continuation's reply, not the home method, so an awaited `then: [... ^v]` returns
the home method's later value, not `v`.

**Discussion record (2026-11-07 to 2026-11-09).** Nothing decided; the code is
unchanged apart from the `Dictionary >> =` fix. The thread moved on to Q24.

*Options considered for a `^` whose home is on another coroutine:*

- **A, reject at run time.** Identity targeting finds that the home frame is not
  on the current stack and raises a `BlockCannotReturn`-style condition, settled as
  the foreign turn's reply so the iterating entity does not crash. Sound with no
  static information, but it reports late.
- **B, reject at compile time.** The compiler refuses a `^`-block that can run on
  another coroutine. If the check is sound, no run-time machinery is needed.
- **C, deliver.** The `^` returns from its home across entities, along the wait
  chain.

*B, analysed.* A `^`-block is safe only if (1) whoever evaluates it shares the home
coroutine, and (2) it does not escape, meaning it is evaluated within the current
synchronous call and not kept for a later turn. A syntactic check can admit
`self`/`super` sends plus a fixed list of inline control selectors (`ifTrue:`,
`whileTrue:`, `to:do:`, `on:do:`, `ensure:`; run 2026-11-08, `^` returns correctly
from each). Its problem is consistency: `^` would work only in a few blessed
contexts and be refused elsewhere, even where it is safe. Admitting more needs two
things Smol does not have: a static way to know a receiver is a Value (there is
none; it needs type inference), and a non-escaping annotation on block parameters.
That cost is not small. User-defined control abstractions are Smol's only way to
extend control flow (no macros or templates), and a value-returning fold cannot
stand in for every control abstraction. Effect tracking also tends to be viral. Q21
(typed messages) is a possible route.

*C with implicit await (user, 2026-11-08).* A send whose block argument lexically
contains `^` implicitly awaits, so the home frame is still live when the `^` fires.
The cases left over (a block returned, stored, or deferred by its callee past its
reply) are the ones Smalltalk also refuses with `cannotReturn:`. Costs found: an
"NLR carrier" protocol up the await chain, with each turn unwinding itself; an
undefined case when an intermediate future has other waiters; reconciling with the
condition system (below); `then:` attachments. The user judged it questionable even
if it all worked: non-local return is close to arbitrary goto, and it breaks
structure.

*Correction: the condition system.* The handler chain does cross entity boundaries;
that is the purpose of HandlerContext. The handler runs at the signal point, on the
signaling coroutine. Its control operations (`return:` / `resume:` / `retry`) act
only on that coroutine's stack and never unwind another coroutine
(`condition_system_guide.md` §1, §4). So the project has already handled "a block
from entity A affects control in B" without unwinding across coroutines. An
earlier claim in this thread, that conditions are confined to the home coroutine,
was wrong.

*Diagnosis.* Smalltalk guarantees that a block sent `value` runs synchronously, on
the caller's stack, inside its home's live dynamic extent. `^`, `ifTrue:`,
`detect:`, `on:do:`, and user control abstractions all rely on that one rule. Smol
replaced it with a guarantee that holds only when the evaluator and the home are
the same entity, which is known only at run time. `^` is the most visible thing
that breaks. Types or effects can make the missing guarantee checkable, but they
do not restore it. No uniform replacement has been found.

*Where it stands.* The user leans toward dropping `^` in blocks altogether, but no
replacement for early return exists yet: methods would become expressions, TCO may
get harder, something like CL `cond` (a macro in CL) would be wanted, and
imperative algorithms may be hard to translate. Taken up as Q24. The `red_*` tests
assert the outcome of delivering `^` across entities and will need relabeling
whatever is decided.

**Related**: I4, I6, Q21, Q23 (decision B2), Q24, D13, R10.

## I6: Sends to the entity whose turn is running are treated as self-sends

**Status**: Open — direction leaning
**Found**: 2026-11-04, I4 discussion
**Where**: `crates/interpreter/src/builtins_iterative.rs:336` and `:1483`

The runtime decides "self-send" by comparing the receiver with the entity whose
coroutine is running (`current_entity_id`). It does not look at who wrote the send.
A block belonging to another entity, evaluated in E's turn, therefore runs a send
to E inline, in the middle of E's turn:

```smalltalk
Object subclass: #Box instanceVariableNames: 'state'.
Box >> reenter: aBlock [ state := 1. aBlock value. ^state ]
Box >> poke [ state := 2 ]

| b | b := Box new.
(b reenter: [ b poke ]) await    "answers 2; a queued poke would answer 1"
```

This is not a race: everything runs on one coroutine, like a callback recursing in
a sequential program. It does mean a send's timing depends on who evaluates the
block, and that `coll do: [:x | coll add: x]` mutates `coll` during the iteration
(`OrderedCollection >> do:` computes its bound once, so this terminates).

**Why it isn't simply fixed by queuing:** the inline path exists to avoid deadlock.
If the send were queued, `col do: [:x | col size > 3 ifTrue: [...]]` would park
`col`'s turn on a reply only that turn could produce (from reading the code; not
run, since it needs the change).

**Recursive sends more generally** (2026-11-05, current tree). `a` is an
`OrderedCollection`, `r` records a result, and `p` is another entity with
`check: c [ ^c size > 1 ]`:

| Shape | Code | Result |
|---|---|---|
| Block on `col` reads `col` | `col do: [:x \| r set: col size]` | works (inline) |
| Block on `col` forces `col` | `col do: [:x \| r set: (col size > 1)]` | works (inline) |
| Block on `col` adds to `col` | `col do: [:x \| col add: x * 10]` | works; 2 elements become 4 |
| Via a peer, block does not wait | `col do: [:x \| r set: (p check: col)]` | works |
| Via a peer, block waits | `col do: [:x \| r set: (p check: col) await]` | **hangs** |
| Nested collections, `a`'s turn waits | `(a do: [:x \| (b do: [:y \| r set: (a size > 0)]) await]) await` | **hangs** |
| Block forces home, home free | `col do: [:x \| r set: (self getN > 3)]` | works |
| Block forces home, home waits | `(col do: [:x \| r set: (self getN > 3)]) await` | **hangs** |

Inline covers only the first shapes, where the sender is on the receiver's own
coroutine. The hangs are I2: a parked turn waits on a future that needs a message
to that same parked entity. These runs reproduce it.

**Direction** (2026-11-06, leaning): keep inline. The test is that a send forced
immediately means what it means sequentially, and a sequential call runs at once.
A queue is rejected: it would only change when unforced sends run. "Self-send" is
the wrong name for this path, since the sender may be a block from elsewhere; the
concept is "a send to the entity whose turn is running". The hanging shapes are
handled by re-entry (I2; Q23 "Positions 2026-11-06").

**Related**: I2, I4, I5, Q23 (decision A), D2.
