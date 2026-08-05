# Next steps after chunk 34

**Written 2026-08-04**, at the close of chunk 34 (`4c4fd0d`). Chunk 34 is complete:
zero public arms remain on any scalar dispatcher, 33 of its 40 inventoried arms
migrated, 7 deferred with recorded reasons. See `chunk_34_plan.md`.

This document exists so the next session can start without re-deriving context. Items
are independent unless noted; §5 recommends an order but does not fix one.

---

## 1. Chunk 35 — the routing-bound arms (READY)

**Scope is already settled** by `chunk_34_plan.md` §6 OQ-6 and §5e. This is the direct
continuation of C34 and the only item whose design questions are already written down.

Seven public arms remain, plus the object/entity-side surface C34 never inventoried.
What unites them — and what makes them *not* a C34-style migration — is that each needs
a **dispatch routing change before any method can be written**.

### 1a. Class-side (`= ~= name asString`, 4 arms)

A class receiver never reaches `Object >> =`: `dispatch_class` resolves against
CLASS-side methods via `lookup_class_method`, so the target is `Object class >>`
definitions, not instance-side ones.

Two open sub-questions:
- Can a class-side method send a `_prim` to `self`? The self-only rule meets a
  `ClassEntity` receiver, which may or may not be a self-send depending on
  `current_entity_id`.
- `dispatch_class` has a **hardcoded synchronous-selector list**. A migrated selector
  missing from it answers a **Future where a value is expected**. This trap fired twice
  in C34 — `==`/`~~` in S2, `asString`/`name` in S5 — so assume it fires again. Consider
  whether class-side dispatch of pure-function selectors should be synchronous *by
  default* rather than by enumeration.

### 1b. Namespace (`do: name asString`, 3 arms)

Hardest of the three, because **`Namespace` is not a class at all**: there is no
`Namespace` in `kernel.smol` and `lookup_user_method` answers `None` for
`Value::Namespace`. Prerequisites, in order:

1. Decide what a `Namespace` class *is* in the hierarchy — presumably a
   `Value vmSubclass:` encoded leaf like `String`/`Symbol`, which pulls in
   `is_encoded_leaf_name` and `value_to_class_handle` (so `aNamespace class` answers
   something sensible) and a decision on whether it is subclassable.
2. Add a `Value::Namespace` arm to `lookup_user_method`.
3. Ground `name` on a `_primNamespaceName`.
4. **`do:` is evaluator step (1g)** — it swaps the active namespace and pushes
   `Cont::NamespaceGuard`. The arm behind it in `dispatch_namespace` is therefore
   **dead**, exactly as `dispatch_int`'s `to:do:` was before C34 S1; verify-and-prune it
   the same way (`panic!` probe + full suite).

Unlike `to:do:`, the operation is **genuinely irreducible** — it manipulates evaluator
state — so the honest target is
`Namespace >> do: aBlock [ ^self _primNamespaceDo: aBlock ]`. That is OQ-1's *rejected*
option (c), and it is the RIGHT answer here precisely because the body cannot be written
in Smol, which was not true of `to:do:`.

### 1c. Object/entity-side (needs its own inventory first)

`obj_prim_core >> respondsTo:` and `obj_prim_history >> historyOf:` /
`historyOf:from:to:` are the two public arms C34's inventory covered. The
`dispatch_object_primitive` chunks as a whole (traits, tx, futures, timers, indexed)
were never inventoried — OQ-6 asks for that pass, plus a judgement per arm about
**genuine VM substrate** (`_primNew`, tx enrollment, coroutine handling) versus
**ordinary protocol that happens to live in Rust**.

### 1d. Finishing Q14

`open_questions.md` Q14 is closed for public protocol on every scalar family, by
construction. It remains inaccurate for these seven arms. Chunk 35 closes it fully.

---

## 2. String construction — `StringBuilder` / `WriteStream` (READY, and now load-bearing)

From the retired `string_representation_plan.md` §5, which C34 S4 **absorbed the rest
of** but left this open.

It is no longer optional. C34 S4 refused `new: n` for all seven encoded leaves, on the
grounds that n empty slots presume mutability, which §5 rejects. `String new: n withAll:
c` covers fixed-shape construction. **Open-ended accumulation has no answer at all**:
building a string in a loop, or conditionally, currently means repeated `,`, which is
O(n²) and allocates a fresh immutable String per step.

The design is already sketched: mutability lives in a **mutable Object**, values stay
immutable, and `contents`/`asString` yields the frozen `String`. This is the
**transients** idea from `web_server_readiness.md` — it *uses* the Value/Object split
rather than fighting it, and is the general answer for building immutable collections,
not just strings.

Worth settling together:
- `StringBuilder` (an Object) vs `WriteStream on: String` (the Smalltalk shape). The
  latter generalises to other collections and to output sinks.
- Whether the same abstraction backs `print`/`printNl`, which currently write straight
  to stdout through `_primWrite`. A `Transcript`-like sink would fall out of it.
- Relationship to `ByteArray` for socket I/O (`web_server_readiness.md` cross-ref).

---

## 3. `Character` (READY, small, unblocks two stand-ins)

From the retired plan §6. `Value::Char(char)`, `Character < Value`, `$x` literal in the
lexer (currently `$` does not lex at all).

