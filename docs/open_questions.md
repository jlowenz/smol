### Open Design Questions

Design questions that arise during implementation work and require a dedicated
design session before they can be resolved. Broader open issues (error handling
model, capability model, gradual typing, etc.) remain in `Project_Summary.md`.

Each entry records: what the question is, why it matters, the tensions to resolve,
and its current status. Questions should be marked **Resolved** (with a pointer to
the decision record) once settled, not deleted.

---

## Q1: Multi-version live entities

**Status**: Open — needs design session  
**Raised**: C27 design discussion

The bitemporal store records the full history of every entity's ivar values. The
question is whether the system should allow multiple *live, message-receiving*
versions of the same entity to coexist — e.g. "AccountEntity as it was on
2026-05-15" running alongside the current version for comparison, debugging,
replay, or branching.

**Tensions to resolve:**

- **Identity**: is a versioned entity the same `EntityId` as its origin, or a new
  one? If new, how is the origin relationship expressed in the store?
- **Mutability**: is a past-version entity read-only (frozen replay) or can it
  diverge (a branch that can receive messages and evolve independently)?
- **Actor semantics**: does a versioned entity have its own mailbox and coroutine,
  or is it a read-only projection with no scheduler presence?
- **Relationship to resume-from-store**: reconstructing a past state from durable
  facts is a prerequisite; the resume mechanism (planned C27.5/C28) likely needs
  to be designed with versioning in mind.
- **Relationship to cross-entity transactions**: if a versioned entity can diverge,
  how do branched versions that later need to be reconciled interact with the saga
  model?

Note: `historyOf:` (C27) is read-only temporal access and is distinct from this.

---

## Q2: Cross-entity transaction model

**Status**: Design substantially complete — see `docs/tx_design.md`  
**Raised**: C27 design discussion  
**Updated**: 2026-06-19 design session

All implementation questions resolved except OQ-TX-6 (saga/condition system,
deferred to saga implementation). Key decisions: `Transaction` is an entity; shadow
writes for reversible ivar changes; lazy participant discovery with wait-for graph
deadlock detection; `#atomic` = mailbox lock + shadow writes; `#stm` = shadow writes
+ optimistic version conflict; nested transactions via savepoints; `OpaqueTransaction`
default / `ObservableTransaction` opt-in. See `docs/tx_design.md` for full analysis
and minimal primitive set.

---

## Q3: Value object GC and structural sharing

**Status**: Open — implementation deferred past C27  
**Raised**: C27 design discussion

Value objects are stored in the triple store using content-hash identity. Because
they are immutable and potentially shared across many entities, they should ideally
be stored once (structural sharing) and removed when no entity references them.
C27 does not implement either optimisation; facts accumulate indefinitely.

**Tensions to resolve:**

- **Structural sharing**: an "already stored?" check on every value object
  assertion adds a store read per ivar set. Options: Bloom filter, session-scoped
  `HashSet<EntityId>`, or accept re-assertion idempotency without the check.
- **GC**: value objects have no owning entity. Reference counting requires tracking
  every `Term::EntityRef` that points to a value hash ID — non-trivial across the
  full fact space. Mark-and-sweep over value IDs is simpler but requires a full
  store scan. Generational GC (most value objects are short-lived within a handler)
  is a third option.
- **Interaction with history**: a value referenced in a historical ivar fact should
  arguably be retained even if no current ivar points to it. What is the retention
  policy for values in historical (closed-interval) facts?

---

## Q4: Schema evolution — ivar changes in a live system

**Status**: Open — not addressed in C27  
**Raised**: C27 design discussion; also noted in `Project_Summary.md`

When a class's ivar set changes (slot renamed, added, or removed), existing entities
have historical facts recorded under the old predicate names and no facts (or stale
facts) under the new ones.

**Tensions to resolve:**

- **Rename**: should a rename create a new predicate and migrate historical facts,
  or alias the old predicate to the new name?
- **Remove**: orphan predicates accumulate. Should the store enforce that no
  live entity holds open facts for a removed predicate, or is that the programmer's
  responsibility?
- **Add**: new slots have no historical facts. Queries over time return absence
  for the period before the slot existed — is that the right semantic, or should
  there be a "default value" mechanism?
