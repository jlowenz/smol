# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Project Overview

This project aims to build a powerful, dynamic, observable, and low ceremony software programming environment inspired by Smalltalk, Common Lisp, Haskell, moldable development, knowledge bases, and my personal experience developing software in a variety of languages and environments over 27 years. Given the necessary low-level control required for eventually implementing a virtual machine to host the environment, we propose to use Rust (tentatively) for development (also considered C++ (known but frustrating), Zig, Odin, OCaml). 

The project summary can be found in `docs/Project_Summary.md`.

The design principles and some early key decisions are in `docs/Design_Principles_and_Key_Decisions.md`.

The _initial_ high-level component architecture can be found in `docs/High-level_Components.md`.

## Development Record

### Step 1: triple store
The triple store PRD is found in `docs/triple_store_PRD.md`.

**Cross-cutting implementation decisions (recorded as established):**

- **Runtime-agnostic storage layer.** The triple store crate does not depend on a specific async runtime. Change-event publication uses `async-channel` (mpmc, unbounded, non-blocking `try_send`), so the store layer itself needs no executor. Downstream consumers (interpreter, UI, future custom VM) pick their own executor. This preserves the option of running under the future custom VM scheduler without a `tokio::Runtime` lurking in storage.
- **`unsafe` is forbidden in the storage layer** (enforced via workspace lints). Any future exception requires an explicit containment + testing argument.
- **Naming collision awareness.** This project's name `smol` overlaps with the `smol` async runtime ecosystem (and `async-channel` originates from it). When searching external Rust references, qualify as "smol-rs runtime" vs. this project. Using `async-channel` does *not* couple us to the smol runtime — it works under any executor.
- **Reproducible builds.** Toolchain pinned via `rust-toolchain.toml`. `Cargo.lock` is committed (this project will be a binary/runtime long-term). Cross-platform (macOS dev, Linux/Windows targets) is in scope; `.gitattributes` normalizes line endings to LF.

## Interpreter — kernel.smol test coverage

`kernel.smol` is loaded as an embedded string at runtime; llvm-cov instruments
Rust source only, so `just cov` numbers say nothing about Smol kernel code coverage.

### Interpreter and `may` coroutines

The current coroutine stack size is set to 128KB, which is a lot (comparatively, BEAM processes start with ~2KB). We should be aware of the Rust interpreter stack depth and try to minimize the stack overhead here, though future VM work may help ameliorate this issue. 

### Futures

Futures are in place for a reason to support the asynchronous-everywhere paradigm and REQUIRE very late resolution. Do NOT try to eagerly resolve futures as a solution to a problem - this is NOT the correct path. Instead, diagnose what part of the interpreter/evaluator made an incorrect assumption with respect to potential futures.

### Other Items

**Interim process (until tooling exists):** whenever kernel.smol is extended, do a
manual method-by-method audit — every method body should be exercised by at least
one Rust test in `eval.rs`, including both branches of every conditional. Ask
Claude Code to do the audit explicitly; it has done this successfully as of chunk 20.

**Planned tooling (recorded here so it survives session boundaries):**

1. *AST span tracking* — add `Span { start: usize, end: usize }` to `Expr` / `Stmt`
   nodes so the evaluator knows the byte offset of every expression it executes.
   A `CoverageCollector` (opt-in via evaluator flag) records touched spans;
   a post-run report maps them back to source lines of any `.smol` file.
   This directly supports the project's "observable" goal and is the right fix
   for the latent coverage gap. Estimated scope: one dedicated chunk.

2. *Smol testing framework (SUnit-style)* — a `TestCase` class with `assert:`,
   `assert:equals:`, `deny:`, `should:raise:`, `setUp` / `tearDown`, and a
   runner that discovers all `TestCase` subclasses and reports results.
   Once this exists, kernel tests can migrate from Rust into Smol, which
   exercises the language under its own weight and is the idiomatic Smalltalk
   approach. The span-based coverage collector makes it possible to measure
   coverage even when tests are written in Smol. Estimated scope: one chunk,
   after the actor model MVP (chunk 21) stabilises the language enough to
   write a non-trivial Smol program.

Neither item blocks ongoing chunk work; both should be scheduled before the
kernel grows significantly beyond its current size.

## Preferences and Rules

I will add to this list as the project proceeds, but may also ask you to add things here as well. If something seems like it should be a preference or rule, please suggest putting it here. 

- Very long thinking sessions must be avoided (i.e. greater than a couple minutes), I need more incremental feedback on the thought process to help avoid significant distractions and investigations that are off course. 
- Do your research and provide references. 
- Do not hallucinate unless otherwise asked for potentially creative solutions.
- Do not be obsequious. Instead, review and accept valid criticism, or form a careful argument for an opposing view or approach if necessary. However, if I make a specific decision, then that decision must be respected. 
- Do not assume you know what changes to make if unexpected build or test errors happen; instead, carefully diagnose the situation and present the diagnosis and options for the fix to the user. This is to prevent inadvertent "solutions" that are not correct and end up causing more problems. 
- After you make edits to a Rust file, make sure to run `cargo fmt` and `cargo lint` to ensure code remains free from obvious issues. 
- I prefer documentation that clearly states contracts; e.g. a function should clearly specify requirements and what it ensures. requirements and ensure should use code statement if possible, or very clear descriptions otherwise.
- Contracts (`Requires:` / `Ensures:`) belong on the **method or function they govern**, not on the enclosing struct, enum, trait, or module. Type-level docs describe *purpose and shape*; per-method docs describe *behavior contracts*. If a constraint is a class invariant maintained across multiple methods, document it at the type level; otherwise put it where it is checked or relied upon.
- Documentation for documentation's sake is not necessary, especially since we should be using clear naming.
- We should aim for domain-driven design principles, even though our domain is technically implementation details. i.e. consistent and ubiquitous language, clear bounded contexts, and documentation of these elements for sharing across multiple sessions. 
