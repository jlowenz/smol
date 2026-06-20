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
