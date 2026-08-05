# Chunk 34 — Rust → Smol protocol migration

**Status:** planning. Depends on: numeric tower complete through S7 (`6c16fd9`).

**Supersedes:** the Rust→Smol migration portions of `string_representation_plan.md`
(String/Symbol protocol). That plan predates the numeric tower and the `_prim` grounding
pattern it validated; where the two disagree, this chunk wins. Reparenting decisions in
the string plan (§3, §7) still stand — only the *migration mechanics* move here.

## 1. Goal and governing principle

Move the remaining **public protocol** out of Rust `match` arms and into `kernel.smol`,
grounded on `_prim` selectors only where the operation is genuinely irreducible.

**Principle (decided 2026-08-02).** Observable, inspectable, modifiable comes **first**;
interpreter/VM performance is a separate, later concern. Gating this migration on
performance would be equivalent to concluding "Smol is slower than Rust, so keep it in
Rust" — which forecloses the reason the project exists. We start with as much as possible
in Smol, then make the interpreter/compiler/VM fast.

Performance is therefore **measured and recorded, never used as a veto**. A baseline
before and after is wanted as information (and to feed the queued performance review),
not as an approval gate.

The numeric tower is the worked example: `<` moved to Smol over `_primStrLt:`/`_primLt:`,
arithmetic over `_primAdd:` and friends, and S7 then deleted 39 superseded arms.

## 2. Why now — what it buys

- **The project's stated goal.** §1 of `numeric_tower_plan.md` opens by objecting that
  arithmetic "can never be seen or extended from Smol." The same objection applies
  verbatim to `String >> ,`, `Symbol >> asString`, `Float >> rounded`.
- **Resolves `open_questions.md` Q14** as a side effect rather than by building the
  builtin-selector registry that question contemplated: once a family's protocol lives in
  the class method dictionary, `respondsTo:` is accurate for it by construction.
- **Retires the shadowing hazard as a category.** A Smol method silently shadows a
  same-named Rust arm (`CLAUDE.md`), so every remaining duplicate is a latent divergence.
  S6 found `=` wrong and `~=` nearly contradicting it; S7 found 39 dead arms and a dead
  helper. Fewer arms, fewer places to diverge.

## 3. Inventory (measured 2026-08-02, post-S7)

40 public (non-`_prim`) arms remain on the scalar/namespace/class dispatchers:

| Family | Arms | Selectors |
|---|---|---|
| String | 10 → **0** | all migrated (S2 `= ~=`; S4 the protocol; S5 rendering) |
| Float | 6 → **0** | all migrated (S3 conversions; S5 rendering) |
| Int | 5 → **0** | all migrated (S1/S1b loops; S3 conversions; S5 rendering) |
| LargeInt | 3 → **0** | all migrated (S3 conversions; S5 rendering) |
| Symbol | 5 → **0** | all migrated — **`dispatch_symbol` deleted entirely** |
| Nil | 3 → **0** | all migrated — **`dispatch_nil` deleted entirely** |
| Namespace | 3 | `do: name asString` — routing-bound, §6 OQ-6 |
| Class | 4 | `= ~= name asString` — routing-bound, §6 OQ-6 |
| fallback | 1 | `error:` (+ `_primIdentical:`/`_primAsString` grounding added in S2) |

Also removed in S2 but not in the table above (it counts only scalar/namespace/class
dispatchers): `obj_prim_core`'s `=`/`~=` for instance receivers, replaced by the same
`Object >> =`.

Not counted: the `dispatch_object_primitive` chunks (`obj_prim_core`, `obj_prim_traits`,
…), which serve instances/entities and need their own inventory pass — see §6 OQ-6.

## 4. Mechanism

Per selector: define the method on the appropriate kernel class, grounded on a `_prim`
where the operation needs host support.

```smalltalk
String >> , aString  [ ^self _primConcat: aString ]
Float  >> rounded    [ ^self _primRounded ]
```

Rules carried from the tower:
- A `_prim` may only be sent to `self` (self-only rule), so grounding lives on the class
  that *is* the encoding; abstract code projects and re-dispatches (§4e of the tower plan).
- One `_prim` selector per operation, branching internally on the `Value::` variant, not
  a per-representation family (tower §4, RESOLVED S2).
- Delete the superseded Rust arm **in the same commit** as the Smol method, verifying
  deadness the way S7 did (static shadowing argument + instrumentation), so no arm is left
  to diverge silently.

## 5. Proposed sequencing

**The split is PROVISIONAL (noted 2026-08-02).** Both the phase boundaries below and the
34/35 division (OQ-6) are a starting point, not a decision — we begin here and revisit as
the work reveals what actually clusters. Expect to re-cut it; do not treat a phase
boundary as a commitment, and re-plan rather than forcing work into the wrong phase.

Each phase is independently committable and leaves the suite green.

- **S0 — Baseline.** Record current benchmark numbers and a selector-frequency profile
  (the S7 instrumentation technique). Information only; does not gate anything.
  **DONE 2026-08-02**, and it surfaced the S1a defect below. Note the bench file had been
  broken since C26 (two benches used the pre-C26 mutable-counter form and failed to
  *parse*), so there were no historical numbers to preserve; it now also carries loop
  coverage and a Pattern-3 `ValueHolder` bench. Baseline/after numbers are in §5a.
- **S1 — Dead code first.** Prune `to:do:`/`to:by:do:` from `dispatch_int` — they are
  already unreachable (§6 OQ-1). Zero behavior change; shrinks the surface before adding.
  **DONE 2026-08-02**: deadness re-verified by replacing both bodies with `panic!`
  (full suite still green), then deleted; `env` became unused in `dispatch_int`.
- **S1a — Method scope is lexical, not dynamic** (inserted 2026-08-02; not in the
  original plan). See §5b. Prerequisite to S1b: without it every tail-recursive Smol
  loop is O(n²) in time and O(n) in memory, so migrating `to:do:` off its evaluator
  special-case would have traded the one linear loop construct for a quadratic one.
- **S1b — `to:do:`/`to:by:do:` in Smol** (OQ-1, decided): write the methods on
  `Number`/`Integer`, delete step 1c **in the same commit** (never leave it as a silent
  fallback), and fix the future-in-loop-bounds bug (OQ-1b). Gated on the O(1) acceptance
  criteria above. This is the largest behavioral step in the chunk; everything after it is
  mechanical by comparison.
  **DONE 2026-08-02** — see §5c. `Smol_Patterns.md` Pattern 1 is updated. Its Pattern-3
  note (recursion at collection scale "exhausts the coroutine stack") is left ALONE and
  still unverified: non-tail recursion ran to depth 16000 with no exhaustion and roughly
  linear cost, but that is not collection scale. Measure before rewriting it.
