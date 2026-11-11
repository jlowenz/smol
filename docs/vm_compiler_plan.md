# Compiler + VM plan

**Status: DRAFT for review (2026-10-16).** Decided so far: VD8 (2026-10-17); VD1, VD2, VD4 and VD5 (2026-10-18); VD3, VD6 except item 4, and VD7 (2026-10-24).
VD6 item 4 (persisting compiled code) is open. Opened after the env-frame redesign
(`perf_env_frame_redesign.md`, COMPLETE) and the kernel coverage triage
(kernel.smol at 100%, `a280290`).

This plan covers six workstreams: the bytecode instruction set and compiler,
primitives, the process scheduler (lightweight, work-stealing, modelled on
BEAM), the triple-store interface, FFI, and the value/memory representation.
It ends with a milestone sequence (§9) in which each milestone can land on its
own and is checked against the tree-walker.

---

## 1. Starting point and fixed constraints

### 1.1 Already decided elsewhere

- **Custom VM**, not JVM/BEAM/V8 (`Design_Principles_and_Key_Decisions.md` D10,
  :255-275). "Bootstrap interpreter first, then incrementally replace
  components. The bootstrap interpreter serves as executable specification."
- **The tree-walker is the reference oracle.** It must stay correct, not fast
  (`perf_env_frame_redesign.md` :864-869). Every VM milestone is tested
  differentially against it (§8).
- **VM design inputs from the P2.1e profile** (`perf_env_frame_redesign.md`
  :871-923):
  - interned selectors;
  - symbol-keyed method dictionaries behind inline caches;
  - cached global bindings (the literal-association pattern);
  - bytecode referenced by index, so an activation doesn't copy it;
  - keep clone/drop off the send path.
- **The resolved AST is the compiler's front half.**
  - `VarRef::{Local(u16), Upvalue(u16), SelfRef, Dynamic}` (`ast.rs:56-67`).
  - `CaptureSource` (`ast.rs:73-78`).
  - Slot layout per scope (`resolve.rs:261-269`).
  - Single-assignment locals with snapshot capture, so a closure never needs
    shared mutable cells.
- **Smol-first:** implement in Smol, make it fast with the compiler/VM.
  Performance is measured but is never a veto (`chunk_34_plan.md` :14-19).
- **Futures resolve late** (CLAUDE.md). The VM must not resolve a Future
  earlier than the tree-walker does.
