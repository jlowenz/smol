# Design Principles and Key Decisions

This document states the core principles of the language and programming
system, and records key design decisions made against those principles.
Principles should be consulted when resolving design conflicts.

---

## Core Principles

### P1: Liveness First
The programming system is always running. Code is modified in a live system,
not compiled from dead text files. Inspection, debugging, and modification
happen in the context of real execution, not hypothetical execution.

### P2: Conceptual Simplicity
The system should have as few fundamental concepts as possible, with complex
behaviors emerging from their composition. When a design choice adds a new
concept, the burden of proof is on that addition.

### P3: Opacity is the Enemy
Running software should be inspectable at all levels, at all times.
State, message flows, type behavior, and temporal history should all be
visible through the environment. Hidden behavior is a defect in the system.

### P4: Concurrency is Structural
Concurrency should not be a bolt-on concern managed by the programmer.
The system should make safe concurrent execution the default path,
with sequential execution as an optimization of concurrent semantics.

### P5: State Has History
Mutation without history is a source of complexity and bugs. All state
changes are captured with temporal context. The system supports querying
the past as naturally as querying the present.

### P6: Safety Through Architecture
Program safety comes primarily from the system's architecture (isolation,
message passing, temporal history, live feedback) rather than from
static type-checking. Type annotations are optional tools for communication,
not mandatory gates.

### P7: Minimal Ceremony
Correct programs should require minimal boilerplate. The system should
infer, default, and automate wherever possible. Ceremony that exists only
to satisfy the runtime or compiler (not to communicate intent) is waste.

### P8: Honest Tradeoffs
When a design decision involves tradeoffs (performance vs safety, simplicity
vs expressiveness), the tradeoff is made explicit and parameterizable where
possible, rather than hiding it behind a single forced choice.

---

## Key Design Decisions

Each decision records the choice made, the alternatives considered,
and the rationale.

---

### D1: Object-Based Core Model

**Decision**: The fundamental unit of computation is an object (entity or
value), communicating via message passing.

**Alternatives considered**:
- Pure functional (functions + ADTs): powerful but loses live environment
  and domain-natural modeling
- Process-only (CSP, channels): good concurrency story but poor domain
  modeling ergonomics
- "Service" metaphor: technically accurate but less domain-natural than objects

**Rationale**: Objects map naturally to domain concepts. "Everything is an
object" provides the coherent single story needed for adoption and reasoning.
Actor semantics are added as a twist, not a replacement.

**Principle**: P2, P7

---

### D2: Asynchronous by Default, Synchronous by Optimization

**Decision**: All message sends are conceptually asynchronous. Synchronous
execution is a runtime optimization applied when exclusive reachability
is provable.

**Alternatives considered**:
- Explicit async/sync distinction in syntax (different operators)
- Two object kinds: `class` (sync) vs `entity` (async)
- Programmer-declared ownership annotations

**Rationale**: A syntactic distinction creates an arbitrary semantic burden.
The GC already tracks reachability; ownership falls out of that naturally.
The programmer thinks "async everywhere" and gets sync performance where safe.

**Open question**: Static escape analysis is conservative; runtime check
overhead must be profiled.

**Principle**: P4, P7

---

### D3: Value / Entity Distinction

**Decision**: Two fundamental kinds of objects — values (immutable, no
identity) and entities (mutable, persistent identity).

**Alternatives considered**:
- Everything mutable (Smalltalk): simpler but loses immutability benefits
- Everything immutable (functional): loses live environment and identity

**Rationale**: The distinction maps to real domain modeling (addresses are
values; customers are entities). It enables safe sharing of values without
coordination, while preserving identity semantics for stateful objects.

**Principle**: P2, P4

---

### D4: Bitemporal Triple Store for State

**Decision**: Entity state is maintained as bitemporal triples (valid time
+ transaction time) in a per-process/cluster store.

**Alternatives considered**:
- Traditional mutable object slots (Smalltalk): loses history, harder to
  instrument
- Purely functional state (immutable versions): poor fit for live environment
  and identity semantics
- External database: too much ceremony, breaks encapsulation

**Rationale**: The store provides history "for free," enabling temporal
debugging, transaction rollback, and live environment instrumentation without
additional mechanism. Per-process/cluster avoids contention while logical
per-entity boundaries preserve encapsulation.

**Principle**: P3, P5, P7

---

### D5: Open-World External Assertions with Provenance

**Decision**: External entities can assert facts about other entities in a
logically separate store layer, with full provenance (asserter + timestamp).
Conflicts are preserved, not resolved by last-write-wins.

**Alternatives considered**:
- No external assertions (strict encapsulation): loses open-world reasoning
  and live environment instrumentation
- Overwrite on conflict: loses information, harder to debug

**Rationale**: The live environment needs to record observations about
running code without modifying objects. External assertions enable this.
Provenance preservation means information is never lost; query-time
resolution is explicit and auditable.

**Principle**: P3, P5, P8

---

### D6: Parameterized Transaction Semantics

**Decision**: Transaction coordination is declared by the programmer as a
semantics choice (`#atomic`, `#saga`, `#stm`, `#bestEffort`). The system
implements the protocol.

