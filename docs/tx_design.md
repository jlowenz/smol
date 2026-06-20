# Multi-Entity Transaction Design

**Status**: Design — revised session 2026-06-20 (unified `TxContext`)
**Supersedes**: Q2 in `open_questions.md` (cross-entity transaction model); the
prior `SharedTxHandle`/`SavepointHandle` two-type design (preserved on branch
`c31-wip`).
**Related**: D6 in `Design_Principles_and_Key_Decisions.md` (parameterised
transaction semantics).

---

## Background

D6 records that transaction semantics are programmer-declared (`#atomic`, `#saga`,
`#stm`, `#bestEffort`). This document works out the concrete implementation for
each mode given the actual architecture.

"Is saga sufficient for all cross-entity coordination?" — answered: no. Saga
cannot provide "no partial state ever visible" guarantees. All four modes are
needed, with different cost/guarantee tradeoffs.

---

## Architectural asset: the shared triple store

Unlike a truly distributed system, all entities share a single `Store` with a
monotonically-increasing `TxId` counter. A single `TxId` can already span writes
to multiple entities' facts — nothing in the `FactStore` trait restricts a
transaction to one entity's subjects. The access-control rule ("only the owning
entity writes its own internal facts") is policy enforced above the trait, not a
structural constraint.

Crucially: **each entity writes its own facts under the shared `TxId`** — the
coordinator does not write on anyone's behalf. Encapsulation is preserved. The
coordinator's only job is holding the `TxId` and calling commit/rollback. The
store's commit is then genuinely atomic across all participants.

---

## The in-memory slot problem (critical)

Entity ivar state lives in **two places simultaneously**:

1. **In-memory slots** (`InstanceState::slots`) — the live source of truth, read
   and written by `assign_var` / `lookup_var`.
2. **Store tentative facts** — durable record buffered under `current_tx`,
   committed at handler boundary.

In `assign_var`:

```rust
crate::store_bridge::buffer_ivar(self, d.id, name, &value); // tentative in store
obj.set_slot(name.clone(), value);                           // in-memory slot: immediate
```

For **single-entity handlers** this is fine: `current_tx` commits at the handler
boundary before the next message is dequeued; the two representations stay in
sync and no external observer reads intermediate state.

For **multi-entity atomic transactions** it breaks: between entity A finishing
its step and the coordinator committing, an external message would see A's
updated in-memory state, and an abort would leave the in-memory slot diverged
from the store. Both problems (visibility and rollback) are solved at the entity
layer via **shadow writes**.

---

## The four modes

### `#bestEffort`

Current behavior. Fire-and-forget sends, no cross-entity coordination. No new
mechanism.

### `#saga`

Sequence of local transactions with compensating messages for rollback.
Intermediate state IS observable between steps — by design. Pure Smol over the
condition system; no tx primitives. (OQ-TX-6, deferred.)

### `#stm` (optimistic)

Entities not locked; a conflict check at commit time. Intermediate state NOT
externally observable. Entity-level versioning (increments on any write).
Shadow writes activated for all participants; mailbox locking is **not** used.
Full design in chunk 32.

### `#atomic` (pessimistic)

True multi-entity atomic commit; no intermediate state ever visible externally.
Requires **mailbox locking** + **shadow writes**. This is the subject of chunk 31.

---

## Unified transaction context (`TxContext`)

A single Rust enum represents the coordinator's view of an in-flight transaction
at any nesting level. Every level — root or nested child — owns its **own** op
buffer; children are full sub-transactions, not savepoint offsets into a shared
buffer (see "Why per-level buffers, not savepoints" below).

