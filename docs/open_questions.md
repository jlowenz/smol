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
deadlock detection; `#atomic` = mailbox lock + shadow writes; `#stm` = shadow writes + optimistic version conflict; nested transactions via savepoints; `OpaqueTransaction`
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

**Status:** Resolved 2026-10-11 (env-frame redesign P2.1d-d3) — moot. Since d2a a
method's free names resolve as `Dynamic` (receiver, then defining class's namespace)
and never consult the env, so the `env.clone()` rooting could no longer leak; d3
removed the env map and the base-env parameter from `push_method_frame` entirely.
Pinned by `class_side_super_method_cannot_see_callers_temp` (`tests/eval.rs`).

**Original status:** Open — surfaced during the env-frame redesign P2.1b-0 setup/teardown
consolidation; behavior preserved, deferred for a dedicated look.

**What:** Every method activation is rooted at a *base env* before its frame is
pushed. Three of the four method-frame entry points root at `env.global_scope()`
— the documented method-scope base: the receiver's own args/temps + storage +
globals, explicitly *not* the caller's frames (`env.rs` `global_scope` contract).
The fourth, `push_class_method_frame` (the class-side **`super`** send path in
`eval_iterative.rs`), roots at `env.clone()` instead, which keeps the *caller's*
frames live as outer scope.

| entry point                                               | base env         |
| --------------------------------------------------------- | ---------------- |
| `push_method_call_frame` (instance / primitive self-send) | `global_scope()` |
| `Evaluator::invoke_class_method` (direct class-side call) | `global_scope()` |
| `push_class_method_frame` (class-side `super`)            | `env.clone()`    |

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

---

## Q17: How does a workspace import a non-Kernel namespace?

**Status**: Open
**Raised**: 2026-10-10, env-frame redesign §11.7b

A workspace has an anonymous namespace that imports Kernel by default. Any other
namespace must be imported explicitly, as in any other context. There is no surface
form for this yet. `Namespace named:imports:` covers *named* namespaces only. Options
include a message to the workspace (e.g. `Workspace import: mylib`) or a
top-of-program import declaration. The answer depends on what a Workspace object is
(REPL session, notebook — see Q8) and is not needed for d2: until then, non-Kernel
names are reachable only by `Ns::Name` or inside `Namespace do: [...]`.

---

## Q18: Which namespace do trait-provided methods resolve names in?

**Status**: Open
**Raised**: 2026-10-11, env-frame redesign d2a (§11.7b)

Since d2a, a method's free names resolve through its *defining class's* namespace.
Trait methods are flattened into the using class's method dictionary (`uses:`), so a
trait method resolves names in the **using class's** namespace, not the namespace the
trait was written in. If trait `T` in namespace `A` references a name only `A` can
see, and class `C` in namespace `B` uses `T`, then `T`'s methods break inside `C`.
That would be surprising: the trait author tested `T` against `A`'s scope.

Direction (user, 2026-10-11): traits probably need their own mechanism. At least in
trait methods, the trait's namespace and the using class's namespace should be
merged, so a trait's implementation cannot break just because it is used elsewhere.

Sub-questions:

- **Where the scope lives.** A flattened method probably needs to carry the namespace
  it was *written* in (`Method` gains a defining-namespace link) rather than relying
  on `defining_class`.
- **Merge order and conflicts.** If both namespaces define a name, the trait's own
  namespace probably wins inside trait methods, so the implementation sees what its
  author saw. The alternative is the using class's namespace winning, or a conflict
  error, consistent with the trait-conflict sentinels (Q12).
- **Requirements.** A trait's required selectors are sends, not names, so they are
  unaffected. Only free *names* (classes, traits) are at issue.

Not urgent: every trait is in Kernel today.

---

## Q19: What owns workspaces (and their anonymous namespaces)?

**Status**: Open
**Raised**: 2026-10-11, env-frame redesign d2a/d2c (§11.7b)

A workspace has an anonymous namespace holding the classes and traits defined there.
`Class → Namespace` is a `Weak` link (avoiding the `Namespace → class handle → Class
→ Namespace` cycle), so something must keep a workspace's namespace alive while
anything defined in it is still in use: live instances of its classes, classes
referenced elsewhere, blocks.

Observations:

- Something must create workspaces: a REPL session, a notebook, a test harness. That
  implies a **root object or environment** above them. Today the de facto root is
  `ClassRegistry`. It owns Kernel, the root namespaces, and every class and instance
  entity, and it serves as the system's image. `World` (C27) is a store-query gateway,
  not this root.
- "Alive while still in use" is reachability, which makes this a **GC** question.
  Today in-memory objects are reclaimed only by `Arc` refcount, and durable facts are
  never reclaimed (Q11).

Interim proposal (for d2c): the root (`ClassRegistry`, or a successor root object)
holds every workspace namespace **strongly** until the root drops. Nothing is freed
early, so there are no dangling links, consistent with the "glacier, not GC" stance
taken for values. Reclaiming a workspace namespace once it is unreachable is deferred
to the GC design.

Observed (d2c/c2): since every `Workspace::new` registers a namespace that the root
holds for its lifetime, code that creates a workspace per evaluation accumulates
namespaces. The benches were changed to reuse one workspace per benchmark for this
reason. Any embedder that evaluates via throwaway workspaces needs reclamation, or
must reuse workspaces.

