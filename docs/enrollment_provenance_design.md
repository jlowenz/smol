# Transaction enrollment redesign — access-based enrollment + born-in-tx exclusion

**Status**: Implemented (C32). `TxCommon.spawned` + `Evaluator::ensure_tx_enrolled`;
message-receipt enrollment removed from `run_entity_loop`. All 729 interpreter tests
pass, including the newly un-ignored `stm_returns_result_before_block_work_completes`.
**Raised**: C32, while debugging the `stm_returns_result_before_block_work_completes`
MNU (`pvtStmPrepare` sent to a `Class` entity).
**Related**: `docs/open_questions.md` Q6 (transient entities), Q10 (handler as
depth-1 tx — anticipates lazy frame creation), Q11 (durable orphan GC),
`docs/tx_design.md`, `docs/chunk_32_plan.md`.

## Problem

Enrollment currently fires on the wrong event: **"received a tagged envelope while
not the coordinator"** (`entity.rs`, the `already_enrolled` block ~L1162–1188).
Every entity transitively touched during a transaction — by *any* message, even
one that reads/writes no transactional state — is pushed into `ctx.participants()`
and later sent the participant protocol (`pvtStmPrepare`, `pvtExitTx:`).

Two failure modes:

1. **Hard failure (the MNU).** A **class entity** enrolls (e.g. `ValueHolder
   initialValue:`, a class-side factory reached via `inject:into:` inside
   `pvtStmCommit`). The coordinator sends it `pvtStmPrepare`, an `Object` *instance*
   method a `Class` does not inherit → `MessageNotUnderstood`. A class entity also
   has **no transactional state**: class variables live in the non-transactional
   `Class.class_var_storage` (`eval.rs` `assign_var`/`lookup_var` write/read it
   directly, no frame, no shadow, no version). So enrolling a class is not merely
   broken — it is *meaningless* under the current representation.

2. **Waste (not a correctness bug).** Stateful utility entities created inside the
   transaction (`ValueHolder`, `OrderedCollection`, …) genuinely write ivars, so
   they enroll, get frames, and are prepared/committed. They understand the
   protocol and their writes are conflict-free (unreferenced outside the tx), so
   this is overhead, not incorrectness — but it is pervasive: any collection- or
   string-building helper inside a tx block drags itself and its class into the
   participant set.

Message-receipt enrollment over-approximates the true participant set.

## Principle

> A participant is a **pre-existing** entity whose **transactional state** the
> transaction actually reads or writes.

Two independent corrections follow, and **both are required** — neither subsumes
the other:

- **(A) Access-based (lazy) enrollment** removes entities that touch *no*
  transactional state (class entities; pure-computation messages).
- **(B) Born-in-tx exclusion** removes *stateful* entities that were *created
  inside* the transaction (they have no committed baseline, are unreachable by any
  other tx, and cannot conflict).

Class entities are fixed by (A) *structurally*: they touch no frame-backed state,
so they never enroll — no class special-case. Stateful temporaries are fixed by
(B).

## Mechanism A — access-based lazy enrollment

Move the enrollment trigger from message receipt to **first access to the entity's
transactional state under the active tag**. The hook points already exist as the
`has_frame_for(cid)` gates:

- **Write** (`eval.rs` `assign_var`, ~L389–419): today, `!has_frame_for(cid)`
  falls through to a live write. New: under an active ctx `C`, `!has_frame_for(C)`
  → **enroll now** (push frame, snapshot version, add to `C.participants()`), then
  shadow.
- **Read** (`eval.rs` `lookup_var`, ~L304–315): today, reads consult the shadow
  frame if present, else the live slot, and register *nothing*. STM
  read-validation currently works *only* because message-receipt enrollment
  snapshotted the version. So the read path **must also** hook lazy enrollment
  (enroll + snapshot version on first read under `C`), or the STM read-set
  collapses to the write-set → missed read-conflicts → **unsound**. This is the
  load-bearing subtlety of Mechanism A.

Consequence: class entities never enroll (class-var storage is non-transactional →
`has_frame_for` is never satisfiable for them).

## Mechanism B — born-in-tx exclusion

**Provenance lives on the ctx, not the entity.** Add, next to `participants`:

```rust
// shared_tx.rs, in the common ctx state
spawned: Mutex<HashSet<EntityId>>,   // ids created under this ctx
```

- **Record at spawn:** when an entity is created while `ev.current_tx_ctx` is
  `Some(C)`, insert its `EntityId` into `C.spawned` — synchronously, *before* the
  new entity processes its first message (`initialize`). The id is minted
  synchronously and the ctx is shared (`Arc` + `Mutex`), so the entity's own
  coroutine sees the entry when it later checks.