- **GC need not precede the VM** (`open_questions.md` Q11).
- **FFI: start with targeted Rust primitives** ("far more tractable than a
  general FFI", `web_server_readiness.md` :107-109).
- **`unsafe` is forbidden workspace-wide, "reserved for the memory subsystem of
  the future VM"** (`triple_store_PRD.md` :557-558).

### 1.2 What the VM replaces

| Today (tree-walker)                                                                | Cost / limit                                                          | VM replacement                                               |
| ---------------------------------------------------------------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------ |
| AST cloned into `Cont` per activation                                              | largest single cost in the P2.1e profile                              | bytecode by index; frames hold a `pc`                        |
| `Message::selector` builds a string per send (`ast.rs:40-50`)                      | allocation + hash per send                                            | selector is an interned constant                             |
| `Class::lookup(&str)`, SipHash per level, read lock per level (`class.rs:163-172`) | no cache                                                              | inline cache per send site                                   |
| `Dynamic` names (ivars, globals) resolved by name at runtime                       | hash per reference                                                    | ivar by index; global via cached binding cell                |
| ivars in `Mutex<HashMap<Arc<str>, Value>>` (`object.rs:37`)                        | lock + hash per access                                                | indexed slots by class layout                                |
| `_prim*` matched by string compare chains (`builtins_iterative.rs` :1590-1663)     | ~82 string compares on the slow path                                  | numbered primitive table                                     |
| one stackful `may` coroutine per entity (64 KB release)                            | 10k connections ≈ 640 MB of stacks (`web_server_readiness.md` :70-73) | stackless processes: heap frame stack, a few hundred bytes   |
| `may::coroutine::yield_now` every 1024 dispatches                                  | cooperative; arithmetic loops don't yield                             | reductions counted in the VM loop; preempt at any safe point |
| a timer is one OS thread + sleep (`os_interface.rs` :101-118)                      | thread per timer                                                      | timer wheel in the scheduler                                 |

---

## 2. Decisions needed

Each decision has a recommendation. Each is framed so it can be decided on its
own.

### VD1 — Register machine, dispatch, and instruction granularity

- **Stack** (Smalltalk-80, Pharo, CPython, JVM): the simplest compiler and
  compact code, but more instructions per expression.
- **Register** (Lua 5, Dalvik): fewer instructions, and operands name frame
  slots directly. Shi et al. measured ~47% fewer executed instructions and ~32%
  less time than the stack equivalent ([R4]). That matters more in Rust, where
  dispatch is a `match` and there is no computed goto or threaded code ([R5]).
  The resolve pass already assigns every local a slot, so the slots become the
  registers; temporaries are allocated stack-wise above them, as in Lua ([R3]).

**Status: DECIDED (user, 2026-10-18).**

**Machine:**

- A register machine with fixed-width 32-bit instructions, in Lua's
  `op:8 | A:8 | B:8 | C:8` / `op:8 | A:8 | Bx:16` formats.
- That caps a frame at 256 registers, the same as Lua; a method that exceeds it
  is a compile error for now.
- Fixed width also means a quickening rewrite (below) is a single atomic 32-bit
  store.

**Dispatch: `loop { match op { … } }` on stable Rust.** The alternatives:

- **Computed goto / direct threading:** Rust has none.
- **Closure compilation** (bytecode turned into a tree of Rust closures):
  executes by Rust recursion, so a process can't be suspended at an arbitrary
  instruction. VD2 rules it out.
- **A function-pointer handler table driven by a loop:** an indirect call plus
  a return per instruction, usually no faster than `match`.
- **Why the shared dispatch branch is acceptable:** modern predictors
  (TAGE-style designs since Haswell, Apple's M-series) predict it well; see
  Rohou et al. [R27].

**Tail-call dispatch is the revisit item.**

- Each handler tail-calls the next one, so every handler gets its own copy of
  the dispatch branch and hot state stays in registers.
- CPython 3.14 reports about 3–5% from this [R28], and Wasmtime's Pulley [R29]
  offers it as an optional backend.
- In Rust it needs guaranteed tail calls (`become`, RFC 3407 [R30]). At the
  time of writing that was believed to be nightly-only; check its status before
  revisiting. The toolchain is pinned to stable (reproducible builds), and
  relying on LLVM to optimise tail calls without the guarantee risks stack
  overflow in debug builds.
- **The handlers are defined once, by a macro, so the dispatch backend can be
  swapped.** Pulley does the same. If `become` stabilises, a tail-call backend
  is a contained change decided by same-session A/B.

**Granularity: fewer instructions per unit of Smol work**, since every
instruction pays dispatch overhead in a `match` interpreter. Three layers:

1. **Base set (V2):**
   
   - fine-grained data movement and coarse sends (one `Send` does the lookup,
     the inline-cache check, frame setup and the transfer of control);
   
   - fused compare-and-branch (`LtJump A B sBx` rather than `Lt` +
     `JumpIfTrue`);
   
   - `ForPrep`/`ForLoop` (Lua's pair), the target of guarded inline `to:do:`
     (VD4);
   
   - immediate-operand arithmetic (`AddImm A B sC`).
2. **Quick methods (V2):** the compiler marks trivial methods (`^self`,
   `^ivar`, `^constant`, a plain setter). A send to one runs without building a
   frame, like Pharo's quick-return primitives. This is a property of the
   compiled method, not an instruction.
3. **Quickening and specialization (V4):**
   
   - instructions are rewritten in place after they first run, e.g. `Send` →
     `SendMono` once the inline cache settles, and `Add` → `AddInt` when the
     operands keep being Ints (Brunthaler [R31]; CPython's PEP 659 [R32]);
   
   - bytecode is shared by all scheduler threads (VD2), so a rewrite is one
     relaxed `AtomicU32` store, and a thread sees either the old or the new
     instruction, both of them valid;
   
   - the inline-cache side tables need the same care; that design is part of
     V4.

**Superinstructions come from profile data.** V2 adds a dynamic
instruction-pair histogram behind a Cargo feature, as `send_count` works today,
so superinstructions are chosen from real kernel and benchmark traces.

### VD2 — Do processes replace `may` (stackless VM, own scheduler)?

This is the large one. The `may` design said "the scheduler is unchanged" when
the VM arrives (`chunk_21_plan.md` :256-257). The request for very lightweight,
BEAM-like processes changes that, so it needs an explicit decision.

- **Keep `may`:** the VM runs each message handler on the entity's coroutine,
  and `await` blocks on a Condvar as it does today. Least work, but it keeps
  stackful coroutines and their per-entity stack memory.
- **Own scheduler (BEAM model):**
  - A process is a heap-allocated frame stack plus a mailbox.
  - The VM loop runs a process until it exhausts its reductions, waits on a
    Future, or waits on an empty mailbox, then returns to the scheduler.
  - One scheduler thread per core, each with a local run queue. An idle
    scheduler steals from others.
  - `await` of an unresolved Future parks the process on the Future's waiter
    list; resolving the Future re-queues it.

**Status: DECIDED (user, 2026-10-18). Own scheduler, parallel from V2.** The
VM never runs on `may`.

**Rejected sequencings, and why:**

- **Run the VM on `may` coroutines first (V2–V4), scheduler in V5.**
  
  - `may` hides violations. A primitive that calls a block re-entrantly, or
    blocks on a Future, still works on a coroutine with a real stack, so code
    that breaks under the real scheduler would build up unseen.
  - The waits (tx mailbox locking, deadlock timers, TCP) would be written twice.
  - Collections are entities, so even V2 sends between processes. Processes are
    core from the start.

- **A single-threaded scheduler first, parallel in V5.** This hides threading
  assumptions the way `may` hides blocking:
  
  - a status read and then acted on as if no race were possible;
  - a mailbox push and a wake that "obviously" happen in that order;
  - two processes resolving or awaiting the same Future at once.
  
  The evaluator surfaced bugs of this kind early because `may` was parallel from
  the start, and the tree-walker oracle is itself parallel. The goal is
  correctness under non-deterministic parallel execution; determinism is a
  debugging aid, not a development phase.

**Built in V2:**

- The process core:
  - `Process`;
  - `run(proc, budget) -> RunOutcome { Yield | Wait(WaitReason) | Done | Error }`,
    stackless, with no Rust recursion per Smol call and every state in `proc`;
  - `PrimResult::Suspend`, and block-call requests from primitives;
  - park/wake.
- N scheduler threads with work stealing (`crossbeam-deque` workers plus a
  global injector). Processes migrate between threads from the first commit.
- Process status managed by atomic transitions. A message arriving while the
  process is about to wait on its mailbox must not lose the wake.

**Deferred to V5:** the efficiency and I/O parts. Their interfaces exist from
V2, so deferring them hides no assumptions.

- Timers: a simple timer thread that resolves Futures through the injector,
  until the timer wheel.
- I/O: helper threads that resolve Futures through the injector, until `mio`.
- The blocking-call pool, the LIFO slot, priorities, and fairness and stealing
  tuning.

**Guards that make violations fail loudly, whatever the thread count:**

- Blocking on a scheduler thread panics in debug builds. Scheduler threads mark
  themselves, and any Condvar wait or forced Future reached from one panics, as
  Tokio does ("Cannot block the current thread from within a runtime").
- A thread-local flag makes a re-entrant `run` (a primitive calling back into
  the VM) an assertion failure.

**Testing modes:**

- **Default: parallel with schedule perturbation.** Random steal choices,
  jittered reduction budgets and forced yields push tests into more
  interleavings. This is the idea behind PCT [R24] and `shuttle` [R25].
- **Seeded single-thread mode:** for replaying a failure only.
- **`loom` [R26]:** exhaustively checks the atomic orderings of park/wake and
  the run queues.
- **Differential testing against the oracle:** exact equality for results that
  don't depend on ordering; property assertions for those that do.

### VD3 — `Value` representation and `unsafe`

The options:

- **(a)** Keep the current `enum Value` (≈24 bytes, `Arc` payloads), with no
  `unsafe`.
- **(b)** Shrink it to 16 bytes with no `unsafe`. Every payload has to become
  a thin pointer or ≤8 bytes:
  - `Symbol(Arc<str>)` becomes an interned id (V1);
  - `Str(Arc<str>)` and `Handle(Arc<dyn Any>)` are fat pointers today, so they
    need a thin wrapper such as `Arc<Box<str>>` (one more indirection on
    string reads) or a thin-arc crate.
- **(c)** 8-byte NaN-boxing or tagged pointers, which need `unsafe` in one
  contained module.

**Status: DECIDED (user, 2026-10-24). (b) behind an accessor/constructor
layer.** Leaning (b) since 2026-10-17. The layer comes first in V1; (c) is
reconsidered only with per-process GC (VD8), under the principle below.

**Principle (user; P10 in `Design_Principles_and_Key_Decisions.md`):** VM encoding choices must not leak into what a developer
has to know about the language. Full i64 integers and full f64 floats need 64
payload bits plus a tag, so honouring this makes 16 bytes the minimum and rules
out the range-reducing forms of (c):

- tagged pointers: 61-bit SmallIntegers, as in Pharo, and boxed or
  range-reduced floats;
- NaN-boxing: ~48–51-bit integers and canonicalised NaN payloads.

Promotion to LargeInt is automatic, so a moved boundary would mostly be a
performance cliff, not a semantic change. That is still the hidden knowledge
the principle rules out.

**Size versus reference counting.**

- **Size.** A 64-byte line holds 2.7 values at 24 bytes, 4 at 16 and 8 at 8.
  Density matters most in large arrays and numeric loops, and typed collections
  (unboxed IntArray, FloatArray, ByteArray; the SoA direction in Q9) recover it
  there without changing `Value`.
- **Reference counting.** This is the clone/drop cost the P2.1e profile
  flagged, and it is independent of size. A register `Move` of a heap value
  increments a reference count; immediates (Int, Float, Bool, Nil) don't touch
  a count.
- **Under V5.** With the work-stealing scheduler, counts on values shared
  across cores (kernel classes, common blocks, literals) bounce cache lines
  between cores.
- **Mitigations that keep reference counting** (§2 VD8 removes it inside a
  process):
  - immortal objects for kernel classes, interned symbols and literals, which
    are never counted (PEP 683, [R19]);
  - biased reference counting, where the owning thread doesn't use atomics
    ([R20]; CPython's free-threaded build uses both, PEP 703 [R21]);
  - primitives borrowing `&Value` from registers instead of cloning.

**Not changing representation twice.** The risk is doing (c) on `Arc` now and
again on GC pointers later. Per-process GC (VD8) brings `unsafe` (raw pointers
into a process heap) regardless of layout, so (c) is reconsidered then, under
the principle above. (b) behind an accessor layer doesn't carry this risk.

**Size of the change.** Outside tests, 509 sites construct or match `Value`
variants (1,317 in tests).

- (b) touches the `Str`, `Handle` and `Symbol` sites, about 120.
- (c) touches all of them, because `Value` becomes opaque. That includes the
  tree-walker oracle, since both engines share `Value`.
- Because both engines share `Value`, a representation bug affects both
  identically and differential testing can't catch it. Representation changes
  therefore land in V1, before the VM, checked by the existing suite.

**Recommendation:**

- **V1, regardless of the option chosen:** put `Value` behind
  constructor/accessor functions, so a later layout change touches one module.
  The accessor layer can later also enforce VD8's invariant at the type level,
  as a process-local value versus a shared value.
- **Then (b):** add a `size_of::<Value>() == 16` assert, and measure the
  24 → 16 gain by A/B rather than assuming it.

### VD4 — Inlining control structures

**Status: DECIDED (user, 2026-10-18). Guarded inlining.** Overrides and kernel
redefinitions are always honoured.

**What unguarded inlining (Smalltalk-80, Pharo) bypasses.** The compiler turns
`x ifTrue: [...]` into a conditional jump, so no method is looked up:

1. **Redefinition of the kernel method itself**, e.g. `True >> ifTrue:` edited
   to add tracing. This is the main concern for Smol: these methods live in
   kernel.smol (Smol-first), and live code modification and observability are
   core goals.
2. **Objects that are not the expected class but answer the selector.**
   
   - In Pharo, a Null Object overriding `ifNil:` is bypassed, because `ifNil:`
     compiles to `== nil`.
   
   - A proxy answering `ifTrue:` survives only through the `mustBeBoolean`
     recompile path.
   
   - In Smol, the important case is a Future receiver.
3. **Subclasses.** Bool and Nil are encoded leaves, closed by construction
   (`numeric_tower_plan.md`), so this concerns only the numeric loops and
   Number wrappers (e.g. `Money >> to:do:`).

Pharo inlines unconditionally for historical speed, since closure creation plus
a send per branch was prohibitive on 1983 hardware ([R1]), and because
overriding is rare. The cost is a semantic exception every Smalltalker has to
learn: the "VM-forced hidden knowledge" ruled out by the VD3 principle. Smol's
stated rule is to "inline the compiled fast path when the method has not been
overridden" (`chunk_34_plan.md` :776-790).

**Inlined selectors.** Each is inlined only when the block arguments (and, for
the `while` family, the receiver) are literal blocks:

- `ifTrue:`, `ifFalse:`, `ifTrue:ifFalse:`, `ifFalse:ifTrue:`, `and:`, `or:`
  (`True`/`False`, kernel.smol :1457-1475);
- `whileTrue:`, `whileFalse:`, `whileTrue`, `whileFalse` (`Block`,
  kernel.smol :1549-1566);
- `to:do:`, `to:by:do:`, `until:do:`, `until:by:do:` (`Number`,
  kernel.smol :1697-1755); the `to:`/`until:` duality keeps one loop shape;
- `ifNil:`, `ifNotNil:`, `ifNil:ifNotNil:`, `ifNotNil:ifNil:`, `isNil`,
  `notNil`. These are protocol, defined in Smol on `Object` and `Nil`, and are
  overridable (the Null Object pattern). They are promoted from today's Rust
  arms in the tree-walker first; see "Nil: identity versus protocol" below.

`timesRepeat:` is a candidate, to be added if the V2 instruction histogram
shows it is hot.

**Guard 1: the receiver check (runtime), against cases 2 and 3.**

- `ifTrue:`/`and:` family: the inlined jump already examines the value, so
  confirming it is a Bool (`JumpIfNotBool → full send`) is nearly free.
- `to:do:` family: both bounds (and the step) must be Ints. Anything else (a
  wrapper, a LargeInt, a Future) takes the full send.
- `ifNil:` family: the class-resolution check described below. A Future
  receiver takes the full send, which keeps late resolution exactly as the
  tree-walker does it.
- `while` family: the receiver is a literal block, so no runtime check is
  needed.

**The general form of guard 1:** the receiver's class resolves the selector
to the kernel method being inlined.

- For closed leaves (Bool, Int, Nil) this reduces to a tag check.
- For instances, the guard loads the class and checks a cached resolution
  result, like an inline cache. The class version tag (VD6) invalidates it.
- A class that overrides the selector fails the guard and gets a full send.

**Nil: identity versus protocol** (user, 2026-10-18):

- **Identity** (`x == nil`): is the reference nil? This is language-level and
  cannot be faked; a Null Object is not nil.
- **Protocol** (`isNil`, `notNil`, the `ifNil:` family): messages, defined in
  Smol (`Object >> ifNil: b [^self]`, `Nil >> ifNil: b [^b value]`, …).
  Overriding them is the Null Object pattern, which guarded inlining preserves.
  Pharo's unguarded inlining breaks exactly this.
- **Another entity as receiver:**
  - If its class doesn't override the selector, the inherited method (`^self`)
    reads no entity state, so the guard's fast path answers locally. That is
    observationally equivalent to sending, minus the mailbox round trip and any
    deadlock risk.
  - If its class does override, the send is a real async message answering a
    Future: the class chose to let the entity decide.

**Guard 2: dependency-tracked recompilation (no runtime cost), against
case 1.**

- The compiler records which inlined selectors each compiled method depends on.
- Redefining an inlined selector on its guarded class (`True`, `False`,
  `Block`, `Number` or a superclass, or the nil-guard family) recompiles every
  dependent method with that selector as a real send.
- Activations already running finish on their old code (the usual Smalltalk
  rule for method replacement).
- Redefinition is rare, so the cost falls on the edit rather than on every
  execution. It sits beside VD6's class version tags, which only make caches miss.

### VD5 — Primitive binding

**Status: DECIDED (user, 2026-10-18).** Keep the source syntax (`self _primAdd: x`). The compiler
resolves `_prim` selectors to a `Prim id` instruction at compile time.

- The ids come from a primitive table registered at startup (§4).
- An unknown `_prim` selector is a compile error. Today it's a runtime
  `doesNotUnderstand:`.
- The kernel doesn't change.

### VD6 — Where compiled code lives

**Status: DECIDED (user, 2026-10-24)**, except for persisting compiled code,
which is open (item 4).

1. **Compile at install.** `Method` gains a compiled form, built when the
   method is installed rather than on first call.
   - VD5 makes an unknown `_prim` a compile error, and it should surface when
     the method is defined.
   - Until the compiler covers the whole language (V1–V3), a method it cannot
     compile is marked uncompiled. The VM refuses to run it; it never falls
     back silently.
   - The class/object/entity runtime structures stay shared by both engines,
     so the tree-walker remains a working oracle on the same objects.
2. **Invalidation: a version tag per class, not a global epoch.**
   - The system is live and asynchronous, with many lightweight processes.
     Classes are defined at runtime (REPL chunks, programs that build classes,
     live edits to running servers). With a global epoch, any install
     anywhere would flush every inline cache in the system, so an edit would
     slow down every running process.
   - Each class carries a `version: AtomicU64`. An inline cache or a VD4
     guard records `(class, version)` and hits only while both still match.
   - Anything that can change what a lookup through the class resolves to
     bumps the version of that class **and of every subclass**: installing or
     removing a method, changing the superclass, or applying a trait. An ivar
     layout change bumps it too (see the layout epoch in §6).
   - So each class keeps a list of its subclasses, held weakly so that a
     dropped class is not kept alive. It has none today (`class.rs:73`). This
     is CPython's `tp_version_tag` with `PyType_Modified`, which invalidates
     a type and all of its subtypes [R33]. A 64-bit tag avoids the overflow
     that CPython's 32-bit tags have to handle.
   - VD4's dependency-tracked recompilation stays separate: it re-emits code
     that inlined a selector. Version tags only make caches miss.
3. **Concurrency (VD2).**
   - Compiled code is an immutable `Arc`, swapped in by an install.
   - A running activation keeps the `Arc` it started with (VD4: running
     activations finish on their old code).
   - Bumping a version is a Release store; an inline-cache check loads it with
     Acquire, so a thread that sees the new version also sees the new method.
4. **Persisting compiled code: OPEN.** The source is the source of truth, and
   compiled code is always derivable from it. A persisted cache of compiled
   code, outside the triple store, is not ruled out; it would cut startup cost
   as the image grows. Anything cached has to be keyed on everything the
   compiled form depends on, and discarded on any mismatch:
   - a hash of the method source (as in CPython's hash-based `.pyc` files
     [R34]);
   - the compiler and ISA version;
   - the PrimTable, since VD5 binds `_prim` sends to ids at compile time;
   - the layout of the defining class, since `GetIvar`/`SetIvar` use slot
     indices that depend on the superclass chain's ivars.

   Dependency registrations (VD4) and inline caches are rebuilt on load,
   never persisted. The decision waits until startup cost is measured.

### VD7 — Milestone order (§9)

**Status: DECIDED (user, 2026-10-24). V4 before V5.**

With VD2 decided, the scheduler core is in V2, so the per-process footprint win
arrives with V2. V5 is reduced to I/O and tuning. V4 (inline caches, speed)
comes before V5 (timer wheel, `mio`, blocking pool), so V5's I/O work is
profiled on the fast send path.

### VD8 — Per-process GC and promote-on-escape

**Status: DECIDED (user, 2026-10-17).** Large immutable values are never copied.

This refines `High-level_Components.md` §2.5 ("values structurally shared") with
the mechanism that makes sharing work under per-process collection. The GC
itself is still post-VM work (§9 "later"); this decision constrains everything
before it.

- **Per-process heaps** with a generational copying collector. A collection
  happens at the process's own yield points, roots are only its frames and
  registers, and there is no stop-the-world. A stackless process (VD2) moves
  between scheduler threads together with its heap.
- **Invariant:** nothing outside a process points into its heap. That covers
  other processes, the shared heap, and the store.
- **Promote-on-escape.**
  - An immutable value is born in the process heap.
  - When it escapes it is copied once into the shared heap. It escapes when it
    is sent in a message, is a Future's resolution, is stored into another
    entity, or is published as a global.
  - Later sends pass the pointer.
  - Immutability makes the copy indistinguishable from the original. This
    follows Doligez–Leroy [R22] and OCaml 5's minor heaps [R23].
  - It is not BEAM's model: BEAM copies every message.
- **Large values are born shared.** Above a size threshold, an immutable value
  is allocated directly in the shared heap, so it is never copied, not even on
  escape. BEAM does the same for binaries over 64 bytes, and OCaml allocates
  large objects directly in its major heap. The threshold is tuned later.
- **Write barriers.** Values are immutable, so new data only points at older
  data. Only mutation (`SetIvar`, indexed `at:put:`) can create an old-to-young
  pointer, so only those instructions carry a barrier.
- **Entities** travel as handles and are never copied.
- **Open:**
  - reclaiming the shared heap: reference counting, with immortal kernel
    objects, or a concurrent tracer;
  - entity liveness, i.e. when a process itself is dead (BEAM never collects
    processes, Pony's ORCA [R17] does; tied to Q19);
  - how content-hashed stored values in the triple store relate to the shared
    heap.

---

## 3. Instruction set (draft)

Notation: `R[x]` is a frame register, `K[x]` a constant in the method's
constant pool, `U[x]` a block's captured value (from its snapshot), `IC[x]` an
inline-cache slot owned by the instruction.

| Group                      | Instructions                                                                                                                        | Notes                                                                                                                                                                                                                                                   |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Load/move                  | `Move A B`; `LoadK A Bx`; `LoadInt A sBx`; `LoadNil/True/False A`; `LoadSelf A`                                                     | `LoadInt` keeps small ints out of the constant pool                                                                                                                                                                                                     |
| Variables                  | `GetUpval A B`; `GetIvar A B`; `SetIvar A B`; `GetGlobal A Bx`; `SetGlobal A Bx`                                                    | ivar by layout index; `SetIvar` also buffers the store fact (§6). `GetGlobal` reads a binding cell (`K[Bx]`) that holds the resolved `Arc<Binding>`, invalidated when the namespace changes                                                             |
| Sends                      | `Send A B C` (receiver and args in `R[A..]`, selector `K[B]`, argc `C`, IC alongside); `SuperSend`; `Prim A B C` (primitive id `B`) | the result lands in `R[A]`                                                                                                                                                                                                                              |
| Special sends              | `Add Sub Mul Lt Le Gt Ge Eq` … `A B C`                                                                                              | fast path for Int/Float operands; anything else (wrappers, LargeInt, a Future) does a full send of the same selector, as Pharo's special selectors do                                                                                                   |
| Arithmetic with immediates | `AddImm A B sC`, `SubImm A B sC`                                                                                                    | `i + 1` without loading a constant; same fallback as the special sends                                                                                                                                                                                  |
| Control                    | `Jump sBx`; `JumpIfTrue/False A sBx`; `JumpIfNil/NotNil A sBx`; `JumpIfNotBool A sBx`                                               | the targets of guarded inlining (VD4). A backward jump costs a reduction                                                                                                                                                                                |
| Fused compare-branch       | `LtJump`, `LeJump`, `EqJump` … `A B`                                                                                                | one dispatch for `a < b ifTrue:` and loop tests. The 32-bit word has no room for two registers and an offset, so the encoding is either Lua's "compare, then skip the next `Jump`" or a trailing offset word; it is settled when the encoder is written |
| Loops                      | `ForPrep A sBx`; `ForLoop A sBx`                                                                                                    | Lua's numeric for pair: the guarded inline `to:do:` / `to:by:do:` costs one instruction per iteration plus the body. Non-Int bounds take the full-send path                                                                                             |
| Blocks                     | `MakeBlock A Bx` (prototype `Bx`; capture list in the prototype, copied from `R`/`U`)                                               | snapshot capture, matching `CaptureSource`                                                                                                                                                                                                              |
| Return                     | `Return A`; `ReturnSelf`; `NonLocalReturn A`                                                                                        | a non-local `^` from a block unwinds to the home frame through `ensure:` frames; it is an error if the home frame is dead                                                                                                                               |
| Literals                   | `NewArray A B C`                                                                                                                    | `{...}` dynamic array                                                                                                                                                                                                                                   |
| Async                      | `Await A B`                                                                                                                         | explicit `await`. The VM never forces a value implicitly: an implicit force happens only where the tree-walker forces (`force_value` sites), and is compiled as an `Await` at that site                                                                 |

- **Quick methods** (VD1) need no instruction: the compiled method carries a
  `QuickKind` (`Self`, `Ivar(n)`, `Const(k)`, `Setter(n)`), and `Send` runs it
  without a frame.
- **Cascade** needs no instruction: the receiver is held in a register.
- **Frame markers** for `on:do:`, `ensure:` and `HandlerContext run:` are flags
  on the frame, set by their primitives, much as Pharo marks contexts with
  primitives 198/199 ([R7]). Unwinding scans the frame stack for these flags,
  replacing today's `EnsureGuard`/`HCRunGuard` continuations.
- **Debug info:**
  - a pc → `SourceTag` table per method, built from the existing `SourceTagged`
    statements, so the kernel coverage tooling ports to the VM;
  - register → name maps for the debugger.

Open: whether `Send` should carry the argument count in the selector constant
rather than in `C`, which would free a field for the IC index. That will be
decided when the encoder is written.

### 3.1 Review findings (2026-10-24)

The table above is unchanged; these findings are applied to it once decided.
Checked against the tree-walker: tail calls are eliminated
(`drain_tail_frames`, `eval_iterative.rs:1435`); the strict block-assignment
rule holds (`resolve.rs:54`); `==` is an ordinary kernel method today
(`kernel.smol:370`).

**Decided:**

- **`==` and `~~` are language-level identity, not overridable** (user,
  2026-10-24). In Smol, as in Smalltalk, `==` is identity and `=` is equality;
  `=` stays overridable. Without this, a Null Object could define
  `== other [ ^other isNil ]` and fake `x == nil`, which VD4's
  identity-versus-protocol rule assumes cannot happen. The compiler emits an
  `Identical` / `JumpIfIdentical` instruction with no send, as Pharo's special
  selector `==` does. This is a language change, so it lands in the
  tree-walker first, as the nil protocol did (`cf6ce0d`).

**Proposed, not yet decided:**

1. **`TailSend` / `TailSuperSend`.** The tree-walker eliminates tail calls, and
   recursive Smol code (user recursion, the `whileTrue:` fallback when a guard
   fails) depends on it; without them the VM grows frames where the oracle
   doesn't. Emitted only when the frame being replaced carries no
   `ensure:`/`on:do:` marker, as `drain_tail_frames` drains only empty frames.
   Cost: elided frames are missing in the debugger (P3), which the tree-walker
   already accepts.
2. **A guard for overridable inlined selectors:** `JumpIfNotDefault A IC sBx`
   continues when R[A]'s class resolves the selector to the kernel method
   (cached on class plus VD6 version tag) and otherwise jumps to the full send.
   `JumpIfNil` alone is wrong for `x ifNil: [...]` now that the nil protocol is
   overridable. The same guard covers `f312f00`'s in-place answers for `=`,
   `~=` and `class` on another entity. The `ifTrue:` family needs only the
   `JumpIfNotBool` tag check (True and False are closed; redefinition is
   guard 2).
3. **Self in `R[0]`, arguments in `R[1..n]`**, which removes `LoadSelf`.
   `ReturnSelf` stays (implicit self-return is common).
4. **`Eq` fast path only for operands of the same encoding** (Int/Int,
   Float/Float); mixed operands take the numeric-tower send (`2 = 2.0`).
5. **Clean blocks:** a block with no captures and no `^` becomes a constant
   (`LoadK`), as Pharo's clean blocks do, so it isn't allocated per evaluation.
6. **Every send costs a reduction**, as in BEAM, not only backward jumps.
7. **No `SetUpval`**, confirmed by the strict block-assignment rule; the table
   should say so.

**Open: class variables** (to explore further, user 2026-10-24). How they work
today:

- No metaclasses: a `Class` carries `methods` and `class_methods`
  (`class.rs:73-110`); the class object at runtime is a `ClassEntity`, a
  singleton with its own mailbox (`entity.rs:602-606`). There is no syntax for
  class-side instance variables.
- Class variables (`classVariableNames:`; one kernel use, `Warning`'s
  `LoggingPolicy`, `kernel.smol:838`) live in `Class.class_var_storage`, a
  locked map on the shared Rust struct. Any instance's process reads and
  writes it directly (`eval.rs:485-499`, `592-613`), as does Rust
  (`builtins_iterative.rs:2669`), bypassing the class entity: no mailbox, no
  transaction/STM shadowing, no store fact. `entity.rs:603` records the
  unification with ivar storage as deferred.

An earlier draft of this review proposed `GetClassVar`/`SetClassVar` through a
shared binding cell, like globals. That carried the current implementation
over without questioning it, and is withdrawn.

Smalltalk distinguishes instance variables, class-instance variables (ivars of
the class object, one per subclass) and class variables (one slot shared by
the declaring class's whole subtree, readable by bare name from instance-side
methods). With classes as entities, class-instance variables are just ivars of
the class entity. Class variables differ only by subtree sharing and
bare-name access, and under the entity model both become messages to the
class entity: a bare-name read from an instance method would be a hidden
message answering a Future (a P3 concern).

Leading option: drop class variables as a separate concept (P2). Class state
becomes ivars of the class entity, accessed through class-side methods;
instance-side code sends to the class explicitly. The VM then needs no
class-variable instructions, and class state gets the transaction and store
treatment of any ivar. Costs: class-side ivar declaration syntax, migrating
`LoggingPolicy`, and losing subtree sharing unless a class-side method
forwards to the declaring class.

**Update (user, 2026-10-26): decided, further than the leading option.**
Classes are immutable, versioned values with an implicit metaclass, not
entities: no class variables and no class-instance variables. Constants are
class-side methods: the compiler folds literal answers, and the VM may cache a
constructed answer when it observes an effect-free evaluation that answered a
Value, guarded by VD6 version tags. Namespaces hold definitions only. Record and
rationale: `namespace_design_exploration.md` §9. Consequences for this plan:

- No class-variable instructions, and no `ClassEntity` dispatch path. A
  class-side send is an ordinary send whose receiver's class is the implicit
  metaclass, so it uses the same inline caches.
- **Open: Value `==`.** Caching a constructed constant is observable while
  `==` on Values is allocation identity (`kernel.smol:359`). Either identity
  of Values becomes unobservable and `==` on Values compares state, as Java's
  value objects do (JEP 401); or identity of Values is left unspecified.
  Leaning to the first. This qualifies the `==` decision above for Values.

---

## 4. Primitives

- **Table.** `PrimTable` maps `PrimId(u16)` ↔ name and holds
  `fn(&mut Process, args: &[Value]) -> PrimResult`. It is built at startup, and
  native modules register into it (§7).
- **Result.**
  `enum PrimResult { Value(Value), Error(EvalError), Suspend(WaitReason), Yield }`.
  - `Suspend` lets an I/O or timer primitive park the process instead of
    blocking a scheduler thread (BEAM's model, [R8]).
  - `Yield` is for long primitives that make progress in chunks.
- **Blocking work.** A primitive registered as `blocking` (file I/O, durable
  commit fsync, later FFI calls) runs on a separate "dirty" thread pool, like
  BEAM's dirty schedulers ([R9]). The calling process is suspended until the
  pool hands back the result.
- **Migration.** The ~82 current `_prim*` selectors are ported group by group
  (the categories in `primitives.md`). Each group is ported in the milestone
  that needs it. Per Q7, every port is also a chance to decide whether the
  primitive is truly irreducible or should move into Smol.

---

## 5. Processes and the scheduler (V2 core, V5 I/O and tuning)

Modelled on BEAM ([R8], [R10]), using Rust's standard work-stealing parts
([R11], [R12]). Staging per VD2: everything here is built in V2, except where a
bullet says V5.

**Process:**

- `{ id, frames: Vec<Frame>, regs: Vec<Value>, mailbox, status, reductions,
  tx_frames, … }`.
- An entity is a process. Value objects, blocks and Futures are not. A
  registered continuation (`then:orElse:`, timer callback) runs as a
  short-lived continuation entity, so it is a process too (Q23 decision B2).
  "Process" is the VM's term for an entity, not a second concept.
- Status is one of `Runnable | Running | WaitingMailbox | WaitingFuture(f) |
  WaitingIo | Done`.

**Scheduler threads:**

- One per core.
- Each has a local `crossbeam_deque::Worker`.
- *(V5)* A LIFO slot for the most recently woken process; this is Tokio's
  message-passing optimisation ([R12]).
- A global `Injector` takes work from non-scheduler threads.
- An idle scheduler steals half of a victim's queue (Chase–Lev deque, [R11]),
  then parks.

**Reductions:**

- Every send, primitive call and backward jump costs 1.
- The budget is `REDUCTIONS_PER_SLICE`; BEAM uses 4000 ([R8]).
- When the budget runs out the loop returns `Yield`, and the process goes to
  the back of its scheduler's queue. Because the loop is stackless this is a
  plain return, which is the preemption `may` couldn't give.

**Message send:**

- Enqueue an `Envelope`: selector (now an interned id), args, reply Future,
  one opaque **context** slot, and the id of the sending turn (Q23 decision D,
  2026-11-03). The context slot replaces the separate inherited handler chain
  and tx tag fields (Q22: handler contexts, dynamic bindings and the tx tag merge
  into one execution-context Value), so new context kinds need no new field. The
  sending turn's id records the causal graph (P3).
- If the receiver is waiting on its mailbox, mark it runnable (an atomic
  status transition, VD2) and push it to the current scheduler's queue (its
  LIFO slot from V5).
- Message values are not copied per send: Smol values are immutable and
  shareable, and entities travel as handles (the value/object split). Until the
  GC exists, `Arc` shares them directly. Once per-process heaps arrive (VD8), a
  send is one of the escape points: a value still in the sender's heap is
  promoted to the shared heap once, and every later send passes the pointer.
  This differs from BEAM, which copies every message.

**Futures:**

- `FutureState` gains a waiter list, and records the process that will settle
  it (Q23 decision D), so a process waiting on a Future is a wait-for edge and
  deadlock detection covers Future waits (issue I2).
- Resolving a Future re-queues its waiters. Late resolution is preserved,
  because only an explicit `Await` site parks.

**Timers:**

- V2/V3: one timer thread that resolves Futures through the injector.
- *(V5)* A hierarchical timer wheel per scheduler ([R13]).

**I/O:**

- V3: TCP primitives return `Suspend(WaitingFuture)`; helper threads do the
  blocking call and resolve the Future through the injector.
- *(V5)* A `mio` poller ([R14]), with TCP primitives returning
  `Suspend(WaitingIo(token))`; BEAM polls inside its schedulers.
- *(V5)* The blocking-call pool (§4).

**Atomic and STM transactions:**

- The transaction protocol itself (`tx_design.md`) is unchanged.
- Waiting on an entity's mailbox lock becomes a park instead of a Condvar wait,
  built in V3 and written once.
- Deadlock detection keeps its timer, from the timer thread (the wheel from V5).

**`may`:**

- The VM never uses it.
- The tree-walker oracle keeps `may`, `COROUTINE_STACK_SIZE` and the C31.5
  stack-depth machinery for as long as it runs.

Open:

- process priorities (BEAM has four);
- fairness under stealing (`web_server_readiness.md` :82-83);
- whether a scheduler can migrate a process holding a transaction frame
  (it should be able to: the state is all in the process).

---

## 6. Triple-store interface

The store boundary doesn't change: `Store` / `OwnedStoreTransaction` (the
triple-store crate) stays the only API. What changes is how the VM reaches it:

- **Per-message transaction.** The process loop opens
  `begin_owned(ExecContext::Entity(id))` at the start of a handler and commits
  at the end, as `run_entity_loop` does today (`entity.rs:1158`).
- **Indexed ivars.**
  - A class layout fixes each ivar's index.
  - The class caches each ivar's `Predicate` when it is defined, so
    `SetIvar A B` writes the slot and calls `buffer_ivar` with a pre-resolved
    predicate. The name lookup (`store_bridge.rs:303`) leaves the hot path.
  - Live code modification (adding an ivar) changes the layout. That needs
    instance migration: an epoch on the layout, with instances upgraded lazily
    on access. This is a sub-design for V3.
- **Durable commit.** If a backend's commit does I/O (WAL fsync), it runs as a
  `blocking` primitive on the dirty pool (§4), so a scheduler thread never
  waits on disk.
- **`historyOf:` and queries** become ordinary primitives.
- **Interning.** The store has its own per-`Store` `StringInterner`
  (`triple-store/src/intern.rs`). The VM's selector/symbol interner is
  separate; whether the two should be unified (one `SymbolId` space) is open,
  and only matters once symbols are stored as facts.

---

## 7. FFI

Staged, following `web_server_readiness.md` :107-109:

1. **Native modules (V7a).**
   
   - A Rust crate registers primitives into `PrimTable` through a small
     registration API (name, arity, `blocking` flag, fn).
   
   - It is statically linked, needs no `unsafe` in the VM core, and covers
     sockets, crypto and TLS through existing crates.
   
   - This is BEAM's NIF shape without the dynamic loading.
2. **Dynamic C FFI (V7b).**
   
   - A separate `smol-ffi` crate is the only place `unsafe` lives, contained and
     tested as the PRD requires.
   
   - It uses `libloading` + `libffi` ([R15]).
   
   - Signatures are declared in Smol and checked when the call is bound, in the
     style of Pharo's uFFI ([R16]). Every call runs on the dirty pool unless it
     is marked `fast`.
   
   - Foreign memory lives behind `Value::Handle`, with an explicit free.

---

## 8. Testing and measurement

- **Differential oracle.**
  - `test_helpers::eval_diff` / `eval_err_diff` were kept as aliases when the
    recursive evaluator was retired. They become real again: run the
    tree-walker and the VM and assert equal results or errors.
  - From V2 on, the whole suite runs through both engines. Any divergence is a
    VM bug unless the tree-walker is shown to be wrong; if it is, report it and
    fix it there first.
- **Kernel coverage.** `just cov-suite` runs on the VM once the pc → `SourceTag`
  table exists (V2), with the same 100% gate.
- **Performance.**
  - Measured only by same-session A/B against the tree-walker on the same tree.
  - Yardsticks: `examples/alloc_probe`, criterion benches, and a send-cost
    microbench.
  - Targets are recorded, never used as gates (Smol-first): V2 at or below the
    tree-walker; V4 below ~120 ns/send, a typical tree-walker; V4 stretch below
    ~50 ns.
- **Scheduler.** From V2: `loom`, perturbed parallel runs, seeded replay
  (VD2). From V5: ring and skynet-style spawn benchmarks, and a 10k-entity
  footprint check against the `web_server_readiness.md` numbers.

---

## 9. Milestones

Each milestone gets its own `docs/vm_vN_plan.md` before work starts.

| #      | Milestone                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Done when                                                                                                                          |
| ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| **V0** | Decisions VD1–VD8 taken (all decided except VD6 item 4); ISA spec frozen for V2                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | this doc updated to DECIDED                                                                                                        |
| **V1** | Interning and identities: the `Value` accessor/constructor layer (VD3), a global selector/symbol interner (`Value::Symbol` keyed by id), class ids, `Value` to 16 bytes with a size assert (VD3 (b)), method dictionaries keyed by interned id                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | full suite green; tree-walker A/B no slower                                                                                        |
| **V2** | Compiler, VM core and parallel scheduler core. Compiler and VM: codegen from the resolved AST, frames, sends (no IC yet), blocks and snapshot capture, local and non-local returns, the `PrimTable` with arithmetic/string/collection prims, the kernel compiled at load, the VD1 base set (fused compare-branch, `ForPrep`/`ForLoop`, immediates), quick methods, guarded inlining with both guards and dependency-tracked recompilation (VD4), and the instruction-pair histogram feature. Scheduler (VD2): `Process`, stackless `run`, atomic park/wake, Future waiter lists, reductions, N work-stealing threads, debug guards against blocking and re-entrancy, `loom` tests, perturbed and seeded test modes. Never runs on `may` | differential suite green (parallel, perturbed) on everything without conditions or transactions; `loom` green; cov-suite on the VM |
| **V3** | Unwinding and the rest of the primitive surface: frame markers for `on:do:`/`ensure:`/HC, signal/resume/retry, DNU/`perform:`, transaction and STM prims with mailbox-lock waits as park/wake, timer and TCP prims via a timer thread and helper threads, indexed ivars + predicate cache, layout migration                                                                                                                                                                                                                                                                                                                                                                                                                             | whole differential suite green; the VM becomes the default engine (the tree-walker stays as the oracle)                            |
| **V4** | Speed: monomorphic inline caches keyed on per-class version tags (VD6), cached global bindings, special-selector fast paths, quickening/specialization with atomic rewrites and superinstructions chosen from the histogram (VD1)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | A/B results recorded against the §8 targets                                                                                        |
| **V5** | Scheduler I/O and tuning: timer wheel, `mio` I/O, blocking-call pool, LIFO slot, priorities, fairness and stealing tuning                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | differential suite green; ring/skynet and 10k-entity numbers recorded                                                              |
| **V6** | Polish and observability: debugger hooks (frame inspection by register name), coverage and profiling on the VM, polymorphic inline caches if the profile asks for them                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | —                                                                                                                                  |
| **V7** | FFI: (a) native modules, (b) dynamic C FFI in `smol-ffi`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | a TLS listener via a native module; a libc call via the FFI                                                                        |
| later  | GC per VD8 (per-process heaps, promote-on-escape, shared heap for large and escaped values; Q3/Q11/Q19); `Value` representation revisited under the VD3 principle; JIT (Project_Summary phase 6)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | —                                                                                                                                  |

---

## 10. References

- [R1] Goldberg, Robson. *Smalltalk-80: The Language and its Implementation*
  ("Blue Book"), 1983, part 4 (the VM). http://www.mirandabanda.org/bluebook/
- [R2] Deutsch, Schiffman. "Efficient Implementation of the Smalltalk-80
  System." POPL 1984 (inline caches).
- [R3] Ierusalimschy, de Figueiredo, Celes. "The Implementation of Lua 5.0."
  J.UCS 11(7), 2005 (register VM, 32-bit instruction formats).
  https://www.lua.org/doc/jucs05.pdf
- [R4] Shi, Casey, Ertl, Gregg. "Virtual Machine Showdown: Stack Versus
  Registers." ACM TACO 4(4), 2008.
- [R5] Ertl, Gregg. "The Structure and Performance of Efficient Interpreters."
  JILP 5, 2003 (dispatch cost).
- [R6] Hölzle, Chambers, Ungar. "Optimizing Dynamically-Typed Object-Oriented
  Languages with Polymorphic Inline Caches." ECOOP 1991.
- [R7] Béra, Miranda. "A Bytecode Set for Adaptive Optimizations." IWST 2014
  (Pharo/Sista bytecode set; marked contexts).
- [R8] Stenman. *The BEAM Book* (scheduling, reductions, processes).
  https://blog.stenmans.org/theBeamBook/
- [R9] Erlang/OTP `erl_nif` documentation, "Dirty NIFs."
  https://www.erlang.org/doc/apps/erts/erl_nif.html
- [R10] Armstrong. *Making Reliable Distributed Systems in the Presence of
  Software Errors.* PhD thesis, KTH, 2003.
- [R11] Chase, Lev. "Dynamic Circular Work-Stealing Deque." SPAA 2005;
  implemented in `crossbeam-deque` (https://docs.rs/crossbeam-deque).
- [R12] Lerche. "Making the Tokio scheduler 10x faster." Tokio blog, 2019
  (LIFO slot, stealing, injector). https://tokio.rs/blog/2019-10-scheduler
- [R13] Varghese, Lauck. "Hashed and Hierarchical Timing Wheels." SOSP 1987.
- [R14] `mio`, Metal I/O. https://docs.rs/mio
- [R15] `libloading` (https://docs.rs/libloading), `libffi`
  (https://docs.rs/libffi).
- [R16] Pharo uFFI booklet. https://books.pharo.org/booklet-uffi/
- [R17] Clebsch, Franco, Drossopoulou, Yang, Wrigstad, Vitek. "Orca: GC and
  Type System Co-Design for Actor Languages." OOPSLA 2017.
- [R19] PEP 683, "Immortal Objects, Using a Fixed Refcount."
  https://peps.python.org/pep-0683/
- [R20] Choi, Shull, Torrellas. "Biased Reference Counting: Minimizing Atomic
  Operations in Garbage Collection." PACT 2018.
  https://dl.acm.org/doi/10.1145/3243176.3243195
- [R21] PEP 703, "Making the Global Interpreter Lock Optional in CPython."
  https://peps.python.org/pep-0703/
- [R22] Doligez, Leroy. "A Concurrent, Generational Garbage Collector for a
  Multithreaded Implementation of ML." POPL 1993.
- [R23] Sivaramakrishnan et al. "Retrofitting Parallelism onto OCaml."
  ICFP 2020.
- [R24] Burckhardt, Kothari, Musuvathi, Nagarakatte. "A Randomized Scheduler
  with Probabilistic Guarantees of Finding Bugs" (PCT). ASPLOS 2010.
- [R25] `shuttle`, randomized concurrency testing.
  https://github.com/awslabs/shuttle
- [R26] `loom`, permutation testing for concurrent Rust.
  https://github.com/tokio-rs/loom
- [R27] Rohou, Swamy, Seznec. "Branch Prediction and the Performance of
  Interpreters — Don't Trust Folklore." CGO 2015.
- [R28] Python 3.14 "What's New": tail-calling interpreter.
  https://docs.python.org/3.14/whatsnew/3.14.html
- [R29] Wasmtime Pulley interpreter.
  https://github.com/bytecodealliance/wasmtime/tree/main/pulley
- [R30] Rust RFC 3407, explicit tail calls.
  https://github.com/rust-lang/rfcs/pull/3407
- [R31] Brunthaler. "Inline Caching Meets Quickening." ECOOP 2010.
- [R32] PEP 659, "Specializing Adaptive Interpreter."
  https://peps.python.org/pep-0659/
- [R33] CPython C API, `PyType_Modified`: "Invalidate the internal lookup
  cache for the type and all of its subtypes."
  https://docs.python.org/3/c-api/type.html#c.PyType_Modified
- [R34] PEP 552, "Deterministic pycs" (hash-based `.pyc` validation).
  https://peps.python.org/pep-0552/
- [R18] Nystrom. *Crafting Interpreters*, part III (clox: bytecode,
  closures/upvalues). https://craftinginterpreters.com
