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

---

## Q11: Durable garbage collection of orphaned entity facts

**Status**: Open — recording now; scheduling deferred (see recommendation)
**Raised**: C32 enrollment redesign (born-in-tx exclusion)

In-memory entities are reclaimed by `Arc` refcount, but the **durable facts** an
entity writes to the store are never reclaimed. An entity can become unreachable
while its facts persist — leaving orphan facts. The concrete driver is the C32
enrollment redesign (see `docs/enrollment_provenance_design.md`): a **born-in-tx**
entity writes its facts *live / per-message* (it is deliberately excluded from the
transaction). If its transaction **rolls back and it never escaped**, those facts
are orphaned by design — an accepted, recorded debt of choosing full born-in-tx
exclusion over shadow-buffering.

**Tensions to resolve:**

- **Unify with Q3 (value-object glacier).** Orphaned entity facts and orphaned
  value objects are the same problem — unreferenced records in the fact space —
  and the same mark-sweep-over-reference-edges pass reclaims both. Building them
  together is the least-wasted-effort path.
- **Retention vs. history (P5, State Has History).** A rolled-back born-in-tx
  temporary arguably *never had legitimate history* → reclaim freely. A committed
  entity later made unreferenced *did* have history → retention policy is the same
  open question Q3 raises for values in closed-interval facts. Distinguish
  "rolled back" (reclaim) from "committed then orphaned" (history policy).
- **VM dependency: none.** Durable reclamation is a store-layer concern; a correct
  glacier/mark-sweep pass does not require the custom VM. Efficient/generational
  collection may later benefit from VM allocation control, but that does not gate a
  first correct pass. GC does **not** need to precede the VM.

**Recommendation:** schedule alongside Q3 as a single store-layer reclamation pass,
when fact accumulation begins to matter (dev/test accumulation is slow). Not a VM
prerequisite.

---

## Q12: Trait composition — compile-time fail-fast for conflicts & requirements

**Status:** Open (deferred). Traits MVP implemented with **deferred, send-time**
enforcement.

`docs/traits_plan.md` §4/§6 specify *fail-fast at class-definition time*: an
unresolved conflict marker or an unmet derived requirement should error when the
class is composed. The MVP cannot honor this because the definition model is
incremental — `Value subclass: #Html uses: <TExpr> …` runs as one statement, and the
class's own methods (`Html >> foo [...]`) arrive on *later* statements. At the
instant `uses:` executes there is no "class method set" to validate against, and no
end-of-definition point to hook (method extensions from other files are coming, so a
"when the source file finishes" hook would be wrong too).

**What the MVP does instead (per-selector send-time):**
- Trait methods are flattened into the class dict at `uses:` time.
- A conflict flattens as a **sentinel method** (`Object >> pvtTraitConflictFor:`) that
  raises `TraitConflict` *when the conflicting selector is actually sent*. A later
  class-defined method (or a non-conflicting override) replaces the sentinel via
  normal dict insertion.
- An unmet requirement is simply a `self`-send that fails (DNU/MNU) when reached.
- `Trait >> requirements` / `conflicts` expose the derived sets for inspection, but
  composition does not consult them to fail.

**Intended follow-up (whole-class send-time gate).** The originally intended
semantics are stronger: a class left with *any* unresolved conflict or unmet
requirement should reject **every** send to the class/instance (not only the offending
selector) — the class is unusable until the author resolves the composition. This
needs a **status flag on `Class`** (e.g. `unresolved: RwLock<{conflicts, reqs}>`) set
at `uses:` time and cleared when later class-defined methods satisfy the last
outstanding item; dispatch consults it before method lookup. Deferred only because it
adds a `Class` field (touches ~15 constructors) plus a per-dispatch check — a
self-contained change on top of the MVP. The current per-selector sentinel is a
correct-but-narrower subset of this behavior.

**Tension / when to revisit:** true fail-fast needs an **active-environment /
moldable feedback loop** — a point at which "the definition surface has settled" is
observable (e.g. a live image where redefinition triggers re-validation), and it must
account for cross-file method extensions (Q-adjacent to Q8 code-as-facts). Once that
exists, re-validate composed classes on change and surface unresolved
conflicts/unmet requirements eagerly.

**Also deferred (traits_plan §8):** per-class provenance/requirements side tables for
reflection/tooling (not stored on `Class` in the MVP to avoid touching every class
constructor); public-protocol markers; class-side trait methods; `super` in traits.

---

## Q13: Standalone `uses:` — applying a trait to an already-defined class

**Status**: Open — needs design session
**Raised**: numeric tower §4f (2026-08-01), extracted here 2026-08-02