Open: whether the root deserves a first-class Smol identity (a `System`/`Image`
object that creates workspaces), and how workspaces relate to notebooks (Q8).

---

## Q20: Sum types and pattern matching without compromising the core model

**Status**: Open — exploratory
**Raised**: 2026-10-11 (user), during env-frame redesign d2b

How can Smol gain the benefits of sum types (closed sets of alternatives,
exhaustiveness) and pattern matching (destructuring, binding), without violating its
core constructs: uniform message sends (D12, P2, P9), inspectable dispatch (P3), the
Value/Object split (D3), async-by-default sends with late-resolved futures (D2), and
live redefinition (P1)?

**What already overlaps:**

- **Polymorphic dispatch is already a match on the receiver's class.** A visitor or
  double dispatch covers most uses of a `match` on a closed hierarchy, though only
  over the receiver.
- **Value classes are product types**: immutable, frozen at construction, shareable.
  A `Value subclass:` family behaves like a sum type in everything except closure.
- **`on:do:` already matches by class**, for conditions.

**Tensions to resolve:**

- **Closed vs open.** Exhaustiveness requires a *closed* set of alternatives, but
  classes are open: anyone can subclass, and live redefinition can add a variant at
  any time. Any "sealed" notion has to say what happens when the live system adds a
  variant to a sealed family.
- **Values only?** Destructuring an *entity* would read mutable actor state across a
  mailbox boundary. Matching probably has to be restricted to Values, or go through
  messages (which makes it ordinary dispatch).
- **Futures.** Matching on a future would force it, which conflicts with the
  late-resolution rule (CLAUDE.md "Futures"). The match itself would have to be
  deferred, like `then:`.
- **Syntax vs messages (P9).** A `match` special form would be a non-message
  construct. A message form (e.g. `x caseOf: {...}`, or blocks with pattern
  parameters) keeps uniformity but needs a way to express patterns as values.
- **Good fit with SSA.** Pattern variables are fresh single-assignment bindings, which
  fits §2.1 of `perf_env_frame_redesign.md` naturally, and the resolver can give them
  slots like any other local.

**Directions to evaluate (none chosen):**

1. Library only: visitor or double-dispatch conventions plus a `caseOf:`-style
   message. No language change.
2. A "sealed Value family" declaration used only by tooling, so the observable
   environment checks exhaustiveness, not the evaluator.
3. Pattern syntax that *desugars to sends*, so dispatch stays inspectable. This
   needs patterns as first-class values.

Revisit after the compiler/VM front half (resolved AST), since pattern bindings would
flow through the same resolver.

---

## Q21: Reifying _typed_ messages for a form of static checking

**Status**: Open
**Raised**: 2026-10-11

Not quite sure how this would work. The idea is something like: maybe we don't/can't know the receiver, but we can allow the system to specify a message/selector that requires certain capabilities for the args. E.g. if a class implements such a message, we can proactively check at both the method def site and any call sites that the arg requirements are met. This almost certainly needs to be fleshed out more, but is a route to support more declarative type-checking even in the context of such a late binding language. 

---

## Q22: Dynamic variables and aspects

**Status**: Open — design discussion started. Propagation (which work a binding
reaches) is continued in **Q23** (contexts and the causal graph), of which
dynamic constants are one client.
**Raised**: 2026-10-26 (user), from the class-variables discussion
(`namespace_design_exploration.md` §9)

Some configuration is neither instance state nor a definition: it is ambient for a
*scope of execution*. The motivating case is `Warning`'s logging policy
(`kernel.smol:838`), today a class variable that a test sets globally
(`Warning loggingPolicy: #suppress`) when what it means is "suppress warnings while
this runs". With class variables gone, this needs a home that is not a global.

**Prior art:** CL special variables; Racket `parameterize`; Java `ScopedValue`
(JEP 506: no mutation, only rebinding in a nested extent); Clojure dynamic Vars
(`binding`); OpenTelemetry context propagation across process boundaries.

**What Smol already has:** two pieces of dynamic context that travel with every
message, the inherited handler chain (`Envelope.inherited_chain`, C28b) and the
transaction tag. A dynamic variable could travel the same way.

**Questions:**

- **What a dynamic variable is.** A special class (as `Value` is), instantiated or
  subclassed as the declaration? Is the variable itself data (an object) rather
  than a definition, and if so, where does a reference to it live without becoming
  a global?
- **Binding form.** Rebinding only in a nested extent (no mutation), consistent with
  single-assignment locals. Is the binding an ordinary message
  (`(policy bind: #suppress) during: [...]`), and what does `during:` answer if the
  block's work is asynchronous?
- **Propagation.** Which bindings apply to work started inside an extent but run
  later or elsewhere: messages sent to other entities, `then:` continuations, timer
  callbacks, entities spawned inside the extent.
- **Interaction** with STM retry, conditions, and whether bindings are recorded in
  store facts so they can be inspected (P3, P5).

