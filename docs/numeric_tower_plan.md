# Numeric tower & primitive rework plan

**Status:** planning. Supersedes the "reparent scalars" portion of
`string_representation_plan.md` §2 for the *numeric* classes (String/Symbol
reparenting still tracked there). Depends on: traits landed (2026-07-11).

**Goal.** Replace the flat, VM-hidden scalar numerics with a real, observable,
extensible **numeric tower** expressed *in the language*, backed by a **minimal**
set of irreducible leaf primitives in Rust. This directly serves the project's
"observable and manipulable in the language" goal: today the small/large integer
split and all arithmetic live in hardcoded Rust (`dispatch_int`/`dispatch_float`)
and can never be seen or extended from Smol. After this chunk, the *concepts*
(Magnitude, Number, Integer, coercion) live in `kernel.smol`; Rust provides only
the operations that genuinely require machine/host support.

---

## 1. Motivation (measured baseline, 2026-07-12)

Current behavior demonstrates the problem:

- `'hi' class` → **MNU** — String/Float/Symbol/LargeInt have **no class object**.
- `42 isKindOf: Value` → **false** — `Int` is bootstrapped under `Object`, not
  `Value` (a correctness bug; Int is an immutable value).
- `2 + 3.5` → **error**, but `3.5 + 2` → `5.5` — arithmetic coercion is
  **asymmetric and incomplete** (Float arm coerces Int; Int arm rejects Float).
- `2 < 3.5` → **error** — comparison rejects cross-type operands.
- Small→large integer promotion works (`(big) class` today reports `Int`), but the
  split is invisible: there is no way to observe or reason about machine-word vs
  arbitrary-precision arithmetic from the language.

The unified "just call it Int" option was rejected: it permanently pushes
representation-hiding into the VM and forecloses ever exposing the distinction.

## 2. Hierarchy (DECIDED — Option A)

Everything remains ultimately under `Value` (fixes the `isKindOf: Value` bug):

```
Value
├── Magnitude              comparison protocol: <, <=, >, >=, max:, min:, between:and:
│   └── Number             arithmetic + coercion: + - * / , generality/coerce:/retry:coercing:
│       ├── Integer        gcd:, factorial, //, \\, even, odd, bit ops, isPrime …
│       │   ├── MachineInteger     fixed machine word (Value::Int, i64)
│       │   └── ArbitraryInteger   heap arbitrary-precision (Value::LargeInt, BigInt)   [name OPEN — see §3]
│       └── Float          (Value::Float, f64)
├── Boolean ── True / False    (already in kernel.smol under Value)
├── String                     (Value::Str)    — direct Value leaf, NOT Magnitude
└── Symbol                     (Value::Symbol) — direct Value leaf
```

Rationale:
- `Magnitude` = "comparable"; comparison protocol is written **once** in terms of
  `<` and `=`, inherited by every comparable. (`Character`, when §6 of the string
  plan lands, joins here.)
- `Number` = arithmetic + the **coercion protocol**; fixes the asymmetric-coercion
  bug generically rather than per-arm.
- `Integer` holds integer-only behavior shared by both leaves.
- The two leaves differ **only** in representation/performance
  (`MachineInteger` = one word, `ArbitraryInteger` = heap, unbounded). Both answer
  `isKindOf: Integer` → true; `class` distinguishes them. Overflow promotes a
  `MachineInteger` result to `ArbitraryInteger`; underflow (a big result that fits a
  word) demotes back (current `normalize_bigint`).
- `String`/`Symbol` are **not** Magnitudes (Smalltalk keeps strings in the
  collection world). They stay direct `Value` leaves; their reparenting/method
  build-out is tracked in `string_representation_plan.md` §3.

The two Integer leaves being distinct enum variants (`Value::Int` vs
`Value::LargeInt`) means the representation **can** carry this one class
distinction — unlike user scalar subclasses, which the unboxed encoding cannot tag
(`string_representation_plan.md` §7). So the non-subclassable-scalar rule is not
violated: these are *built-in* leaves, not user subtypes, and users still cannot
subclass any of them.