**Alternatives considered**:
- Single transaction model (always atomic): too restrictive, performance cost
- Manual coordination (programmer implements protocol): too much ceremony
- No transactions (just async messages): insufficient for many use cases

**Rationale**: Concurrency tradeoffs are real and different use cases need
different guarantees. Making the tradeoff explicit and named is more honest
than hiding it. The system provides implementations; the programmer picks intent.

**Principle**: P8, P7

---

### D7: Gradual / Optional Typing

**Decision**: Dynamically typed by default. Type annotations are optional,
local, and when present enforce via blame tracking at runtime.

**Alternatives considered**:
- Static typing throughout: better exhaustiveness checking but compilation
  barrier, rigidity, poor fit for live environment
- No types at all: loses intent communication, "unethical SLA" concern
- Full gradual typing (sound, like Typed Racket): runtime overhead
  (up to 1000x in some configurations), complexity

**Rationale**: Safety comes from architecture (isolation, temporal history,
live feedback), not type gates. Optional annotations communicate intent
without mandating static verification. Blame tracking attributes runtime
violations to responsible call sites. The live environment surfaces type
observations from actual execution.

**Known risk**: Runtime checking overhead at annotated boundaries. Mitigation:
optimize to static analysis phase where possible; use inline cache information.

**Principle**: P6, P7, P8

---

### D8: Store-Mediated UI Synchronization

**Decision**: UI components subscribe to change streams from the triple store.
Objects remain UI-unaware. UI sends intent messages to entities for mutation.

**Alternatives considered**:
- Observer pattern (objects push to UI): objects become UI-aware, violates
  encapsulation
- Polling: inefficient, high latency
- Direct property binding with getters/setters: forces structural exposure,
  ceremony

**Rationale**: The store already captures all state changes. UI subscription
is a natural consequence — no additional mechanism required. Objects preserve
encapsulation. Setters remain legitimate for entities because the store
changes their consequence (history preserved, observers notified automatically).

**Principle**: P7, P2

---

### D9: Functions as Values

**Decision**: First-class functions are included, modeled as singleton value
objects. Functions are capability-bounded: their effects are visible through
captured entity references.

**Alternatives considered**:
- No standalone functions (pure OO, everything a message): leads to
  "noun-ification" where behavior-focused code is awkward
- Full referential transparency (Haskell-style): incompatible with
  entity message sends from within functions

**Rationale**: Functions are valuable for transformation pipelines, callbacks,
and computation without ceremony. "Purity" means no direct state mutation, not
freedom from all effects. Effects through entity sends are visible via
captured references — a dynamic capability system.

**Principle**: P2, P7

---

### D10: Custom VM / Runtime

**Decision**: Build a custom VM and runtime rather than targeting an
existing VM (JVM, BEAM, V8).

**Alternatives considered**:
- JVM/GraalVM: good JIT, but hot-swapping limitations block live environment
  requirements; heavyweight thread model conflicts with actor design
- BEAM: excellent process model and per-process GC, but no object model —
  building one on top fights the VM's assumptions
- V8: excellent JIT, but single-threaded event loop conflicts with actor model

**Rationale**: No existing VM satisfies the combination of requirements:
per-process GC (no stop-the-world), live code modification as first-class,
object model with actor semantics, and GC reachability feeding ownership
determination. A custom VM allows all of these as first-class design constraints.

**Phased implementation**: Bootstrap interpreter first, then incrementally
replace components. The bootstrap interpreter serves as executable specification.

**Principle**: P1, P4

---

### D11: Implementation Language (Bootstrap)

**Decision**: Rust (tentative).

**Alternatives considered**:
- C++: most familiar, largest ecosystem, but unpleasant to write, poor
  build tooling, no memory safety
- OCaml: pleasant, excellent for compiler work, but two-layer architecture
  required for custom memory management adds friction
- Zig: interesting, better than C, but small ecosystem and immature
  async/multicore story
- Rust: memory safety, ownership model alignment, good tooling (cargo),
  growing VM/runtime community

**Known concern**: Borrow checker friction for graph-like object structures
(common in VM implementation). Mitigation: strategic `unsafe` for memory
subsystem, safe Rust above that layer. Precedent: lunatic, boa, piccolo,
rustpython all succeed with this approach.

**Validation approach**: Start with triple store implementation in Rust.
Two weeks of real work will confirm or refute the language choice with evidence.

**Principle**: P1 (implementation must support liveness from the start)

---

## Design Tensions to Watch

The following tensions are unresolved and should be revisited as
implementation proceeds:

1. **Conceptual simplicity vs feature richness**: Every addition (gradual
   typing, transaction semantics, open-world assertions) adds concepts.
   Regularly audit against P2.

2. **Performance vs transparency**: Sync optimization through escape analysis
   must not make behavior unpredictably variable. The environment must surface
   this clearly (P3).

3. **Dynamic flexibility vs tooling support**: Dynamic typing enables the live
   environment but limits static analysis. The balance between runtime
   observation and static tooling support needs ongoing calibration.

4. **Self-hosted ambition vs bootstrap pragmatism**: The system should
   eventually be developed in itself. But premature self-hosting before
   the design is stable is a trap. Defer until the language can express
   its own VM cleanly.