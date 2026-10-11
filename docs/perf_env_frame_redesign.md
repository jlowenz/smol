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

**One hole in "observationally identical," and its resolution (P2.1c, decided
2026-09-13).** The equivalence argument assumes the capture happens *after* the
local's single assignment. Capturing *before* it — `| x b | b := [x]. x := 5. b
value` in a method — is the one case a value-snapshot and a Smalltalk cell diverge:
Smalltalk's cell yields `5`, a snapshot yields the nil placeholder captured at
creation. Under single-assignment this capture can *only ever* snapshot the
placeholder (the sole assignment is still in the future), so it is never useful —
always a latent bug, and silent. Rather than accept the silent nil, the evaluator
**rejects capture-before-assignment at install time** (`check_capture_before_assignment`,
run on the resolved body): a block whose `ParentLocal` capture names an enclosing
declared local not yet assigned at the block's textual position is a compile error.
This keeps "a local names a value" honest — you cannot observe a local before it has
one — and closes the divergence without special runtime handling. (Top-level/REPL
code stays exempt, as it is from §2.1: it is the mutable global scope, resolved with
an empty local scope, so its blocks read live via the root env.)

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

---

## 11. P2.1 detailed design — flat frames + snapshot capture (CONFIRMED 2026-08-11)

Written after P1 (`fa89efa`) and P2.0 (`5182706`) landed; the four decisions below
(§11.1 phasing, §11.2 representation, §11.3 `self`, §11.6 top-level) were reviewed
and confirmed 2026-08-11. This is the invasive step.

### 11.1 Phasing revision: P2.1 subsumes the old P3

The doc's original split — P2 = flat method frames with *blocks on the old path*,
P3 = snapshot capture — **is not separable**, and P2.1 should land flat frames and
snapshot capture together. Reason: control-flow blocks (`ifTrue:`/`ifFalse:` arms)
capture method locals (`self`, params) and sit squarely on the hot path. A flat
method frame only helps if the blocks inside the method read from it — and, since a
block can outlive its frame, "read from it" means **snapshot** (§2.2). There is no
useful intermediate in which methods are flat but their blocks are not.

### 11.2 Resolved AST (the representation — resolved-AST direction, confirmed)

P2.0's resolver emits a classification *list* (`refs`); execution needs the
resolution *attached to each variable node*. **Recommendation:** change
`Expr::Var(Arc<str>)` to `Expr::Var { name: Arc<str>, res: VarRef }`, add
`VarRef::Unresolved` as the parser's output, and have the resolve pass functionally
rewrite each method/block body into a resolved copy (once, at install — not hot).

- One AST — no parallel `RExpr` duplication; the only surface change is `Var`'s
  shape (no four new `Expr` variants and no `unreachable!` arms in the install-time
  checks, which run before resolution and would otherwise have to match them).
- The evaluator reads `res` directly: no runtime hashing, no name walk for
  locals/upvalues.
- Churn: ~6 `Expr::Var(name)` match sites become `Var { name, .. }` (parser,
  evaluator, the two install-time checks, `resolve.rs`, the `capture_audit`
  example). Mechanical. P2.0's `refs`-list output and its tests get reworked into
  the tree rewrite (the *logic* is unchanged and stays covered).
- Rejected alternatives: (a) new resolved `Expr` variants — more variants + dead
  arms in checks; (b) a separate resolved AST type — clean but duplicates the whole
  enum for a bootstrap.

### 11.3 Frame stack

- `Frame = Vec<Value>` (candidate `SmallVec<[Value; N]>` to skip the heap for small
  frames — measure before committing).
- The evaluator gains `frames: Vec<Frame>`, a LIFO stack: push on activation
  (method/block), pop on teardown. Because blocks snapshot, **no frame outlives its
  activation** → pure LIFO, nothing refcounted, no `Arc`/`Mutex`/`HashMap`.
- `self` is **not** a slot. `SelfRef` reconstructs the receiver from the current
  `SelfBinding` on `method_stack` (`Instance`/`ClassSide` handle, or `Primitive`
  self-value) exactly as that data exists today. Frame slots hold params + locals.

### 11.4 Block value + snapshot

- Drop `Block.env: Env`; add `snapshot: Arc<[Value]>` and the block's slot count.
  `Upvalue(i)` reads `snapshot[i]`.
- At block creation, fill the snapshot from the creating scope's frame slots /
  its own snapshot, following each capture's `CaptureSource` (`ParentLocal` /
  `ParentUpvalue`).
- A running block's activation carries its snapshot handle so `Upvalue(i)` resolves.
  `home_depth` / `home_self` / `home_entity` are unchanged.

### 11.5 Cont stack

- Replace `env: Env` in every `Cont` variant with a frame index into `ev.frames`
  (stable for the activation's lifetime — LIFO guarantees the referenced frame is
  not popped while its Conts are live). Block-frame Conts also carry the snapshot.
- Var evaluation: `Local(i)` → `ev.frames[frame][i]`; `Upvalue(i)` → `snapshot[i]`;
  `SelfRef` → `method_stack`; `Dynamic(name)` → the existing ivar/classvar/global
  resolution.

### 11.6 Interaction points (delta from §6)

- **ivar / classvar / global** — unchanged resolution, now triggered by
  `VarRef::Dynamic` rather than a failed local lookup. `assign_var`'s ivar/classvar
  arms stay; a declared-local assignment becomes a slot write; the P1b
  undeclared-assignment error can become static once resolution has the registry
  (or stays the runtime check).
- **TCO** — a tail self-send reuses the top frame (clear/resize) instead of
  pop-then-push.
- **async `then:` continuations** — snapshot makes the block self-contained, so the
  continuation carries its snapshot; no frame-stack transfer. Verify against
  `value_object_update_captures_outer_var` and the `then:`/`await` tests.
- **top-level / REPL scope (RESOLVED 2026-08-11 — top-level temps are globals).**
  *SUPERSEDED 2026-10-10 by §11.7a* — the premise that every activation shares one
  root frame is false; top-level temps are **Workspace** bindings, not globals. Text
  below kept for the record.
  Top-level `| x |` already defines `x` in the *root* frame — the same name-keyed
  `Env` frame that holds classes and namespaces — and a method's `global_scope()`
  shares that frame, so top-level temps have always been globals. So there is **not**
  a second local-variable system: there are **slots** for closed local scopes
  (methods, blocks) and **one irreducible global namespace** (the root `Env` frame,
  needed for classes regardless), with `VarRef::Dynamic` the single bridge between
  them. What P2.1 removes is the *per-activation* frame; the *global* root frame
  stays because globals are name-addressed by nature.
  - **Resolution rule:** resolve top-level code with an empty local scope → every
    `Var` there is `Dynamic` (→ root env), `self` → `SelfRef` (no anchor → nil/error,
    as today), `| x |` defines a global. Equivalently, leave top-level `Var`s
    `Unresolved` and let the evaluator's `Unresolved` arm *be* the root-env lookup —
    same operation.
  - This also dissolves the block-capture wrinkle: a top-level block `[x]` does not
    *capture* `x` (it is `Dynamic`), it reads it live from the root env — nothing to
    snapshot. Consistent with P1, which already exempts top-level from
    single-assignment and no-implicit-locals (both scoped to `method_depth > 0`).
  - **Revisit when namespaces are fleshed out** — a richer namespace model may change
    how the global/top-level scope is structured; this resolution holds until then.

### 11.7 Implementation sub-sequence

- **P2.1a** — resolved AST: change `Var`, add `Unresolved`, make `resolve` rewrite
  bodies, store the resolved body on `Method`/`Block`; update match sites and P2.0
  tests. Evaluator still runs via name lookup (honoring `res` where present) so the
  suite stays green.
- **P2.1b** — evaluator frame stack; method-local `Local` reads and writes go
  through slots. **DONE** (this commit). Details:
  - **P2.1b-0 (prerequisite refactor):** consolidated the duplicated frame
    setup/teardown into single choke points — `release_frame` (one teardown,
    replacing four hand-copied sequences) and `push_method_frame` (one method-frame
    setup for all four entry points); `eval_block` now delegates to
    `push_block_frame`. Behavior-preserving; surfaced the class-side `super`
    base-env inconsistency (open_questions Q16).
  - `Stmt::Assign` gained `res: VarRef` (parser emits `Unresolved`; resolver fills
    it) — write-target resolution, deferred from P2.1a.
  - `Evaluator::frames: RefCell<Vec<Vec<Value>>>` — a strictly-LIFO slot stack
    pushed per method/block activation (params, then locals, then body temps, in
    resolver order; `self` is not a slot) in `push_method_frame`/`push_block_frame`,
    popped in `release_frame`. Balanced on the same paths that keep `method_stack`
    balanced (continuable `Signal` errors drain via `teardown_frame`; raw `?` errors
    fault-and-terminate the entity). No `current_frame` cell: `frames.last()` is
    always the current scope (Conts fire LIFO; async continuations run on a fresh
    evaluator with a fresh stack).
  - `Local(i)` reads (`handle_eval_expr`) index `frames.last()[i]`; writes
    (`assign_var`) write the slot and mirror into the env (dual write — two clones)
    so nested-block `Upvalue`-by-name reads stay correct until P2.1c.
  - **Deliberately deferred:** `SelfRef` / `Dynamic` still route through
    `lookup_var` by name (behavior-identical; routing them by `res` is a marginal,
    optional follow-on — only `Local` avoids a hash). The name-keyed `Env` and its
    `HashMap` remain (dual storage); P2.1c/d collapse them.
- **P2.1c** — snapshot capture on block creation; `Upvalue` reads. **DONE** (this commit). Details:
  - `CaptureSource` moved to ast.rs; `Expr::Block` gained `captures: Vec<CaptureSource>`
    (parser emits empty, resolver's `rewrite_scope` now returns the scope's capture
    plan and attaches it to the block node — methods assert an empty plan).
  - `Block` gained `snapshot: Arc<[Value]>`; the evaluator's frame element became
    `Frame { slots, snapshot }`. At block creation `build_capture_snapshot` fills the
    snapshot from the creating (top) frame — `ParentLocal`→its slots, `ParentUpvalue`
    →its snapshot; capture-free scopes share a cached `empty_snapshot()`.
  - A running block's frame carries `block.snapshot`; `Upvalue(i)` reads
    `frames.last().snapshot[i]`. A method frame's snapshot is empty.
  - The P2.1b `Local`-write **env mirror is removed** — with `Upvalue` on snapshots,
    no reader consults env-local entries, so `Local` writes are back to a single
    clone. Verified: full suite green after removal; poison-test confirmed `Upvalue`
    reads are live and index-sensitive.
  - **Capture-before-assignment is now a compile error** (§2.2, decided 2026-09-13):
    `check_capture_before_assignment` runs on the resolved body at install and
    rejects a block whose `ParentLocal` capture names an enclosing declared local
    not yet assigned at the block's position. Closes the one snapshot-vs-Smalltalk
    divergence; the kernel and full suite pass unchanged (nothing captures before
    assigning).
  - **Still deferred to P2.1d:** `Block.env` is retained (dual storage) because
    `Dynamic` (ivar/classvar/global) and `SelfRef` still resolve by name; the
    per-block `env.clone()` at creation therefore remains until P2.1d de-`Env`s the
    `Cont` variants and removes it.
- **P2.1d** — remove the per-activation `env`. Reshaped by the `frames.last()`
  insight: the Conts don't need a frame index (§11.5's original plan), they can drop
  `env` entirely, once its three remaining uses are rehomed. Landing in sub-steps:
  - **d1 (DONE)** — `SelfRef` resolves via `method_stack`, not env: new
    `Evaluator::self_value()` reconstructs the receiver from `method_stack.last()`.
    A block's captured self is generalized from Instance-only
    `home_self: Option<InstanceHandle>` to `home_binding: Option<SelfBinding>` (any
    kind), re-pushed by `push_block_frame`; `Cont::BlockFrame`'s `block_home_self`
    becomes a `pushed_self: bool`. Fixes a latent divergence — a class-side/primitive
    block that outlives its home now answers `self` from `method_stack` rather than
    the captured env's `"self"`. (`SelfBinding` gains a manual `Debug`.)
  - **d2** — *SUPERSEDED 2026-10-10 by §11.7a (premise false).* Hoist the shared
    global-root `Env` onto the `Evaluator`; route `Dynamic`
    globals + top-level through it (namespaces unchanged). All entities already share
    one root frame (`Arc`-cloned), so this is a rehoming, not a new store.
  - **d3** — stop building the per-activation env map (setup no longer defines
    self/params/locals); drop `Block.env`.
  - **d4** — drop `env: Env` from the ~20 Cont variants + the handler params.
- **P2.1e** — re-profile (`alloc_probe soak`); TCO frame reuse; `SmallVec` decision.

### 11.7a P2.1d-d2 revised — Globals vs Workspace (DECIDED 2026-10-10)

*Partly SUPERSEDED 2026-10-10 by §11.7b:* "Globals = Kernel", the trait-only
registration, the REPL-rebinding paragraph and the d2a–d2c sub-steps are replaced
there. The diagnosis, the probe evidence, the Workspace scope rules (single-assignment,
declared, snapshot capture) and the measured corpus impact still stand.

**Why d2 changed.** The original d2 assumed that "all entities already share one root
frame (`Arc`-cloned)". **They do not.** Each spawned entity is given a *fresh*
`Env::new()` filled by `registry.populate_env` (`builtins_iterative.rs`
`spawn_entity_bare` call sites). Class entities re-run `populate_env` on every dispatch
(`entity.rs` `dispatch_class_envelope`); instance entities never do. Probes (throwaway
tests, spawn ordering forced with `await`) showed:

| Probe | Result |
|---|---|
| top-level temp `GG := 7`, read from an **entity** method | `UndefinedVariable("GG")` |
| same, read from a **value-object** method (runs inline on the caller) | `7` |
| Kernel class defined *after* an instance entity spawned, used in its method | `UndefinedVariable("PB")` |

So a variable's visibility depended on whether the receiver was a Value or an Object,
which contradicts the Value/Object split. Instance entities also saw a stale class set.
Kernel **traits** are bound only in the bootstrap `Env` (`bind_trait_name`) and are
registered in no namespace.

**Decision: two bounded notions replace "the root `Env`".**

- **Globals** — the registry, read *live*: Kernel-namespace classes, Kernel traits,
  root namespaces. These are the only true globals. Every activation on every
  evaluator sees the same set, with no per-entity snapshots and no per-dispatch
  `populate_env`.
- **Workspace** — the top-level (script/REPL) scope's bindings. It is visible to
  top-level code and to blocks created at top level, and **never to methods**. (The
  value-object probe above becomes `UndefinedVariable`, which is the consistent
  result.) The Workspace obeys the same rules as every other scope:
  - **single-assignment** — a workspace temp is bound once;
  - **declared** — no implicit creation by assignment (top level loses its P1
    exemption);
  - **blocks capture it by snapshot** — the strict block-assignment rule
    (`parser.rs`, `NonLocalBlockAssignment`) already forbids a block from assigning
    any name outside its own scope, top level included. With single assignment and
    the capture-before-assignment check, a snapshot cannot be distinguished from a
    live read, so top-level blocks need no `Env` handle. That is what lets d3 drop
    `Block.env` uniformly.
- Rejected: a shared mutable globals table on the registry (top-level temps as true
  globals). It is mutable state shared across actors, against the entity isolation
  model.

**Measured impact on the existing corpus** (probes, 907 lib tests + `kernel.smol`):

- top-level workspace-temp reassignment: **0** occurrences;
- reassignment of a block-local inside a *top-level block*: **5** tests
  (`block_with_locals`, four `invoke_restart_*`). This is a **pre-existing hole**: the
  SSA check runs only at method install, so blocks at top level were never checked;
- undeclared top-level assignment: **2** programs;
- `kernel.smol`: no violations.

**Name resolution after d2.**

- In a method or method-block: `Local` / `Upvalue` / `SelfRef` as now; `Dynamic` →
  ivar → classvar → **Globals**.
- At top level or in a top-level block: workspace temp → slot or snapshot capture
  (resolved like any scope); other names → **active namespace** (with imports) →
  **Globals**. The active-namespace step replaces the frame-scoped `env.define` that
  `handle_subclass` / `bind_trait_name` use today for non-Kernel classes inside
  `Namespace do: [...]`. Visibility is unchanged: a non-Kernel class is reachable by
  simple name only while its namespace is active, and otherwise only as `Ns::Name`.
- Open, carried forward: whether a method of a non-Kernel class should resolve
  sibling classes through its *defining* namespace. Today it cannot, because a method
  roots at the Kernel-only global frame. d2 keeps that behavior and records the
  question in `open_questions.md` rather than changing it.

**REPL across programs.** Each `eval_program` call is checked statically (SSA,
declared, capture-before-assignment). Across calls the Workspace persists. A new
`| x |` declaration in a later program introduces a **fresh** binding that shadows
the earlier one (the REPL rebinding idiom). A bare `x := ...` against an
already-bound workspace temp is a second assignment, so it is an error.

**Sub-steps (each landable, suite green):**

- **d2a — Globals on the registry.** Register Kernel traits in the Kernel namespace.
  Method `Dynamic` falls through to a live registry lookup. Delete the per-entity
  `populate_env` snapshots (spawn sites + `dispatch_class_envelope`). Fixes the stale
  class probe.
- **d2b — Workspace semantics.** Top-level SSA + declaration + capture checks (static
  per program, runtime across REPL programs), SSA check extended to top-level blocks.
  Fix the 7 violating tests: rewrite them as new bindings, or keep the 2 undeclared
  ones as declaration-required error tests.
- **d2c — Workspace representation.** The top-level scope is resolved like any scope.
  Top-level blocks snapshot workspace temps. Other top-level names go through active
  namespace → Globals. `Namespace do:` bindings move to the active-namespace lookup.
  Methods no longer see workspace temps (the value-object probe flips; add it as a
  regression test, along with the entity and late-class probes).

d3 (drop the per-activation env map and `Block.env`) and d4 (drop `env` from `Cont`)
follow as before. After d2c, nothing reads `Env` except what they remove.

### 11.7b d2 namespace-relative revision (DECIDED 2026-10-10)

§11.7a treated Kernel as *the* global scope. That would hard-code an assumption the
planned namespace design rejects (`chunk_17_plan.md`: "a class belongs to exactly
one namespace; namespaces can import other namespaces"; `chunk_18_plan.md`: a simple
name is searched among the namespace's own entries, then its imports). Decisions:

1. **All free-name resolution is namespace-relative.** `ns.resolve(name)` checks the
   namespace's own entries, then its imports in order. A namespace does **not
   implicitly** inherit or look up its parent's names; a parent is visible only if
   it is explicitly imported, like any other namespace. Kernel is not special
   either; it is simply what namespaces import.
   - **In a method** (and its blocks): `Dynamic` → ivar → classvar → the **defining
     class's namespace**. This needs a `Class → Namespace` back-link (none exists
     today).
   - **At top level:** workspace temps (scope bindings) → the **workspace's
     namespace**. `Namespace do:` switches which namespace that is, as today.
   - **Qualified `Ns::Name`** resolves from the registry's root-namespace table,
     unchanged.
2. **A namespace maps names to any named definition** (class, trait,
   sub-namespace), not one table per kind. Traits stop being `Env`-only bindings.
3. **Names stay name-keyed until use.** `VarRef::Dynamic` carries the name, and no
   class handle is cached into the AST at install, so redefinition, live imports and
   future store-backed namespaces (`open_questions.md` Q8) keep working. Inline
   caching is a VM concern.
4. **A workspace *has* an anonymous namespace.**
   - Definitions made in the workspace (classes, traits) go into that anonymous
     namespace, **not Kernel**.
   - It imports Kernel by default. Any other namespace must be imported, as in any
     other context (import form: `open_questions.md` Q17).
   - No other namespace can reach it: it has no name, so it cannot be imported or
     `::`-qualified.
   - Workspace **temps are scope bindings, not namespace entries**. Methods (even
     methods of classes defined in that workspace) never see them.
   - `kernel.smol` is not loaded as a workspace. Its loader evaluates with Kernel as
     the namespace, so kernel definitions land in Kernel.
5. **Each REPL program is a nested scope** inside the workspace, the way a block
   nests in a method. §2.1 applies in full within a program. A later program may
   redeclare `| x |`, which shadows the earlier binding exactly as a block-local
   shadows an outer one. A bare `x := …` naming an already-bound temp is still a
   second assignment, so it is an error.

**Sub-steps (replace §11.7a's d2a–d2c; each landable, suite green):**

- **d2a — namespace-relative resolution for methods.** Add the `Class → Namespace`
  link, set at class creation from the active namespace. Generalize namespace entries
  so traits register too. Method `Dynamic` resolves through the defining namespace,
  live. Delete the per-entity `populate_env` snapshots. Every class is still in Kernel
  at this point, so the only behavior change is the stale-class fix (late-class probe
  → regression test).
  **Landed (2026-10-11).** One deviation from the plan above: methods stopped seeing
  workspace temps here, not in d2c, because a method's free names are all `Dynamic`
  and `Dynamic` no longer reads the env. `primitive_method_reads_outer_env_var` was
  inverted to `method_cannot_see_workspace_temp`, with value-object and entity
  variants. Follow-ons recorded: trait-method name resolution (`open_questions.md`
  Q18) and ownership of workspace namespaces (Q19).
- **d2b — Workspace scope rules.** Per-program SSA + declaration +
  capture-before-assignment, including top-level blocks. Nested-scope redeclaration
  across programs. Fix the 7 violating tests (§11.7a).
- **d2c — Workspace representation.** Workspace = anonymous namespace (imports
  Kernel) + nested program scopes. Top-level definitions go to the anonymous
  namespace. Top-level blocks snapshot workspace temps. The frame-scoped
  `env.define` for `Namespace do:` definitions is replaced by namespace registration.
  The kernel loader uses Kernel directly. Methods no longer see workspace temps (the
  value-object probe flips → regression test).
  Risk: Rust tests that look up user-defined classes via `registry.kernel()` will
  need the workspace namespace instead; measure before landing.

### 11.8 Risks

- The `Cont`-carries-frame-index change touches every `Cont` variant and all their
  construction/read sites — the single largest edit; frame-index lifetime across
  nested activations is the main correctness risk.
- Async continuation frame handling — must be re-verified against existing tests.
- `Frame` allocation behavior (`Vec` vs `SmallVec`) — measure, don't assume.