```rust
pub enum TxContext {
    Root(Arc<RootTxInner>),
    Child(Arc<ChildTxInner>),
}

pub struct RootTxInner {
    pub ctx_id: TxCtxId,                            // unique context identity
    pub tx_id: TxId,                                // shared store TxId for the whole tree
    pub tx: Mutex<Option<OwnedStoreTransaction>>,   // this level's op/value buffer
    pub enrolled: Mutex<Vec<InstanceHandle>>,       // participants joined at root level
}

pub struct ChildTxInner {
    pub ctx_id: TxCtxId,                            // unique context identity
    pub outer: Arc<TxContext>,                      // parent; arbitrary nesting depth
    pub tx: Mutex<Option<OwnedStoreTransaction>>,   // this child's own op/value buffer
    pub participants: Mutex<Vec<InstanceHandle>>,   // participants joined at THIS level
}
```

`TxCtxId` is a process-unique id minted per context (root and each child). It is
the identity used for lock admission and the deadlock wait-for graph. The whole
tree commits under one store `TxId` (the root's), so the final commit is atomic.

Rationale for the unification (this session's correction to the WIP design):

- **One handle type.** Root and child share the generic operations (enroll a
  participant, commit, rollback); two distinct Rust types forced every call site
  to branch anyway. A single enum centralises that branch.
- **No new `Value` variant.** The handle is opaque to Smol, so it is carried as
  `Value::Handle(Arc<TxContext>)` — exactly the use case `Value::Handle`'s doc
  comment describes ("concrete type is downcast by the primitive that created the
  handle"). Adding `Value::SharedTx`/`Value::Savepoint` was forbidden variant
  growth and is removed.
- **General nesting.** `ChildTxInner::outer` links to the enclosing `TxContext`,
  so a child under a child is just a longer chain; ancestry is a walk up `outer`.

### Why per-level buffers, not savepoints

The earlier design gave the root a single op buffer and recorded a `sp_index`
(buffer length) per nested level, rolling back by truncating to that index. This
**only works for LIFO nesting**. It breaks for *parallel* children — the
"try many things, keep what succeeds" pattern:

```
root: ops = [a, b]
  child A: sp=2, appends c,d  → [a,b,c,d]
  child B: sp=2, appends e    → [a,b,c,d,e]
  child A aborts → truncate(2) → [a,b]   ← also discarded B's write e
```

A linear buffer with a truncation offset can only unwind the most recent region;
independent children that each commit-or-abort on their own need independent
buffers. So each context owns its buffer:

- **Child commit** → `parent.tx.absorb(child.tx)` (requires equal `TxId`).
- **Child abort** → drop `child.tx`. Independent of any sibling.
- **Root commit** → commit `root.tx` to the store. **Root abort** → drop it.

This handles nesting *and* parallelism uniformly and removes `savepoint()`,
`rollback_to()`, `sp_index`, and any `TentativeOp::Savepoint` marker entirely.

### What lives where

- **Where writes go** — each participant buffers into the context level it is
  enrolled at; that buffer merges upward on commit. All buffers share the root's
  `TxId` (walk `outer` to the `Root`) so the eventual store commit is atomic.
- **Where membership is recorded** — the **active context head** when the
  participant's first tagged message arrives. An entity that first joins during a
  child belongs to that child's `participants`, **not** the root, so child abort
  unwinds exactly those entities while ancestor-level entities survive.

---

## Lock + shadow: a per-entity stack of context-tagged frames

Shadow writes and the mailbox lock are the **same** per-entity structure: a stack
of frames, each tagged with the context that owns it.

```rust
pub tx_frames: Mutex<Vec<TxFrame>>,
// where:
pub struct TxFrame {
    pub ctx_id: TxCtxId,                  // owning context
    pub shadow: HashMap<Arc<str>, Value>, // speculative named-ivar writes
}
```

### Nested-transaction locking (Moss's model)

A child may access an entity already held by an **ancestor** (locks inherit down
the tree); **siblings conflict** and must serialize, exactly as two root
transactions would. On a child's commit its locks **pass up** to the parent; on
abort they are released.

Because two sibling subtrees can never hold the same entity simultaneously, an
entity is only ever locked along a **single ancestry path** at any instant — so a
**stack** (deepest holder on top) is sufficient; we never need a tree.

### Admission and lifecycle

- **Admission for a tagged message from context C** (`run_entity_loop` dequeue):
  allowed iff `tx_frames` is empty **or** the top frame's `ctx_id` is an
  **ancestor-or-equal** of C (walk `C → C.outer → …` looking for the top's id).
  If the top is a *sibling/unrelated* context, the message **parks** until the
  entity pops back to a common ancestor — this is sibling serialization.
- **Enroll / first touch at a deeper level** → push a new `TxFrame` tagged with C;
  open a local `current_tx` under the root `TxId` (first enrollment only).
- **`assign_var` under a tx** → write to the **top** frame's `shadow` (instead of
  `obj.set_slot`); still `buffer_ivar` into the local `current_tx`.
- **`lookup_var` under a tx** → scan frames top-to-bottom, fall through to the
  live slot on miss (read-your-writes with nesting).
- **Commit a level** → merge the top frame's `shadow` into the frame below and
  retag that frame to the parent context (**lock passes up**); if it was the only
  frame, apply to live slots and pop (entity unlocks). Absorb the local
  `current_tx` into the committing context's buffer.
- **Abort a level** → pop and discard the top frame; drop the local `current_tx`.

**Depth is the nesting/lock count.** `tx_frames.len()` is how many levels along
the current ancestry path the entity is in; the mailbox is locked while it is
non-empty and unlocks when it drains to zero. The envelope filter keys on
**context identity + ancestry**, not the root `TxId` (sibling children share the
root id, so a root-id filter would wrongly admit a sibling).

The coordinator needs only the per-level participant list
(`RootTxInner::enrolled` / `ChildTxInner::participants`) — whom to send
`pvtExitTx:` at that level's commit/abort. Everything else derives from each
entity's own `tx_frames`.

(Indexed-slot shadowing is deferred — named ivars cover all current kernel
entities. See "Out of scope" in the chunk plan.)

---

## Tagged envelopes and lazy enrollment

The `Evaluator` gains `current_tx_ctx: RefCell<Option<Arc<TxContext>>>`. When
set, any entity message send tags the outgoing `Envelope` with the handle. This
is the only propagation mechanism — blocks and method calls within the
transaction inherit the tag because they share the evaluator;
`ContinuationContext::fork_context()` carries it into async continuations.

When `run_entity_loop` dequeues a tagged envelope from context C for an entity not
yet enrolled at that level, it auto-enrolls before dispatching: pushes a `TxFrame`
tagged with C, adds itself to C's participant list, and (first enrollment only)
begins a local `current_tx` via `begin_owned_with_id` under the root `TxId`. No
separate `enterTx:` round-trip.

**Mailbox locking (ancestry-based).** When `tx_frames` is non-empty, the dequeue
loop only takes an envelope tagged with context C if the top frame's `ctx_id` is
an ancestor-or-equal of C; a sibling/unrelated C's message stays queued (in
order) and runs after the entity pops back to a common ancestor. The filter keys
on **context identity + ancestry**, not the root `TxId` — sibling children share
the root id.

---

## Deadlock detection

Part of C31 — an `#atomic` implementation without it is incomplete. Follows the
classic RDBMS pattern (e.g. PostgreSQL): wait-for graph with threshold checking,
built on the `Timer`/`Duration` primitives from C31a.

The `Transaction` entity tracks `locked` (acknowledged participants) and
`waiting` (sent a tagged message, not yet acknowledged). When a tagged message
goes to a new participant it is added to `waiting` and a timer is armed; on
acknowledgment it moves to `locked` and the timer is cancelled if `waiting`
empties. If the timer fires first, `pvtCheckDeadlock` queries the store
(`participates-in` facts) to build the local wait-for graph and looks for a cycle:
for each entity in `waiting`, who holds it, and does that holder's `waiting` set
include any entity in `self locked`? A cycle → raise a `Deadlock` condition to the
initiating caller (this transaction is the victim); the other proceeds. No
victim-selection heuristic in C31; the detector nominates itself.

**Graph nodes are contexts, not root transactions.** Because sibling children
serialize on shared entities, two siblings under the *same* root can deadlock each
other. The wait-for graph therefore keys on `TxCtxId` (per-context identity), and
`participates-in` facts record the holding **context**, not just the root `TxId`.

---

## Transaction Smol interface

Client code receives the `Transaction` entity as the block argument. The raw
handle is a private ivar, never exposed to Smol callers.

```smalltalk
Transaction atomic: [:tx |
    tx status.        "=> #pending | #committed | #aborted"
    tx txId.          "=> opaque id, useful for store correlation"
    tx abort.         "abort from within the block"
    "... do work ..."
]
```

Participants do not interact with the `Transaction` entity to do their work —
they process tagged messages normally. The interface is for callers/observers.

Nesting is uniform: `Transaction atomic: [...]` works identically whether
outermost or nested; whether the handle is a `Root` or a `Child` is internal.

---

## Minimal primitives

```
_primBeginSharedTx                → Handle(TxContext)   (Root if none active, else Child)
_primCommitSharedTx: handle       → Root: commit store buffer. Child: absorb into parent.
_primRollbackSharedTx: handle     → Root: drop store buffer. Child: drop own buffer.
_primEnrolledParticipants: handle → collection of participants at this level
```

Entity-side (called internally by enrollment/exit; named so they are testable):

```
_primLockMailbox: handle            "push a TxFrame tagged with handle's context"
_primActivateShadowWrites: handle   "begin local current_tx under root TxId"
_primExitTxMode: #commit            "absorb local current_tx into ctx buffer; merge top frame down (lock passes up); pop"
_primExitTxMode: #rollback          "discard local current_tx; pop+discard top frame"
```

Everything else — `Transaction atomic:`, retry loops, saga orchestration,
deadlock detection — lives in Smol. `#saga` needs none of these.

---

## Open questions

**OQ-TX-1: Shadow writes vs. snapshot — RESOLVED.** Shadow writes; no
snapshot/restore.

**OQ-TX-2: `enterTx` timing — RESOLVED.** Tagged messages queue behind the
in-progress handler; the handler boundary is the natural acquisition point.

**OQ-TX-3: `#stm` write participants — RESOLVED (moot).** Shadow writes for all
participants; role not declared upfront.

**OQ-TX-4: `#stm` retry and failure — RESOLVED.** Default retry limit 3,
configurable; exhaustion raises a condition; no auto-escalation to saga.

**OQ-TX-5: Nested transactions — RESOLVED.** Per-level child transactions via the
`TxContext` `outer` chain; each level owns its op buffer (merge-up on commit, drop
on abort). Uniform Smol interface. Replaces the savepoint-index model, which could
not represent parallel children.

**OQ-TX-6: Saga and the condition system — DEFERRED.** Revisit at
`Transaction saga:`.

**OQ-TX-7: Smol-level representation and observability — RESOLVED.**
`Transaction` is an entity; participation is a store fact; raw handle private;
`ObservableTransaction` deferred (opaque/in-memory shadow is the C31 default).

**OQ-TX-8 (NEW): Unified `TxContext` as `Value::Handle` — RESOLVED.** Root and
child are one enum carried as `Value::Handle(Arc<TxContext>)`; no new `Value`
variants; nesting via `ChildTxInner::outer`; per-level enrollment.

**OQ-TX-9 (NEW): Parallel children and shared entities — RESOLVED.** Each context
level owns an independent op buffer (not a savepoint offset), so parallel children
("try many things, keep what succeeds") roll back independently. Lock + shadow are
one per-entity stack of context-tagged `TxFrame`s following Moss's nested-locking
model: descendants inherit ancestor locks; siblings serialize on a shared entity
(message parks until the holder's subtree exits). Because siblings can never hold
the same entity at once, an entity is locked along a single ancestry path, so a
stack suffices. Admission and the deadlock graph key on `TxCtxId` + ancestry, not
the root `TxId`.
