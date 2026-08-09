# Env-frame redesign — slot resolution + snapshot capture

**Status:** design, written 2026-08-08. Opened from `next_steps_after_c34.md` §4.
The verify step is complete (§1); the semantic decision (**single-assignment
locals** + snapshot capture) is taken (§2), grounded in the audit (§2.3). **Not yet
implemented.** This doc specifies the target design, the interaction points that
must be preserved, and a phased plan where each phase is independently landable
and re-profiled.

Reproducible evidence lives in two committed examples:
`crates/interpreter/examples/alloc_probe.rs` (per-send timing + samply soak) and
`crates/interpreter/examples/capture_audit.rs` (single-assignment + capture audit).

---

## 1. Problem (measured, not assumed)

Per message send costs **~1.2 µs** — ~10× a typical tree-walker. A coroutine-free
scalar soak profiled with samply (19.5k samples) decomposes the cost as:

| Cause | Self-time |
|---|---|
| alloc / free (`nanov2_malloc`, `_free`, `_malloc_zone_malloc`) | 34% |
| hashing (SipHash on `Arc<str>` keys) | 10% |
| clone / drop / vec-grow | 9% |
| mutex lock/unlock | 7% |
| memset / memcmp / memmove | 4% |
| **interpreter logic proper** | **35%** |

**~65% is non-logic overhead**, and it traces to one structure. Every method or
block activation allocates a fresh `Arc<Mutex<HashMap<Arc<str>, Value>>>` frame
(`env.rs` `push_frame`) and hashes `self` + each parameter name into it; and
every `Cont` variant carries an `Env` (= `Vec<Arc<Frame>>`) that is cloned on
each push (`eval_iterative.rs`, all `Cont` variants). That one design explains
alloc + hashing + mutex + memset (~55%) directly, plus much of the clone bucket.

Precedent for the payoff: C34 S1a was a single structural fix in this area and
gave ~12× on dispatch-bound benchmarks.

---

## 2. The two semantic commitments (foundation)

The performance design rests on one language-level commitment. It is stated first
because everything downstream depends on it.

### 2.1 Single-assignment locals (SSA)

> A local is bound once and never reassigned. `:=` to an already-bound local is a
> compile error, as is `:=` to an undeclared name (no implicit locals).

This is the decision, taken 2026-08-09, and it **subsumes** two earlier framings:

- It *includes* the chunk-26 strict block-assignment rule (a block can't assign a
  home temp) as a special case — a block can't reassign *anything* it didn't bind.
- It makes the once-considered **freeze-on-capture** rule **vacuous**: "may not
  reassign a local after a block captures it" is trivially satisfied when a local
  is never reassigned at all. There is no separate capture rule to state, teach,
  or test.

**No expressiveness is lost.** Smol already confines reassignment to straight-line
method code (the strict rule bars it in every block/branch/loop). Any straight-line
`x := e` is expressible as a shadowing `let`-style rebinding `x := e` whose RHS
still sees the prior binding — and Smol locals have **no observable identity** (you
cannot alias a local, take its address, or compare slots), so "reuse the slot" vs
"new slot" is unobservable. A mutable local would therefore be pure sugar for
slot-reuse, never a capability. `let mut`/`var` is deliberately **not** provided.

The principle, stated once: *a local names a value, not a cell.* This is the
Value-immutability philosophy pushed down to locals, and it hands the future VM an
SSA IR for free.

### 2.2 Snapshot capture

> A block captures the *values* of the lexical variables it reads, copied at
> block-creation time — not a live reference to the defining environment.

Because locals are immutable (§2.1), snapshot and a live reference are
**observationally identical** — a captured value can never change, so there is no
program that can tell a copy from a shared cell. Snapshot is therefore purely a
**lifetime** decision, not a semantic one: the home frame is a LIFO stack frame
freed when its activation returns, while a block can outlive it (returned, stored,
sent cross-entity). Copying the read-set makes the block **self-contained and
`Send` by construction**, which is what lets frames become thread-local flat slot
arrays with no lock and no refcounting.