- **S2 — Identity + simple families:** introduce `==` / `_primIdentical:` and
  `Object >> =`/`~=` in terms of it (OQ-3), then Nil, Symbol, Namespace, Class (15 arms).
  Settling identity first is what lets the per-variant `=`/`~=` arms go, and it makes S6's
  numeric `=` bounce rest on one uniform root rather than scattered Rust arms.
  **Identity + Nil + Symbol DONE 2026-08-02** (see §5e). **Namespace and Class are NOT
  done** — see §5e's re-cut note; they need dispatch *routing* changes rather than method
  definitions, which is the same category of work OQ-6 defers.
- **S3 — Numeric conversions:** Float's `asInt ceiling floor rounded truncated`, Int/
  LargeInt's `asFloat asString` (11 arms). Pure `_prim` grounding, no protocol design.
  **DONE 2026-08-02** — see §5f.
- **S3a — `to:`/`until:` range duality** (inserted 2026-08-02; not in the original plan).
  Settles the `copyFrom:to:` convention found during S3 and completes the
  inclusive/exclusive pair across both the iteration and copy protocols. **DONE** — §5g.
- **S4 — String** (10 arms). Also folds in and retires `string_representation_plan.md`
  entirely, per OQ-5. `copyFrom:to:`'s convention is already settled by S3a, so its
  migration is now mechanical. **DONE 2026-08-02** — see §5h.
- **S5 — rendering + console output** across all families. **DONE 2026-08-04** — see
  §5i. OQ-4 resolved: `printString` is gone, replaced by `asString`/`asDebugString`,
  and console output moved to `print`/`printNl` over a new write primitive.
- **S6 — Prune + audit**, mirroring tower S7: delete superseded arms, verify irreducibility
  of what remains, update `docs/primitives.md`. **DONE 2026-08-04** — see §5j.

## 5a. Measurements (release, this machine)

`before` = `7a2702c` + S1; `after` = + S1a. The `to:do:` family is untouched by S1a
(it is the `Cont::ToDoStep` special-case, not a method) and is the S1b before-number.

| bench | before | after | |
|---|---|---|---|
| `timesRepeat 1000` | 189.7 ms | 16.87 ms | **11.2×** |
| `valueholder accumulate 1000` | 189.7 ms | 17.37 ms | **10.9×** |
| `object new 1000` | 187.5 ms | 16.95 ms | **11.1×** |
| `method send increment 1000` | 187.5 ms | 16.94 ms | **11.1×** |
| `fibonacci 20 iterative` | 480 µs | 361 µs | 1.33× |
| `to:do: 100000` | 50.7 ms | 51.2 ms | unchanged |
| `to:by:do: 100000` | 50.7 ms | 51.0 ms | unchanged |
| `nested to:do: 300x300` | 47.4 ms | 47.4 ms | unchanged |
| `array do: 10000` | 12.5 ms | 12.3 ms | unchanged |

The four ~188 ms entries were not measuring their nominal subjects at all — each was
dominated by the `1000 timesRepeat:` driving it. Test-suite side effects: whole suite
14.9 s → 10.4 s, `entity_stress_100x1000` 10.1 s → 4.1 s, `user_recursive_count_down`
4.10 s → 0.28 s, `times_repeat_1000` 4.08 s → 0.53 s.

## 5b. S1a — method scope is lexical, not dynamic (DONE 2026-08-02)

**Symptom.** Every tail-recursive Smol loop was O(n²) in time and O(n) in memory:
`^self loopA: n - 1` measured 0.21 / 0.86 / 3.60 / 20.58 s at n = 1000 / 2000 / 4000 /
8000 (×4 per doubling). `to:do:` was the sole linear loop in the language, precisely
*because* it is an evaluator special-case rather than a method.

**Not TCO.** Instrumenting `step_loop` over 2000 tail calls showed the `Cont` stack flat
at ≤8, `method_stack` flat at 3, `method_depth` flat at 3. TCO works correctly. What grew
was the **environment**: `env_frames` climbed 8 → 16 → … → 1024+.

**Root cause.** `push_method_call_frame` built a method activation's env as
`env.clone()` + `push_frame()` — the *caller's* env plus one frame — so method scope
nested into the caller's **dynamic** chain. `Env::get` walks that vec and every
`Env::clone` copies it, so each call cost O(depth). Ordinary returns were unaffected
(2000 sequential calls never exceeded 8 frames); it only compounded under TCO, where each
iteration's env is cloned from the previous iteration's rather than from a common base.

**It was a correctness bug too, not merely a performance one.** `eval.rs` already
documented the intended contract — step 4 of `lookup_var` is commented
`// 4. outer env (globals)` — but `lookup_outer_frames` walks every frame except the
innermost, which included all callers. So a callee could read its caller's temporaries:

```smalltalk
Integer >> callee [ ^hidden ]
Integer >> caller [ | hidden | hidden := 42. ^self callee ]
1 caller      "answered 42; now: undefined variable: hidden"
```

**Fix.** `Env::global_scope()` answers an env holding only the outermost frame (sharing
the same `Arc`, so globals stay live and writable). The three method-activation sites —
`push_method_call_frame`'s `Instance` and `Primitive` arms and `Evaluator::
invoke_class_method` — root there instead of cloning the caller. Blocks are the opposite
case and were deliberately left alone: `push_block_frame` clones `block.env`, which is
genuine closure capture, verified by tests that a block invoked inside an unrelated
method still sees its defining method's temps and arguments.

**Lesson for OQ-1's acceptance criteria.** Those criteria are stated in terms of *stack*
(a deep-iteration test plus a `step_probe` measurement). Both would have **passed** on a
Smol `to:do:` built over this defect — the `Cont` stack really does stay O(1) — while the
loop was quadratic in time and linear in memory. S1b therefore adds a third criterion:
**measured linear scaling** across at least three doublings of the trip count, since
stack flatness alone does not imply the loop is usable.

## 5c. S1b — `to:do:` / `to:by:do:` are methods now (DONE 2026-08-02)

Three methods on `Number` (`kernel.smol`), and the evaluator special-case is gone:
step (1c) of `dispatch_to_stack` **and** the `Cont::ToDoStep` variant with its
`step_loop` arm were deleted in the same commit, so nothing can silently fall back.

```smalltalk
Number >> to: stop do: aBlock
Number >> to: stop by: step do: aBlock         "validates step, picks a worker"
Number >> pvtUpTo: stop by: step do: aBlock    "ascending worker"
Number >> pvtDownTo: stop by: step do: aBlock  "descending worker"
```

Two workers rather than one branching on `step > 0` per iteration: the sign is
loop-invariant, so deciding once at entry hoists two sends and a block evaluation out of
every iteration (2.33 s → 1.42 s at 100k, bringing `to:by:do:` level with `to:do:`) and
leaves each worker in exactly the `to:do:` shape.

**What it bought** (each covered by a test in `tests/kernel.rs`):
- `3 respondsTo: #to:do:` → `true` (was `false`). This is what closes Q14 for the
  construct: the method dictionary is the answer, no builtin-selector registry needed.