- **Use at the enrollment predicate (Mechanism A's hook):** on first state access
  under ctx `C`, if `C.spawned` contains `self.id` → **skip enrollment**, fall
  through to the existing non-enrolled **live-write** path. No frame, no shadow, no
  version, not a participant.

**Why the ctx, not an `EntityData` field:**

1. Provenance is only consulted while the birth ctx is alive; it is genuinely
   transaction-scoped and dies with the ctx. A field permanently stains every
   entity with post-tx-meaningless data.
2. Per-entity density: an `Option<TxCtxId>` on `EntityData` costs bytes on *every*
   entity forever; the ctx-set costs nothing outside transactions.
3. Exact-ctx match is automatic — each ctx has its own set, so E born in A1 is in
   `A1.spawned`, not `A2.spawned`; a child `A2` therefore enrolls E normally. This
   *is* summary point 1 ("A1 creates E, child A2 modifies E ⇒ E enrolled in A2"),
   for free.

**Store `EntityId`, not `InstanceHandle`** — a handle set would pin every transient
alive until commit, defeating in-memory `Arc` reclamation. The access-time check
has `self` in hand, so `C.spawned.lock().contains(&self.id)` suffices.

**Escape & durability (accepted debt).** A born-in-tx entity writes live /
per-message. If it *escapes* — its ref is written into a pre-existing
participant's ivar — those live-committed facts are exactly what makes the ref
resolve at that participant's commit; correct with no extra work. If its tx *rolls
back and it never escaped*, its facts are orphaned in the store. This is the
deliberate coupling to **Q11 (durable orphan GC)**: full born-in-tx exclusion
trades shadow-buffering overhead for GC debt. Chosen knowingly (we need GC anyway,
and are already generating garbage).

## Consequences & verification items

- **Full exclusion + GC debt** (decided), tracked as Q11; recommend reclaiming with
  the Q3 value-object glacier as one store-layer pass. Not a VM prerequisite.
- **Atomic mailbox-lock timing shifts** from message-receipt to first-state-access.
  Plausibly fine or better (a message touching none of A's state imposes no
  consistency requirement on A), and it interacts with the **C31 S5c wait-for
  graph / deadlock detection**, which is built from those locks. *Verified:* the
  full atomic + S5c deadlock suite passes unchanged.
- **`is_tx_coordinator` guard is retained** — a nested inner coordinator still
  writes its own `status`/`handle` ivars under the outer tag and would otherwise
  self-enroll.
- **Read-path enrollment + version snapshot** is new and load-bearing (see
  Mechanism A). Add STM read-conflict coverage that would fail if a read-only
  participant were dropped from the validated set.

## Rejected alternatives

- **Teach class entities the participant protocol (former "option 3").** Symptom
  only: the stateful-temporary waste remains, and doing it *non-fakely* requires
  making `class_var_storage` transactional — i.e. it *is* the future "classes are
  real participants" chunk, not a fix for this bug. Deferred to that chunk; the
  same lazy hook will enroll classes for real once class vars are frame-backed.
- **`born_in_ctx` field on `EntityData`.** Rejected for the density and
  ephemerality reasons above.
- **Suppressing only the coordinator's own commit-protocol sends.** Too narrow —
  the problem is any entity-spawning utility code inside *any* tx block, not just
  the commit path.

## Related finding — the HC / errors-in-handlers question (resolved)

A WIP experiment had rewritten `pvtStmAttempt:` from the nested form
`[ HandlerContext new on: OkToCommit do: […] run: […] ] on: Error do: […]`
into a single multi-handler `HandlerContext new on: { OkToCommit -> … . Error -> … }
run: …`. That collapsed `OkToCommit` and `Error` into *sibling* handlers of one HC,
so an error signalled **inside** the `OkToCommit` handler (e.g. `pvtStmCommit`
failing, or `tx abort` surfacing on the commit path) was no longer caught — it
escaped as `_primStmPrepare: no active transaction context` and broke the
abort/conflict/retry tests.

**Answer:** the **nested** form is correct precisely because the `OkToCommit`
handler runs *within the dynamic extent* of the outer unwinding `on: Error do:`, so
any error raised while committing is caught, rolled back, and rejected. That is the
desired STM behavior (any commit-path error → rollback + reject). The multi-handler
sibling form does not provide this. The nested form was restored; the "HC issue"
was self-inflicted by the experiment, not a defect in the handler system.