## 3. Naming (DECIDED 2026-07-21)

- Fixed-width leaf: **`MachineInteger`** (`Value::Int`, i64).
- Arbitrary-precision leaf: **`ArbitraryInteger`** (`Value::LargeInt`, BigInt).
- Bare **`Int` is retired**: the abstract shared class is **`Integer`**; integer
  **literals produce `MachineInteger`**, promoting to `ArbitraryInteger` on overflow.
  The kernel class formerly named `Int` becomes abstract `Integer`; all `Int`
  references (kernel.smol, bootstrap, `value_to_class_handle`, tests) migrate.

## 4. Primitive rework — philosophy

**Principle:** Rust exposes only the *irreducible* operations — those that need
machine arithmetic, host representation, or overflow detection. Everything
composable is written in Smol on the tower classes. `dispatch_int`/`dispatch_float`
shrink from "the implementation" to "a thin primitive layer."

**Stays in Rust (irreducible primitives, `_prim…`):**
- Arithmetic that needs machine/BigInt ops with overflow→promotion:
  `_primAdd:`/`_primSub:`/`_primMul:`/`_primQuo:`/`_primRem:`. **OPEN (S2):**
  whether these split per-representation (`_primMachineAdd:`/`_primLargeAdd:`) or a
  **single** `_prim…` branches internally on the `Value::` variant of the receiver
  (Int vs LargeInt vs Float). Leaning **single-selector, internal branch** — the
  Rust primitive already has the receiver value and can dispatch on its variant, so
  per-rep selectors are likely needless ceremony. Decide when S2 lands.
- Same-representation comparison: `_primLt:` (+ `=`), same single-vs-split question.
- Float ops that need libm/host: `sqrt`, `sin`, `ln`, `truncated`, `rounded`,
  `asFloat`.
- Conversion feeding coercion: `asFloat`, `largeFitsWord`/`_primNormalize`
  (demote-if-fits), `machine→large`.
- Radix/parse/print primitives as needed.

Most of this **reuses arithmetic already written** inside `dispatch_int`/
`dispatch_float` — S2–S4 mostly **relocates** that logic behind `_prim…` selectors
and deletes the broad fallback arms (S5 prune), not new math.

**Moves to Smol (`kernel.smol`, on the tower):**
- All of `Magnitude` (`<=`, `>`, `>=`, `max:`, `min:`, `between:and:`) in terms of
  `<`/`=`.
- **`Number` coercion via double-dispatch (DECIDED — Squeak/Pharo `adaptTo…`
  school, NOT a central `generality` table).** Public `+ - * / < …` on a leaf: if
  the arg is the same kind, call the same-rep primitive; otherwise
  `^arg adaptToInteger: self value andSend: #+` (and `adaptToFloat:andSend:` etc.).
  A new number type (`Fraction`, `Complex`, user type) integrates by implementing
  its own `adaptTo…:andSend:` — **no central generality ordering to edit.** Chosen
  over blue-book `generality`/`coerce:`/`retry:coercing:` because it is the *more
  open* mechanism (no global table), matching Smol's ethos. Fixes `2 + 3.5` /
  `2 < 3.5` symmetry.
- **The `value` projection seam (DECIDED).** `Integer >> value` is a
  `subclassResponsibility`; leaves answer `^self` (`MachineInteger >> value [^self]`),
  a user subclass answers its ivar (`AuditedInteger >> value [^amount]`). Abstract
  `Integer` arithmetic/derived methods route through `value` + same-rep primitives,
  so a **user `Integer` subclass implements essentially ONE method (`value`)** and
  inherits all of `Integer` (`even`, `gcd:`, `to:do:`, `factorial`, coercion). This
  is what makes `Integer` genuinely open+useful rather than closed-or-vacuous —
  representation-specific `_prim` work lives on the leaves; a subclass reaches it
  transitively through `value`. Minor wart: `value` is a slightly unnatural public
  method (`3 value` is legal); naming (`value` vs `primitiveValue`) is a small open
  call, not a design issue.
