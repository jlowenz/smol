# Smol Glossary

The ubiquitous language of the project: terms as Smol uses them, with their status.
Where a word has a different meaning elsewhere (another language, a paper, our own
older docs), the entry says so.

Entry shape:

- **Definition**: one or two sentences.
- *Status*: **decided** (use it this way), **working** (current best wording, may
  change), **open** (named, not yet defined).
- *Not to be confused with*: neighbouring terms, or the same word used elsewhere.
- *Source*: Smol doc, or prior art.

Related work (how Smol differs from prior art, and why) belongs in its own
discussion; entries here only cite sources.

---

## Methods and messages

### Answer

- **Definition**: the value a method gives as its reply to the message that invoked
  it. A method states its answer with `^expr` as its last statement; a method
  without `^` answers `self`. The sender receives the answer as the value of its
  send, as a Future when the receiver is another entity.
- *Status*: working (DRAFT for the user to edit). Decided parts:
  - `^` may appear only as a method's last statement and never in a block (Q24,
    2026-11-10);
  - a method without `^` answers `self` (C2, `chunk_21_plan.md`).
- *Not to be confused with*:
  - *Return.* In Smalltalk, `^` is a return, and from a block it is a non-local
    return that exits the home method. Smol has no non-local return (Q24, I5).
  - *The VM's `Return` instructions.* These are frame mechanics in the VM's own
    context: pop the frame, settle the reply. They are not a language term.
  - *Finishing.* In actor systems, answering and finishing a turn can differ
    (Erlang's `gen_server:reply/2` answers and keeps running). In Smol today, a
    method answers when it finishes.
- *Source*: Smalltalk convention (Squeak/Pharo method comments read
  "Answer the …"); Q24 in `docs/open_questions.md`.

---

## Contexts and causality

Working material: `docs/20261031 Thoughts on concurrent theory and contexts.md`;
open question Q23 in `docs/open_questions.md`.

### Scope

- **Definition**: the region in which a *name* refers to a given binding. Lexical
  (file, function, block), namespace and class scopes are scopes, not contexts:
  they are bounded in program text, not by events.
- *Status*: working
- *Not to be confused with*: context. Q22 used "scope" for activation, causal and
  ownership extents; in the new usage those are contexts, not scopes.
- *Source*: Wikipedia, "Scope (computer science)".

### Context

- **Definition**: the situation within which something exists or happens, and that
  can help explain it: supporting information for an event or activity, which may
  affect its behavior. A context is bounded with respect to the events of a system:
  at any time, some activities are inside a given context and others are outside.
  Every context has an originating event from which its bounds are determined.
- *Status*: working
- *Not to be confused with*: scope (names). Existing uses of the word that need
  reconciling with this definition: `HandlerContext`, the execution-context Value
  (Q22), `Context let:in:` (Q22), and the transaction context captured by the
  evaluator's `fork_context`.
- *Source*: Cambridge Dictionary, "context".

### Terminating context / non-terminating context

- **Definition**: a terminating context was active over a bounded time interval;
  a non-terminating context started at some point but has no explicit end. What
  counts as a context's termination differs by kind of context (e.g. graph
  termination vs activity termination, below).
- *Status*: working

### Kinds of context

- *Status*: open. Candidates: lifetime (entity, object, thread, resource?); program; ownership; transaction;
  condition handler; dynamic constant; resource (separate from lifetime?); causal.
  Many may be flavors of others.

### Event

- *Definition*: a meaningful, logical action that happens at some point in time (instant). For example, a message send is initiated at some instant, and eventually triggers a turn for the receiving entity. Other examples: a timer interrupt, an input event, a network connection. 
- *Status*: open.
- *Not to be confused with*: an activity. An activity is caused by some event, and may itself produce additional events, but occurs over an interval of time.

### Activity

- *Definition*: a related chunk of computation over a period of time (interval), triggered by an event. The activity is _caused_ by the event, but the event instant is not necessarily the lower bound of the activity interval (there can be other activities in between); but the event "happens before" the activity. 
- *Status*: open.

### Turn

- **Definition**: an entity's processing of a single message. A turn is a kind of
  activity; the message send that causes it is an event.
- *Status*: working
- *Not to be confused with*: E's turn. An E turn never blocks; a Smol turn can
  park while it forces a future (see Q23 discussion).
- *Source*: Miller, Tribble & Shapiro, "Concurrency Among Strangers" (TGC 2005).

### Send (causal)

- **Definition**: every send is causal: it causes the work done in response. Only
  a send to another (non-owned?) entity causes a *new turn* and so is an edge of
  the causal graph; a self-send or a send to a Value causes work within the
  current turn.
- *Status*: working

### Causal graph

- **Definition**: the graph whose nodes are turns and whose edges are the events
  (such as message sends) by which one turn causes another. Contexts propagate
  along its edges. A turn is *caused* by a send or a registration; it may be
  *triggered* by something else (a future settling, a timer expiring, input
  arriving). For a plain send, cause and trigger coincide.
- *Status*: working
- *Not to be confused with*: dependency (see below), which does not propagate
  context.