- **Versioned class definitions**: the store could record class definitions as
  facts themselves (code as facts), making schema changes auditable and replayable.
  This aligns with the project's "code is data" goal but requires block/AST storage.

---

## Q5: Visibility model for cross-entity store queries

**Status**: Open — deferred from C27  
**Raised**: C27 design discussion

In C27 all entity ivars are registered as `Visibility::Private`. This means
`World class >> entity:historyOf:` is blocked by the store's access check for any
entity other than the querying entity. A visibility model is needed for shared
queries to be useful.

**Tensions to resolve:**

- What Smol syntax expresses that a slot is `Shared` or `Public`? A pragma, a
  keyword in the class definition, or a convention?
- Who can promote a predicate's visibility — only the defining class, or any entity?
- Does visibility apply to current state only, or to history as well? (A slot
  might be publicly readable now but have private historical values.)
- How does visibility interact with external assertions (facts asserted by one
  entity about another, which the store already supports via `asserter != subject`)?

---

## Q6: Transient entities

**Status**: Open — needs careful design  
**Raised**: C27 design discussion

Some entities may be purely computational — created for the duration of a
computation, with no need for temporal history in the store. Clojure's transients
are an analogue: a performance optimisation that doesn't change observable semantics.

**Tensions to resolve:**

- **P5 (State Has History)**: does exempting an entity from store assertions violate
  the principle, or is "no history needed" a legitimate, explicit declaration?
- **Declaration mechanism**: a pragma (`transient: true` in the class definition),
  a superclass convention, or a runtime hint?
- **Observability**: transient entities would not appear in `World instancesOf:`
  queries and would leave no trace after termination. Is this acceptable, or should
  they at least record their existence and lifetime?
- **Interaction with value objects**: value objects are already "history-free" from
  the entity perspective (they use content-hash IDs, not temporal facts). Is there
  a unified concept here, or are transient entities semantically distinct?

---

## Q7: Implementation/responsibility split and self-hosting boundary

**Status**: Open — not urgent; needs design session when self-hosting becomes a near-term goal  
**Raised**: C27 design discussion (surfaced by `content_hash_id` placement)

Where should the boundary sit between what is implemented in Rust (interpreter
primitives), what lives in the Smol kernel, and what is user-overridable? And how
far does self-hosting go — should the system eventually be able to host its own
interpreter/compiler, in the spirit of Smalltalk's image?

**Current working model (C27-era):**

- **Rust `_prim*` methods**: operations requiring direct struct access, or whose
  correctness depends on being non-overridable (e.g. `content_hash_id`, object
  allocation, identity comparison).
- **Smol kernel (`kernel.smol`)**: standard library behavior expressible purely
  from primitives; can in principle be overridden by user code.
- **System internals**: Rust functions never exposed as message sends (e.g. store
  bridge, hash for content-addressing); migrate to VM kernel functions during
  self-hosting, not to user-visible methods.

**Tensions to resolve:**

- How far does self-hosting go? Smalltalk hosts its entire environment in the image,
  including the compiler and debugger. Is that the goal here, or is a hybrid
  (Rust VM + Smol kernel + Smol standard library) the intended end state?
- Which `_prim*` methods eventually become expressible in Smol (and thus migrate to
  `kernel.smol`), and which remain permanent VM intrinsics?
- For content-hash specifically: the store's hash must be stable across sessions and
  non-overridable. Even in a self-hosted system this should remain a VM intrinsic,
  not a user method. This is a useful concrete example of a *permanent* primitive.
- How does the migration path work in practice? Rust `_prim*` → VM intrinsic →
  (optionally) Smol kernel method. When is each step taken?

---

## Q8: Block and namespace storage (code as facts)

**Status**: Open — deferred from C27  
**Raised**: C27 design discussion

Blocks (AST + captured environment) and namespaces are storable in principle and
important for the "code as facts" and notebook/literate programming goals.

**For blocks:**

- Requires `serde::Serialize/Deserialize` on `Expr`/`Stmt` in `ast.rs`.
- A stored block captures entity references — these become `Term::EntityRef` values
  that must resolve on load (interaction with Q1 resume-from-store).
- Method definitions are a special case: they are blocks with a selector and
  belong to a class. Storing them as facts makes the class definition auditable
  and diff-able.

**For namespaces:**