- `Float` and `ArbitraryInteger` receivers iterate. Both previously answered
  `doesNotUnderstand` on the tower's most-used iteration construct, because (1c)
  matched only `Value::Int`. Defining on `Number` fixed both at once.
- It can be overridden by a `Number` subclass.
- **OQ-1b is fixed**: a `Future` in the receiver, limit *or* step position of either
  selector now resolves. (1c) called Rust `as_int` on the bound and rejected a Future
  outright. The Smol version compares bounds with ordinary sends, which force futures —
  so this dissolved rather than needing a separate fix, as OQ-1b anticipated. Tested in
  all three positions anyway, per that entry.

**Acceptance criteria — all three met.**
1. Deep iteration: 100k iterations and nested loops, no stack exhaustion.
2. `step_probe_nested_loops`: 5-deep nesting measures `depth = 1, per-level delta = 0 B`
   (it allowed ≤2 under `ToDoStep`).
3. Linear scaling, release: 0.35 / 0.68 / 1.36 / 2.69 s at 25k / 50k / 100k / 200k —
   ×2 per doubling across three doublings.

**Behaviour change, deliberate.** A zero step raised `EvalError::TypeError` from Rust;
it now raises a **condition**, which is catchable and inspectable from Smol. One test
was updated to encode the new contract and one added to demonstrate the catch.

**Note the method signals via `Error signal:`, not `self error:`.** `error:` sent to a
*primitive* receiver is uncatchable — dispatch step (1d) gates flattened signal dispatch
on an `Instance` receiver, so a Number leaf falls through to
`dispatch_primitive_fallback`'s bare `EvalError::Signal`, bypassing the handler chain.
`[3 error: 'x'] on: Error do: [...]` does not catch; `[Error signal: 'x'] on: Error do:
[...]` does. **This is a pre-existing defect, not introduced here**, and it affects other
kernel methods whose `self` may be a leaf — `Integer >> factorial` raises an uncatchable
error today for the same reason. `error:` is the `fallback` row of §3's inventory and is
S6's to resolve; the fix should make `error:` on a primitive route through the same
signal machinery as on an instance, after which kernel code can go back to `self error:`.

**Cost, recorded not vetoed** (§1). `to:do:` was the only linear loop in the language
*because* it was a special-case; it is now ordinary sends. The loop family pays for it;
nothing else moves.

| bench | after S1a | after S1b | |
|---|---|---|---|
| `to:do: 100000` | 51.2 ms | 1.56 s | 30× slower |
| `to:by:do: 100000` | 51.0 ms | 1.42 s | 28× slower |
| `nested to:do: 300x300` | 47.4 ms | 1.27 s | 27× slower |
| `array do: 10000` | 12.3 ms | 134 ms | 11× slower |
| `timesRepeat 1000` | 16.87 ms | 16.76 ms | unchanged |
| `fibonacci 20 iterative` | 361 µs | 355 µs | unchanged |
| `valueholder accumulate 1000` | 17.37 ms | 18.01 ms | unchanged |
| `object new 1000` | 16.95 ms | 15.52 ms | unchanged |
| `method send increment 1000` | 16.94 ms | 15.95 ms | unchanged |

Test suite 10.4 s → 26.3 s, of which ~10 s is the two ≥100k acceptance tests themselves.
Note the whole-suite figure is still *below* the 14.9 s it stood at before S1a, because
S1a's 11× on method dispatch more than paid for S1b.

Per §1 this is information for the queued performance review, not an approval gate. The
recorded long-term answer is OQ-1's: implement in Smol first, then inline the compiled
fast path when the method has not been overridden — which is why `to_do_can_be_overridden`
exists as a test, since that inlining must fall back to a real send when it has.

One practical consequence: `cargo bench -p interpreter` now takes ~15 minutes, dominated
by the three loop benches at 100k. S6 may want to shrink them, but not before the
before/after pair above is recorded — that comparability is the point of S0.

## 5d. Selector-frequency profile — where the 30× goes (S0 deliverable, taken after S1b)

Counting every dispatch during `0 to: 20000 do: [:i | i]` gives **13 message sends per
iteration** (200,000 sends / 15,382 iterations), against one Rust loop step before:

```
  30763  value          2× — block evaluation inside ifFalse: / ifTrue:ifFalse:
  15382  <  <=  _primLt:  ifFalse:  not  to:do:
  15381  +  _primAdd:  ifTrue:ifFalse:  isEncodedNumber  value:
```

**`to:do:` itself is 1 of the 13.** `self <= stop` costs **5** (`<=` → `Ordered`'s
`(other < self) not` → `<` → `value` projecting the argument → `_primLt:` → `not`) and
`self + 1` costs **4** (`+` → `isEncodedNumber` → `ifTrue:ifFalse:` → `_primAdd:`). Nine
of thirteen are the **numeric tower's** abstraction cost, which every piece of Smol
arithmetic was already paying; S1b merely put it in a loop that runs it 100k times.

Dividing out: **≈1.2 µs per message send**. That is the figure worth attacking, and it
is universal rather than loop-specific. For the queued performance review, in order of
expected leverage:

1. **Per-send cost.** A tree-walking interpreter is usually 50–200 ns/send, so this looks
   ~10× off. HYPOTHESIS, not yet measured: every method activation allocates a fresh
   `Arc<Mutex<HashMap>>` env frame (`push_frame`) plus a `Vec` (`global_scope`) and hashes
   `self` and each parameter into it — ~3–4 heap allocations and several string hashes per
   send — and every `Cont` carries an `Env`, cloning that `Vec` again. The conventional fix
   is resolving names to integer slots at parse time and using a flat `Vec` frame, which
   removes the allocation and the hashing together. S1a is the precedent for how much a
   single structural fix in this area is worth (11× on dispatch).
2. **Sends per operation.** OQ-1 already records the answer — inline the compiled fast
   path when the method has not been overridden — and it applies to `<` and `+` on the
   leaves at least as much as to `to:do:`.

## 5e. S2 — identity, and why Namespace/Class did not follow (2026-08-02)

```smalltalk
Object >> == other [ ^self _primIdentical: other ]
Object >> ~~ other [ ^(self == other) not ]
Object >> =  other [ ^self == other ]
Object >> ~= other [ ^(self = other) not ]
```

Grounded twice, not per family: `_primIdentical:` lives in
`dispatch_primitive_fallback` (which now takes the receiver, so every primitive kind
shares one implementation) and in `obj_prim_core` (instances). Eight arms went:
`=`/`~=` for Nil, Symbol, String and instances. Symbol's `asString`/`asSymbol` went too —
`asSymbol` needed no primitive at all, since a Symbol already is one.

