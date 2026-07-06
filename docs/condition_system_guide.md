# Condition System Guide (design spec)

**Status:** design spec — locks the intended model. Parts are implemented; parts
are the target we are converging on. Divergences from current code are flagged
`⚠ CURRENT`. Companion to `docs/enrollment_provenance_design.md`.

This document exists so we stop re-deriving these semantics. Smol's condition
system is CL-inspired (handler-bind / restarts) and Smalltalk-flavored
(`resume:`/`return:`/`retry`, `isResumable`), operating in an **async-everywhere,
highly-concurrent** runtime. That last fact drives most of the non-obvious rules.

---

## 1. Vocabulary

- **Condition** — a reified, signalable object (a `Condition` instance). Signaling
  it walks the active handler chain.
- **Handler** — a block registered for a condition class. It runs **at the signal
  point** (handler-bind), on **whatever coroutine raised the signal**, found via
  the (possibly replayed) `handler_chain` — *not* by unwinding a stack to reach it.
- **Restart** — a choice offered to a handler for how to proceed: the built-in
  transfers `resume:` / `return:` / `retry`, plus named restarts
  (`addRestart:block:` / `invokeRestart:`).
- **Signaling coroutine** — the coroutine on which the condition is raised. All
  control transfers are **local to this coroutine's stack.**

---

## 2. Resumability is keyed on the HANDLER's declared class

The single most important rule. Following CL (`signal` vs `error`) and ANSI
Smalltalk (`Exception >> isResumable`):

- **`Error` (and subclasses) are non-resumable.** When a handler *declared for*
  `Error` finishes *without* invoking an explicit restart, the default disposition
  is to **abort** (locally unwind the protected computation).
- **`Condition` / `Warning` / `Notification` / control conditions (e.g.
  `OkToCommit`) are resumable.** A handler declared for one of these, on normal
  completion, **resumes** the signal point with the handler's value.

The disposition is read from the class **named at the handler site** — the
`SomeClass` in `on: SomeClass do:` / `HandlerContext on: SomeClass do:` — **not**
the concrete runtime class of the signalled instance. This is deliberate: it gives
the *handler author* a static, predictable contract. A handler written for `Error`
assumes it unwinds; if disposition were read from the signalled instance, a
resumable `Error` subclass could silently flip that handler from abort to resume,
continuing an errored computation the author meant to abandon. Keying on the
declared class removes that entire silent-bug class while still letting a signaler
define a resumable subclass and a handler opt into it *by declaring that class*.

`isResumable` is a **class-side** method (`Condition class >> isResumable ^true`,
`Error class >> isResumable ^false`; an instance-side `Condition >> isResumable`
delegates for ergonomics). Subclasses inherit the sane default and **may override**
(`MyRecoverableError class >> isResumable [ ^true ]`). A handler may *also* override
per-signal by explicitly invoking a restart (§4). So: the declared class sets the
disposition, a class-side override refines it, explicit restarts refine per-handler
— safe default **and** full control.

Implemented: `signal_condition` and the flattened `SignalGuard` path branch on
`class_is_resumable(handler_class)` — the class of the *matched handler*, not the
condition. `class_is_resumable` reads the class-side `isResumable` predicate by
**constant-folding** its body (a literal `^true`/`^false`), so it performs no Smol
send and never re-enters `step_loop` (conditions subclass `Value`, so a real send
would run inline and rebuild Rust stack depth — the C31.5 regression). A predicate
that is absent or not a bool literal falls back to the `Error`-hierarchy default.
This applies to **both** handler forms (§3), so a `Warning` caught by an `on:do:`
*resumes* past the signal — a deliberate divergence from classic Smalltalk (where
`on:do:` always unwinds), chosen so disposition is one consistent rule.

> Contract: `isResumable` must be a class-side method returning a literal `true`
> or `false`. A computed (instance-state-dependent) override is not honored — it
> falls back to the hierarchy default — because resumability is a class-level
> policy resolved without evaluating Smol in the signal path.

---

## 3. Handler forms — prefer `HandlerContext`; `on:do:` is the exception

Because the system is async-everywhere, **default to `HandlerContext`.**

- **`HandlerContext` (preferred).** Registers handlers as **data** on
  `handler_chain`. `fork_context` captures that chain, so a `then:orElse:`
  continuation running in a *different coroutine* **replays the handlers** — a
  condition raised in the fork is still handled. This is the only form that works
  across the async boundaries the runtime is built on.