`uses:` is currently **definition-time only**: it exists solely as a keyword clause of
`subclass:`/`vmSubclass:` (`define_subclass` in `builtins_iterative.rs`), and the trait
MVP is stateless and *flattened* — the trait's methods are copied into the class's
method dictionary when the class is defined, with no live link retained afterward.

A standalone `SomeClass uses: SomeTrait.` statement is independently valuable — adding
a capability to an existing class is exactly the live-image manipulation this project
exists for — but it is a trait-system step with its own design questions.

**Immediate motivation (numeric tower).** The tower's coercion fallbacks
(`isEncodedNumber`, `adaptTo…andSend:` refusing defaults) currently sit on `Object`,
which is domain leakage: "can be asked to participate as a number in an arithmetic
operation" is a *capability*, so by the numeric plan's §4d it belongs on the trait axis
as a `NumberCoercion` trait. That placement is blocked here: `Object` is bootstrapped
in Rust and never passes through `define_subclass`, so `Object uses: NumberCoercion` is
currently inexpressible. The `Object` placement stands as explicitly provisional until
this question is resolved.

**Tensions to resolve:**

- **Collision with existing methods.** Definition-time `uses:` flattens into a fresh
  method dictionary. Applying to a *populated* class must decide: does a trait method
  overwrite a same-named existing method, lose to it, or raise a conflict sentinel (the
  mechanism the trait MVP already uses for trait/trait conflicts)?
- **Ordering against later `>>` definitions.** After `C uses: T`, a subsequent
  `C >> foo` plainly wins (it is a later definition). But the reverse order gives a
  different result for the same pair of statements, so the rule must be *stated* rather
  than left to file order.
- **Subclasses already defined.** Flattening copies into `C` only; classes inheriting
  from `C` see the new methods through normal lookup, but any class that had already
  flattened a *conflicting* trait does not change. Whether that is acceptable or needs
  propagation is unresolved.
- **Bootstrapped classes.** Whether `Object`/`Value`/the encoded leaves may be targets
  at all, given their Rust-side construction and the `is_encoded_leaf_name` allowlist.
- **Observability.** Flattening loses the fact that a method came from a trait. If
  `uses:` becomes a live operation, being able to ask a class *which* traits it uses
  (and re-apply an edited trait) becomes materially more valuable than it is today.

Related: Q12 (trait composition fail-fast), Q8 (code as facts — cross-file method
extension interacts with both).

---

## Q14: `respondsTo:` is blind to builtin selectors

**Status**: CLOSED for all scalar protocol (chunk 34, 2026-08-04); open only for the
seven routing-bound arms, which chunk 35 finishes. Resolved **by construction** rather
than by building the builtin-selector registry this question contemplated: once a
family's protocol lives in the class method dictionary, `respondsTo:` is accurate for it
automatically. `'s' respondsTo: #size` was false before C34 S4; `3 respondsTo: #to:do:`
was false before S1b. `_prim*` selectors stay invisible, which C34 §6 OQ-2 settled as
correct — a `_prim` is grounding, not protocol. Remaining gap: `dispatch_namespace` and
`dispatch_class_builtin` (see `next_steps_after_c34.md` §1).
**Raised**: numeric tower S5 (2026-08-02)

`respondsTo:` resolves by **class-dictionary lookup**, so it sees only Smol-defined
methods. Builtin selectors are Rust `match` arms with no enumerable registry, so they
are invisible to it:

```smalltalk
's' size                "works"
's' respondsTo: #size   "false"
nil respondsTo: #isEmpty   "true — Nil >> isEmpty is Smol-defined"
```