Two C34 decisions are explicitly waiting on it, both currently using a **one-character
String** as the stand-in, consistently:
- `String >> at:` answers a one-character String; it should answer a `Character`.
- `String class >> new: n withAll: aChar` takes a one-character String.

Both change together when this lands. Also wanted: `asInteger`, comparisons,
`isDigit`/`isLetter`, and the `cr`/`lf`/`tab`/`space` constants — the last of which
`printNl` would use if the console work in §2 unifies with it.

---

## 4. Performance review — per-send cost (READY, highest leverage)

C34 §5d measured this precisely and it is the single largest lever in the system.

**~1.2 µs per message send.** A tree-walking interpreter is usually 50–200 ns, so this
looks ~10× off, and it taxes *everything* — C34's 30× loop slowdown is 13 sends per
iteration multiplied by this number, not a property of `to:do:`.

**Leading hypothesis, reasoned from the code but NOT yet profiled** (stated as a
hypothesis deliberately): every method activation allocates a fresh
`Arc<Mutex<HashMap>>` env frame (`push_frame`) plus a `Vec` (`global_scope`), then
hashes `self` and each parameter name into it — roughly 3–4 heap allocations and several
string hashes per send — and every `Cont` carries an `Env`, cloning that `Vec` again.

**First step is to verify, not to fix.** Profile a send-heavy benchmark and decompose
the 1.2 µs by cause before choosing a change. The conventional fix if the hypothesis
holds is resolving names to integer slots at parse time and using a flat `Vec` frame,
which removes the allocation and the hashing together.

Precedent for the payoff: C34 S1a was one structural fix in exactly this area and gave
**~12×** on dispatch-bound benchmarks.

Second lever, already recorded in OQ-1: **inline the compiled fast path when a method
has not been overridden**, falling back to a real send when it has. That applies to
`<` and `+` on the numeric leaves at least as much as to `to:do:`. `to_do_can_be_overridden`
exists as a test precisely because that inlining must respect overrides.

Note `cargo bench -p interpreter` now takes ~15 minutes, dominated by the three loop
benches at 100k. Shrinking them is reasonable now that C34's before/after pair is
recorded.

---

## 5. Suggested order

1. **Performance review (§4)** — highest leverage, benefits every later chunk, and the
   measurement work is already done. Verify the hypothesis first.
2. **Chunk 35 (§1)** — finishes what C34 started and closes Q14 completely. Scope is
   already written; no design session needed to begin.
3. **`Character` (§3)** — small, and unblocks the two stand-ins C34 left.
4. **String construction (§2)** — larger design surface, and better once `Character`
   exists so the streaming API is specified against a real element type.

§2 and §3 could swap if user-facing string capability matters more than internal
consistency. §1 and §4 are independent of both.

---

## 6. Pre-existing queue (unchanged by C34, status noted where checked)

- **C32 STM** — memory records this as outstanding, but the kernel already has
  `TxConflict`, `pvtRunStm:` and an STM retry loop, and `obj_prim_tx` has
  `_primStmPrepare`. The `_primCheckVersions:` named in the old plan does **not** exist.
  **Verify actual status against `chunk_32_plan.md` before planning work here** — it may
  be substantially done under different primitive names.
- **Smol-level coverage** — AST span tracking + `CoverageCollector`, so `kernel.smol`
  coverage is measurable at all (llvm-cov sees only Rust). Nothing exists yet
  (no `Span` struct, no collector). See `CLAUDE.md` "Planned tooling" item 1. This
  matters more now: C34 moved a large amount of behaviour *into* `kernel.smol`, so the
  blind spot grew.
- **SUnit-style Smol test framework** — `CLAUDE.md` planned tooling item 2; depends on
  the coverage work to be measurable.
- **`then:orElse:` fluent-chain / `bind:` sugar** — additive kernel, deferred by design.
- **Q11 durable orphan-fact GC** — accepted debt, naturally post-VM.
- **Q13 standalone `uses:`** — trait applied to an already-defined class; needs a design
  session.
- **Q15 `2 = 2.0`** — implemented as true, explicitly flagged for re-weighing.
- **Async coordinator #2 and #3** — `project_chunk32_c_future_hang_session` lists four
  issues; #1 and #4 are resolved, #3 is "likely fixed but unverified", #2 (self-sends
  running synchronously) was never re-verified. Re-check before relying on either.

---

## 7. Carried-forward hazards (read before touching these areas)

- **A Smol method silently shadows a same-named Rust arm.** This is how `2 = 2.0` stayed
  wrong and how `~=` nearly contradicted `=`. Every migration in C34 deleted the
  superseded arm *in the same commit*; keep doing that.
- **`dispatch_class`'s synchronous-selector list** — see §1a. Fired twice in C34.
- **The `_prim` self-only rule shapes APIs**, and usually for the better: it forced
  `String >> repeated:` into existence (class-side `new:withAll:` could not ground
  directly), which turned out to be the more useful selector.
- **Acceptance criteria stated in terms of stack are not enough.** OQ-1's two criteria
  would both have PASSED on a `to:do:` built over the S1a defect, while the loop was
  quadratic in time. C34 added a third: measured linear scaling across ≥3 doublings.
