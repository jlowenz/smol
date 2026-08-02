# Smol primitive inventory (`_prim*`)

Running list of interpreter primitives exposed to `kernel.smol`, for review and
optimization. Scope: `builtins_iterative.rs` + `eval_iterative.rs` — the single
evaluator path. (The recursive evaluator was retired 2026-07; an earlier version of
this note described it as merely "slated for retirement".)

Columns: **Selector** · **Receiver** (kind the prim expects) · **Args** ·
**Semantics** · **Handler** (`file:line`) · **Notes / optimize?**

## Universal (dispatched at top of `dispatch()`)

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primHash` | any Value | 0 | Content/identity hash | builtins_iterative.rs:68 | Whole-valued Floats hash as the equal Integer (S6) |
| `_primClass` | any Value | 0 | Class of the receiver | builtins_iterative.rs:74 | |
| `_primIsKindOf:` | any Value | 1 (class) | Bool ancestry test | builtins_iterative.rs:82 | |

## Numeric tower (S2–S7)

Grounding for the tower's public protocol. Each is sent to `self` on an **encoded
leaf** — a `_prim` may only be sent to `self` (self-only rule), so abstract `Number`
code reaches these by projecting with `value` and re-dispatching a public selector
(`docs/numeric_tower_plan.md` §4e). Every one is a *single* selector that branches
internally on the `Value::` variant and promotes across encodings (any `Float` ⇒ f64,
else any `LargeInt` ⇒ BigInt, else i64 with overflow promoting and results demoting via
`normalize_bigint`), rather than a per-representation family.

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primAdd:` `_primSub:` `_primMul:` `_primDiv:` `_primQuo:` `_primRem:` | numeric leaf | 1 | Promoting arithmetic | builtins_iterative.rs:814 (`arith_prim_op` → `numeric_binop`) | `_primQuo:`/`_primRem:` are integer-only |
| `_primLt:` | numeric leaf | 1 | Promoting `<` | builtins_iterative.rs:1182 (`numeric_lt`) | Errors on a non-numeric operand |
| `_primEq:` | numeric leaf | 1 | Promoting `=` | builtins_iterative.rs:1191 (`numeric_eq`) | **Total** — answers false rather than erroring (S6) |
| `_primStrLt:` | String | 1 | Lexicographic `<` | builtins_iterative.rs:1440 | |

**Irreducibility audit (S7).** The non-`_prim` arms surviving on the numeric
dispatchers are host-representation operations with no Smol expression:
`asFloat` (i64/BigInt → f64), `asString`/`printString` (host number formatting), and
Float's `truncated` `rounded` `ceiling` `floor` `asInt` (libm/host). `to:do:` and
`to:by:do:` remain on `dispatch_int` but are **not** numeric primitives — they are
iteration primitives of the iterative evaluator (C29.5/C31.5), kept so loops run on the
heap `Cont` stack.

`printString`/`asString` are the one arguable case: digit extraction is expressible in
Smol now that `//` and `\\` exist, so they are reducible in principle and retained for
cost, not necessity. Recorded rather than resolved.

**Pruned in S7:** 39 arms across `dispatch_int` / `dispatch_largeint` /
`dispatch_float` (`+ - * / // \\ < <= > >= = ~= abs negated max: min:`), plus the
orphaned `as_float_coerce` helper. All were shadowed by kernel Smol methods — user
lookup runs first (`eval_iterative.rs:1844`) and walks the full superclass chain — and
instrumenting the three dispatchers confirmed none was reached across the whole suite.