- **`on:do:` (synchronous — avoid unless truly local).** Its defining problem is
  not just "stack-only" — it **forces its protected block to run synchronously**
  (each statement's result is forced/awaited before the block proceeds; see
  `eval_iterative.rs`, the `push_block_frame(… sync=true …)` for `on:do:`). That
  directly violates async-everywhere: it turns a would-be non-blocking region into
  a blocking one. A consequence of that forcing is that it *does* catch a rejection
  of a future its body **implicitly awaits** (the forced `await` re-signals the
  condition on the same stack, and `on:do:` catches it). What it **cannot** catch
  is a condition raised in work it forks-and-does-not-await — a `then:orElse:`
  continuation, a spawned coroutine, an entity reply it never forces. So it's not
  "always escapes"; it's "on:do: only sees what its own synchronous forcing pulls
  in, at the cost of blocking."

**Rule of thumb:** if the protected work does *anything* async you want to stay
non-blocking about (`await`, an entity send, `then:orElse:`), use `HandlerContext`.
Reaching for `on:do:` around async work either blocks it or misses conditions it
didn't await — usually a latent bug.

**Unified lookup (implemented).** Both forms register their handlers in the **one**
`handler_chain`, so a signal finds the *dynamically innermost* handler across both
forms in a single ordered walk — an inner `on:do:` correctly shadows an outer
`HandlerContext` and vice versa. `on:do:` is now literally "a `HandlerContext` with
a single handler pair that forces its body synchronously" (`HCRunGuard` with
`sync = true`); `HandlerContext run:` is the same guard with `sync = false`.
Disposition (§2) is by condition kind for both. This retired the old split where
`on:do:` handlers lived only as stack `OnDoCatch` entries invisible to the signal
walk — which let an outer `HandlerContext` wrongly pre-empt an inner `on:do:`.
Regression coverage: `inner_on_do_shadows_outer_handler_context`,
`inner_handler_context_shadows_outer_on_do`, `on_do_forces_its_body_future`,
`on_do_disposition_is_by_condition_kind`.

---

## 4. The control ops — all LOCAL to the signaling coroutine

A handler proceeds by invoking one of these. Each is scoped to the signaling
coroutine's stack; **none reaches back into a different (possibly already-returned)
coroutine's dynamic extent.**

- **`resume: v`** — deliver `v` to the signal point; the `signal` expression
  yields `v` and execution **continues from there**. Only meaningful for a
  resumable condition. (Data-safe: routed by `signal_condition` itself, so it
  works even in a replayed/forked handler.)
- **`return: v`** — **abandon** the protected computation: unwind the signaling
  coroutine's stack to the nearest enclosing handler boundary *on that stack*
  (the `HandlerContext run:` / `on:do:` frame), which yields `v`. If no such
  boundary is on the local stack (the handler is running in a fork), it unwinds
  to the **fork top** — it does not chase a boundary on another coroutine.
- **`retry`** — re-run the protected body from the top (again, the body on the
  local stack / fork).
- **`invokeRestart: name` / `invokeRestart:with:`** — run a **named restart**
  the signaler attached to the condition (`addRestart:block:`). Named restarts are
  data (blocks), so they compose across coroutines; use them to offer callers
  choices richer than resume/abort/retry.

A **named restart** is the signaler saying "here is a recovery path you may take."
The handler picks one by name; the restart block runs its recovery and then
completes the control flow with a transfer. A restart may do several things and
then `resume: v` (continue with a repaired value), `return: v` (abort the region),
or `retry` — the restart *block itself* chooses the transfer.

The restart block **receives the condition** as its first argument (and, for
`invokeRestart:with:`, the supplied value as its second), so it can call
`c return:` / `c resume:` / `c retry` explicitly. `invokeRestart:` does **not**
force resume: it evaluates the block, and any transfer the block performs unwinds
before the enclosing `resume:` is reached — so the block's own transfer wins. A
block that completes normally defaults to resuming with its value. This makes
control restarts like `#abort` expressible:

```smalltalk
"signaler offers an abort restart"
e := e addRestart: #abort block: [:c | c return: 'aborted'].
"handler chooses it — unwinds the region rather than resuming"
... on: Error do: [:e | e invokeRestart: #abort]
```

The **default disposition** (§2) is simply the implicit restart chosen when the
handler returns without invoking one: `resume:` when the handler's declared class
is resumable, an implicit local `return:` (abort) when it is not.

---

## 5. Handling vs propagation — "handling consumes a condition; not-handling propagates it via Future rejection"