- `Integer` protocol: `even`, `odd`, `gcd:`, `lcm:`, `factorial`, `isPrime`,
  `timesRepeat:` (already there), `to:do:`/`to:by:do:` (already there), bit ops if
  desired.
- Small→large **overflow promotion** as Smol logic over the overflow-signaling
  machine primitives (moves policy out of Rust).

Generality ordering (ascending): MachineInteger < ArbitraryInteger < Float. (Fraction
deferred — see §7.) Coercion always retries at the higher generality.

## 4b. Encoded leaves vs abstract classes — `vmSubclass:` (RESOLVED 2026-07-16)

Closedness is **intrinsic to representation**, not a declarable modifier (a
user-facing `final`/`leaf`/`sealed` would enable closed systems antithetical to
Smol's open-system ethos). Two kinds of class, distinguished by *how they are
defined*:

- **`vmSubclass:` creates an *encoded leaf*** — a class whose instances are a fixed
  native/VM representation (immediate or boxed-primitive). It is the **only** way to
  introduce a class with a VM encoding, and such a class is **closed by
  construction**: the encoding carries no room for a subclass tag, so `subclass:` on
  it is refused. Users cannot usefully invoke `vmSubclass:` — there is no Smol way to
  supply a native representation. Encoded leaves: `MachineInteger`,
  `ArbitraryInteger`, `Float`, `String`, `Symbol`, `True`, `False`.
- **Abstract tower classes are *ordinary* classes** defined with plain `subclass:`,
  exactly like `Boolean` today (`Value subclass: #Boolean`). `Magnitude`, `Number`,
  `Integer` carry **no encoding of their own** and are **fully open** to user
  subclassing — this is what lets users add `Fraction`, `Complex`, a domain `Money`
  under `Number`, or a (representable, if arguably poor) `ModularInteger` under
  `Integer`. A user subclass's instances are ordinary boxed `ValueObject`s.

So the kernel reads `Number subclass: #Integer` (abstract, open) and
`Integer vmSubclass: #MachineInteger` (encoded leaf, closed). Migration note:
`True`/`False` move from `Boolean subclass:` to `Boolean vmSubclass:` (they are
encoded `Value::Bool` leaves); `Boolean` itself stays an open abstract `subclass:`.

**One rule:** *`vmSubclass:` ⇒ encoded, closed leaf; everything else is open.*
"leaf" is descriptive vocabulary for the encoded classes, not a declarable marker.
Taste (LSP-questionable subtypes like a wrapping `ModularInteger`) is never a hard
VM constraint; soft guidance (a lint) may come later and never blocks. ("immediate"
rejected as the unifier — true for `MachineInteger` but false for the boxed
`ArbitraryInteger`/`String`; the unifier is *has-a-VM-encoding*, named by
`vmSubclass:`.)

`vmSubclass:` and the `_prim…` operation surface together form the **VM-substrate
bounded context**; the `_prim` naming/rework is a **separate** thread (2026-07-12)
but should stay consistent with `vmSubclass:`.

## 4c. Subclassing the abstract tower — encoding-agnostic protocol

For `Integer subclass: #ModularInteger` (etc.) to work, the abstract classes must
never assume an encoding. `self`-is-the-raw-encoded-value holds **only for the
encoded leaves**; a user subclass's `self` is a boxed `ValueObject` with ivars.
Therefore (classic Smalltalk template-method / subclass-responsibility split):

- **Abstract `Number`** owns the coercion machinery — `generality`, `coerce:`,
  `retrySumCoercing:`, `retryRelationalOp:coercing:` — all written purely as
  self-sends of protocol; they never touch a `_prim` or an encoding.
- **Abstract `Integer`** owns integer behavior via protocol: `even` = `(self \\ 2)
  = 0`, `gcd:`, `factorial`, expressed with `\\`/`<`/`+`, never `_prim…`.
- **Each concrete class supplies the primitives for its own representation**
  (the "subclass responsibilities"): the leaves implement `+`/`<`/`\\` via their
  `_prim`s plus a coercion fallback; a user `ModularInteger` implements the same
  selectors over its ivars. Fast-path `_prim` methods live on the **leaves**, not on
  abstract `Integer`.

`self`-is-a-raw-value and `self`-is-a-boxed-object coexist because dispatch already
handles both (encoded values via `lookup_user_method` arms → primitive `self`;
`ValueObject`s via their class); shared abstract code never depends on which.
**Design constraint for S2–S4:** every method promoted onto `Magnitude`/`Number`/
`Integer` must be encoding-agnostic (protocol self-sends only).

## 4d. Two axes: representation (`is-a`) vs algebra (traits) — DECIDED

The tower carries **representation** down the `is-a` spine
(`MachineInteger`/`ArbitraryInteger`/`Float` — where `value`/`_prim` grounding
lives). **Algebraic structure** is orthogonal and belongs on the **trait** axis
(`can-act-as`), reusing the trait MVP (stateless, requirement-keyed, derived from
self-sends):

- `Ring` trait — requires `+ - * zero one`; provides derived behavior.
- `Field` trait — `uses: Ring` + requires `/ reciprocal`.
- `Integer uses: Ring`, `Float`/`Fraction uses: Field`; a user `Matrix uses: Ring`
  gets ring behavior with **no false `is-a Number`**.

This is why `Integer` stays usefully open (the `value` seam) *and* algebraic reuse
needs no inheritance: representation and algebra are different questions answered by
different mechanisms. **Deferred past S1** (traits exist; these bundles are S4+).

**Future capability — trait laws (RECORDED, NOT built).** A trait already declares
requirements; it could also carry **laws** — property blocks over its required
operations (`a + zero = a`, associativity, distributivity) — and the system could
auto-generate a `TestCase` verifying any class that `uses:` it. Pedigree: Haskell
QuickCheck law-classes, Scala `cats-laws`, Pharo algebraic test traits. Fits Smol's
"observable" goal and the planned Smol SUnit framework (CLAUDE.md tooling item 2).
Depends on: Smol testing framework + a property/generator mechanism — neither
exists. Cross-link there; do not build now.

## 5. Construction contract (unchanged from string plan §4)

Encoded numeric classes never route `new`/`new:` through generic
ValueObject/Entity construction. Numeric instances arise only from literals,
primitives, and arithmetic results. `MachineInteger`/`ArbitraryInteger`/`Float`
are abstract-to-instantiate from Smol (no public `new`); `Integer new` / `Number new`
are meaningless and should error or be absent.

## 6. Step breakdown (proposed; refine when starting)

- **S1 — Hierarchy + reflection (correctness fix). DONE 2026-07-21** (S1a =
  hierarchy+reflection, committed `82e8c13`; S1b = `vmSubclass:` + leaf closedness).
  Closedness is enforced by a name-based allowlist (`is_encoded_leaf_name`) rather
  than a new `Class` field — avoids touching ~15 `Class` constructors; the allowlist
  mirrors the encodings `value_to_class_handle` produces. `subclass:` rejects a leaf
  superclass; `vmSubclass:` requires an allowlisted name. Note: `vmSubclass:` has a
  capital S, so the routing arm needed `starts_with("vmSubclass:")` alongside the
  `contains("subclass:")` check. Reparent `Int` under a chain
  to `Value`; introduce `Magnitude`, `Number`, `Integer` (abstract) and the two
  leaves + `Float` via `vmSubclass:` in kernel.smol; implement `vmSubclass:`
  (VM-substrate class definition; user-invocation rejected) and make `subclass:`
  reject the encoded leaves (representationally-impossible rule, §4b) while leaving
  abstract VM classes open; add `lookup_user_method` arms for
  `Str`/`Float`/`Symbol`/`LargeInt`; make `value_to_class_handle` map
  `Value::Int` → MachineInteger, `Value::LargeInt` → ArbitraryInteger,
  `Value::Float` → Float. Tests: `class`/`isKindOf:` for every scalar and every
  tower level. **No behavior change to arithmetic yet** — `dispatch_int` etc. stay
  as-is, now reachable as the primitive layer. This is the smallest shippable
  increment and closes the observed correctness bugs (`class`, `isKindOf: Value`).
- **S2.0 — `value` projection seam (foundation for S2/S3).** Before any protocol
  moves to Smol, establish the projection seam that lets both comparison (S2) and
  coercion (S3) be written against a representation-agnostic hook (§4 "value
  projection seam"):
  ```
  Number >> value          [ ^self subclassResponsibility ]   "placement OPEN, see below"
  MachineInteger >> value  [ ^self ]
  ArbitraryInteger >> value[ ^self ]
  Float >> value           [ ^self ]
  ```
  A user subclass answers its ivar (`AuditedInteger >> value [^amount]`) and inherits
  everything above. **OPEN: does `value` live on `Integer` or `Number`?** If the seam
  is uniform across the whole numeric tower (Float grounds arithmetic through it too),
  it belongs on `Number` (leaning); if only integers need it, on `Integer`. Decide
  when writing S2.0. Ground the same-representation `_prim` operations on the leaves
  here too (see §4: single `_primLt:`/`_primAdd:` branching internally on the
  `Value::` variant is the leaning choice over per-representation selectors).
- **S2 — Comparison protocol.** Move `Magnitude` comparisons (`<=`, `>`, `>=`,
  `max:`, `min:`, `between:and:`) to Smol in terms of `<`/`=`; leaf `<` routes
  `^self value _primLt: aNumber value` for same-kind, else double-dispatch; fix
  cross-type `<`. Tests incl. `2 < 3.5`.
- **S3 — Coercion protocol.** Double-dispatch `adaptToInteger:andSend:` /
  `adaptToFloat:andSend:` (§4 — NOT a central `generality` table); rewrite public
  `+ - * /` in Smol over the same-rep primitives + `adaptTo…` fallback; fix
  `2 + 3.5` symmetry. Tests for every operand-type pair + promotion/demotion.
- **S4 — Integer protocol build-out in Smol** (`gcd:`, `factorial`, `even`, …),
  demonstrating the payoff: real numeric behavior authored in the language.
- **S5 — Prune** now-dead Rust arms in `dispatch_int`/`dispatch_float` that Smol
  fully supersedes; audit that every remaining `_prim` is irreducible. Update
  `docs/primitives.md`.

Each step is independently committable; S1 alone fixes the reported bugs.

## 7. Open questions / deferred

- **§3 name** for the arbitrary-precision leaf (blocker for S1). MachineInteger
  decided; arbitrary side leaning `ArbitraryInteger`.
- **RESOLVED (§4b):** closedness mechanism — `vmSubclass:` + representational-block
  rule; no user-facing modifier; abstract classes (incl. `Integer`) open.
- **`_prim` naming/rework** — kept a **separate thread** (2026-07-12); co-design
  vocabulary with `vmSubclass:` when it runs.
- **Fraction / Rational** — not in this chunk; slots above Float in generality when
  added. Design later.
- **Float subtyping under Number only** — Float is a `Number` but not an `Integer`;
  confirm no code assumes Number == Integer.
- **`Int` name retirement** — kernel currently says `Int`. Decide whether
  `MachineInteger` fully replaces the user-facing name `Int`, or `Int` survives as
  an alias / as the abstract `Integer`. (Leaning: `Integer` is the abstract shared
  class; literals produce `MachineInteger`; drop bare `Int`.)
- **Performance** — moving arithmetic dispatch into Smol adds message sends on the
  hot path. Mitigation: keep same-type fast path as a single primitive call;
  measure before/after (ties into the queued performance review).
- **`value.rs::kind()`** bootstrap tags vs class names — keep `kind()` as the
  error-message tag; class names come from the tower.
- Whether `String`/`Symbol` reparenting rides along in S1 or stays in the string
  plan's own step (they need no numeric tower; can be done together for one
  "all scalars answer `class`" milestone).

## 8. Cross-references

`string_representation_plan.md` (String/Symbol/Character, non-subclassable §7),
`web_server_readiness.md` (stdlib gating), `primitives.md` (primitive surface),
memory `design_scalar_representation`, `design_value_object_split`.