The predicate is therefore **unsound as a general capability test**: a false answer does
not mean the send would fail. This is pre-existing — it has always been true of the
`Value::Instance` arm — and S5 merely made it reachable from every value kind (it
previously MNU'd on scalars, which was worse).

**Why it matters now.** S5's coercion edge asks an arbitrary operand whether it
implements the receiver's adapt selector (`docs/numeric_tower_plan.md` §4g). That use is
sound, because adapt selectors are always Smol-defined — but it is the first *load-bearing*
caller, so the gap is no longer purely cosmetic.

**Tensions to resolve:**

- **Is a registry wanted?** Making it complete needs an enumerable set of builtin
  selectors per value kind. The arms are currently a hand-written `match` per variant;
  deriving a registry from them (macro, or a static table consulted by both) is real work
  and touches every dispatch function.
- **Or should builtins move into Smol?** The tower's direction is the opposite one:
  behavior migrates *out* of Rust arms into kernel Smol (S7 prunes superseded arms). If
  that continues, the gap shrinks on its own and a registry may be wasted effort.
- **Related inconsistency.** "Object protocol" is universal in Rust only where each
  variant's arm list happens to include it — `=`/`~=`/`printString` are duplicated per
  variant, `respondsTo:`/`hash` are handled once in `dispatch`. There is no single place
  that defines what every value responds to.

Related: numeric tower plan §4g and S7 (prune superseded Rust arms).

---

## Q15: Should `2 = 2.0` be true? (cross-encoding numeric equality)

**Status**: Open — implemented as TRUE in S6, explicitly flagged for re-weighing
**Raised**: numeric tower S6 (2026-08-02)

S6 made numeric `=` promote across encodings, so `2 = 2.0` is true and `hash` was
aligned to match. This question records the case *against*, which was raised after
implementation and is not settled.

**For promotion (the status quo):**

- **Consistency with everything else in the tower.** Arithmetic already promotes
  (`2 + 3.5` ⇒ Float) and `<` already promotes (`2 < 2.5`). If `=` alone were strict,
  `a < b`, `a = b`, `a > b` would not partition, and the `Ordered` trait — which
  *requires* `=` and derives `<=`/`>=` from `<` — would have an unmet contract. Strict
  `=` therefore forces strict `<`, which breaks `2 < 3.5`.
- **They conceptually are the same value.** The tower's whole premise (§4b) is that
  `MachineInteger`/`ArbitraryInteger`/`Float` are *encodings*, not distinct
  mathematical kinds. `2` and `2.0` denote the same number; treating them as unequal
  elevates a representation detail to a semantic one.
- Precedent: Smalltalk-80, Pharo, Python, Ruby, Julia all answer true.

**Against (the safety argument):**

- **Float/integer confusion is a known source of catastrophic defects.** Silent
  cross-encoding equality can mask a unit or representation error that a strict
  comparison would have surfaced.
- **Float equality is unreliable in its own right**, independent of mixing:
  `0.1 + 0.2 = 0.3` is false. Making `=` promote invites `=` to be used on floats at
  all, where a tolerance-based comparison is usually what is meant. Arguably the deeper
  hazard, and one promotion neither causes nor cures.
- A language aiming to be *observable* might prefer the encoding to be visible at the
  point of comparison rather than silently bridged.

**Options if revisited:**

1. Keep promotion (status quo), and add a distinct exact/identity comparison for when
   encoding matters.
2. Keep promotion, and add tooling — a lint or a `Warning` — for `=` between a Float
   and an Integer literal, or for `=` on floats generally.
3. Strict `=`, and accept that `Ordered`'s `=` requirement must be dropped or redefined
   (see §2a), plus the trichotomy loss above.

Note that Option 3 is a substantially larger change than reverting S6: `hash` alignment,
`Dictionary` key unification, and the `Ordered` contract all follow from the choice.

Related: numeric tower plan §2a (Ordered requires `=`), §4g/S6.

---

## Q16: Class-side `super` uses `env.clone()` as its method base scope, not `global_scope()`

**Status:** Open — surfaced during the env-frame redesign P2.1b-0 setup/teardown
consolidation; behavior preserved, deferred for a dedicated look.

**What:** Every method activation is rooted at a *base env* before its frame is
pushed. Three of the four method-frame entry points root at `env.global_scope()`
— the documented method-scope base: the receiver's own args/temps + storage +
globals, explicitly *not* the caller's frames (`env.rs` `global_scope` contract).
The fourth, `push_class_method_frame` (the class-side **`super`** send path in
`eval_iterative.rs`), roots at `env.clone()` instead, which keeps the *caller's*
frames live as outer scope.

| entry point | base env |
|---|---|
| `push_method_call_frame` (instance / primitive self-send) | `global_scope()` |
| `Evaluator::invoke_class_method` (direct class-side call) | `global_scope()` |
| `push_class_method_frame` (class-side `super`) | `env.clone()` |

**Why it matters:** with `env.clone()`, a name unresolved in a class-side `super`
method's own frame/classvars can fall through `lookup_var`'s
`lookup_outer_frames` step to a *caller's* local — a lexical leak the
`global_scope()` rooting is specifically designed to prevent. It is likely
unobserved today (a `super` method body rarely reads a name that shadows a caller
local), which is why the suite is green either way.

**Tensions / options:**
1. **Bug — unify on `global_scope()`.** Most likely correct; makes all four method
   entry points consistent. Needs a test that would actually catch the leak
   (class-side `super` reading a name that exists as a caller local) to confirm
   nothing depended on the `clone()` behavior.
2. **Intentional — document why.** If class-side `super` genuinely needs caller
   scope visible (none is apparent), record the reason at the call site so it
   stops reading as an accident.

Post-consolidation the difference is a single visible argument at the four
`push_method_frame(...)` call sites, so whichever way it resolves is a one-line
change. Revisit alongside the env-frame redesign (the redesign replaces name-keyed
outer-frame lookup with slot/snapshot resolution, at which point this leak either
disappears or must be reproduced deliberately).
