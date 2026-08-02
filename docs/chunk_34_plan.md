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
| String | 10 | `, at: copyFrom:to: includesSubString: reversed size asSymbol = ~= printString` |
| Float | 6 | `asInt ceiling floor rounded truncated printString` |
| Int | 5 | `asFloat asString printString to:do: to:by:do:` |
| LargeInt | 3 | `asFloat asString printString` |
| Symbol | 5 | `= ~= asString asSymbol printString` |
| Nil | 3 | `= ~= printString` |
| Namespace | 3 | `do: name printString` |
| Class | 4 | `= ~= name printString` |
| fallback | 1 | `error:` |

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

Each phase is independently committable and leaves the suite green.

- **S0 — Baseline.** Record current benchmark numbers and a selector-frequency profile
  (the S7 instrumentation technique). Information only; does not gate anything.
- **S1 — Dead code first.** Prune `to:do:`/`to:by:do:` from `dispatch_int` — they are
  already unreachable (§6 OQ-1). Zero behavior change; shrinks the surface before adding.
- **S1b — `to:do:`/`to:by:do:` in Smol** (OQ-1, decided): write the methods on
  `Number`/`Integer`, delete step 1c **in the same commit** (never leave it as a silent
  fallback), and fix the future-in-loop-bounds bug (OQ-1b). Gated on the O(1) acceptance
  criteria above. This is the largest behavioral step in the chunk; everything after it is
  mechanical by comparison.
- **S2 — Identity + simple families:** introduce `==` / `_primIdentical:` and
  `Object >> =`/`~=` in terms of it (OQ-3), then Nil, Symbol, Namespace, Class (15 arms).
  Settling identity first is what lets the per-variant `=`/`~=` arms go, and it makes S6's
  numeric `=` bounce rest on one uniform root rather than scattered Rust arms.
- **S3 — Numeric conversions:** Float's `asInt ceiling floor rounded truncated`, Int/
  LargeInt's `asFloat asString` (11 arms). Pure `_prim` grounding, no protocol design.
- **S4 — String** (10 arms). Also folds in and retires `string_representation_plan.md`
  entirely, per OQ-5.
- **S5 — `printString` / `asString`** across all families, pending the naming decision
  (OQ-4) — deliberately last, because it may rename the selector.
- **S6 — Prune + audit**, mirroring tower S7: delete superseded arms, verify irreducibility
  of what remains, update `docs/primitives.md`.

## 6. Open questions

All settled as of 2026-08-02 except **OQ-2** (minor, statable during implementation) and
**OQ-4** (deferred by design, sequenced last). Decisions are recorded inline below with
the reasoning, including options rejected.

### OQ-1 — `to:do:` is an evaluator special-case, not a method *(needs discussion)*

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
until both hold:
1. A **deep-iteration test** that exhausts the Rust stack if the loop is not O(1) —
   ≥100k iterations, plus nested loops (the release coroutine stack is 64 KB, so a
   per-iteration Rust frame fails fast and unambiguously).
2. A **`step_probe` measurement** (the debug-only Cargo feature built for exactly this)
   showing `step_loop` depth and per-level Rust-stack delta stay constant as iteration
   count and nesting grow.

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

### OQ-1b — futures are not awaited in loop bounds *(BUG — in scope, see S1b)*

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

### OQ-1c — stale kernel comments about the retired recursive evaluator *(cleanup)*

`kernel.smol:1284` and `:1395` still describe `_primWhileTrue:` / `_primTimesRepeat:`
Rust arms as "direct fallbacks for the old (recursive) evaluator". Doubly stale: the
recursive evaluator was retired 2026-07, and those arms no longer exist anywhere in
`crates/interpreter/src/` (verified). Fix alongside S1b, since it is the same
loop-machinery documentation.

### OQ-2 — do `_prim` selectors need to be visible to `respondsTo:`? *(minor)*

After migration, `respondsTo:` answers accurately for public protocol. `_prim*` selectors
remain invisible (they are Rust arms by construction). Probably fine — they are internal
grounding, not protocol — but it should be stated, since Q14 is closed on the strength of it.

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

### OQ-4 — `printString` is misnamed *(deferred, but scheduled)*

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

### OQ-6 — object/entity/class-side surfaces — DECIDED (2026-08-02): next chunk

`dispatch_object_primitive`'s chunks and `dispatch_class_builtin` hold a much larger arm
count (construction, traits, tx, futures, reflection). This work **is wanted** — the same
observability argument applies — but it is separated into a **following chunk (35)** to
keep this one reviewable. It needs its own inventory pass first, and its own judgement
about which arms are genuine VM substrate (`_primNew`, tx enrollment, coroutine handling)
versus ordinary protocol that happens to live in Rust (`name`, `printString`,
`respondsTo:`).

## 7. Cross-references

`numeric_tower_plan.md` (the worked example; §4e seam, §4g coercion),
`string_representation_plan.md` (superseded for migration mechanics),
`primitives.md` (primitive inventory, to be updated per phase),
`open_questions.md` Q13 (standalone `uses:` — separate trait round), Q14 (resolved by this
chunk), Q15 (numeric `=`).