- A namespace is a collection of class bindings and sub-namespaces.
- Storing it supports the notebook capability: a namespace can carry documentation,
  literate examples, and runnable snippets as first-class facts.
- Requires designing how namespace facts relate to class definition facts (avoiding
  duplication while maintaining a single source of truth).

----

## Q9. Supporting ultra lightweight objects and massive parallelism on GPU

**Status**: Open - Research - Experimental

**Raised**: External discussions

Can the system support designs where 10-100s millions of objects are run in a kind of SIMD architecture, potentially on GPU. This would require interesting optimizations that may not be readily available within the current constructs of the Smol architecture. Cache coherence, method caching, message broadcast, gpu/cpu transfers, etc. 

Example: large, complex, constructive solid geometry models and real-time raytracing or analysis. 

This concept is very ambiguous, but worth recording for future consideration.

**Refinement (C32 planning, 2026-07-03) — broadcast vs. data-parallel.** A
colleague asked whether we can *truly* broadcast a message (SIMD-style) to many
entities at once. Splitting the idea into what the actor model permits:

- **True simultaneous delivery is incompatible with the actor model as built.**
  Each entity owns a mailbox + independent coroutine; delivery is a per-mailbox
  enqueue processed when that entity's coroutine gets there. One send that all
  recipients *execute at the same instant* would require abandoning per-entity
  autonomy (the opposite of SIMD's single-instruction-lockstep model).

- **(a) Broadcast/multicast primitive** — feasible and useful: one Rust-level
  call fans an envelope out to N mailboxes (shared `Arc`'d args; no per-send Smol
  dispatch) and returns N reply futures. Not simultaneity — cheaper *async*
  fan-out. Immutable Values make the payload free to share. Candidate surface:
  `aGroup broadcast: aMessage`. Relevant to C32's 2PC prepare/commit fan-out as a
  perf option (does not affect correctness).

- **(b) Data-parallel bulk operation** — the *real* SIMD case: many same-class
  entities + a pure, uniform operation over their own state, executed vectorized
  over a columnar (SoA) state layout (rayon/SIMD/GPU). Requires same class, pure
  op, vectorizable layout, and giving up per-entity message-ordering. A
  specialized fast path in the "moldable / bulk transform" spirit, not the general
  send. This is the concrete shape Q9's "SIMD architecture" would most likely
  take on CPU before any GPU work.

Neither is needed for C32; recorded here for a later chunk.

## Q10: Transactional message dispatch (handler as a depth-1 transaction)

**Status**: Open — candidate future chunk
**Raised**: 2026-06-20 C31 implementation session

While building the shadow-write + per-frame durable-buffer machinery for C31's
multi-entity transactions, an observation: a single message handler is *already*
effectively transactional at the store level (durable ops buffer into `current_tx`
and commit on success / roll back on crash). The in-memory live slots, however,
are written **directly** during the handler, not shadowed.

The question: should a normal message handler be a true depth-1 transaction —
shadowing its in-memory writes too, and atomically **applying or reverting both
the shadow and the store buffer** based on whether the handler completed without
error?

Two distinct things this would buy:

1. **A different failure policy.** Today the actor model is *let-it-crash*: an
   unhandled signal terminates the entity. A transactional handler could instead
   **fully revert (in-memory + store) and keep the entity alive and clean** — an
   "atomic apply-or-revert message" alternative to crash-and-die. This is a
   substantive change to the actor failure model and deserves its own discussion.
2. **Unification.** A normal handler becomes a depth-1 transaction reusing the
   exact shadow + per-frame buffer machinery built for C31. The two-path
   "shadow vs. direct" split in `assign_var`/`lookup_var` collapses into one path,
   and the multi-entity tx code becomes a strict generalization.

**Why deferred (not in C31):** (a) it changes the actor failure model — a
philosophical decision, not a side effect of C31; (b) it puts the shadow on the
hot path of *every* message, so it needs a performance story (e.g. lazy frame
creation, only when the first write occurs); (c) folding it in re-expands C31's
scope. Nothing in C31 blocks it — C31's machinery is exactly what it would reuse.

**Note:** there is no current *consistency bug* to fix here. For a surviving
entity the existing path always commits on success, so live slots and the store
do not diverge; the crash case terminates the entity, making leftover live slots
moot. The value of this idea is the new failure policy and the unification, not a
correctness repair.
