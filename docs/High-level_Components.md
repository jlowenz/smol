# High-Level Components

This document describes the major components of the programming system.
It is intended to be iterated on as design stabilizes.

---

## 1. Language Core

### 1.1 Syntax and Parser
- Smalltalk-inspired, modernized surface syntax
- Keyword messages, binary operators, unary messages
- Block/closure literals
- Value and entity literals
- Optional type annotation syntax (TBD)
- Module/namespace declarations
- Outputs: AST

### 1.2 AST and Semantic Analysis
- Abstract syntax tree representation
- Name resolution and scoping
- Escape analysis (determines owned vs shared)
- Type annotation propagation and blame label insertion
- Macro/metaprogramming expansion (if supported)

### 1.3 Bytecode Compiler
- AST → bytecode translation
- Instruction set design (message send, async send, future await,
  store read/write, process spawn, etc.)
- Optimization passes (inline caching hints, constant folding)
- Debug information embedding

---

## 2. Runtime System

### 2.1 Object / Value Representation
- Tagged pointer or NaN-boxing scheme
- Value types: integers, floats, booleans, symbols, strings, collections
- Entity handles: opaque references to process mailboxes
- Immutable value structural sharing (persistent data structures)

### 2.2 Process / Entity Model
- Lightweight process per entity
- Per-process message queue (mailbox)
- Process scheduler (cooperative or preemptive)
- Process lifecycle: spawn, suspend, resume, terminate
- Process registry / naming / discovery

### 2.3 Message Dispatch
- Synchronous dispatch (owned objects, within-process)
- Asynchronous dispatch (cross-process, via mailbox)
- Polymorphic inline caches (PICs) for hot message sends
- Method lookup table per class/entity type

### 2.4 Implicit Future System
- Future representation (pending / resolved / error)
- Dataflow propagation: passing a future where a value is expected
  automatically inserts await at latest safe point
- Scheduler integration: suspend process waiting on future,
  resume on resolution

### 2.5 Memory Management / GC
- Per-process heap (no stop-the-world at system level)
- Generational copying GC within each process heap
- Value heap: shared, reference-counted or immutable region
- Inter-process message passing: copy semantics (owned objects copied
  on send, values structurally shared)
- Reachability tracking feeds ownership/sharing determination

### 2.6 Synchronization / Transaction Coordinator
- Transaction protocol dispatch based on declared semantics
  (#atomic, #saga, #stm, #bestEffort)
- STM-like version tracking via per-triple version numbers
- Saga compensating message management
- Deadlock detection / timeout for #atomic transactions

---

## 3. Triple Store

### 3.1 Core Store Engine
- Subject / Predicate / Object triple representation
- Bitemporal indexing: valid time + transaction time
- Per-entity logical partition (internal facts)
- Global partition (external assertions with provenance)

### 3.2 Query Engine
- Pattern matching over triples (subject, predicate, object, time)
- Cross-entity queries across partitions
- Temporal queries: state-at-time, history-of-entity
- Provenance-filtered queries: assertions by specific asserter

### 3.3 Access Control
- Enforcement of private / shared / public declarations
- Capability-based assertion authorization
- Boundary enforcement: internal facts not externally writable

### 3.4 Change Stream / Subscription
- Change event emission on fact insert/update
- Subscriber registration by query pattern
- Rate limiting / debounce at store level
- Delivery to subscribed entity mailboxes (async)

### 3.5 Transaction Log
- Tentative fact recording during transaction
- Commit: promote tentative facts to permanent
- Rollback: discard tentative facts (never written permanently)
- Timestamp management for transaction epochs

---

## 4. Live Environment

### 4.1 Object / Process Inspector
- Real-time view of entity state (current store facts)
- Message queue inspection
- Process lifecycle status
- Temporal history browser (query store at past timestamps)

### 4.2 Code Browser
- Navigate entity definitions, message implementations
- Live modification of running code (hot-swap)
- Diff view for changes
- Provenance: who changed what, when

### 4.3 Message Flow Visualizer
- Real-time visualization of inter-entity message flows
- Async/sync determination display per message send
- Future resolution tracking

### 4.4 Notebook / Documentation Environment
- Literate programming support: code and documentation unified
- Executable examples embedded in documentation
- Exportable to external artifacts (HTML, PDF, markdown)
- Stored as facts in the triple store (same provenance model)

### 4.5 Living Test System
- Tests as first-class entities in the store
- Dependency graph: which tests cover which code
- On code change: run only affected tests
- Continuous test execution in background
- Results surfaced in live environment

### 4.6 Runtime Type Observer
- Captures type information from actual message flows
- Stored as provenance-annotated external assertions
- Surfaces inferred type information in code browser
- Flags potential blame violations before they occur at runtime

---

## 5. Source and Package Management

### 5.1 Auto-Export System
- Continuous serialization of store facts to text files
- Git-compatible output format
- Conflict resolution support (text diff + store merge)
- Module/namespace → directory structure mapping

### 5.2 Package System
- Package declaration and dependency management
- Versioning and compatibility
- Package store: reusable entity/value definitions
- Import/namespace resolution

---

## 6. Standard Library (Minimum Viable)

### 6.1 Value Types
- Numeric tower (integers, rationals, floats)
- String and symbol
- Collections: Array, OrderedCollection, Dictionary, Set
  (immutable value variants of each)
- Optional / Result types

### 6.2 Entity Patterns
- Registry / naming service entity
- Supervisor entity (fault tolerance)
- Timer / scheduler entity
- Logger entity

### 6.3 IO Entities
- File system entity
- Network socket entity
- Standard input/output

---

## Component Dependencies
```
Parser → AST → Semantic Analysis → Bytecode Compiler
↓
Bytecode Interpreter
↙     ↘
Object/Value Rep    Process Model
↓                  ↓
Memory/GC          Message Dispatch
↓
Future System
↓
Triple Store
↙         ↘
Query Engine    Change Streams
↓
Live Environment
↙           ↘
Code Browser    Test System
```
## Build Order (Suggested)

1. Triple store (standalone, testable immediately)
2. Object/value representation
3. Parser + AST
4. Bytecode compiler (simple subset)
5. Interpreter loop + message dispatch (sync only first)
6. Process model + async message queue
7. Future system
8. Memory/GC (replace bootstrap allocator)
9. Transaction coordinator
10. Live environment (incremental)
11. Standard library
12. Auto-export / package system