### Dependency (settlement)

- **Definition**: a turn that awaits a future depends on the turn that settles
  it, but is not caused by it. If A passes C a future F obtained from B and C
  awaits F, C's turn is caused by A's send and depends on B's turn. Settlement
  carries a value, not context.
- *Status*: working
- *Not to be confused with*: causation. Dependency matters for liveness
  (deadlock, wait-for); causation for context propagation. Lamport's
  happens-before (CACM 1978) includes both.

### Root turn

- **Definition**: the one turn not caused by another; every other turn is caused,
  transitively, by it. Likely an example of a non-terminating context.
- *Status*: working. External input (a socket, the UI) does not add roots: it is
  handled because some turn registered for it, so it continues that turn's chain.

### Causal chain

- **Definition**: a path in the causal graph.
- *Status*: working. Hypothesis (user): causal chains do not intersect, which is
  why contexts are bounded, and why a causal chain is a reasonable surrogate for
  a call stack.

### Edges of the causal graph

- **Message send**: A sends message M to B; B eventually runs a turn, caused by
  A's message. *Status*: working.
- **Spawn**: not a distinct causal edge. Creating entity B is a birth plus a
  message send (`new` sends `initialize`, `kernel.smol:463`); causally it is the
  send. The birth matters to lifetime and ownership contexts, not to the causal
  graph. *Status*: working.
- **Registration**: A sends a message to B with a block C that may run later
  (e.g. a Timer, `then:orElse:`). If C runs, A caused it, so C continues A's
  causal chain. C runs as its own short-lived *continuation entity*:
  registration is birth plus a send delivered when the trigger fires. *Status*:
  decided (Q23 decision B2, 2026-11-03).
- **Reply**: not a causal edge. Settlement of a future is a dependency (above);
  a continuation is caused by its registration. (Q22's framing listed reply as an
  edge; withdrawn.) *Status*: working.
- Completeness of the edge list: working. With spawn and registration both
  reducing to sends, the causal graph has one kind of edge, the send.

### Leaf turn

- **Definition**: a turn that causes no other turn: it sends no message to
  another non-owned entity and makes no registration. Leaf turns are what let a
  causal graph terminate.
- *Status*: working (depends on *ownership*, not yet defined).

### Graph termination / activity termination

- **Definition**: a causal graph (or chain) terminates when no further edges can
  be created: every frontier turn is a leaf. Its activity terminates when, in
  addition, every turn in it has finished. A deadlocked or looping turn leaves the
  graph terminated but the activity not.
- *Status*: working
- *Source*: Dijkstra & Scholten, "Termination Detection for Diffusing
  Computations" (IPL 11(1), 1980): a diffusing computation is a rooted causal
  tree of this kind, with a detection algorithm.

### Extent

- *Status*: open. Q22 named activation, causal and ownership extents; to be
  restated as contexts.

### Provenance / brand

- **Definition** (working): a brand is a structureless tag on a value recording the turn or process that produced it. Provenance flows with data; context flows
  with work.
- *Status*: working; not sure Smol currently needs this atm. 
- *Source*: Gabriel, "The Design of Parallel Programming Languages" (1991).

### Entity

- **Definition**: Smol's one concept for independent, concurrent behavior: an
  instance of a non-Value class, with its own state and mailbox, processing one
  message at a time (turns are exclusive).
- *Status*: decided (D3, D13; Q23 decision A)
- *Not to be confused with*: Value (immutable, shareable, no turns).

### Process

- **Definition**: the VM's runtime representation of an entity (`vm_compiler_plan.md`
  §5). An implementation term, not a second language concept.
- *Status*: working
- *Not to be confused with*: Gabriel's "process" (1991), a unit of parallel
  execution; a thread, which here means only a scheduler/OS thread.

### Continuation entity

- **Definition**: the short-lived entity that runs a registered block (a
  `then:orElse:` arm, a timer callback) when its trigger fires, in the context of
  the registering turn.
- *Status*: decided (Q23 decision B2, 2026-11-03)

### Ownership

- *Definition*: A kind of context defined by one entity that is _responsible_ for the work/turns taken in its causal subgraph, even when those turns occur in other entities. This is a special context in Smol, since entities are conceptually independent and generally only responsible for their own turn behavior (send events and ivar manipulation); i.e. they cannot otherwise "own" or manipulate behavior in other entities. Only `Transaction`s own work today, and rely on the system to produce correct behavior related to the desired ACID properties. 
- *Status*: open. 

### Entity lifetime

- *Definition*: An entity is an instance of a non-Value class, and its lifetime always begins when the entity is created. An entity lifetime ends "naturally" when there are no reachable references (an entity will be garbage collected); an entity lifetime may also end "prematurely" or deterministically with an EntityDeath condition. 
- *Status*: open. It seems like an entity needs a way to stop processing if some condition occurs such that it cannot safely continue - however, I'm unsure about how it should affect anyone that has a reference to the dead entity. and - if an entity cannot "die" then how does it stop doing work or otherwise stop processing/responding? 
