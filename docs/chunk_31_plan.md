# Chunk 31: `#atomic` Multi-Entity Transactions

## Goal

Implement pessimistic multi-entity atomic transactions with savepoint-based
nesting and deadlock detection. The full protocol (shadow writes, shared TxId,
mailbox locking, lazy participant enrollment, savepoints, deadlock cycle
detection) runs in Rust and Smol; the orchestration (`Transaction atomic:`,
`TxResult`) lives in `kernel.smol`.

**Prerequisite**: C31a complete (`Duration`, `Timer`, `_primScheduleTimer:block:`).

This is a **redesign** of the earlier C31 attempt (preserved on branch
`c31-wip`), which failed across several sessions. The two changes that motivate
the restart:

1. **Unified `TxContext` enum** carried as `Value::Handle` — replaces the
   separate `SharedTxHandle`/`SavepointHandle` types and the forbidden
   `Value::SharedTx`/`Value::Savepoint` variants.
2. **Per-level child transactions + ancestry locking** — each context owns its op
   buffer (merge-up on commit, drop on abort), so parallel children roll back
   independently. Lock + shadow are one per-entity stack of context-tagged frames
   (Moss's model): descendants inherit ancestor locks, siblings serialize. Depth
   (`tx_frames.len()`) is the lock count; entity unlocks at zero. Replaces the
   savepoint-index model (couldn't represent parallel children) and the
   `prior_enrolled`/`layered` bookkeeping.

See `docs/tx_design.md` for the full rationale (OQ-TX-1..9).

Deliverables:

- `Store::alloc_tx_id`, `begin_owned_with_id`, `OwnedStoreTransaction::absorb`,
  `TripleStore::{alloc_tx_id, new_transaction_with_id}`,
  `Transaction::absorb` in `triple-store` (shared TxId + per-level buffers).
- `TxContext` enum (`Root`/`Child`, each with own buffer) in the interpreter,
  carried as `Value::Handle(Arc<TxContext>)`. **No new `Value` variants.**
  **No `TentativeOp::Savepoint`/`savepoint()`/`rollback_to()`** — the WAL and
  backend `apply` are untouched.
- Seven primitives (four coordinator + three entity-side).
- Context-tagged `TxFrame` stack in `assign_var` / `lookup_var` (entity named
  ivars only).
- Tagged-envelope mechanism: `Envelope.tx_tag`, `Evaluator.current_tx_ctx`,
  auto-enrollment in `run_entity_loop`.
- `TxResult` value class and `Transaction` entity class in `kernel.smol`,
  including nested detection and deadlock cycle detection via `Timer`.
- `Transaction atomic:` end-to-end (nested + deadlock), green tests.
- `cargo fmt`; `cargo clippy --all-targets` clean.

**Out of scope for C31**: indexed-slot shadowing (deferred until a concrete use
case exists — named ivars cover all current kernel entities); `#observable` mode
(deferred; needs benchmarking before choosing default).

---

## Background

See `docs/tx_design.md` for: the shared-store asset, the in-memory slot problem,
the unified `TxContext`, lazy/symmetric shadow writes, tagged envelopes, and
deadlock detection. Summary of the data model:

```rust
pub enum TxContext {
    Root(Arc<RootTxInner>),
    Child(Arc<ChildTxInner>),
}

pub struct RootTxInner {
    pub ctx_id: TxCtxId,                            // process-unique context identity
    pub tx_id: TxId,                                // shared store TxId for the whole tree
    pub tx: Mutex<Option<OwnedStoreTransaction>>,   // this level's buffer
    pub enrolled: Mutex<Vec<InstanceHandle>>,
}

pub struct ChildTxInner {
    pub ctx_id: TxCtxId,
    pub outer: Arc<TxContext>,                      // parent; ancestry = walk outer
    pub tx: Mutex<Option<OwnedStoreTransaction>>,   // this child's OWN buffer
    pub participants: Mutex<Vec<InstanceHandle>>,
}

// per entity (on EntityData):
pub tx_frames: Mutex<Vec<TxFrame>>,   // lock + shadow, one frame per held level
pub struct TxFrame { pub ctx_id: TxCtxId, pub shadow: HashMap<Arc<str>, Value> }
```

`Value::Handle(Arc<TxContext>)` carries the handle to Smol. Consumers downcast
via `handle.downcast_ref::<TxContext>()`.

**Per-level buffers, not savepoints.** Each context owns its op buffer (child
commit → `parent.tx.absorb(child.tx)`; child abort → drop). This handles parallel
children correctly; a savepoint-index model could not (see tx_design OQ-TX-9).

**Where writes go**: each participant buffers at its enrolled context level;
buffers merge upward, all sharing the root `TxId` (walk `outer` to the `Root`).
**Where membership is recorded**: the active context head when the participant
first joins (`Root::enrolled` or `Child::participants`).

**Lock + shadow = one per-entity stack of context-tagged frames** (Moss nested
locking): descendants inherit ancestor locks; siblings serialize on a shared
entity. Admission keys on `ctx_id` + ancestry, **not** root `TxId` (siblings share
it). `tx_frames.len()` is the nesting/lock depth; entity unlocks at depth 0.

---

## Steps

### S0: Triple-store additions

`crates/triple-store/src/store.rs`:

- `pub fn alloc_tx_id(&self) -> TxId` — advance the counter, return the new TxId
  without opening a buffer.
- `pub fn begin_owned_with_id(&self, ctx: ExecContext, tx_id: TxId) -> OwnedStoreTransaction`
  — like `begin_owned` but uses the supplied TxId.

`crates/triple-store/src/triple_store.rs` (`TripleStore` trait, impl in both
backends + `test_support`):

- `fn alloc_tx_id(&self) -> TxId` — advance the `next_tx` counter, return the new
  id without building a transaction.
- `fn new_transaction_with_id(&self, tx_id: TxId) -> Transaction<TFact>` — like
  `new_transaction` but binds the supplied id.

`crates/triple-store/src/store.rs`:

- `pub fn alloc_tx_id(&self) -> TxId` — delegate to backend.
- `pub fn begin_owned_with_id(&self, ctx: ExecContext, tx_id: TxId) -> OwnedStoreTransaction`
  — like `begin_owned` but uses the supplied TxId.
- `OwnedStoreTransaction::absorb(&mut self, other: OwnedStoreTransaction)` — move
  all `TentativeOp`s and `value_buf` entries from `other` into `self`; requires
  `other.tx.tx_id == self.tx.tx_id` (`debug_assert`).

`crates/triple-store/src/transaction.rs` (crate-visible):

- `pub(crate) fn absorb(&mut self, other: Transaction<TFact>)` — `debug_assert`
  equal `tx_id`; `self.ops.extend(other.ops); self.value_buf.extend(other.value_buf)`.

**No `savepoint()` / `rollback_to()` / `TentativeOp::Savepoint`** — per-level
buffers (each child has its own `OwnedStoreTransaction`) replace the savepoint
model, so the op buffer, the WAL, and both backends' `apply` are untouched.

### S1: `TxContext` type

New module `crates/interpreter/src/shared_tx.rs`: the enum and inner structs
above, plus:

- `SharedTxId = u64` alias; `TxCtxId = u64` alias (process-unique counter).
- `impl TxContext` helpers: `ctx_id(&self)`, `root(&self) -> &RootTxInner` (walk
  `outer`), `tx_id(&self) -> TxId`, `buffer(&self) -> &Mutex<Option<…>>` (this
  level's), `participants(&self)`, and `is_ancestor_or_self(&self, other_ctx_id)
  -> bool` (walk `outer` from `self`).
- `Debug` impls.

`Value::Handle` already exists — **no `value.rs` change**.

`Evaluator` gains `current_tx_ctx: RefCell<Option<Arc<TxContext>>>`.
`ContinuationContext::fork_context()` carries it.

### S2: Lock + shadow frame stack

`EntityData` gains `tx_frames: Mutex<Vec<TxFrame>>` (`TxFrame { ctx_id, shadow }`).
A non-empty stack means the entity is locked along the current ancestry path.

`assign_var` (eval.rs, `InstanceKind::Entity` branch): if `current_tx_ctx` is
`Some`, write into the **top** frame's `shadow` instead of `obj.set_slot`; still
call `store_bridge::buffer_ivar`.

`lookup_var` (eval.rs, `InstanceKind::Entity` branch): if `tx_frames` is
non-empty, scan frames top-to-bottom; fall through to live slot on miss.

Frame lifecycle (symmetric — push on enroll, pop on exit):

- **Enroll at a level (context C)**: push `TxFrame { ctx_id: C.ctx_id, .. }`.
- **Commit a level**: merge top frame's `shadow` into the frame below and retag
  that frame to the parent context (**lock passes up**); if it was the only frame,
  apply to live slots and pop.
- **Rollback a level**: pop and discard top.
- Entity stays locked while `tx_frames` non-empty; fully unlocks at depth 0.

### S3: Tagged envelopes, ancestry admission, auto-enrollment

`Envelope` gains `tx_tag: Option<Arc<TxContext>>`.

`send_message` gains a `tx_tag` parameter; existing call sites pass `None`; the
evaluator's entity-send path passes `ev.current_tx_ctx.borrow().clone()`.

`run_entity_loop` dequeue:

1. **Ancestry admission.** If `tx_frames` is non-empty, take an envelope tagged
   with context C only if `top.ctx_id.is_ancestor_or_self(C)` (walk C's `outer`
   chain for `top.ctx_id`). A sibling/unrelated C's envelope stays queued (parks)
   until the entity pops back to a common ancestor — sibling serialization.
2. Before dispatching a tagged envelope from C to an entity not yet enrolled at
   C's level: push a `TxFrame` for C, add self to C's participant list, and
   (first enrollment only) begin a local `current_tx` via `begin_owned_with_id`
   under the root `TxId`. Record a `participates-in (ctx_id)` store fact for
   deadlock detection.

### S4: Primitives (`builtins_iterative.rs` + `builtins_recursive.rs`)

| Primitive                           | Action |
| ----------------------------------- | ------ |
| `_primBeginSharedTx`                | `current_tx_ctx` `None`: `alloc_tx_id`, build `Root` (own buffer), set `current_tx_ctx`, return `Handle`. `Some(outer)`: build `Child{outer, own buffer via begin_owned_with_id(root tx_id)}`, set head, return `Handle`. |
| `_primCommitSharedTx: handle`       | `Root`: commit own buffer to store; clear `current_tx_ctx`. `Child`: `outer.buffer.absorb(child.buffer)`; restore `outer` as head. (Per-participant shadow merge happens in `_primExitTxMode: #commit`.) |
| `_primRollbackSharedTx: handle`     | `Root`: drop own buffer (rollback); clear `current_tx_ctx`. `Child`: drop own buffer; restore `outer` as head. |
| `_primEnrolledParticipants: handle` | Return this level's participant list as a Smol collection. |
| `_primLockMailbox: handle`          | Push a `TxFrame` tagged with handle's `ctx_id`. |
| `_primActivateShadowWrites: handle` | Begin local `current_tx` via `begin_owned_with_id` under root TxId. |
| `_primExitTxMode: outcome`          | `#commit`: absorb local `current_tx` into the context's buffer; merge top frame's shadow down (retag to parent / apply to live slots if last); pop; unlock if depth 0. `#rollback`: discard local ops; pop+discard top frame; unlock if depth 0. |

Note: `_primExitTxMode:`/`_primLockMailbox:`/`_primActivateShadowWrites:` are
internal (used by enrollment/exit paths) but named for direct testing.

### S5: Kernel classes

`TxResult` (value class: `transaction`, `result`), `Future` Smol class
(reifies `FutureHandle` — see OQ-C31-5), `Object >> pvtExitTx:`, and the
`Transaction` entity (`atomic:`, `atomic:timeout:`, `pvtRunAtomic:`,
`pvtCommit`, `pvtRollback`, `status`, `txId`, `abort`). Carry forward verbatim
from the WIP plan (`c31-wip:docs/chunk_31_plan.md` §S5) — the Smol orchestration
is unchanged by the Rust-side redesign; `pvtCommit`/`pvtRollback` dispatch on the
handle via `_primEnrolledParticipants:` and the commit/rollback primitives, which
now resolve the `TxContext` variant internally.

### S5b: Child transactions (nested + parallel `atomic:`)

Covered by S1 (`ChildTxInner` with its own buffer), S2 (frame stack with
ancestry admission), and S4 (`_primBeginSharedTx`/commit/rollback `Child`
variants). No `prior_enrolled`/`layered`/`sp_index`/savepoint marker. On child
abort, participants that joined only at the child level reach depth 0 via their own
`_primExitTxMode: #rollback` and unlock; ancestor-level participants keep depth
≥ 1. Parallel children each own an independent buffer, so one aborting does not
disturb a sibling. Siblings touching the same entity serialize via ancestry
admission (S3) — the second child's tagged message parks until the first exits.

### S5c: Deadlock detection

Carry forward from WIP plan §S5c (Smol-side, `Timer`-based; `locked`/`waiting`
sets, `pvtCheckDeadlock`, `Deadlock` condition, default 500 ms timeout). **Graph
nodes are contexts (`TxCtxId`), not root transactions** — sibling children under
one root can deadlock, and `participates-in` facts record the holding context.

### S6: Tests

- Happy path: two entities, value transfer, both reflect new state after commit.
- Shadow correctness: mid-block read returns shadow value, not stale slot.
- Rollback: `tx abort`; both live slots remain pre-tx.
- Mailbox lock: external message delivered after commit.
- Shared TxId: both entities' facts appear under one TxId.
- Nested happy path: inner `atomic:` commits as a child; outer commit includes
  both (child buffer absorbed into parent).
- Nested inner rollback: child abort discards only child ops/shadow; outer
  continues.
- Parallel children: two children under one root over disjoint entities; one
  aborts, the other commits; root commit reflects only the survivor.
- Sibling serialization: two children both target entity E; the second's tagged
  message is processed only after the first child exits.
- Deadlock: two transactions cross-wait; `Deadlock` raised before timeout (short
  timeout in test).

---

## Design resolutions

OQ-C31-1..6 carry forward from `c31-wip:docs/chunk_31_plan.md` (async Future
wiring, class-side dispatch, shadow stack, participant bridging, Future Smol
class, `current_tx_ctx` in `ContinuationContext`).

**OQ-C31-7 (NEW): Unified `TxContext` as `Value::Handle`.** Root and child are one
enum, carried as `Value::Handle(Arc<TxContext>)`. No new `Value` variants. Nesting
via `ChildTxInner::outer`. Enrollment is per-context-level; the root supplies only
the `TxId`. `tx_frames.len()` is the lock/nesting count, so the entity unlocks
exactly when depth reaches zero. `prior_enrolled` and `layered` are eliminated.

**OQ-C31-8 (NEW): Per-level buffers + ancestry locking (= tx_design OQ-TX-9).**
Each context owns its op buffer (child commit absorbs into parent; child abort
drops), so parallel children roll back independently — the savepoint-index model
could not represent this. Lock + shadow are one per-entity stack of context-tagged
`TxFrame`s following Moss's nested-locking model: descendants inherit ancestor
locks; siblings serialize on a shared entity. Admission and the deadlock graph key
on `TxCtxId` + ancestry, not root `TxId`. No `savepoint()`/`rollback_to()`/
`TentativeOp::Savepoint`; the WAL and backend `apply` are untouched.

---

## Resume state (2026-06-22) — C31.5 done; resume at C31 S6 then S5b

**C31.5 (complete the iterative evaluator) is COMPLETE** (commits 9f6c8c7..b87443c).
All synchronous nesting now runs on the heap `Cont` stack — methods, class
methods, `value`/`on:do:`/`ensure:`/`then:`, `HandlerContext run:`, signals
(CL handler-scoping), `_primNewValue:`, all iteration (`to:do:`), and
`Namespace do:`. Per-entity release stack lowered 256 KB → 64 KB (debug stays
256 KB via `cfg(debug_assertions)`). Debug-only `step_probe` Cargo feature
(`probe::ProbeState` on `Evaluator`) measures `step_loop` depth/per-level deltas.
The 3 nested-HC tests are un-ignored. Full debug suite: 690 pass, 1 ignored
(the S5b gap below).

**C31 status:** S0–S5 committed earlier (triple-store additions, `TxContext`,
frame stack, tagged envelopes, 7 primitives, `Transaction`/`TxResult` kernel
classes). Happy paths green: `atomic_no_participants_*`, `atomic_transfer_*`,
`atomic_abort_*` (tests/entity.rs), plus S2 `tx_frames` unit tests.

### Plan: do S6 (single-level) first, then S5b (nested), then S5c (deadlock)

**S6 single-level matrix — WRITE THESE (tests/entity.rs, `run_in_coroutine`):**
- shadow correctness: a mid-block read returns the shadow value, not the stale slot.
- mailbox lock: an external (untagged) message is delivered only after commit.
- shared TxId: both participants' facts appear under one TxId.
(Happy/abort already covered.)

**S5b nested `atomic:` — KNOWN BUG (ignored test
`atomic_nested_inner_commit_absorbs_into_outer`):** the inner `Transaction
atomic:` send is tagged with the OUTER tx context, so the inner coordinator (the
`Transaction` class entity AND the instance created by `self new`) auto-enrolls as
an outer participant in `run_entity_loop` (entity.rs ~line 678). Outer
`pvtCommit` then sends `pvtExitTx:` to the class → `MessageNotUnderstood`.
Design decision needed: exclude transaction coordinators / class entities from
tagged auto-enrollment, while still letting the inner coordinator SEE the outer
`current_tx_ctx` so `_primBeginSharedTx` builds a `Child`. (C31.5's prerequisite
fixed a coordinator enrolling in its OWN tx; this nested facet is distinct.)
Then add: nested inner-rollback, parallel children, sibling serialization tests.

**S5c deadlock detection — NOT STARTED.** Needs kernel-side `pvtCheckDeadlock`,
`Deadlock` condition, `participates-in (TxCtxId)` facts, `Timer`-based, graph keyed
on `TxCtxId` (not root TxId). See §S5c above.

**Workflow reminders:** `cargo fmt` + `cargo clippy --all-targets` before commit
(hook enforces); tx tests run via `run_in_coroutine` (needs a may coroutine for
the coordinator to block on participant replies).

---

## Resume state (2026-06-23) — S6 DONE (uncommitted); Future seamless-interface UNRESOLVED

**Nothing committed this session.** Working tree has all the changes below. Suite:
695 pass / 1 ignored (S5b). `cargo fmt` + clippy clean.

### S6 single-level matrix — DONE (3 new tests, tests/entity.rs)
- `atomic_shadow_read_returns_speculative_value_mid_block` — read-your-writes shadow (150 vs stale 100).
- `atomic_mailbox_lock_defers_external_message_until_commit` — two `Future` gates force
  determinism; a post-gate `deposit: 25` makes committed (175) ≠ mid-tx shadow (150),
  so it actually proves deferral (a broken lock would read the 150 shadow).
- `atomic_transfer_commits_both_participants_under_one_tx_id` — subscribes (System ctx,
  wildcard `Pattern`) and asserts one `TxId` spans both participant subjects. Inlines its
  own coroutine runner (not `run_in_coroutine`) to hold the `Subscription`.

### Future seamless-interface work — PARTIALLY DONE, DESIGN NOT SETTLED (user wants to revisit)
Goal (user): the explicit `Future` must be **seamless — behave exactly like an implicit
`Value::Future`**: clients never touch `handle`; awaiting/forcing it transparently yields
the resolved value ("magically be the receiver of the resolved value").

Done so far (committed to working tree, but design is being reconsidered):
- Removed `Future >> handle` and `Future >> syncValue`. Added `Future >> await [^handle await]`,
  `Future >> then: aBlock [^handle then: aBlock]`.
- `eval_iterative.rs`: `then:` interception now skipped for `Value::Instance` receivers
  (`&& !matches!(&receiver, Value::Instance(_))`) so `Future >> then:` is reachable via
  user-method lookup. Rationale: `then:` is intercepted *before* `force_value` only to avoid
  eagerly forcing a raw `Value::Future`; a ValueObject is never force-blocked so it can route
  normally. `await` needed NO Rust change (lookup_user_method runs before the `dispatch()`
  await interceptor).
- Single-winner resolution: `FutureHandle::try_resolve_ok/try_resolve_err` (atomic
  check-and-set under the existing mutex) in entity.rs; prims `_primResolveFuture:value:` /
  `_primRejectFuture:error:` now return `Value::Bool(won)`.
- Typed restartable condition: `Error subclass: #FutureAlreadyResolved` (kernel.smol) with an
  `#ignore` restart; `resolveWith:`/`rejectWith:` funnel through `Future >> pvtSettle: won`.
- `TxResult` now stores the `Future` itself (`result: txf`, was `txf handle`).
- Tests migrated off `handle`/`syncValue`: eval.rs `future_resolve_then_sync_value`,
  `future_then_runs_after_resolution`, `signal_in_then_continuation_caught_by_enclosing_hc`;
  added `future_double_resolve_signals_already_resolved_and_keeps_first_value` and
  `future_double_resolve_condition_is_catchable_as_error`.

**OPEN — two user objections still unresolved (resume here):**
1. **WRONG edit at `tests/entity.rs` `hc_then_signal_resolves_future_within_entity` (~L617):**
   I changed `^txf handle` → `^txf await`. User: awaiting when the caller didn't ask is
   semantically different from *returning the future/handle*. Must revert to returning the
   future (`^txf`) — but that only yields the right value once objection 2 is fixed.
2. **Explicit `Future` is STILL not seamless:** a returned `Future` instance is NOT
   transparently resolved when it becomes the value of an (implicit) future that gets
   awaited. Implicit futures get this via `force_value` unwrapping `Value::Future`; the
   ValueObject wrapper does not.

**Core tension blocking a quick fix (document for next session):** making `force_value`
unwrap a `Future` *instance* would also force the **receiver** of `resolveWith:`/`rejectWith:`
(because `dispatch_to_stack` force-resolves receivers before user-method lookup, eval_iterative.rs
~L1868) → deadlock on an unresolved future. This is the same reason raw `Value::Future` needs
`then:`/resolve verbs intercepted *before* force.

**Leading candidate design (NOT yet agreed — discuss with user):** make the explicit `Future`
literally *be* a `Value::Future` (raw handle), not a ValueObject wrapper:
- `Future class >> new [ ^self _primNewFutureHandle ]` returns `Value::Future`.
- Then it is transparently forced everywhere for free (no `force_value` coupling) → fully
  seamless, identical to implicit futures. `await`/`then:` already work.
- `resolveWith:`/`rejectWith:` must become **receiver-non-forcing** selectors: either (a) skip
  `force_value` for these selectors when receiver is `Value::Future` and add a `Future` class
  mapping in `lookup_user_method` (currently `_ => None` for `Value::Future`, builtins L334) +
  `value_to_class_handle(Value::Future)`; or (b) intercept them in Rust like `then:` and signal
  `FutureAlreadyResolved` from Rust.
- Feasibility already checked: `lookup_user_method` (builtins_iterative.rs:266) returns `None`
  for `Value::Future` today; would need a `Value::Future => Future class` arm. `then:` is
  intercepted in `dispatch_to_stack` before force; the same hook can carve out resolve/reject.
- Trade-off: 2 more receiver-non-forcing selectors; `Future` becomes a class whose "instances"
  are `Value::Future`. Cleanest match to "seamless like an implicit future".

After the Future design is settled: revert objection-1 edit, re-verify suite, then **commit S6 +
Future work together**, then proceed to **S5b** (nested-atomic auto-enroll bug), then **S5c**.
