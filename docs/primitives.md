# Smol primitive inventory (`_prim*`)

Running list of interpreter primitives exposed to `kernel.smol`, for review and
optimization. Scope: the **iterative** evaluator path (`builtins_iterative.rs` +
`eval_iterative.rs`), which is the live path. The recursive evaluator
(`builtins_recursive.rs`, slated for retirement) has parallel `_prim*` arms plus
legacy loop prims (`_primWhileTrue:`, `_primTimesRepeat:`, `_primAt:put:`) that
the iterative path does **not** use — the iterative kernel does loops in pure Smol
via TCO. Those are intentionally omitted here.

Columns: **Selector** · **Receiver** (kind the prim expects) · **Args** ·
**Semantics** · **Handler** (`file:line`) · **Notes / optimize?**

## Universal (dispatched at top of `dispatch()`)

| Selector | Receiver | Args | Semantics | Handler | Notes |
|---|---|---|---|---|---|
| `_primHash` | any Value | 0 | Content/identity hash | builtins_iterative.rs:56 | |
| `_primClass` | any Value | 0 | Class of the receiver | builtins_iterative.rs:62 | |
| `_primIsKindOf:` | any Value | 1 (class) | Bool ancestry test | builtins_iterative.rs:70 | |

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