**`==` required a lexer change.** `('=', '=')` was missing from the two-char operator
table, so `a == b` lexed as two `=` tokens and `kernel.smol` failed to *parse*. `('~','~')`
was already there, but nothing implemented `~~`, so it could only answer
doesNotUnderstand — S2 defines it as the negation of `==`, completing the pair.

**Class identity had to join the synchronous list.** A class object is an entity, so a
behavioural send answers a Future; `=`/`~=` were already special-cased as synchronous in
`dispatch_class`. Introducing `==` without adding it there made `Array == Array` answer a
**Future** while `Array = Array` answered a Bool. `==`/`~~` now sit alongside.

**The `==` semantics decision** (OQ-3's open sub-question) is recorded in full in the
`kernel.smol` comment above `Object >> ==`. In short: structural for encoded scalars
(`'abc' == 'abc'` is true, unlike Pharo), same-allocation for anything heap-allocated
**including ValueObjects** (two equal Associations are not `==`; that is `=`'s job, and
keeping `==` non-recursive is what keeps it O(1)). `2 == 2.0` is false while `2 = 2.0` is
true — the clearest illustration of why both selectors exist.

**Re-cut: Namespace and Class move out of S2.** Both were listed here, but neither is a
matter of writing a method:

- **Namespace** is not routed at all — `lookup_user_method` answers `None` for
  `Value::Namespace`, and there is no `Namespace` class in the kernel. Migrating it means
  making Namespace a first-class kernel class (with the `value_to_class_handle` /
  `is_encoded_leaf_name` reflection consequences, and a decision about whether it is
  subclassable), adding a routing arm, and grounding `name`. Its `do:` is *also* an
  evaluator special-case — step **(1g)**, with a correspondingly dead arm in
  `dispatch_namespace`, exactly the shape S1 and S1b found for `to:do:`. Unlike `to:do:`
  the operation is genuinely irreducible (it swaps evaluator state), so the honest target
  is `Namespace >> do: aBlock [ ^self _primNamespaceDo: aBlock ]` — reflection and a real
  method, without pretending the body can be written in Smol.
- **Class** does not reach `Object >> =` at all: `dispatch_class` resolves against
  CLASS-side methods via `lookup_class_method`, so migrating `=`/`~=`/`name` needs
  `Object class >>` definitions plus a decision about `_prim` self-sends from class-side
  methods. That is precisely the class-side surface OQ-6 defers to chunk 35.

So both belong with the routing/class-side work rather than with "simple families". Their
arms are left in place and correct; nothing is half-migrated.

## 5f. S3 — numeric conversions (DONE 2026-08-02)

Nine arms out, four `_prim`s in, and one arm that turned out to need no primitive:

```smalltalk
MachineInteger   >> asFloat  [ ^self _primAsFloat ]      ArbitraryInteger likewise
MachineInteger   >> asString [ ^self _primAsString ]     ArbitraryInteger likewise
Float >> truncated [ ^self _primTruncated ]   Float >> ceiling [ ^self _primCeiling ]
Float >> rounded   [ ^self _primRounded ]     Float >> floor   [ ^self _primFloor ]
Float >> asInt     [ ^self truncated ]
```

`asInt` was a fifth Rust arm performing the same `as i64` cast as `truncated`; it is
now one line of Smol saying exactly that. `_primAsString`/`_primAsFloat` live in the
shared `dispatch_primitive_fallback` and branch on the encoding (§4's one-prim-per-
operation rule) — `Symbol >> asString` and `MachineInteger >> asString` are one
operation over two encodings. The binary numeric prims (`_primLt:`, `_primAdd:`, …)
deliberately stay in the per-leaf dispatchers: those promote across two operands.

**Held to "no protocol design", deliberately.** Grounding sits on the LEAVES only, so a
`Number` wrapper still answers doesNotUnderstand to `asFloat` — exactly as before this
step. The §4e projecting forms (`Number >> asFloat [^self value asFloat]`,
`Integer >> rounded [^self]`, `Float >> asFloat [^self]`) are real additions and are
left for a later step rather than smuggled in here. Recorded so they are not forgotten.

`ArbitraryInteger >> asFloat` preserves the replaced arm's saturating behaviour
(`to_f64().unwrap_or(INFINITY)`) verbatim rather than erroring. Note that fallback is
sign-blind — a huge negative would answer `+inf` — but it is defensive-only, since
`to_f64` answers `Some(±inf)` out of range. Flagged for S6; a migration step should not
quietly change behaviour.

## 5g. S3a — the `to:` / `until:` range duality (DONE 2026-08-02)

**Correction first.** This section originally recorded String's `copyFrom:to:` as
"1-based and exclusive". That was wrong. The Rust arm did `chars[(from-1)..to]`, and
because the slice's exclusive upper bound cancels the decremented lower bound, the net
semantics were **1-based INCLUSIVE** — the standard Smalltalk-80/Pharo convention.
`'abcdef' copyFrom: 1 to: 3` → `'abc'` was correct *for that convention*. The mistake was
reading the Rust slice mechanics as the Smol-level contract.

So the real disagreement was narrower: `String` was 1-based inclusive while `Array`,
`ByteArray` and `OrderedCollection` are 0-based inclusive. Both inclusive; different base.
One genuine bug came with it — `saturating_sub(1)` made `copyFrom: 0` indistinguishable
from `copyFrom: 1`, silently accepting an out-of-convention index.

**Decision: 0-based, inclusive**, and String now matches. The argument is not taste — the
language's own vocabulary already means exactly this:

- `at:` is 0-based (`Array >> first [ ^self _primAt: 0 ]`).
- **`to:` is already inclusive**: `0 to: 5 do: […]` runs six times. So `copyFrom:to:`
  being inclusive is what makes it agree with `to:do:` rather than contradict it.

**And the exclusive reading gets its own name rather than losing.** Dijkstra's
[EWD831](https://www.cs.utexas.edu/users/EWD/transcriptions/EWD08xx/EWD831.html) makes a
real case for half-open ranges — trip count is `stop - start` with no `+ 1`, an empty
range is `start = stop` rather than `stop < start`, and adjacent ranges share a bound.
Rather than pick a winner, `until:` names it, which is what Ruby (`a..b` / `a...b`) and
Swift (`a...b` / `a..<b`) did with syntax; keyword selectors are the natural equivalent
here. The duality is completed across **both** the iteration and the copy protocol, so
there is no corner where only one reading is available:

```smalltalk
0 to: 10 do: [...]              0 until: 10 do: [...]
0 to: 10 by: 3 do: [...]        0 until: 10 by: 3 do: [...]
coll copyFrom: 1 to: 3          coll copyFrom: 1 until: 4
```

`Number >> until:do:` / `until:by:do:` are the `to:` methods with `<` for `<=` (and `>`
for `>=` on a negative step), so the TCO-recognised shape, the once-at-entry zero-step
validation and the acceptance criteria all carry over unchanged. **Iteration does not
delegate in either direction** — neither form pays an adjustment.

**For copying, the HALF-OPEN form is the implementation and the inclusive one derives**
(`^self copyFrom: start until: stop + 1`). This is Dijkstra's argument made concrete
rather than merely cited: a half-open copy needs no adjustment at all, since the length
*is* `stop - start`, whereas writing the inclusive form directly forces a `+ 1` for the
length and a `- 1` for the loop bound. The first cut had it backwards — `until:`
delegating to `to:` with a `- 1` — which made the cheap form pay for the expensive one.
Arithmetic sends per call, for `Array`:

| | before S3a | S3a first cut | now |
|---|---|---|---|
| `copyFrom:to:` | 3 | 3 | **2** |
| `copyFrom:until:` | — | 4 | **1** |

`OrderedCollection` does better still: `start until: stop do:` is already exactly the
range to copy, so its half-open form needs no adjustment arithmetic whatsoever. One
implementation per collection either way.

`String` is the one place the derivation still runs the other way, because
`copyFrom:to:` is still a Rust arm. **S4 should ground that primitive HALF-OPEN**, after
which String matches the collections and its `- 1` disappears.

**Blast radius was small**: no caller of `copyFrom:to:` inside `kernel.smol` at all, and
exactly one test asserted the old String behaviour (`'hello' copyFrom: 2 to: 4` → `'ell'`,
now `'llo'`). Out-of-range still answers `''` rather than erroring, as before.

Still Rust: `String >> copyFrom:to:` itself. S4 migrates it; the convention is now correct
so that migration is mechanical. `String >> copyFrom:until:` is already Smol, since it
just re-dispatches.

## 5h. S4 — the String protocol (DONE 2026-08-02)

Seven arms out; `dispatch_str` now holds only `_prim` grounding plus `printString`
(S5's). The grounding is genuinely irreducible — indexing into UTF-8, concatenating,
interning a Symbol all need host support:

```smalltalk
String >> size  at:  ,  reversed  asSymbol  includesSubString:
String >> copyFrom: start until: stop [ ^self _primCopyFrom: start until: stop ]
String >> copyFrom: start to: stop    [ ^self copyFrom: start until: stop + 1 ]
String >> isEmpty / notEmpty
```

**`at:` is now 0-based**, which is a behaviour change. It was 1-based
(`chars().nth(i - 1)`, the Smalltalk-80 convention) while every collection was
0-based. Leaving it would have made String incoherent with **itself** after S3a:
`'hello' copyFrom: 0 to: 0` answered `'h'` while `'hello' at: 0` was out of bounds and
`at: 1` answered `'h'` — one class, two bases. The retired plan's §3 specified 1-based
"as today"; this chunk supersedes it. One test asserted the old behaviour.

**`copyFrom:until:` is the primitive** (`_primCopyFrom:until:`), with `copyFrom:to:`
deriving, so String now matches the collections after S3a and the `- 1` that stood in
for it is gone.

**`isEmpty` was answering the wrong thing.** `'' isEmpty` was **false**: String had no
method dictionary to define it in, so the send fell through to
`Object >> isEmpty [^false]`. A wrong answer rather than a missing feature, and having
a real String class is exactly what makes it fixable.

**Q14 moves visibly.** The pinned assertion in `s5_responds_to_is_universal_across_value_kinds`
was `'s' respondsTo: #size` → **false**; it is now true, along with the rest of the
String protocol. The pin moved to `printString`, which is still Rust on every family
(S5). This is Q14 being closed family-by-family by construction, exactly as §2 claimed,
rather than by building a builtin-selector registry.

**OQ-5 discharged, but not by deleting.** `string_representation_plan.md` is marked
RETIRED with a per-section status table rather than removed, because three of its
sections are live design work chunk 34 does not cover: §4 construction contract, §5
immutability/`StringBuilder`, §6 `Character`.

**§4 is absorbed, not merely noted** — the plan was to fold that document in, so a live
defect it had already diagnosed does not get left behind. The hazard was real and wider
than String: **all seven** encoded leaves reached `Value class >> new`, which builds a
frozen `ValueObject` via `_primNewValue:`, so each answered something that *claimed* to
be a String/Integer/Float but was not the encoding. `String new size` failed with
"receiver has no indexed storage" and `String new = ''` was false.

The rule now: **a leaf answers its identity value when one exists that is honestly an
instance of that class, and refuses otherwise.** The test is `(X new) class = X`.

| | `new` | why |
|---|---|---|
| `String` | `''` | identity for `,` |
| `MachineInteger` | `0` | additive identity |
| `Float` | `0.0` | additive identity |
| `True` / `False` | `true` / `false` | the sole instance |
| `Symbol` | **refuses** | symbols are interned names; an "empty symbol" names nothing |
| `ArbitraryInteger` | **refuses** | `0 class` is `MachineInteger` — the two integer leaves are one domain and the small encoding wins, so there is no ArbitraryInteger zero to hand back |

None of the five needs a primitive: a literal already *is* the encoding. The two
refusals are refusals because the constraint **fails**, not for lack of imagination.

`new:` is refused for all seven. Indexed construction means `n` slots to fill, which
presumes mutability, and the retired plan's §5 rejects mutable strings outright. The
previous error for `String new: 3` came from deep inside the generic path ("requires
class to declare indexedSlots:") and explained none of that. Both refusals are ordinary
catchable conditions.

**Construction without mutability.** Refusing `new: n` would otherwise leave no way to
build a String programmatically except repeated `,`, so §4's `new: n withAll: aChar`
factory is absorbed too. Immutability forbids mutating a value after it exists; it does
not forbid *constructing* one — the repetition happens in a host buffer and the String is
born complete and frozen. That is precisely why `String new: 20` is refused while
`String new: 20 withAll: 'X'` is not: **you cannot get n empty slots to fill in later,
but you can get n filled ones.**

```smalltalk
String >> repeated: n             [ ^self _primRepeated: n ]   "one primitive"
String class >> new: n withAll: c [ ^c repeated: n ]           "pure Smol"
```

The primitive is INSTANCE-side because a `_prim` may only be sent to `self`, and
class-side `self` is the class rather than the fill value. That constraint forced the
better factoring: `repeated:` also handles multi-character strings, which
`new:withAll:` alone would not (`'ab' repeated: 3` → `'ababab'`). It follows the
answers-a-copy naming `reversed` already sets.

`withAll:` takes a **one-character String** until `Character` exists (§6), consistent
with `at:` already answering one-character Strings; both change together when that class
lands. Open-ended accumulation — building in a loop, or conditionally — still wants a
mutable `StringBuilder`/`WriteStream` (§5, open). Factories cover fixed-shape
construction; they do not replace the builder.

## 5i. S5 — rendering and console output (DONE 2026-08-04)

**OQ-4 resolved, and against its own premise.** That entry recorded `printString` as
misnamed because it *returns* rather than prints, and wondered whether renaming was
worth it. The evidence said the name was a symptom and the `println!` buried inside it
was the disease:

- The side effect was **already inconsistent** — 4 of 9 Rust arms had it (Int, String,
  instances, classes); Float, LargeInt, Symbol, Nil and Namespace did not; and the 9
  `printString` methods already written in `kernel.smol` *could not*, there being no
  primitive for it.
- It **leaked compositionally**. `Duration >> printString` is `^ms printString , 'ms'`,
  so rendering a Duration emitted a stray line for its milliseconds; an Association
  emitted two. Every composite rendering — which is how all the Smol ones are written —
  corrupted the console it was supposedly serving.

So rendering and printing are now separate, and both are ordinary Smol.

**Two renderings, because one is not enough.**

| | `asString` (bare) | `asDebugString` (disambiguating) |
|---|---|---|
| `'hi'` | `hi` | `'hi'` |
| `#hi` | `hi` | `#hi` |
| `3`, `2.5`, `nil`, `true` | `3` … | *(same — inherited from `Object`)* |

They differ only where a bare rendering would be ambiguous, so everything else inherits
`asDebugString` from `Object` and answers identically. Collections render their
**elements** with `asDebugString` for exactly that reason:
`#('hi' #hi 3) asString` → `('hi' #hi 3 )`.

**`print`/`printNl` emit the bare form**, and `Nl` is the newline rather than a different
rendering. Bare is the only **non-circular** choice: if printing decorated were the
default there would be no way to print a plain string, since converting first and then
printing would decorate the result again. Decorated output is therefore
conversion-first, which composes — `x asDebugString printNl`. Both answer `self`, so
printing chains.

The write primitives live on `String` rather than `Object` because a `_prim` may only be
sent to `self`; `Object >> print` converts and delegates to `String >> print`. This is
the **only** channel by which Smol writes to the console — there was none before other
than the side effect being removed here.

**Two dispatchers disappeared.** `dispatch_nil` and `dispatch_symbol` were left holding
nothing but the shared fallback, so both functions are deleted and the top-level match
routes straight to `dispatch_primitive_fallback`. Every scalar family is now at **zero**
public arms (§3).

**Three things this shook out, each a real defect:**
1. `ByteArray` genuinely has *both* renderings — `asString` decodes the bytes as UTF-8
   (a conversion, like `asSymbol`), `asDebugString` lists them. A blind
   `printString`→`asString` rename briefly collapsed the two.
2. `_primAsString` is now **one operation, total over all receivers**, so the
   byte-decoding chunk had to *decline* rather than error when there is no byte storage,
   letting the generic `a ClassName` default answer. Two tests asserted the old erroring
   behaviour.
3. `asString`/`name` on a **class** had to join `dispatch_class`'s synchronous selector
   list — the same trap S2 hit with `==`. Without it `'x: ' , Array asString` would
   concatenate a **Future**.

## 5j. S6 — prune and audit (DONE 2026-08-04)

**`error:` fixed — the defect S1b had to work around.** It had *three* Rust
implementations: `dispatch_primitive_fallback`, `obj_prim_conditions`, and an evaluator
special-case at step (1d). The first was broken — it returned a bare
`EvalError::Signal` that bypassed the handler chain, so `[3 error: 'x'] on: Error do:
[…]` did not catch while `[Error signal: 'x'] on: Error do: […]` did. The casualty was
real: `Integer >> factorial`'s domain error was raised on an encoded leaf and so could
not be caught at all.

All three are gone, replaced by one Smol method:

```smalltalk
Object >> error: aMessage [ ^Error signal: aMessage ]
```

Every receiver kind now routes through the same condition machinery, and S1b's
`to:by:do:` / `until:by:do:` are back to the idiomatic `self error:` rather than the
`Error signal:` they needed while this was broken.

**Dead arms pruned.** `dispatch_largeint` still carried public `+`/`-`/`*`, superseded
by the tower's `ArbitraryInteger >> +` → `_primAdd:`. Verified dead by `panic!` probe
(full suite green), then deleted along with its now-orphaned `bigint_from_value` helper —
the same static-argument-plus-instrumentation method tower S7 used.

**Sign-blind saturation fixed.** `ArbitraryInteger >> asFloat`'s `unwrap_or(INFINITY)`
would have answered `+inf` for a huge *negative*. Verified unreachable (`to_f64` answers
`Some(±inf)` out of range), but made sign-correct rather than left as a latent wrong
answer.

**Final audit.** Zero public arms remain on any scalar dispatcher. Ten remain overall,
every one in a category §6 OQ-6 defers to chunk 35, none of them scalar:

| dispatcher | public arms | why still Rust |
|---|---|---|
| `obj_prim_core` | `respondsTo:` | instance-side reflection |
| `obj_prim_history` | `historyOf:` `historyOf:from:to:` | instance-side, store-backed |
| `dispatch_namespace` | `do:` `name` `asString` | not routed by `lookup_user_method` |
| `dispatch_class_builtin` | `=` `~=` `name` `asString` | class-side lookup, not `Object >>` |

Against §3's original inventory of 40: **33 migrated, 7 deferred** with recorded
reasons. 64 `_prim` grounding arms remain, which is the point — they are the
irreducible host surface, inventoried in `docs/primitives.md`.

## 6. Open questions

All settled as of 2026-08-02 except **OQ-2** (minor, statable during implementation) and
**OQ-4** (deferred by design, sequenced last). Decisions are recorded inline below with
the reasoning, including options rejected.

### OQ-1 — `to:do:` is an evaluator special-case, not a method — RESOLVED (S1b, 2026-08-02)

**Finding (verified 2026-08-02).** There is **no `to:do:` method anywhere** — not in
`kernel.smol`, not in Rust dispatch. `0 to: 5 do: […]` parses as an ordinary message
send; evaluating it reaches `dispatch_to_stack`, where step **(1)** `lookup_user_method`
finds nothing, and step **(1c)** (`eval_iterative.rs:1885`) hardcodes
`if let Value::Int(n) = receiver` + `match selector` and pushes `Cont::ToDoStep`
(`eval_iterative.rs:952`) — a self-pushing continuation running one block frame per
iteration on the heap `Cont` stack (C31.5).

So `to:do:` is a **selector special-cased in the evaluator**: a syntactic form wearing a
message send's clothing. Measured consequences:

```
3 respondsTo: #to:do:              →  false
1.5 to: 3 do: [:i | i]             →  does not understand #to:do:
(a LargeInt) to: 3 do: [:i | i]    →  does not understand #to:do:
```

It is invisible to reflection, cannot be overridden, cannot be inspected, and silently
exists only for `MachineInteger` receivers — `Float` and `ArbitraryInteger` MNU on the
tower's most-used iteration construct. This is the Q14 observability complaint in its
purest form, and a stronger argument for migrating than a mere "Rust arm" would be.

**Also dead:** `builtins_iterative.rs:1219`/`1230` still hold the *old* Rust `while`-loop
implementation, unreachable since 1c intercepts first. Verified three ways: S7
instrumentation never logged them despite ~25 kernel call sites; replacing both bodies
with `panic!` left all 810 tests passing; and the one plausible bypass (a receiver that is
not yet an `Int`, e.g. a Future) routes through Future's DNU → `sendTo:` → re-dispatch →
back into 1c. Pruning them is uncontroversial (S1 above).

**The ordering hazard.** Step 1 precedes step 1c, so *defining `Integer >> to:do:` in Smol
would silently shadow the flattening* — loops would stop using `Cont::ToDoStep` with no
error. Exactly the failure mode this chunk exists to retire, so 1c must be deleted in the
same commit as the Smol method, never left as a fallback.

**DECIDED (2026-08-02): option (b) — write the method in Smol, delete step 1c.**
Not "move a method": *write the method that never existed* and remove an evaluator
special-case. Rejected: (a) keep the special-case — leaves the construct unreflectable and
`MachineInteger`-only, the exact complaint this chunk answers; (c) Smol method delegating
to a `_primToDo:` that still pushes `Cont::ToDoStep` — buys reflection without making the
loop modifiable, so it pays the cost of the change without the benefit.

**Prior art is real and directly applicable** (verified, not assumed). `Block >>
whileTrue:` is already pure Smol and tail-recursive, carried by the TCO + inline block
dispatch built in C29.6:

```smalltalk
Block >> whileTrue: aBlock [
    self value ifFalse: [ ^nil ].
    aBlock value.
    ^self whileTrue: aBlock ]
```

`to:do:` takes the same shape. Note the classic counter form
(`i := self. [i <= stop] whileTrue: [… i := i + 1]`) is **not** available — method temps
cannot be assigned inside blocks — so recursion is the idiom here, as it was for S4's
`gcd:`/`factorial`:

```smalltalk
Number >> to: stop do: aBlock [
    self <= stop ifTrue: [ aBlock value: self. ^(self + 1) to: stop do: aBlock ].
    ^nil ]
```

Defining it on `Number`/`Integer` rather than one encoding **fixes the Float/LargeInt gap
for free**.

**O(1) stack is the acceptance criterion, not an expectation.** The step does not land
until all three hold:
1. A **deep-iteration test** that exhausts the Rust stack if the loop is not O(1) —
   ≥100k iterations, plus nested loops (the release coroutine stack is 64 KB, so a
   per-iteration Rust frame fails fast and unambiguously).
2. A **`step_probe` measurement** (the debug-only Cargo feature built for exactly this)
   showing `step_loop` depth and per-level Rust-stack delta stay constant as iteration
   count and nesting grow.
3. **Measured linear time scaling** across at least three doublings of the trip count.
   Added after S1a (§5b): criteria 1–2 would both have passed while the loop was
   quadratic, because the growth was in the *environment*, not the stack. Stack flatness
   does not imply the loop is usable.

If either fails, **we fix the situation — we do not fall back to (a)** (decided
2026-08-02). A loop whose Rust stack grows with trip count is a defect in the evaluator's
flattening, not a reason to retreat to a special-case: the machinery already carries
`whileTrue:` iteratively, so a failure here means something specific is wrong and
diagnosable. Ship neither the regression nor the retreat — fix it.

**Pharo precedent, and the future optimization (RECORDED, not built).** Pharo has
`to:do:` as a real method but notes it is "normally compiled inline, and therefore not
overridable" — i.e. the same trade, resolved at the compiler rather than in the
evaluator's dispatch path. The intended long-term shape here is the strictly better one:
**implement in Smol first, then inline the compiled fast path when the method has not been
overridden**, falling back to the real send when it has. That keeps observability and
modifiability as the default and treats speed as an optimization over a correct semantics —
rather than Pharo's position, where inlining silently wins and the method is a fiction.
Depends on a compiler/JIT step that does not exist. Cross-ref: the queued performance
review.

*Why allow override at all?* Rarely to actually override it — the point is that the
construct is **inspectable and modifiable in a live image** like everything else, rather
than being a privileged form the language cannot describe. A domain `Number` subclass
wanting different iteration is the secondary benefit.

### OQ-1b — futures are not awaited in loop bounds — RESOLVED (S1b, 2026-08-02)

```
1 to: aResolvedFuture do: [:i | i]   →   TypeError("to:do:: argument must be Int")
```

Step 1c calls Rust `as_int` on the bound, which rejects a Future rather than awaiting it —
contradicting the asynchronous-everywhere principle (`CLAUDE.md`: futures "REQUIRE very
late resolution... diagnose what part of the evaluator made an incorrect assumption").
**DECIDED: fix it in this chunk (step S1b).** It lives in the code OQ-1 replaces, and
option (b) largely dissolves it — a Smol `to:do:` compares bounds with ordinary sends,
which already force futures — but "largely" is not "verifiably", so it gets its own test
either way: a future in the receiver, bound, and step positions of both `to:do:` and
`to:by:do:`. If any residual `as_int`-style eager coercion survives on a loop path, that
is the evaluator making an incorrect assumption about futures (`CLAUDE.md`) and is fixed
at the seam, not by resolving the future early.

### OQ-1c — stale kernel comments about the retired recursive evaluator — RESOLVED (S1b)

`kernel.smol:1284` and `:1395` still describe `_primWhileTrue:` / `_primTimesRepeat:`
Rust arms as "direct fallbacks for the old (recursive) evaluator". Doubly stale: the
recursive evaluator was retired 2026-07, and those arms no longer exist anywhere in
`crates/interpreter/src/` (verified). Fix alongside S1b, since it is the same
loop-machinery documentation.

### OQ-2 — do `_prim` selectors need to be visible to `respondsTo:`? — ANSWERED (S6): no

After migration, `respondsTo:` answers accurately for public protocol. `_prim*` selectors
remain invisible (they are Rust arms by construction).

**Stated and settled (S6): that is correct, not a gap.** A `_prim` is not protocol — it
is the grounding a protocol method sends to, and it is deliberately not part of any
class's public surface (the self-only rule already forbids sending one to anything but
`self`). `respondsTo:` answering *false* for `_primAsString` is the honest answer to
"is this part of what the receiver understands as protocol". Pinned as an assertion in
`s5_responds_to_is_universal_across_value_kinds` so the boundary is visible rather than
assumed, alongside the now-true `respondsTo:` answers for the migrated protocol.

This is what Q14 is closed on: `respondsTo:` is accurate for **public protocol** on
every scalar family, by construction, with no builtin-selector registry. It remains
inaccurate for the seven routing-bound arms (OQ-6), which is recorded rather than fixed.

### OQ-3 — `=` / `~=` and identity — DECIDED (2026-08-02)

**Follow Pharo's shape:** `==` is an identity **primitive**, and `Object >> =` is defined
in terms of it. Placement is **`Object`**, not `Value` — Values inherit it, and any class
needing domain equality overrides `=` with content equality.

```smalltalk
Object >> == other [ ^self _primIdentical: other ]
Object >> =  other [ ^self == other ]
Object >> ~= other [ ^(self = other) not ]
```

`==` does not exist in the language today (verified) — this chunk introduces it. That is a
language-level addition, not a refactor, and it is worth doing on its own merits: identity
is currently inexpressible from Smol.

**Fits what S6 already built.** The numeric `=` bounce (tower §4g) relies on "a non-number
answers with identity `=`" — which today comes from scattered per-variant Rust arms.
Routing that through one `Object >> =` makes the story uniform instead of coincidental.
`Number` keeps its overriding `=` (promoting, delegating), so `2 = 2.0` is unaffected.

**Sub-question to settle during implementation: what does `==` mean for an encoded
Value?** The existing `Value` `PartialEq` (`value.rs:71`) is a hybrid — pointer identity
for `Instance`/`Block`/`Future`/`Namespace`/`Handle`, but **structural** for
`Int`/`Float`/`Bool`/`Nil`/`Symbol`/`Str`/`LargeInt` (`Str` compares contents, and
`Int`↔`LargeInt` cross-compare numerically). So grounding `_primIdentical:` on it gives
`'abc' == 'abc'` → **true**, where Pharo answers false for two distinct String instances.

Recommendation: **keep the structural behavior and state it as the rule** — `==` is
"indistinguishable value" for Values and "same object" for Objects. That is not a
compromise, it is the project's Value/Object split applied honestly: an immutable,
shareable Value *has* no identity separate from its content, so asking whether two 2s are
"the same 2" is a question about the encoding, not the value. Pharo's answer is an
artifact of everything being a heap object. Decide explicitly rather than inheriting it
from `PartialEq` by accident.

### OQ-4 — `printString` is misnamed — RESOLVED (S5, 2026-08-04)

Recorded 2026-08-02: `printString` is `toString`-shaped — it *returns* a string rather than
printing — and its Rust arm contains a `println!` that is currently the only debugging
channel. Renaming, and separating "render to string" from "write to console", wants real
console support first. S5 above is sequenced last so this decision can land or be deferred
without blocking the rest.

### OQ-5 — string plan — DECIDED (2026-08-02): fold in and retire

`string_representation_plan.md` is small. Fold its remaining content into this chunk
(migration mechanics **and** the reparenting decisions) and retire the old plan with a
pointer here, rather than maintaining a split that has already caused one stale
cross-reference. Do this as part of S4.

### OQ-6 — routing-bound surfaces: object/entity, class-side, **and Namespace** — DECIDED (2026-08-02): next chunk

`dispatch_object_primitive`'s chunks and `dispatch_class_builtin` hold a much larger arm
count (construction, traits, tx, futures, reflection). This work **is wanted** — the same
observability argument applies — but it is separated into a **following chunk (35)** to
keep this one reviewable. It needs its own inventory pass first, and its own judgement
about which arms are genuine VM substrate (`_primNew`, tx enrollment, coroutine handling)
versus ordinary protocol that happens to live in Rust (`name`, `printString`,
`respondsTo:`).

**Scope widened after S2 (2026-08-02): the Namespace and Class arms join this question.**
They were originally scheduled in S2 as "simple families", which was wrong — what unites
this chunk's remaining families (Nil, Symbol, String, the numeric leaves) is that
`lookup_user_method` already routes them to a kernel class, so migrating is purely a
matter of writing methods. Namespace and Class are not routed that way, so each needs a
dispatch change *before* any method can be written. That is the same precondition the
object/class-side arms have, which is why they belong together.

**Class — 4 arms (`= ~= name printString`).** A class receiver never reaches
`Object >> =`: `dispatch_class` resolves against CLASS-side methods
(`lookup_class_method`), so the migration target is `Object class >> =` / `== ` / `name`,
not instance-side methods. Open sub-questions: can a class-side method send a `_prim` to
`self` (the self-only rule vs. a `ClassEntity` receiver, which may or may not be a
self-send depending on `current_entity_id`), and does the synchronous-selector list in
`dispatch_class` need to grow for every migrated selector, or should class-side dispatch
of pure-function selectors be synchronous by default? Note S2 already had to add
`==`/`~~` to that list; each further migration risks the same
"answers a Future where a Bool was expected" trap.

**Namespace — 3 arms (`do: name printString`), and it is not a class at all.** There is
no `Namespace` class in `kernel.smol`, and `lookup_user_method` answers `None` for
`Value::Namespace`. Migrating requires:
1. deciding what a `Namespace` class *is* in the hierarchy — presumably a
   `Value vmSubclass:` encoded leaf like `String`/`Symbol`, which pulls in
   `is_encoded_leaf_name` and `value_to_class_handle` (so `aNamespace class` answers
   something sensible) and a decision on whether it is subclassable;
2. a `Value::Namespace` arm in `lookup_user_method`;
3. `_primNamespaceName` grounding for `name`.
4. **`do:` is an evaluator special-case — step (1g)** in `dispatch_to_stack`, which swaps
   the active namespace and pushes `Cont::NamespaceGuard`. As with `to:do:` before S1b,
   the arm in `dispatch_namespace` behind it is therefore **dead**, and should be
   verified-and-pruned the way S1 did. Unlike `to:do:`, though, the operation is
   genuinely irreducible — it manipulates evaluator state — so the honest target is
   `Namespace >> do: aBlock [ ^self _primNamespaceDo: aBlock ]`. That is OQ-1's rejected
   option (c) — reflection without modifiability — and it is the RIGHT answer here
   precisely because the body cannot be written in Smol, which was not true of `to:do:`.

Until then all seven arms stay in place and correct; nothing is half-migrated. §3's
inventory still counts them.

## 7. Cross-references

`numeric_tower_plan.md` (the worked example; §4e seam, §4g coercion),
`string_representation_plan.md` (superseded for migration mechanics),
`primitives.md` (primitive inventory, to be updated per phase),
`open_questions.md` Q13 (standalone `uses:` — separate trait round), Q14 (resolved by this
chunk), Q15 (numeric `=`).