**Aspects.** Behavior that varies with context, or concerns kept apart from the code
they apply to. Candidates: CLOS-style `:before`/`:after`/`:around` method
combination; context-oriented programming layers activated for a dynamic extent
(Costanza & Hirschfeld, "Language Constructs for Context-oriented Programming", DLS
2005), which would make dynamic variables (data) and layers (behavior) one
mechanism; AspectJ-style pointcuts and advice (Kiczales et al., ECOOP 2001), which
are declarative but modify code at a distance. Requirement (user): whatever is
chosen must be inspectable. Where an aspect applies must be visible from the code
it applies to (P3).

**Discussion 2026-10-26:**

- *Shape considered and set aside:* the binding activation as an entity
  (`(var bind: x) during: [...]` as an async send, with the block run in the
  entity's process). It gives the extent identity, but it moves the code: `self`
  sends in the block become cross-entity sends, and the caller continues before
  the block has run. `Transaction atomic:` has this shape today
  (`kernel.smol:1988`), and its contract that the block "must sequence the
  participant work it wants included" (`kernel.smol:1898`) shows the cost.
- *Guiding rule (proposed):* a context construct changes the context code runs
  under, not the process it runs in.
- *Two block-evaluation modes exist today, and the word "synchronous" covers
  both:*
  - **in-process, non-forcing:** `value` and `HandlerContext run:` run the
    statements in the caller's process, in order, without awaiting Futures between
    statements (`push_block_frame(..., sync=false)`, `eval_iterative.rs:1897`,
    `2049`). The answer is the last statement's value, possibly a Future.
  - **forcing:** `syncValue`, `ensure:` and `on:do:` insert `ForceAcc` after each
    statement, so each statement's Future is resolved before the next runs
    (`eval_iterative.rs:1366-1369`). This serializes the block's work.
    A binding construct would use the in-process, non-forcing mode, as
    `HandlerContext run:` does. Then wrapping code changes only its context, plus
    the lexical rules any block imposes (block-assignment rule, non-local `^`).
    The two modes need distinct names (input to the naming-conventions pass).
- *Decided (user):* handler contexts and dynamic bindings merge into **one
  execution-context Value**, built functionally and run with an in-process `run:`.
  Whether the transaction tag joins it is open.
- *Decided (user):* work ownership (structured concurrency, supervision) is a
  separate concept, likely its own entity, as Erlang supervisors are.
- *Gap found:* `Block` is a `Value::Block` in Rust (immutable, snapshot capture)
  but is registered as a subclass of `Object` (`class_registry.rs:138`), so the
  kernel does not say a Block is a Value. The scalars were reparented under
  `Value` by the numeric tower work; Block should probably follow. To discuss
  before changing.

**Discussion 2026-10-27:**

- *Terminology (user):* Smol objects are either **Values** or **entities**; there is
  no third kind. A message to a non-entity receiver is a message to a Value and runs
  in the sender's process. So the two block-evaluation modes are:
  - **normal**: `value`, a message to a Value, in the sender's process, with no
    forcing between statements;
  - **sequential**: `syncValue`, each statement's Future resolved before the next.
    `ensure:` and `on:do:` use this mode because they need to observe completion
    (cleanup after the body's work; errors from async work).
- *Classification audit:* Block is a Value but is not declared one in the kernel.
  Other kernel objects may also be unclassified or misplaced (Future is the first to
  check: resolved once, later). Expect more of these during the move from the
  evaluator to the compiler/VM, along with more Rust that should move to Smol.
- *An activation is state (user).* A binding activation sets one or more named
  bindings for a context: it is the configuration in effect, which is state (of the
  execution, immutable within its extent), even if it does not by itself answer
  *why* something changed. Recording it is therefore a P5 question.
- *Recording in the store.* Options: (a) nothing; (b) every commit records the
  context it ran under (extent id plus bindings); (c) as (b), only for dynamic
  variables declared as recorded; (d) only in a tracing mode. Prior art: Datomic's
  reified transactions, which carry annotation facts. Constraint (user): avoid
  "more than one way to do it". The store already has an open fact mode for
  entities; whatever is chosen should reuse an existing mechanism rather than add
  one. `enrollment_provenance_design.md` also records provenance for enrollment, so
  check whether one mechanism serves both.
- *Transactions stay as they are (user).* `Transaction` is a coordinator that owns
  its work (it waits for it, commits or aborts, collects failures, re-runs it under
  `stm:`). That is the work-ownership category, so running the block in the
  coordinator's process is consistent with the rule that *pure* context constructs
  do not move code. Moving the block into the caller would need an in-process
  shadow-write path, would split error collection, and would re-open a protocol
  that took C31, C32 and the enrollment redesign to get right.
- *Likely (user):* the transaction tag becomes a field of the unified
  execution-context Value instead of the separate `current_tx_ctx`. An internal
  change to how the tag travels, not to the protocol; do it when the unified
  context is built.
- *Next:* aspects (CLOS method combination, ContextL layers, AspectJ pointcuts),
  with inspectability as the requirement.

**Discussion 2026-10-27 (aspects):**

- *Obliviousness is not the problem by itself (user).* Code never knows how it is
  used, and Smol is already oblivious by design: what `x foo` runs is decided by
  `x`'s class, what a `signal:` leads to is decided by a handler, what a dvar read
  answers is decided by a binding. P3 asks that inspectability be the norm, not that
  every influence be declared at the target.
- *Criterion: explicit vs implicit variation points.* A send, a `signal:`, or a dvar
  read is an *explicit* variation point: the code shows that something else decides
  here, and the decider is found by following the receiver's class, the handler
  chain, or the binding. AspectJ makes every join point an *implicit* variation
  point: nothing in the code marks it, and finding what applies needs a whole-system
  query. That is the residue of Steimann's critique ("The Paradoxical Success of
  Aspect-Oriented Programming", OOPSLA 2006). Kiczales & Mezini ("Aspect-Oriented
  Programming and Modular Reasoning", ICSE 2005) answer it with tool-computed
  interfaces that depend on the deployed configuration; a live, queryable system
  makes that answer stronger. So implicit variation points are not ruled out by P3,
  but "what affects this method?" must always be answerable. The concern (user) is
  making the implicit variation already present in most code worse.
- *Single dispatch does not exclude layers.* ContextS (Squeak) and ContextJ are
  single-dispatch (Hirschfeld, Costanza & Nierstrasz, "Context-oriented
  Programming", JOT 2008). What each mechanism varies:
  - a **proxy** (C33 DNU) varies one *reference*; explicit to its holder; no
    quantification over classes. Fits ocap attenuation and per-entity tracing.
  - an **annotation/pragma** marks a definition for a system tool to find and act
    on; declared at the target, interpreted elsewhere.
  - a **layer** varies a (class, selector) pair for a dynamic extent, including
    methods written without a hook. The only one of the three that changes behavior
    at unmarked points.
    The plain OO alternative to a layer is a dvar holding a policy object, with the
    code sending to it (`(Display current) render: self`). It needs a hook written in
    advance; layers exist for the case where nobody wrote one.
- *Three tiers of what "aspects" covers:*
  - **Tier 0:** vary behavior at explicit hooks: the execution-context Value
    (dvars holding Values or policy objects) plus conditions. Already decided.
  - **Tier 1:** observe without changing behavior (tracing, probes, coverage):
    environment tooling, as Pharo MetaLinks (Denker et al., "Sub-method
    Reflection", JOT 2007) or the coverage collector. Not a language concept.
  - **Tier 2:** change behavior at unmarked points, across many places: layers, or
    definition-time transforms selected by a query. If done as transforms, the
    query produces explicit new definitions and is recorded as their provenance, so
    the code stays explicit and "why does this method contain X?" is answered from
    the store.
- *Decision (user):* do not design Tier 2 yet. Build Tier 0. The first concrete
  case Tier 0 cannot handle cleanly triggers Tier 2. Display modalities (views added
  to classes their authors did not anticipate, as GT's `<gtView>` methods) were the
  likeliest candidate; see 2026-10-28 for why they may not be.
- *If layers exist (user):* they cross entity boundaries, travelling with the
  execution context as handlers and the transaction tag do.
- *Other concerns placed:* validation is value invariants (a constrained value type
  checked at construction). Security follows object capabilities (Miller, *Robust
  Composition*, 2006); removing class variables and keeping namespaces
  definitions-only removed the main sources of ambient authority.
- *Tension (open):* a dvar holding a capability is ambient authority. The
  execution-context Value may need a rule about what bindings may carry.

**Discussion 2026-10-28 (dynamic constants):**

- *Constants are not re-run per call.* §9 item 3 of
  `namespace_design_exploration.md` already says so: semantics never depend on
  caching. A dynamic-variable declaration must answer an equal Value on every
  evaluation, which a name-based identity gives.

- *Identity is the FQN, supplied by the system (user).* The context's bindings are
  keyed by the variable's fully qualified name, not by its other contents. Two
  variables created with the same default are distinct if their names differ.

- *Name: `DynamicConstant` (user).* Relates it to CL's dynamic variable and marks
  the difference: a CL special variable can be assigned at any time (`setq` changes
  the innermost binding, or the global value), while a `DynamicConstant` is never
  assigned, only rebound for a nested extent (as Java `ScopedValue`, JEP 506, and
  Racket `parameterize`). *Binding* names one entry in a context (this name to this
  value in this extent).

- *Working shape (user sketch):*
  
  ```smalltalk
  Warning class >> *policy* [^DynamicConstant named: ~ withDefault: #stderr]
  
  Warning class >> withPolicy: p run: aBlock [
      ^Warning *policy* bind: p in: aBlock
  ]
  
  Context let: { Warning *policy* -> #suppress. Warning *frob* -> 11 } in: aBlock
  ```
  
  - No place concept: the method answers an ordinary Value; reading
    (`Warning *policy* value`) and rebinding (`bind:in:`) are messages to it.
  - `Context let:in:` sets several bindings at once in the execution-context Value.
    Every key must be a `DynamicConstant`, and holding one is the authority to
    rebind it: a class can keep its constant private and publish only
    `withPolicy:run:`. This addresses the ambient-authority tension for rebinding.
  - `bind:in:` and `let:in:` answer whatever the block answers (possibly a Future),
    in normal mode, as `HandlerContext run:`.

- *`~` (user):* intended as a general token for the enclosing definition's FQN,
  usable anywhere. Spelling open (`thisDefinition fqn` is the spelled-out form;
  `thisDefinition` alone would answer the definition object, as Pharo's
  `thisContext`). `~` in primary position is unambiguous to the parser, since a
  binary operator cannot appear there; it can stay a binary-operator character
  elsewhere (`lexer.rs:277`).

- *Marker for dynamic constants (open).* A read site should show that it consults
  something dynamic. Earmuffs recall CL but need a whitespace-sensitive lexer rule:
  `x *y* z` is legal binary arithmetic today. Alternatives: another delimiter that
  is not an operator character, or a `dyn` prefix (user: changes how the selector
  reads). The environment can also render dynamic constants distinctly, as a
  complement. For the naming/syntax pass.

- *Who can create one (user).* Anyone, with explicit names allowed, but a name
  resolves in the creator's lexical namespace (and class, inside a class
  definition), so names cannot be forged into another namespace. Restricting names
  to literals was rejected as arbitrary for a dynamic language.

- *Same-name collisions within a namespace (open).* CL precedent: `defvar` takes an
  unevaluated symbol, but `proclaim` and `progv` take computed ones, and a second
  `defvar` of the same symbol names the *same* variable (it does not re-initialize
  a bound one; SBCL is silent). The package boundary is the only guard. Options:
  accept aliasing within a namespace as CL does, with the environment able to list
  every definition that creates a given name and warn on visible duplicates; or
  find a better registration scheme. Explicit sharing needs no second constructor:
  code that wants the variable asks its owner for it (by reference).

- *Views (deferred).* GT finds views by querying the `<gtView>` pragma
  (`#gtView gtPragmas`), and they are often Pharo extension methods (a
  `*PackageName` protocol) that add to a class its package does not own (Chiș,
  Gîrba, Syrel & Nierstrasz, "The Moldable Inspector", Onward! 2015). The part
  that matters may be declarative registration plus query-based discovery, not
  changing the class; a view declared elsewhere would see only public protocol. A
  view that needs internals points at a design problem (user). Not designed; wait
  for a real case. It may mean display does not trigger Tier 2.

- *Dependency (open):* whether code in one namespace may add definitions to a class
  in another (class extensions), and what `~` answers inside one. The namespace
  naming rules must answer this.

**Discussion 2026-10-29 to 2026-10-31 (propagation, and what "scope" means):**

- *Decided (user):* a `DynamicConstant`'s default is mandatory; `nil` is allowed.
- *Propagation is not settled; it reopened a larger question.* Attempts to define
  which work a binding reaches all failed to be mechanical:
  - "A send carries the sender's context" works for messages, but a timer
    callback or a `then:orElse:` continuation is started by the runtime, not sent.
  - Implicitly wrapping block arguments at each send (`[Context current runBlock:
    aBlock]`) runs backwards: the captured context replaces any binding the
    receiver sets around the call, so `bind:in:` would cancel itself. A send cannot
    tell a block called during the send from one stored and called later.
  - Restricting wrapping to cross-process sends, or making callback capture a
    library decision, was not what the user wants.
  - Prior art does not derive it from a general rule either: CL closures do not
    capture dynamic bindings and SBCL threads do not inherit them; Java
    `ScopedValue` inherits only through `StructuredTaskScope`; Racket threads
    inherit the parameterization at creation; the TC39 AsyncContext proposal
    decides registration vs dispatch context per host API.
- *The evaluator is not precedent (user).* `fork_context` (`eval.rs:896`) already
  captures the handler chain and transaction context at registration for
  `then:orElse:` (`builtins_iterative.rs:237`) and timers (`:3138`). That is
  expedient, bespoke handling, not a principle; the VM needs a principled answer.
- *Kinds of "scope" that coincide in CL but not in Smol:* lexical scope; the
  activation extent (one turn: one entity processing one message, E's term);
  the causal extent (all work a turn causes, unbounded under async sends); the
  ownership extent (work something waits for and answers for; today only
  `Transaction`); entity lifetime.
- *Framing (not a design):* the structure Smol always has is the **causal graph of
  turns**, with edges for send, reply (a future settles into a continuation),
  schedule (timer) and spawn. Handler search, transactions, dynamic bindings and
  ownership are each a property that flows along some edges and stops at some
  boundary. The call stack is the special case of a single chain.
- *Structured concurrency (Smith, "Notes on structured concurrency, or: Go
  statement considered harmful", 2018). User's reading:* good points and worth
  attention, but thread-oriented, starting from a sequential language; actors are
  not considered. An async send is not a "go statement": it triggers work on a
  sovereign entity with its own lifetime, and work outliving the caller is by
  design. P4 means concurrency is inherent in the language and environment, not
  added to a sequential base; related to, but different from, structured
  concurrency. Smith's ideas may apply to resource management, not yet discussed.
- *Ownership (user, open):* an actor cannot own a request to another actor, yet
  Smol needs transactions and condition handling without a call stack and without
  ceremony. Smol separates contexts (message-passing actors) and these features
  add context back. Parent/child (lifetime and failure, as Erlang supervisors),
  reparenting, and short- vs long-lived entities are open.
- *Gabriel, "The Design of Parallel Programming Languages" (1991).* Brands are
  structureless tags on objects, used to record provenance (a value produced by
  process `p1` carries `p1`'s brand) and to dispatch on it. They are the dual of
  context: context flows with work along sends; provenance flows with data from
  the turn that produced it. A continuation could dispatch on a value's provenance
  instead of relying on a hidden registration-or-trigger rule. The essay's ideas
  also touch process environments (ownership as data), triggered functions
  (aspects), process-qualified names, and temporal abstraction (P5). Costs for
  Smol: whether brands take part in Value equality; a per-value tag (P10);
  overlap with fact provenance in the store (D5). Colors (function coloring,
  taint) are special cases; information flow control (Myers & Liskov, SOSP 1997)
  is the security version.
- *Prior art to read:* Miller, Tribble & Shapiro, "Concurrency Among Strangers"
  (TGC 2005) for E's turns, eventual sends and patterns; Stanley, Close & Miller,
  "Causeway" (HP Labs 2009) for presenting the causal graph in place of a stack;
  Garnock-Jones, *Conversational Concurrency* (PhD, Northeastern 2017) for facets,
  structured scopes inside actors; Orleans `RequestContext` for ambient data on
  calls between actors.
- *Next (user):* formalize terminology in a glossary (turn; send, reply, schedule
  and spawn edges; causal graph; context vs provenance; the properties that flow
  and their boundaries; extent; ownership; entity lifetime). The user has
  observations and open questions to bring to that session.

---

## Q23: Contexts and the causal graph

**Status**: Open — terminology first
**Raised**: 2026-11-01 (user), split from Q22 (discussion 2026-10-29 to 2026-10-31)

Q22's propagation question (which work a dynamic binding reaches) turned out to be
an instance of a more general one: what a *context* is in Smol, how contexts are
bounded, and how each flows along the causal graph of turns. Handler search,
transactions, dynamic constants, ownership and resource lifetime are each a context
of this kind; under pervasive concurrency they no longer coincide with the call
stack as they do in a sequential language.

**Working material:**

- `docs/20261031 Thoughts on concurrent theory and contexts.md` (user's notes:
  context vs scope, terminating and non-terminating contexts, turns, causal-graph
  edges, leaf turns, non-termination).
- `docs/glossary.md` (terms as they are defined; status per entry).
- Q22, discussion 2026-10-29 to 2026-10-31 (failed attempts at a mechanical
  propagation rule; prior art; extents; Gabriel's brands; structured concurrency).

**Questions** (to be sharpened once the glossary settles):

- What the edges of the causal graph are, and whether the list is complete.
- How a context's bounds follow from its originating event, and when (whether) it
  terminates.
- For each context kind: along which edges it flows, and where it stops.
- How Smol relates to, and differs from, prior work (E turns, structured
  concurrency, CL dynamic extent), and why.

**Decisions 2026-11-03** (from asking which choices would be hard to reverse once
VM work starts):

- *A. Turns may block, and turns are exclusive* (decided). A send to a future
  waits, and an entity processes one message at a time ("single-threaded").
  Consequence: deadlock between entities is possible (issue I2); the response is
  fewer blocking points, detection that covers future waits, and coordination
  through a coordinator entity, not a non-blocking language (contrast E, which
  never blocks inside a turn: `related_work.md` R1).
- *B2. A registered continuation runs as its own short-lived entity* (decided).
  Registration (`then:orElse:`, timers) is a send to a newborn continuation
  entity, delivered when the trigger fires (settlement, timer expiry). Its context
  comes from the registering turn. The causal graph keeps one kind of edge, the
  send; spawn is birth plus a send, registration is birth plus a deferred send.
  Rejected: B1, running the continuation as a turn of the registering entity
  (E's `when`), because a registrant that forces the result in the same turn
  (`(b foo then: [...]) syncValue`) would wait on itself, and that pattern is
  expected to be common. Open inside B2: ivar reads from the continuation's
  block (issue I4).
- *C. No provenance tag inside the `Value`* (decided). Brands or provenance, if
  adopted, are wrapper values or a side table; how they would be used is open.
- *D. Envelope and Future fields for V2* (accepted): one opaque context slot and
  the sending turn's id on the envelope; each future records its settler.
  Recorded in `vm_compiler_plan.md` §5.

**Positions 2026-11-06** (from I4; leaning, not final, and decision A is not yet
amended):

- *Send to the entity whose turn is running:* inline (I6).
- *Forced cycle within a turn's own wait chain:* re-enter. The parked turn serves
  the message on its own coroutine, nested inside the force, then parks again.
  Unrelated senders still wait. This would change decision A to "exclusive except
  for your own wait chain".
- *Unrelated cycles:* traditional deadlock detection, signaling a condition. They
  have no sequential meaning.
- *Ivars in blocks:* forbid without explicit capture; accessors for live reads (I4).
- *Help with bad design:* environment tooling, not language rules. A compile-time
  lint for the shape "result of a send held in a temp, ivar written, then a send to
  the temp" (false positives acceptable), and a run-time report of re-entries
  actually taken: method, force, chain, ivars changed since the send, and a rollup
  of which entity classes re-enter each other. Ideas only.

**Design test:** a send that is forced immediately means what it means
sequentially. Bugs that are also bugs in sequential code are out of scope.

**Which messages re-enter:** only one that closes a wait cycle (the sender waits on
it, and the parked turn transitively waits on the sender), not any causal
descendant. Arguments on paper, nothing built or modeled:

- *No data races:* nested code runs on the entity's own coroutine while the outer
  turn is suspended.
- *Programs that finish today are unchanged:* a closed cycle cannot otherwise make
  progress, apart from entity shutdown and the transaction deadlock detector.
- *No new deadlocks from stack order:* the outer turn cannot resume until the nested
  one finishes, which is already true because it waits on it. A coroutine waits on
  one thing at a time, so there is one path to follow. A descendant tag loses this
  and the previous point: a fire-and-forget message would block the outer turn for
  no reason.

**Evidence, run 2026-11-06.** Examples with mutual dependencies between classes
mostly dissolved under design review (data should be a value, the responsibility
was misplaced, or the object would be owned and run inline). The cases that held:

- `self rate` in a block under the I4 rule (table in I4).
- A cycle-protected Visitor whose nodes only dispatch (`accept: v [ ^v visitFolder:
  self ]`), forcing `size > 50` in `visitFolder:`. Value nodes answer correctly and
  entity nodes hang, with the visitor unchanged. Its nested `seen add:` must persist
  for later visits, so correct code can rely on mutation during re-entry.

No realistic sequentially correct program broken by re-entry has been found, which
is not proof that none exists. The one constructed needed a shared entity called
with a function's job, plus a write placed between send and force.

**Not recorded as an issue:** an unforced callback to a parked sender runs after the
sender's turn, unlike sequential code (`Warehouse >> reserve: order [ order
markReserved ]` while `Order >> submit` waits). Confirmed by a run, but judged
unmotivated without a realistic reason for that design.

**Transactions under re-entry:** nested transactions are unaffected; a child merges
into its parent and only the root is durable (`tx_commit_frame`). The remaining
requirement is that store order must match live order. Traced from the code, not
run: a turn with no transaction does `x := 1` (live, and buffered in its private
`current_tx`); a root transaction re-enters and commits `x := 2`; the turn ends and
commits `x = 1`. `AssertSingle` supersedes whatever fact is open with no version
check (`in_memory_store.rs:350`), so the store ends at 1 and live at 2, with both in
history. Direction: commit the parked turn's buffer before a re-entrant root
transaction can change the store, which splits that turn's store transaction in two.
Nested turns with no transaction join the parked turn's buffer, like a sequential
call, so they cause no split.

**Still open:**

- `^` across entities (I5).
- Explicit capture syntax (I4).
- The boundary between own chain and unrelated, which needs the sending-turn id from
  decision D; not implemented.
- Save and restore of per-turn state: `handler_chain` (its dispatch contract requires
  it empty), `current_tx`, `current_tx_ctx`.
- Nesting depth on the interpreter's Rust stack.
- Whether re-entry reopens running blocks on their home entity, since a self-wait was
  B1's rejection reason. Its conflict with condition handlers remains.

**Precedents:** [MTS](https://discmaster.textfiles.com/file/29135/APC47-1.ISO/workshop/win95/pws/mts4.cab/mtxpg04_16.htm?html=true)
re-enters on the same logical thread and warns objects to expect calls while blocked.
[SE-0306](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0306-actors.md)
left "task-chain reentrancy" as a future direction: not tried at scale, and no known
efficient implementation. [Orleans](https://learn.microsoft.com/en-us/dotnet/orleans/grains/request-scheduling)
offers `AllowCallChainReentrancy`. All three identify the chain by causal id, not by
a closed wait cycle.

---

## Q24: Control flow without non-local return

**Status**: Open, brainstorm pending
**Raised**: 2026-11-09, from I5

I5 showed that `^` in a block has no coherent meaning once the block can run on
another entity. Rejecting it there, statically or at run time, makes `^` work in
only a few blessed contexts, which is inconsistent. Supporting it across entities
(even with an implicit await) needs large machinery, and non-local return is close
to an arbitrary goto that breaks structure. The underlying cause: Smol dropped
Smalltalk's guarantee that a block runs synchronously inside its home's dynamic
extent, and has nothing uniform in its place (I5, "Diagnosis").

The current lean is to drop `^` in blocks altogether. Nothing replaces it yet. The
tradeoffs need to be understood (P8), and the result has to fit the model (P9).

**Questions for the brainstorm:**

1. When all computation happens, conceptually, through asynchronous message sends,
   what does imperative programming mean?
2. What ways are there to end a method body early? Are there any besides signaling
   a condition or structuring the method as an expression?
3. If methods become send expressions, what tools help keep code readable,
   especially when translating very imperative algorithms?
4. Blocks have been the main way to build control flow. Is another built-in object
   with its own syntax needed, like blocks, to replace `^`?

**Known so far:**

- Without early return, a method body becomes an expression. Something like CL
  `cond` would be wanted, and in CL that is a macro; Smol has no macros.
- TCO might be harder (unverified).
- Existing structured tools: conditions with restarts (their control operations
  are local to the signaling coroutine, `condition_system_guide.md` §4); folds
  that stop early by returning a value (Clojure `reduced`, Rust `ControlFlow`);
  delimited constructs (Scala 3 `boundary`/`break`, which replaced non-local
  returns). See R10.
- Kernel use of `^` inside blocks today: inline control (`ifTrue:` and friends),
  and `self do: [... ^each]` in `detect:` / `includes:`. All of it would need
  rewriting.

**Brainstorm (2026-11-09).** The lean is unchanged: drop `^` in blocks.

*Findings:*

- **D13 already removed imperative control from blocks.** A block cannot assign
  method temps, so it affects the outside only through its value, its sends, and
  `^`. `^` is the only one of these that assumes a shared stack. Without it the rule
  is uniform: a block answers a value and sends messages, wherever it runs. It then
  means the same thing inline or on another entity, which is P4 ("sequential
  execution as an optimization of concurrent semantics").
- **TCO is not harder in the current evaluator.** In
  `docs/experiments/q24_tco_shapes.smol`, guard style
  (`n = 0 ifTrue: [^0]. ^self guard: n - 1`) and expression style
  (`^n = 0 ifTrue: [0] ifFalse: [self expr: n - 1]`) both run in constant space
  (about 40 MB peak at depths 3×10⁵ and 3×10⁶, release build).
- **A `cond` needs no macro.** Blocks already delay evaluation, so a multi-way
  conditional is an ordinary message over block pairs (Squeak
  `caseOf:otherwise:`). The cost is noise and allocation; VD4 can inline a literal
  form.
- **A condition is not an early exit under async.** In
  `[coll do: [... signal]. nil] on: ... do: [:e | e return: ...]` the protected
  block answers `nil` before `do:` has run, and `return:` unwinds `coll`'s
  coroutine (`condition_system_guide.md` §4). Every sound early exit brings the
  result back as the value of something the caller waits on.

*The four questions:*

1. A method body is a sequence of sends issued in order, with names bound to their
   replies: a `let*` chain, or `do` notation over futures with implicit forcing.
   What stays imperative is the order of effects, rebinding temps, and mutating
   ivars at method level. Control flow as jumps goes. The actor model separates
   *answering* from *finishing* (Erlang `gen_server:reply/2` answers and keeps
   running). `^` fuses the two, and I5 is the two coming apart by accident.
2. Sound ways to end early:
   - write the method as an expression;
   - let the block answer a stop value that the callee honors (Clojure `reduced`,
     Rust `ControlFlow`);
   - use query methods that keep the loop inside the receiver
     (`detect:ifNone:`, `allSatisfy:`);
   - add a guard statement at method level, not in a block.

   Two candidates are not independent. A delimited escape object
   (`boundary`/`break`) raises the I5 question again when `exit` is called on
   another coroutine, unless escapes are tracked. "Reply now, keep running" is
   not an exit at all.
3. Readability tools: Composed Method; query methods; a multi-way conditional
   message; a state-threading loop; `and:`/`or:` chains. Records or tuples with
   sugar would reduce the ceremony of packing loop state (and are wanted anyway).
   Cost (P8): algorithms with many interleaved exits read worse. Not yet measured.
4. Lean: no new construct. Add a stop/continue protocol (a library matter: `loop:`
   is plain double dispatch and needs nothing from the VM) and the missing query
   methods. One syntax candidate is kept: a guard statement at method level.
   Ranked last: non-closure "inline clauses" for the compiler's control selectors
   (Kotlin `inline` lambdas). They are compile-time rejection made visible, and
   user-defined control abstractions would need inline methods, which are macros by
   another name.

*Positions (user, 2026-11-09):*

- **A method answers its last expression; answering `self` becomes explicit.** An
  explicit `self` marks a procedure-like method. Consequences to work through:
  - C2 is reversed. Implicit `self` was adopted so that `new`'s
    `^self _primNew initialize` answers the instance (`chunk_21_plan.md` C2).
  - A method whose last statement is a send to another entity answers that send's
    future, so a caller that forces the reply also waits for that send.
  - `^` may no longer be needed anywhere.
- **Whether a callee may ignore a stop value is a contract question.** A method
  that advertises stop/continue should honor it unless its contract says
  otherwise. This belongs to the library, not the VM.

*Next:* an experiment that translates a non-trivial imperative algorithm in several
styles, to measure the cost.

**Experiment (2026-11-09):** a calculator (tokenizer, shunting-yard, RPN) written in
four styles. Details, metrics and run commands are in
`docs/experiments/q24_calculator_results.md`. All runnable styles pass the same 17
cases.

- **Today's Smol already pays most of the cost, because of D13.** Loop variables
  live in ivars changed by pvt methods, and errors are re-raised with `^err`. Without
  `^` in blocks and with library support (`cond:`, `loop:`, a condition for errors),
  the result is about the same size as today's style, with shallower nesting.
- **Error exits fit a condition** signalled and handled on the same coroutine.
  **Ordinary exits** fit recursion base cases or a loop's stopping value. Neither
  needs a jump.
- **Nested loops:** an anonymous `loop:` forces
  `Break with: (Continue with: …)`. A *named* loop (Scheme named `let`, here
  `recur:` with an `again` argument) lets the inner loop continue the outer one by
  calling the outer `again`. These are plain tail calls, so they run in constant
  space (measured) and mean the same thing wherever the block runs.

  This suggests two protocols: named loops for loops the caller owns, and stop
  values for iterators the callee owns.
- **Records or tuple values are wanted:** for loop state now packed in Associations,
  and as an immutable literal so a `cond:` need not allocate an `Array` entity.
- **Correction:** a `cond` on an `Array` forced by the home inside its own turn did
  *not* hang on the current tree. The reason was not traced, and it differs from
  I6's `do:` row.

**Related**: I5, Q21, Q23, R10, D13, P8, P9.
