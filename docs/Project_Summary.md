# Project Summary: A New Programming System

## Overview

This document summarizes ongoing design discussions for a new programming
language and environment intended to address fundamental pain points in
modern software development. The project is in early design/planning phase,
with implementation beginning soon.

## Core Motivation

- Slow compile times and compile-debug-fix-recompile cycles
- Software rigidity: systems are hard to change safely and quickly
- Software opacity: running systems are difficult to inspect and understand
- Long time-to-working-solution in typical development workflows

## Primary Inspirations

- **Smalltalk**: live image-based environment, object model, message passing
- **Common Lisp**: interactive development, REPL-driven workflow, metaprogramming
- **Haskell**: type system ideas, expressive abstraction
- **Erlang/BEAM**: lightweight process model, actor semantics, fault tolerance,
  per-process GC
- **Moldable Development** (Glamorous Toolkit): environment adapts to domain
- **"Out of the Tar Pit"** (Moseley & Marks): functional-relational state
- **E Language** (Mark Miller): object-capability model, sync-inside/async-outside
- **"Don't Write Dead Programs"** (Jack Rusher): living, inspectable systems

---

## Key Design Decisions

### 1. Fundamental Model: Objects with Twists

Core metaphor is objects — familiar and domain-natural — but departing from
classical Smalltalk in important ways:

- Isolated lightweight processes (actor semantics): each entity is a process
  with its own message queue
- Asynchronous messages by default
- Implicit futures: every async send returns a future; compiler manages
  dataflow propagation
- No shared mutable state between entities

### 2. Value / Entity Distinction

**Values**: immutable, identity-free, freely shareable, always synchronous.
Examples: integers, strings, addresses. Replaced wholesale, never mutated.

**Entities**: persistent identity, mutable state, temporal history,
asynchronous. Examples: BankAccount, Customer, Order, UI components.

### 3. Sync-Inside / Async-Outside

Conceptually everything is asynchronous. Synchronous execution is a runtime
optimization applied automatically when exclusive reachability is provable:

- Within a single entity's message handler: sequential and synchronous
- Between entities: asynchronous by default
- Owned objects (reachable from exactly one entity): synchronous, direct call
- Shared objects (reachable from multiple entities): async, via message queue

Ownership is determined automatically via escape analysis and runtime
reachability — not by programmer annotation. The live environment surfaces
ownership/sharing status visually.

### 4. State Management: Bitemporal Triple Store

Each entity maintains state as facts in a per-process/cluster triple store:

- **Bitemporal**: valid time (world truth) and transaction time (recorded)
- **Two logical layers**:
  - Internal facts: owned by entity, not externally writable
  - External assertions: open-world, with full provenance annotation
- **Declared access**: private / shared / public
- **Cross-object queries** supported across the store
- **Provenance-preserved conflict resolution**: conflicting assertions kept
  with asserter identity and timestamp

The store serves triple duty: domain state, transaction rollback, and live
environment instrumentation.

### 5. Concurrency: Parameterized Transactions
transactional: #atomic      -- synchronous rendezvous, strongest guarantee
transactional: #saga        -- eventual consistency, compensating messages
transactional: #stm         -- optimistic, retry on conflict
transactional: #bestEffort  -- fire and forget with notification

STM-like optimistic concurrency without shared memory: per-triple version
numbers enable optimistic locking via message-based version negotiation.

### 6. UI / Model Synchronization

- UI subscribes to store change streams, not directly to entities
- Objects remain UI-unaware
- UI sends intent/update messages directly to entities
- Setters (`address: newAddress`) are legitimate for entities — history is
  preserved automatically, subscribers notified, zero ceremony

### 7. Gradual / Optional Typing

- Dynamically typed by default
- Optional annotations where intent communication matters
- Safety through architecture, not type system: actor isolation contains errors
- Blame tracking: runtime type violations attributed to responsible call site
- Static effect verification is not a goal — dynamic feedback preferred

### 8. Functions

First-class, modeled as singleton objects of a built-in Function class:

- Functions are values: immutable, freely shareable, synchronous
- Capability-bounded: effects visible through captured entity references
- Purely computational when closing over only values
- Subject to same escape analysis as other objects

### 9. Syntax

Smalltalk-inspired, modernized:
- More natural infix/binary operators
- Cleaner block/closure syntax
- String interpolation
- Module/namespace support built in
- Type annotation syntax: TBD (own deep dive needed)

### 10. Development Environment

- Triple store as source of truth: code is facts with provenance and history
- Auto-export to files: continuous sync, git-compatible
- Live environment as a view onto the store
- Notebook/literate programming support: documentation is first-class
- Visual ownership/sharing status
- Runtime type observations as provenance-annotated store assertions

---

## Open Issues

### Fundamental
- No coherent single "story" equivalent to "objects all the way down"
- Gradual typing implementation details (deferred pending more design work)
- Runtime type-checking performance at annotated boundaries

### Technical
- STM-like optimistic concurrency: needs deeper investigation
- Nested transaction composition: not fully designed
- Store subscription flood control for high-frequency entities
- Initial/pending UI state before store has facts
- Implicit future propagation: compile-time management complexity

### Design Gaps
- Error handling model and resumption
- Entity lifecycle (creation, destruction)
- Entity discovery and naming
- Store schema evolution in a live system
- Security and capability model detail
- Package/module/namespace design

---

## Implementation Direction

**Language**: Rust (tentatively) — memory safety, ownership model alignment,
cargo tooling, growing VM/runtime community. Strategic `unsafe` accepted for
memory subsystem.

**Phased approach**:
1. Triple store (self-contained, good first Rust project)
2. Bootstrap tree-walking/bytecode interpreter
3. Async process model and message scheduler
4. Live environment tooling
5. Bytecode compiler and VM
6. Incremental JIT for hot paths

**VM/Runtime**: Custom, informed by BEAM — per-process GC (no stop-the-world),
lightweight process scheduler, message copying between heaps.