This is the rule that keeps async error flow coherent, and it maps onto futures.
The two halves are *handled* vs *not-handled*, and the propagation vehicle is the
Future's rejection carrying the condition.

- **Handled here.** A matching handler runs and (for an `Error`) does not resume →
  the error is **consumed**. The signaling coroutine unwinds locally; the
  protected computation is abandoned; any *internal* continuation future
  **resolves normally**. The failure is expressed by the handler's side effects
  (e.g. rejecting the caller-visible result). **No future is rejected by the act
  of handling.**

- **Propagated elsewhere.** If nothing handles the condition locally, or the code
  explicitly forwards it, the error must **travel** to a distant handler/`await`.
  The vehicle is a **rejected future**: the continuation future rejects carrying
  the condition, and when some downstream `await`/`force_value` resolves it, the
  condition is **re-signalled at that site** (closing the loop into that site's
  handler chain). See §6.

So a rejected future is a *propagation vehicle for a not-handled condition*, not
part of local handling. Restated: **handling consumes the condition; not-handling
propagates it — as a Future rejection — until some downstream `await` re-signals
it into a handler chain (or the program surfaces it).**

---

## 6. Async chaining: `then:orElse:` and `force_value`

Bare `then:` was removed: it silently disposed of the rejection path (a partial
match). Async chaining is **total** over `{resolved, rejected}`:

```smol
future then: [:v | …on resolution… ] orElse: <rejection arm>
```

The rejection arm is a 1-arg block **or** a symbol idiom:

- `#signal` — re-signal the condition into the current (replayed) handler chain:
  *try to handle here* (consumed if caught; otherwise propagates).
- `#reject` — propagate the rejection **down the chain**: reject this result
  future so a downstream `then:orElse:` / `await` surfaces it.
- `#value` — resolve the result with the condition as an ordinary value.
- `[:e | …]` — custom.

`then:orElse:` returns a composable future in every case. `await` / `force_value`
is the **pull** point: forcing a rejected future re-signals the carried condition
**at the await site**, with full handler-bind semantics (resumable there, or
unwinds if unhandled). This is why an error that propagates down a chain is never
lost — the eventual `await` is its catch point.

Guidance: use `#signal` when a handler for it is in scope (e.g. a coordinator's
`HandlerContext`); use `#reject` to forward to a downstream consumer; write a
block when the branch has real recovery logic.

---

## 7. Worked example — the transaction coordinator

```smol
(((HandlerContext new
    on: OkToCommit do: [:c | self pvtCommit. txf resolveWith: c value. c resume: c value])
    on: Error do: [:e | self pvtRollback. txf rejectWith: e]))   "no return: — Error is non-resumable"
  run: [ | f |
    f := aBlock value: self.
    f then: [ (OkToCommit value: f await) signal ] orElse: #signal ]
```

- `OkToCommit` is a resumable `Condition`: its handler commits, resolves `txf`, and
  resumes the continuation **with the block's own value** (`c value`), not `nil` —
  most transactions ignore the continuation's result (the tx is a side effect), but
  forcing `nil` where a real value exists is gratuitously lossy. (Under §2, since
  `OkToCommit` is resumable, the handler could even omit the explicit `resume:` and
  let normal return resume with its last value.)
- `Error` is non-resumable: its handler rolls back and rejects `txf`, then — by the
  §2 default — **aborts** (local unwind), so control never falls into the commit
  path. No `return:` is written; the condition's kind supplies it.
- The block's async failure reaches the `Error` handler via `orElse: #signal`
  (re-signal into the replayed chain). Handled ⇒ consumed ⇒ the internal
  continuation future resolves; `txf` carries the failure to the caller.

(Implemented as written: the `Error` handler needs no `return:` — `Error` is
non-resumable, so its normal completion aborts by §2.)

---

## 8. Anti-patterns

- **`on:do:` around async work.** It will not catch anything raised after the
  block yields. Use `HandlerContext`.
- **Assuming a handler's normal return resumes.** For an `Error` it must abort;
  writing recovery that relies on continuing is wrong. Resuming an error is
  deliberate and requires an explicit `resume:`.
- **Bare "then with no rejection arm."** Removed; always state the rejection path
  (`then:orElse:`).
- **Rejecting a future to signal a locally-handled error.** Handling consumes;
  don't also reject. Reject only to *propagate* to a distant consumer.
- **Expecting `return:`/`retry` to reach a coroutine that already returned.** They
  are local to the signaling stack (or fork top). To transfer across coroutines,
  use data: named restarts, or reject-and-await.
