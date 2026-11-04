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

**Status**: Open (from code reading; no test reproduces it yet)
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

**Related**: Q23 (decision A; glossary: Turn, Dependency); I3.

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

**Status**: Open — design conflict
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

**Related**: Q23 (decision B2), I1, D13.
