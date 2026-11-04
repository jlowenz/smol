# Related Work

How Smol relates to prior work, and why it chose differently where it did.
Organized by **decision**, not by paper: each section states Smol's position, the
prior work it builds on or departs from, and the reason. Terms are as defined in
`docs/glossary.md`.

Section shape:

- **Smol's position** and its status: **decided**, **working**, or **open**
  (with a pointer to the open question or issue).
- **Prior work**: what others do, with references.
- **Difference and why**: where Smol departs, and the argument for it.

Related records elsewhere: `Design_Principles_and_Key_Decisions.md` (principles P1–P10,
decisions D1–D13; D12 covers the relationship to Smalltalk), `open_questions.md`,
`condition_system_guide.md`, `tx_design.md`.

---

## R1: Turns, and blocking inside a turn

**Smol's position** (working): computation proceeds in *turns*. A turn is one
entity's processing of one message; the entity processes no other message until
the turn ends. Unlike E, a Smol turn can park while it forces a future
(`syncValue`, `ensure:`, `on:do:`, and today `new`).

**Prior work**: E (Miller, Tribble & Shapiro, "Concurrency Among Strangers", TGC
2005). An E turn runs to completion and never blocks; results are consumed only by
registering a `when` reaction, which runs as a later turn. This rules out deadlock
between vats by construction, at the cost of "datalock" (a reaction that never
fires) and of writing all dependent code as reactions.

**Difference and why**: open. Blocking inside a turn is what lets Smol code read
sequentially (P7, D2), but it reintroduces deadlock (issue I2). Whether the
forcing points stay, become detectable, or change is tracked in I2 and Q23.

## R2: Async sends are not "go statements"

**Smol's position** (working): every send to another entity is asynchronous and
returns a future. Work started by a send may outlive the sender's turn by design.

**Prior work**: structured concurrency. Smith, "Notes on structured concurrency,
or: Go statement considered harmful" (2018) argues that unscoped task spawning
breaks the abstraction of a function call (control returns before the work ends;
errors have nowhere to go; resources cannot be cleaned up), and proposes nurseries
that bound every task's lifetime to a lexical block. Java's `StructuredTaskScope`
and Kotlin coroutine scopes follow the same idea.

**Difference and why** (user, Q22 2026-10-29..31): Smith's argument starts from a
sequential, thread-oriented language and adds concurrency to it; actors are not
considered. In Smol concurrency is structural (P4), not added to a sequential
base. A send is not a go statement for two reasons: it triggers work on a
sovereign entity with its own lifetime, and every send returns a future, so there
is always a flow from the turn back to the sender, which the sender may use or
ignore. Smith's ideas may still apply to resource management (open).

## R3: The causal graph in place of the call stack

**Smol's position** (working): the structure Smol always has is the *causal graph*
of turns (edges: message send and registration). Contexts propagate along its
edges. A causal chain stands in for the call stack of a sequential language; the
call stack is the special case of a single chain. Settlement of a future is a
*dependency*, not a cause, and carries no context.

**Prior work**:
- Lamport, "Time, Clocks, and the Ordering of Events in a Distributed System"
  (CACM 21(7), 1978): *happens-before* combines process order and message order.
  Smol's causal relation is narrower: it excludes dependency.
- Stanley, Close & Miller, "Causeway: A message-oriented distributed debugger"
  (HP Labs, 2009): presents a program's history as message order and process
  order instead of a stack.
- Dijkstra & Scholten, "Termination Detection for Diffusing Computations" (IPL
  11(1), 1980): a *diffusing computation* is a rooted tree of activity spread by
  messages, much like a Smol causal graph with its single root turn, together with
  an algorithm to detect that it has ended.

**Difference and why**: to write.

## R4: Dynamic context propagation

**Smol's position** (open, Q22 and Q23): handler contexts, dynamic constants and
the transaction tag are contexts that flow along causal edges. A continuation's
context comes from its registration, not from the turn that settles the future
(glossary: Dependency). Which edges each context crosses, and where it stops, is
not settled.

**Prior work** (none derives propagation from a general rule):
- Common Lisp special variables: dynamic extent on one stack; closures do not
  capture bindings, and SBCL threads do not inherit them.
- Racket `parameterize`: new threads inherit the current parameterization at
  creation.
- Java `ScopedValue` (JEP 506): rebinding only in a nested extent; inherited only
  by tasks forked in a `StructuredTaskScope`.
- Clojure dynamic Vars (`binding`), with explicit conveyance (`bound-fn`).
- TC39 AsyncContext proposal: chooses per host API whether a callback sees the
  registration-time or dispatch-time context.
- Orleans `RequestContext` and OpenTelemetry context propagation: ambient data
  carried on calls between processes or actors.

**Difference and why**: to write once Q23 settles. The registration rule above
already answers the question TC39 leaves to each API.

## R5: Context and provenance

**Smol's position** (working): context flows with *work* along causal edges;
provenance flows with *data* from the turn that produced it. They are duals.

**Prior work**: Gabriel, "The Design of Parallel Programming Languages" (1991):
*brands*, structureless tags on objects recording the producing process, usable
for dispatch. Information flow control (Myers & Liskov, "A Decentralized Model for
Information Flow Control", SOSP 1997) is the security form; function colors and
taint tracking are special cases.

**Difference and why**: to write. Open costs for Smol: whether brands take part in
Value equality, the per-value tag (P10), overlap with fact provenance in the store
(D5).

## R6: Ownership and supervision

**Smol's position** (open, Q23): an actor cannot own a request to another actor,
yet Smol needs transactions and condition handling without a call stack. Only
`Transaction` (a coordinator) owns work today. Parent/child relationships,
reparenting, and short- vs long-lived entities are open.

**Prior work**: Erlang/OTP supervision trees (Armstrong, *Making reliable
distributed systems in the presence of software errors*, PhD thesis, KTH 2003);
Garnock-Jones, *Conversational Concurrency* (PhD thesis, Northeastern 2017):
facets as structured scopes inside actors; structured concurrency (R2).

**Difference and why**: to write.

## R7: Conditions without a call stack

**Smol's position** (decided in part; `condition_system_guide.md`): CL-style
handler-bind and restarts, Smalltalk-flavored (`resume:`/`return:`/`retry`).
Handlers run at the signal point on the signaling coroutine; the handler chain is
inherited across sends (C28b).

**Prior work**: Common Lisp condition system (Pitman, "Condition Handling in the
Lisp Language Family", 2001); Smalltalk exceptions (ANSI Smalltalk).

**Difference and why**: to write.

## R8: Aspects and context-dependent behavior

**Smol's position** (working, Q22 2026-10-27): the criterion is explicit vs
implicit variation points. Tier 0 (explicit hooks via the execution-context Value)
is decided; behavior change at unmarked points is not designed until a concrete
case needs it. Whatever is chosen must be inspectable from the code it applies
to (P3).

**Prior work**: CLOS method combination (`:before`/`:after`/`:around`);
context-oriented programming (Costanza & Hirschfeld, "Language Constructs for
Context-oriented Programming", DLS 2005); AspectJ (Kiczales et al., "An Overview
of AspectJ", ECOOP 2001).

**Difference and why**: to write.

## R9: Asynchronous everywhere

**Smol's position** (decided, D2, P4): asynchronous by default, synchronous by
optimization.

**Prior work**: to collect (actor languages, E, Erlang, async/await languages).

**Difference and why**: to write. This is where Smol has to justify the apparent
added complexity of async everywhere; `Smol_Patterns.md` was a start and may be
out of date. Deferred (user, 2026-11-01).