This mirrors Rust's `move` closures for crossing thread boundaries; Smol blocks
cross entity/`may`-coroutine (real OS-thread) boundaries, and `ValueHolder` remains
the sanctioned mechanism for genuinely shared mutable state.

### 2.3 Evidence (audited)

`capture_audit.rs` parsed all of `kernel.smol` with the real parser:

- 391 methods, 711 scopes.
- **0 straight-line reassignments** — every local in the entire kernel is already
  written exactly once. The stdlib already lives in the SSA world §2.1 mandates.
- **0 capture-after-reassignment cases** (the freeze rule would have had nothing to
  do anyway). The 29 captured-and-assigned locals are all the accumulator idiom —
  `result := …` bound once, then `self do: [:e | result add: …]`.

Caveat: this is the kernel's style, not proof SSA is painless for *all* future
code (one test snippet reassigns a block-local). But since shadowing covers every
legal reassignment position losslessly (above), the constraint is ergonomic, not
expressive.

**Syntax is unchanged for now:** `| … |` continues to declare locals, now with
single-assignment semantics. Replacing `| … |` with an inline `let` binding form
is a separate, deferred ergonomic improvement (§9) and is not required by this
redesign.

---

## 3. Ubiquitous language (bounded context: variable resolution)

- **Scope** — a method body or a block body. Introduces names: `self` (methods
  only), params, and locals (`| … |`, including mid-body `LocalDecl`s).
- **Slot** — an integer index into a flat frame. Assigned at analysis time.
- **Frame** — the runtime storage for one activation's slots: a flat `Vec<Value>`
  (or boxed slice), thread-local, no lock, no hash.
- **Capture set / read-set** — the ordered list of enclosing-scope lexical
  variables a block reads. Its snapshot is filled at block creation.
- **Snapshot** — the block's owned copy of its capture set's values.
- **Dynamic name** — a `Var` reference that is *not* a lexical variable of any
  enclosing scope: an instance variable, class variable, or global. Resolved at
  runtime through the receiver / class / namespace, exactly as today.

A `Var` reference is classified, once, at analysis time, into exactly one of:

- `LocalSlot(i)` — a param/local of the current scope → `frame[i]`.
- `CapturedSlot(i)` — a lexical variable of an enclosing scope → the running
  block's `snapshot[i]`.