## Value / instance slot & string ops

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primAt:` | indexed value/instance | 1 (index) | Read indexed slot/element | builtins_iterative.rs:1384 | |
| `_primAt:put:` | mutable indexed entity | 2 (index, val) | Write indexed slot | builtins_iterative.rs:1423 | Object (mutable) only |
| `_primSize` | indexed value/instance | 0 | Element/slot count | builtins_iterative.rs:1510 | |
| `_primAsString` | any Value | 0 | String representation | builtins_iterative.rs:1522 | |

## Condition system

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primSignalCondition:` | Condition | 1 (condition) | Signal a condition up the handler chain | builtins_iterative.rs:1562 / eval_iterative.rs:1973 | flattened onto Cont stack |
| `_primResume:` | — | 1 (value) | Resume signaling point with value | builtins_iterative.rs:1572 | |
| `_primReturn:` | — | 1 (value) | Return value from `on:do:` handler | builtins_iterative.rs:1576 | |
| `_primRetry` | — | 0 | Retry the protected block | builtins_iterative.rs:1580 | |
| `_primHCRun:` | HandlerContext | 1 (block) | Run body with handler pairs registered | builtins_iterative.rs:1584 / eval_iterative.rs:1887 | flattened (HCRunGuard) |

## Timers

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primScheduleTimer:block:` | — | 2 (duration, block) | Schedule a timer callback | builtins_iterative.rs:2519 | callback runs in fresh coroutine |
| `_primCancelTimer:` | — | 1 (Handle) | Cancel a pending timer | builtins_iterative.rs:1711 | |
| `_primTimerPending:` | — | 1 (Handle) | Bool: is timer still pending | builtins_iterative.rs:1729 | |

## Transactions (shared tx / `atomic:`)

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primBeginSharedTx` | — | 0 | Begin a shared transaction, returns handle | builtins_iterative.rs:1756 | |
| `_primCommitSharedTx:` | — | 1 (tx) | Commit shared tx | builtins_iterative.rs:1788 | |
| `_primRollbackSharedTx:` | — | 1 (tx) | Roll back shared tx | builtins_iterative.rs:1813 | |
| `_primEnrolledParticipants:` | — | 1 (tx) | List enrolled participants | builtins_iterative.rs:1831 | |
| `_primExitTxMode:` | entity | 1 (#commit/#rollback) | Entity leaves tx mode | builtins_iterative.rs:1847 | |

## Futures

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primNewFutureHandle` | — | 0 | Allocate a new future handle | builtins_iterative.rs:1923 | |
| `_primResolveFuture:value:` | — | 2 (handle, val) | Resolve future (Bool: won the race) | builtins_iterative.rs:1927 | single-winner via try_resolve_ok |
| `_primRejectFuture:error:` | — | 2 (handle, err) | Reject future (Bool: won the race) | builtins_iterative.rs:1941 | single-winner via try_resolve_err |

## Construction & reflection

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primNew` | class | 0 | New entity instance | builtins_iterative.rs:2433 | |
| `_primNew:` | class | 1 (size) | New indexed entity | builtins_iterative.rs:2539 | requires `indexedSlots:` |
| `_primNewValue:` | class | 1 (init block) | New value object (frozen after init) | builtins_iterative.rs:2469 / eval_iterative.rs:2011 | flattened init block |
| `_primNewValueIndexed:init:` | class | 2 (size, init) | New indexed value object | builtins_iterative.rs:2494 / eval_iterative.rs:2035 | |
| `_primInstancesOf:` | — | 1 (class) | All live instances of a class | builtins_iterative.rs:2570 | |

## Chunk 33 (DNU/Message/perform) — landed, no new `_prim*`

C33 added **no** new primitives. The pieces are interpreter-level, not `_prim`
selectors:

- **`perform:` / `perform:withArguments:`** — intercepted in `dispatch_to_stack`
  (eval_iterative.rs, before user-method lookup); re-dispatches the target
  selector on the same Cont stack. Not a `_prim`.
- **`Message` reification** — built by the Rust helper `build_message`
  (builtins_iterative.rs), which instantiates the Smol-declared `Message` class
  directly (like Array-literal construction). No `_primNewMessage:` was needed.
- **`doesNotUnderstand:` hook** — on a `MessageNotUnderstood` miss, both
  `dispatch_to_stack` (synchronous / value receivers) and `dispatch_envelope`
  (entity mailbox) reify the send and invoke `doesNotUnderstand:`; the default
  `Object >> doesNotUnderstand:` signals a `MessageNotUnderstood` condition.

---
_Maintenance: append a row when a `_prim*` is added; this doc is finalized as part
of each chunk that touches the primitive surface. Last updated: 2026-07-01 (C33)._
