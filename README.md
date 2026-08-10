# Smol

*A live, observable, low-ceremony programming system — a language and its
environment — built around asynchronous objects with history.*

Smol takes ideas from Smalltalk, Common Lisp, Erlang, and moldable/relational
development seriously and asks what a programming *system* could be if they were
combined from the ground up. It is in **early development**: a bootstrap
tree-walking interpreter written in Rust, on the way to a custom VM.

> **Status: pre-alpha.** The language and runtime run real programs — isolated
> actors, implicit futures, a Common-Lisp-style condition system, traits, a
> numeric tower, and multi-entity transactions. The live/observable environment
> and the custom VM are still ahead. Design is recorded in `docs/` and evolves as
> rough edges are found.

## What makes it different

- **Asynchronous by default, synchronous by optimization.** Every message send is
  conceptually async and yields an implicit future; the runtime runs
  synchronously when it can prove exclusive reachability. Concurrency is
  structural, not a library you opt into.
- **Values vs. Entities.** Two kinds of objects: *values* are immutable,
  identity-free, and freely shareable; *entities* are isolated actors with durable
  identity and mutable state. There is no shared mutable state between entities.
- **State has history.** Entity state lives in a bitemporal triple store, so
  querying the past is as natural as the present — history, rollback, and live
  instrumentation all fall out of one mechanism.
- **Opacity is the enemy.** A running system should be inspectable at every level:
  state, message flow, temporal history. (The observable environment is the north
  star, not yet built.)
- **Minimal ceremony.** Correct programs shouldn't need boilerplate; where Smol
  departs from tradition, it is to remove ceremony or better serve the model.
- **Parameterized concurrency.** Transaction guarantees are chosen by name (e.g.
  `#atomic`, `#stm`) rather than hand-coded coordination protocols.
- **Safety through architecture, not a type gate.** Isolation, message passing,
  and temporal history do the work; typing is dynamic, with optional annotations
  planned as a way to communicate intent — not a mandatory verification step.

## Relationship to Smalltalk

Smol borrows Smalltalk's surface grammar — unary/binary/keyword message sends,
blocks, cascades, `^` return — because a uniform "everything is a message send"
notation fits the model. It is **not** a Smalltalk: sends are asynchronous,
objects split into values and entities, state lives in a triple store, and
execution targets isolated multicore actors. Familiarity is an asset; fidelity to
Smalltalk is not a goal. Concrete divergences already include 0-based indexing and
a strict rule that a block may assign only its own parameters and locals.

## Design principles

Liveness first · conceptual simplicity · opacity is the enemy · concurrency is
structural · state has history · safety through architecture · minimal ceremony ·
honest tradeoffs · syntax serves the model.

Full statements, the decision log, and rationale are in
[`docs/Design_Principles_and_Key_Decisions.md`](docs/Design_Principles_and_Key_Decisions.md)
and [`docs/Project_Summary.md`](docs/Project_Summary.md).

## Repository layout

- `crates/triple-store/` — the bitemporal fact store; the state substrate, kept
  runtime-agnostic.
- `crates/interpreter/` — the bootstrap evaluator, the `kernel.smol` standard
  library, and the `smol` REPL binary.
- `docs/` — design docs, the decision log, and per-chunk development records.

## Building

```sh
just build              # build the workspace
just test               # run the test suite
cargo run -p interpreter # start the `smol` REPL
```

Uses the toolchain pinned in `rust-toolchain.toml`. `unsafe` is forbidden in the
current codebase (enforced by workspace lints); any future exception is confined
to the memory subsystem with an explicit containment argument.

## A note on the name

This project is the `smol` language. It is unrelated to the `smol` async-runtime crate ecosystem — when searching, qualify that as "smol-rs". Smol depends on `async-channel` (which originates there) but is not coupled to that runtime.