- `Dynamic(name)` — ivar / classvar / global → runtime resolution (today's path).
- `SelfRef` — `self` (bound per activation, not a slot).

---

## 4. Target representation

### 4.1 Frames

`Frame = Vec<Value>` sized to the scope's slot count. Owned by an evaluator LIFO
frame stack (`frames: Vec<Frame>`), pushed on activation and popped on teardown.
Because snapshot makes every block self-contained, **no frame ever needs to
outlive its activation** — the stack is pure LIFO, nothing is refcounted, and the
`Mutex`/`Arc`/`HashMap` all disappear.

### 4.2 Methods vs blocks

- A **method** never captures lexical variables (its env is rooted at
  `global_scope`, so it sees only its own locals + dynamic names). A method
  activation is therefore **just a frame**.
- A **block** activation is **a frame + a reference to its snapshot** (owned by
  the `Arc<Block>`). `CapturedSlot(i)` resolves against that snapshot; it can only
  appear inside a block body.

### 4.3 `Block` value

Drop `env: Env`. Add `captured: Arc<[Value]>` (the snapshot) and keep
`home_depth`, `home_self`, `home_entity`. Metadata semantics are unchanged:
`home_self` still gives blocks live ivar/classvar access via a synthetic
`SelfBinding::Instance` (§6.2), and `home_depth` still targets non-local returns.

### 4.4 `Cont` stack

`Cont` variants stop carrying `Env`. Each carries the minimal activation context
needed for teardown/restoration (a frame index and/or the running block's
snapshot handle) instead of a cloned `Vec<Arc<Frame>>`. This removes the
per-`Cont` `Vec` clone — the clone/vec-grow bucket.

---

## 5. The analysis pass

A pass over the AST, run once per method/block definition (at parse time or as a
distinct resolve pass — see §9), producing a **resolved form**: each scope gains a
slot count; each `Var` gains its classification (§3); each block literal gains its
capture plan (an ordered list of sources, each either "current frame slot j" or
"enclosing block snapshot slot k", so nested captures compose).

**Contract.**

- *Requires:* the AST has passed the existing strict block-assignment check.
- *Ensures:*
  - Every param/local of every scope is assigned a unique slot in `0..slot_count`.
  - Every `Var` is classified into exactly one of `LocalSlot` / `CapturedSlot` /
    `Dynamic` / `SelfRef`, shadowing-aware (innermost declaration wins).
  - Every block's capture plan lists exactly the enclosing lexical variables its
    body reads, in a fixed order matching its snapshot layout.
  - **Single-assignment is enforced (§2.1):** a `:=` whose target is an
    already-bound local, or a name that resolves to neither a local, ivar,
    classvar, nor global, is rejected with a new resolve error (mirror of
    `NonLocalBlockAssignment`). Distinguishing "undeclared" from "ivar/classvar/
    global" requires class/namespace context, so this check needs the registry,
    not the AST alone (see §9).

The audit tool (`capture_audit.rs`) already implements the reassignment and
capture analysis and can seed the enforcement check and its tests.

---

## 6. Interaction points that must be preserved

Each of these is load-bearing today; the redesign must keep them correct.

1. **Implicit `self` return.** Methods answer `self` absent an explicit `^`
   (`MethodFrame.implicit_self`). Unchanged — `self` is per-activation, not a slot.
2. **ivar access from blocks via `home_self`.** `push_block_frame` pushes a
   synthetic `SelfBinding::Instance` for `block.home_self` so ivars resolve after
   TCO removed the home method frame. **ivars are NOT snapshotted** — they are
   `Dynamic`, read live from the instance at block-run time. This is correct and
   must stay: a block sees the *current* ivar value, not a stale one. Only lexical
   temps/params are snapshotted.
3. **classvars / globals.** `Dynamic`; resolved via class / namespace as today.
4. **tx-shadow ivar reads/writes (C32).** These live on the ivar path
   (`lookup_var` step 2 / `assign_var`), not in the env frame, so they are
   untouched by the frame change. Read-your-writes and lazy enrollment stay as-is.
5. **TCO teardown / frame reuse.** A tail self-send currently drains the frame and
   pushes a new one. With flat frames this becomes pop-then-push (or reuse the top
   frame in place when slot counts match) — cheaper, and the place the per-send
   allocation was.
6. **`super` sends.** Receiver is `self` from `method_stack`; resolution of the
   *method* changes (superclass), not variable resolution. Unchanged.
7. **Non-local return (`^` in a block).** Targets `home_depth` via `method_stack`.
   Unchanged.
8. **`Namespace do:`** swaps the active namespace (evaluator step 1g). Globals are
   `Dynamic`, so a namespace swap still affects them at runtime. Unchanged.
9. **`perform:` / `doesNotUnderstand:` / reified `Message` (C33).** Dynamic sends
   do not name local variables, so they do not interact with slots. Confirm no
   reflective *variable* access exists that would need name→slot at runtime.
10. **Rust-side `Block` construction.** `store_bridge.rs:591` and several tests
    build `Block` literals directly; all must move from `env` to `captured`.
11. **Observability (project goal).** Retain a per-scope `slot → name` table so a
    debugger/inspector can still show frames by name. Design it in, not on.

---

## 7. What this removes (mapped to the profile)

- `HashMap` frame → gone: removes **hashing (10%)** and the bucket-zeroing memset.
- `Mutex` per frame → gone: removes **mutex (7%)**.
- `Arc<Mutex<HashMap>>` alloc per activation → a poolable `Vec` (often reused via
  TCO): cuts the bulk of **alloc (34%)**.
- `Env` `Vec` clone per `Cont` → gone: cuts **clone/vec-grow (9%)**.
- (Separate lever, not this doc) the AST body clone into a `VecDeque<Stmt>` per
  activation (`push_block_frame` / `push_*_method_frame`) — `Arc` the body or index
  into it. Tracked in `next_steps_after_c34.md` §4 lever 2.

---

## 8. Phasing (each phase landable + re-profiled)

- **P0 (optional warm-up).** Swap the frame `HashMap`'s hasher to a non-crypto one
  (`rustc-hash`/`ahash`). ~10% for a one-line change, no semantics. Skippable —
  the slot work removes hashing entirely — but useful as an early validated win.
- **P1 — rule + analysis, no representation change.** Add the resolve pass and the
  single-assignment enforcement (§2.1): `:=` to an already-bound local, or to a
  name that is neither local/ivar/classvar/global, is a resolve error. Full suite
  stays green (audit says 0 kernel reassignments; verify the kernel also has no
  implicit-local assignments). Ships the language rule with targeted tests before
  any risky rewrite.
- **P2 — flat frames for the common case.** Introduce the evaluator frame stack +
  `LocalSlot`/`Dynamic` resolution for method frames; keep blocks on the old path.
  Re-profile: expect the alloc/hash/mutex share to drop for method-heavy soaks.
- **P3 — snapshot capture.** Fill block snapshots from capture plans;
  `CapturedSlot` resolution; drop `Block.env`. Re-run the cross-entity capture
  tests (`value_object_update_captures_outer_var` et al.).
- **P4 — de-`Env` the `Cont` stack.** Remove the `Env` field from `Cont` variants.

Re-run `alloc_probe` + the `cargo bench` loop/object benches after each phase.

---

## 9. Open questions

- **Where the analysis lives.** Extend the parser (single pass, but couples
  resolution to parsing and complicates error spans) vs a separate resolve pass
  over the AST producing a resolved AST (cleaner, and the compile step the future
  custom VM will want anyway). Leaning: separate pass.
- **Resolved-AST shape.** New `Expr::ResolvedVar { … }` variants vs a side-table
  keyed by node identity. Leaning: transform into resolved variants so the
  evaluator never re-classifies at runtime.
- **`Cont` activation handle.** Exactly what each `Cont` must carry for correct
  teardown once `Env` is gone (frame index, block snapshot handle, saved
  block-home). Needs a careful pass over all `Cont` variants.
- **Frame pooling.** Whether to pool freed frames by slot-count to eliminate the
  residual per-activation `Vec` allocation, or rely on TCO reuse + allocator.
- **Inline `let` binding syntax (deferred, §2.3).** Replace top-of-scope `| … |`
  with an inline `let name := expr` binding. Pure ergonomics — SSA semantics are
  already the decision; this is a later low-stakes syntax change with a kernel
  rewrite, not part of this redesign. Whether shadowing-only reads clearly enough
  is the open sub-question.
- **Debugger/inspector contract.** How much of the `slot → name` table and live
  frame contents must be exposed, given the observability goal.
- **Relationship to the future custom VM.** This resolved form is a stepping stone
  toward bytecode; keep the slot model compatible with that direction.

---

## 10. Acceptance criteria

- Full test suite green; single-assignment rule (§2.1) added with targeted
  pass/fail tests (the `x := 1. … x := 2` reassignment rejection, the undeclared-
  assignment rejection, and shadowing acceptance where applicable).
- Cross-entity capture semantics unchanged (existing tests pass unmodified).
- Re-profile via `alloc_probe soak`: alloc + hashing + mutex self-time shares drop
  substantially toward the predicted floor; state a concrete ns/send target when
  P2 lands (candidate: < 500 ns, i.e. > 2× overall).
- `capture_audit` remains at 0 reassignments / 0 capture violations (now enforced,
  not just observed).
