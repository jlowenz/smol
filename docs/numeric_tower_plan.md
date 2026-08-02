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
(`Ordered`, Number, Integer, coercion) live in `kernel.smol`; Rust provides only
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

## 2. Hierarchy (DECIDED — Option A; comparability revised to a trait 2026-08-01)

Everything remains ultimately under `Value` (fixes the `isKindOf: Value` bug).
**Comparability is no longer a class node** — it is the `Ordered` trait on the
capability axis (see §2a below and §4d). The is-a spine carries *representation*
only:

```
Value
├── Number             arithmetic + coercion: + - * / , adaptTo…:andSend:   (uses: Ordered)
│   ├── Integer        gcd:, factorial, //, \\, even, odd, bit ops, isPrime …
│   │   ├── MachineInteger     fixed machine word (Value::Int, i64)
│   │   └── ArbitraryInteger   heap arbitrary-precision (Value::LargeInt, BigInt)
│   └── Float          (Value::Float, f64)
├── Boolean ── True / False    (already in kernel.smol under Value)
├── String                     (Value::Str)    — direct Value leaf         (uses: Ordered)
└── Symbol                     (Value::Symbol) — direct Value leaf
```

Rationale:
- `Number` = arithmetic + the **coercion protocol**; fixes the asymmetric-coercion
  bug generically rather than per-arm. `Number uses: Ordered`.
- `Integer` holds integer-only behavior shared by both leaves.
- The two leaves differ **only** in representation/performance
  (`MachineInteger` = one word, `ArbitraryInteger` = heap, unbounded). Both answer
  `isKindOf: Integer` → true; `class` distinguishes them. Overflow promotes a
  `MachineInteger` result to `ArbitraryInteger`; underflow (a big result that fits a
  word) demotes back (current `normalize_bigint`).
- `String` is **Ordered** (lexicographic `<`) but is **not** a Number and has no
  magnitude; it stays a direct `Value` leaf and `uses: Ordered`. `Symbol` reparenting
  is tracked in `string_representation_plan.md` §3.

## 2a. Comparability & magnitude are traits, not classes (DECIDED 2026-08-01)

The former `Magnitude` **class** is dissolved: its entire content was the comparison
protocol derived from `<`/`=` — no state, no representation — which is the textbook
definition of a trait, not a superclass. Per §4d (algebra lives on the trait axis),
comparability is a *capability*, not a representation, so it belongs on `uses:`.

Two layered traits, only the first built now:

- **`Ordered` (BUILD in S2).** The base comparability capability. *Requires* `<` and
  `=`; *provides* `<= > >= max: min: between:and:` (self-sends of `<`/`=`, so they are
  derived requirements). Members: `Number`, `String`, and later `Character`, `Symbol`,
  enums — anything with a total order. This is what "sortable / totally ordered" means,
  nothing more.
- **`Magnitude` (RECORD, defer).** A *richer* trait, `uses: Ordered`, adding **affine/
  metric structure**: a meaningful `-` between two values yielding a *difference/
  interval*, and usually a **zero/origin** ("how much"). Members: `Number`, `Duration`,
  `Measurement`/`Quantity`, `Date`/`Time`. **`String` is excluded** — `"cat" - "cab"`
  is nonsense; a string is Ordered but has no magnitude. Deferred because it is
  vacuous without an interval type to return (`Number - Number` is a Number, but the
  payoff members `Duration`/`Measurement` do not exist yet). This is the affine
  point-vs-vector distinction Smalltalk's flat `Magnitude` class conflated with mere
  orderability; splitting `Ordered` (base) from `Magnitude` (metric) separates them.
  Cross-ref: Rust `Ord` vs `Sub`/`Zero`; Haskell `Ord` vs `Num`.

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
  `_primAdd:`/`_primSub:`/`_primMul:`/`_primQuo:`/`_primRem:`. **RESOLVED (S2):**
  **single selector, internal `Value::` branch** (not per-representation
  `_primMachineAdd:`/`_primLargeAdd:`). S2's `_primLt:` set the precedent — the Rust
  primitive already holds the receiver value and branches on its variant, promoting
  cross-type; per-rep selectors were needless ceremony. S3 arithmetic follows suit.
  Note the grounding is sent to `self` on the **leaf** (self-only rule), with the
  argument projected (`aNumber value`) — see §4e.
- Cross-representation comparison: `_primLt:` — implemented in S2 as `numeric_lt`
  (single selector; any-Float ⇒ f64, else any-LargeInt ⇒ BigInt, else i64). Plus
  `_primStrLt:` for lexicographic String order.
- Float ops that need libm/host: `sqrt`, `sin`, `ln`, `truncated`, `rounded`,
  `asFloat`.
- Conversion feeding coercion: `asFloat`, `largeFitsWord`/`_primNormalize`
  (demote-if-fits), `machine→large`.
- Radix/parse/print primitives as needed.

Most of this **reuses arithmetic already written** inside `dispatch_int`/
`dispatch_float` — S2–S4 mostly **relocates** that logic behind `_prim…` selectors
and deletes the broad fallback arms (S5 prune), not new math.

**Moves to Smol (`kernel.smol`, on the tower):**
- The comparison protocol (`<=`, `>`, `>=`, `max:`, `min:`, `between:and:`) in terms
  of `<`/`=` — now the **`Ordered` trait** (§2a), not a `Magnitude` class. `Number
  uses: Ordered`; the leaves supply the required `<`.
- **`Number` coercion via double-dispatch (DECIDED — Squeak/Pharo `adaptTo…`
  school, NOT a central `generality` table).** Public `+ - * / < …` on a leaf: if
  the arg is the same kind, call the same-rep primitive; otherwise
  `^arg adaptToInteger: self value andSend: #+` (and `adaptToFloat:andSend:` etc.).
  A new number type (`Fraction`, `Complex`, user type) integrates by implementing
  its own `adaptTo…:andSend:` — **no central generality ordering to edit.** Chosen
  over blue-book `generality`/`coerce:`/`retry:coercing:` because it is the *more
  open* mechanism (no global table), matching Smol's ethos. Fixes `2 + 3.5` /
  `2 < 3.5` symmetry.
- **The seam pair: `value` / `withValue:` (DECIDED 2026-07-31, refines the earlier
  single-method `value` seam).** See §4e.
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
  exactly like `Boolean` today (`Value subclass: #Boolean`). `Number` and
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

- **Abstract `Number`** owns the coercion machinery, written purely as self-sends of
  protocol; it never touches a `_prim` or an encoding.
  > **SUPERSEDED (S3, §4f).** This bullet originally named `generality`, `coerce:`,
  > `retrySumCoercing:`, `retryRelationalOp:coercing:` — the Smalltalk-80 central
  > generality table, which §4f **rejected** in favour of `adaptTo…andSend:` double
  > dispatch precisely because a global total order is the thing that makes a tower
  > closed. §4g then generalized the edge to domain-named dispatch. Those four
  > selectors do not exist and will not.
- **Abstract `Integer`** owns integer behavior via protocol: `even` = `(self \\ 2)
  = 0`, `gcd:`, `factorial`, expressed with `\\`/`<`/`+`, never `_prim…`.
- **The encoded leaves supply the primitives for their own representation**: they
  implement `+`/`<`/`\\` via their `_prim`s plus the coercion edge. Fast-path `_prim`
  methods live on the **leaves**, not on abstract `Integer`.
  > **SUPERSEDED (S2.0/§4e).** This bullet originally required a *user* subclass to
  > implement the same selectors over its ivars. The `value`/`withValue:` seam removed
  > that burden: a wrapper implements **two** methods and inherits the whole tower.
  > Re-implementing the operators is now the exception — for a **structured** number
  > (Fraction, Complex) that has no single encoded leaf to project onto, and which
  > therefore overrides the arithmetic itself and never reaches the seam.

`self`-is-a-raw-value and `self`-is-a-boxed-object coexist because dispatch already
handles both (encoded values via `lookup_user_method` arms → primitive `self`;
`ValueObject`s via their class); shared abstract code never depends on which.
**Design constraint for S2–S4:** every method promoted onto the `Ordered` trait or
onto `Number`/`Integer` must be encoding-agnostic — either pure protocol self-sends,
or the project-then-redispatch/reconstruct seam of §4e (`Number >> < aNumber [ ^self
value < aNumber ]`, `Integer >> + aNumber [ ^self withValue: (self value + aNumber
value) ]`). It must never name a `Value` variant, assume `self` *is* a leaf, or send
a `_prim` to anything but literal `self` (self-only rule). The `_prim` grounding lives
on the leaves; abstract code reaches it only by projecting with `value` and
re-dispatching a public selector.

## 4d. Two axes: representation (`is-a`) vs algebra (traits) — DECIDED

The tower carries **representation** down the `is-a` spine
(`MachineInteger`/`ArbitraryInteger`/`Float` — where `value`/`_prim` grounding
lives). **Algebraic structure** is orthogonal and belongs on the **trait** axis
(`can-act-as`), reusing the trait MVP (stateless, requirement-keyed, derived from
self-sends):

- `Ordered` trait (§2a) — requires `< =`; provides `<= > >= max: min: between:and:`.
  The base comparability capability; `String uses: Ordered` with **no false
  `is-a Number`/`is-a Magnitude`**.
- `Magnitude` trait — `uses: Ordered` + metric structure (`-` → interval, zero);
  members are quantities (`Number`, `Duration`, `Measurement`), not `String`.
  Recorded, deferred (§2a).
- `Ring` trait — requires `+ - * zero one`; provides derived behavior.
- `Field` trait — `uses: Ring` + requires `/ reciprocal`.
- `Integer uses: Ring`, `Float`/`Fraction uses: Field`; a user `Matrix uses: Ring`
  gets ring behavior with **no false `is-a Number`**.

Comparability (`Ordered`) is the first concrete instance of this principle to be
built: it was originally drawn as a `Magnitude` **class** on the is-a spine, which
contradicted this very section (ordering is structure/capability, not
representation). §2a corrects it.

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

## 4e. The seam pair — `value` / `withValue:` (DECIDED 2026-07-31)

Two subclassing stories coexist under the tower, and the seam decides how far the
cheap one reaches. Earlier drafts (§4, §4c) stated them in tension; this section is
authoritative.

- **Story A — delegation.** A *wrapper* (`AuditedInteger` holding `amount`,
  `ModularInteger` holding a residue + modulus, a domain `Money`) is represented by
  an underlying encoded number. It implements the seam pair and inherits the whole
  abstract protocol.
- **Story B — subclass responsibility.** A *structured* number (`Fraction`,
  `Complex`) has no single underlying scalar. It overrides `+ - * / <` itself and
  inherits only the derived protocol (`even`, `max:`, `between:and:`, …). Abstract
  code never forces it through the seam.

### Placement

- **The `Ordered` trait's seam is `<` (plus `=`), NOT `value`.** Its members include
  non-numeric comparables — `String`, `Character`, later `Date`, version tuples,
  compound keys — for which "the underlying encoded scalar" is meaningless or a lie
  (a `Date` is several ivars; projecting to one silently picks an axis). `<=`, `>`,
  `>=`, `max:`, `min:`, `between:and:` need only `<`/`=` (the trait's requirements),
  which each member supplies for its own representation.
- **The `subclassResponsibility` override lives on `Number`, not `Integer`** (the
  `^self` *default* itself was later scope-lifted to `Value` — see Contract below).
  Placing the "must project" demand on `Number` lets the coercion protocol (§4) and
  every same-representation `_prim` call be written once against `value`, and gives
  wrappers of *any* numeric representation (a `Money` over `Float`, an
  `AuditedInteger` over `MachineInteger`) the same freebie. Putting it on `Integer`
  only would cost `Number`'s coercion code its uniform way to reach a raw operand. The price — `Fraction`/`Complex` inherit a `value` they cannot honor —
  is paid in a `subclassResponsibility` error they would have hit anyway under Story
  B, which is what that mechanism is for.

### Naming — `value` (DECIDED)

`value` is kept. It does **not** collide conceptually with `Block >> value`: both
mean *reduce the receiver to its value* — a block by running, a number by being
itself. That uniformity is the long-standing `aBlockOrValue value` idiom (Pharo
historically carried `Object >> value [^self]` for exactly this). `primitiveValue` /
`encodedValue` were rejected: they assert substrate status that is false of a user
wrapper answering the same selector. Accepted risk: `value` is the highest-traffic
selector in the image, so a user class that is a `Number` *and* uses `value` for its
own domain meaning has a conflict — narrow, since it only reaches classes under
`Number`.

### Contract

```
Value  >> value              [ ^self ]                 "default: a value reduces to itself (§ scope-lift 2026-08-01)"
Value  >> withValue: aValue  [ ^aValue ]              "default: escape to the leaf"
Number >> value              [ ^self subclassResponsibility ]   "override: open class ⇒ must project"
MachineInteger >> value      [ ^self ]                "leaf override back to identity"
```

**Scope lift (DECIDED 2026-08-01).** The `value`/`withValue:` *defaults* live on
`Value`, not `Number`. `value`'s default `^self` is the block-or-value idiom (a value
reduces to itself), so `String`/`Symbol`/`Bool` answer it too — they are not `closed`.
`Number` *overrides* `value` to `subclassResponsibility` because it is **open** to
subclassing and `^self` is wrong for a boxed wrapper whose `self` is not a leaf — that
override is the price of openness. A closed `vmSubclass:` leaf (String) safely inherits
`^self`. This resolves the apparent inconsistency ("why does Number project but String
not?") — the override tracks open-vs-closed, and it also completes the block-or-value
idiom (`x value` now works on any value). Consequence: no value DNUs `value`, which
retired the `as_block_type_error` test (it asserted an accident) — a non-block loop
body now evaluates to itself.

`value` — *Requires:* nothing. *Ensures:* answers an **encoded leaf**
(`MachineInteger`/`ArbitraryInteger`/`Float`), never merely "a Number" — so a
wrapper-of-a-wrapper answers `^inner value` and recurses to ground.

`withValue:` — the **reconstruction** half, without which arithmetic on a wrapper
would answer a bare leaf and silently discard the receiver's identity. *Requires:*
`aValue` is an encoded leaf. *Ensures:* answers the receiver's kind carrying the
receiver's other state, or a leaf if the operation escapes the wrapper. Written on
the receiver (not `self class new:`) precisely so it can carry sibling ivars forward
and normalize: `ModularInteger >> withValue: n [ ^self class value: n \\ modulus ]`.

**The seam→prim grounding lives on the LEAF, not the abstract method (CORRECTED
S2, 2026-08-01).** A `_prim` may only be sent to `self` (eval.rs self-only check), so
an abstract method CANNOT write `self value _primOp: …` — that sends `_prim` to a
*projection* of self. Instead:
- each **leaf** grounds the public operator by sending the `_prim` to `self` (which
  *is* the raw encoding), projecting only the argument:
  `MachineInteger >> < aNumber [ ^self _primLt: aNumber value ]`;
- the **abstract** class projects self with `value` and **re-dispatches the public
  operator** (comparison) or **reconstructs** (arithmetic):
  `Number >> < aNumber [ ^self value < aNumber ]`
  `Integer >> + aNumber [ ^self withValue: (self value + aNumber value) ]`.

So the arithmetic template is `self withValue: (self value + aNumber value)` — the
inner `+` re-dispatches to the leaf, whose `+` sends `_primAdd:` to self. A wrapper
implements **two** methods (`value` + `withValue:`) to get closed arithmetic; the
`_prim`s stay on the leaves. (Earlier drafts wrote `self value _primAdd: aNumber
value`; that form is illegal under the self-only rule and is superseded here.)

**Immutability wrinkle (confirmed S2.0).** A user `Number` subclass is a `Value`,
so its instances are **frozen** after construction — ivars can be mutated *only*
inside a `_primNewValue:` builder block (the `Condition` pattern), never by a plain
setter on a live instance. Therefore a wrapper's `withValue:` reconstruction must
route through a class-side value factory
(`ModularInteger >> withValue: n [ ^self class value: n \\ modulus ]`, where
`ModularInteger class >> value: n [ ^self _primNewValue: [:m | m setN: n ...] ]`),
**not** `self copy setN:` — `copy` of a frozen value followed by a setter fails the
same freeze check. The S2.0 seam test (`user_number_subclass_inherits_via_value_seam`)
constructs its wrapper via exactly this builder path. S3, when it implements the
leaf/wrapper `withValue:` reconstructions, must follow it.

**`Number` projects SELF ONLY (CORRECTED S3, 2026-08-01).** Earlier drafts wrote
`Integer >> + aNumber [ ^self withValue: (self value + aNumber value) ]`. Projecting
the *argument* defeats case 2 below: `wrapperA + wrapperB` would silently
receiver-wins-wrap instead of reaching double dispatch. The correct form projects
self only —

```
Number >> + aNumber [ ^self withValue: (self value + aNumber) ]
```

— because the inner `+` re-dispatches on a *leaf* receiver, so a leaf argument
grounds in the primitive while a wrapper argument still reaches `adaptTo…`. This is
strictly more general and identical for the wrapper+leaf case. (Comparison is
unaffected: `Number >> <` answers a Bool and needs no reconstruction.)

## 4f. The coercion edge vs. the branch mechanism (DECIDED S3, 2026-08-01)

A leaf's `+` receives an argument it may know nothing about, and control must get
from there to the argument's coercion code. Only three mechanisms exist: **test the
argument** (a predicate), **attempt and detect failure** (a sentinel or a signal from
the primitive), or **send to the argument** (double dispatch). The decision that
matters is *not* which of these to use — it is recognising that they answer different
questions and live at different levels:

- **The extension edge — `adaptToInteger:andSend:` / `adaptToFloat:andSend:` — is
  the architectural commitment**, and it must exist from the moment arithmetic moves
  into Smol. It is what makes the tower open: a new number type joins by implementing
  two methods **on itself**, with no central generality table, no allowlist, and — the
  decisive criterion — **no edit to the closed `vmSubclass:` leaves**. If extending the
  tower required editing the leaves, the tower's openness would be fiction. (This also
  rules out deferring the edge to a later step "once Fraction exists": deferral is
  precisely what would force the retrofit.)
- **The branch mechanism behind that edge is an implementation detail** — predicate,
  sentinel, or attempt-and-fail all sit *behind* `adaptTo…`, none is visible to an
  extender, and swapping one for another changes no public protocol.

Implemented as the predicate `Object >> isEncodedNumber [ ^false ]`, overridden
`true` on the three encoded leaves. Named for §4b's unifier — *has-a-VM-encoding* —
not "leaf": a user `ModularInteger` is objectively a leaf but is definitively not
encoded, so `isNumberLeaf` would be non-discriminative.

**SUPERSEDED in part by §4g (2026-08-02): the predicate is no longer leaf-private, and
was RENAMED `pvtIsEncodedNumber` → `isEncodedNumber` to match.** S3 described it as
leaf-internal and named it with the `pvt` prefix accordingly, which also answered the
§4d objection that a representation test has no business on the capability axis. §4g's
specificity rule requires the *abstract* layer to ask the same question — "is this
operand a bare encoding or a domain-carrying type?" — and S6's `=` asks it too, so the
predicate is deliberately promoted out of leaf-private. Keeping the `pvt` prefix on a
selector sent from abstract code would have made the name contradict the design.

It remains a *representation* test, not a capability claim, and a new number type still
inherits the correct `false` without knowing it exists.

### `NumberCoercion` trait — intended shape, DEFERRED (2026-08-01)

S3 ships the three fallbacks (`isEncodedNumber`, `adaptToInteger:andSend:`,
`adaptToFloat:andSend:`) as plain methods on **`Object`**. That placement is
provisional and is *not* the intended end state: numeric-tower vocabulary accreting
on the root class is domain leakage, and "can be asked to participate as a number in
an arithmetic operation" is a **capability**, so by §4d it belongs on the trait axis.
The intended shape:

```
Trait named: #NumberCoercion.
NumberCoercion >> isEncodedNumber [ ^false ]
NumberCoercion >> adaptToInteger: anInteger andSend: aSymbol [ ^(NoCoercion …) signal ]
NumberCoercion >> adaptToFloat: aFloat andSend: aSymbol      [ ^(NoCoercion …) signal ]
```

The refusals signal the **typed `NoCoercion` condition** (§4g), not `self error:` — a
handler must be able to distinguish "cannot coerce" from any other error and inspect
the operands. That change is made in §4g's step and is independent of where the
methods eventually live.

This also gives the three methods one named home, which is the real answer to "does
`Object` need `isEncodedNumber`?" — see the next paragraph for why simply *dropping*
the root default is not that answer.

**Why a root default is needed at all (considered and kept, 2026-08-01).** Defining
the fallbacks only on `Number` is structurally reasonable — a non-Number would just
not understand them — but diagnostically poor, and on the most common arithmetic
mistake there is: `3 + 'bad'` would report *"String does not understand
isEncodedNumber"*, naming the wrong problem (the issue is that `'bad'` is not a number,
not that String lacks a predicate). Same for `3 + nil`, `3 + aBlock`, `3 + anEntity`.
That diagnostic argument is unchanged by the rename, and is now reinforced by S6: `=`
sends `isEncodedNumber` to *any* operand, so the root default is load-bearing for
equality as well as arithmetic. So the universal
fallback stays; the open question is only *where it is written*, not *whether it
exists*.

**Blocked on a standalone `SomeClass uses: SomeTrait.` statement**, applying a trait to
an already-defined class. `uses:` is currently definition-time only and `Object` is
bootstrapped in Rust, so `Object uses: NumberCoercion` is inexpressible today. That is a
trait-system question with its own design tensions and is deliberately NOT bundled into
the numeric tower — **see `docs/open_questions.md` Q13**, which records the motivation
and the five tensions (collision with existing methods, ordering against later `>>`
definitions, already-defined subclasses, bootstrapped classes, observability).

Until that lands, the `Object` placement stands and is marked here as provisional so it
is not mistaken for a considered end state.

Rejected alternatives, recorded: a **`nil` sentinel** returned by a failing primitive
(authentic to Smalltalk's primitive-failure fallthrough and marginally cheaper, but
legible only by convention, and it conflates "wrong type" with "not my job"); a
**signal** from the primitive (heavy machinery on the arithmetic path, and the
condition-system guide steers away from `on:do:` for control flow); and **unconditional
per-operation double dispatch** (`sumFrom:`/`differenceFrom:`, Smalltalk-80 style —
correct and sentinel-free, but it needs reversed-operand primitives for `- / // \\`
and ~18 kernel one-liners, and costs a new number type six methods instead of two;
Squeak's `adaptTo…andSend:` collapses exactly that N², and we do not have the N² it
was invented for, since one promoting primitive already covers all three leaf
encodings). Note that the leaf form of `adaptTo…` is *not* blocked by the self-only
`_prim` rule as an earlier analysis claimed — inside `adaptToInteger:andSend:` the
receiver **is** the leaf argument, so `self _primAdd: anInteger` is legal; the rule
only blocks forwarding a `_prim` back to the original receiver.

### Three cases where blind reconstruction is wrong

1. **Not closed under the operation.** `audited / 2` may be a `Float` or `Fraction`;
   wrapping that back into `AuditedInteger` is a lie. Division (and any op that can
   leave the wrapper's domain) must be allowed to escape — the inherited default
   `withValue: [^aValue]` is the escape hatch, and such ops call it deliberately.
2. **Two different wrappers.** `audited + money` — receiver-wins reconstruction
   silently breaks type symmetry. This is the job of the `adaptTo…:andSend:` double
   dispatch (§4): a wrapper participates in coercion like any other number rather
   than getting a free wrap. *(How that dispatch reaches the argument **without first
   erasing the receiver** is settled in §4g: the adapt selector names the receiver's
   domain, so `money` receives `audited` intact.)*
3. **Leaves.** The leaves inherit `withValue: [^aValue]` unchanged, which keeps the
   same template correct at the bottom of the tower with no leaf-specific code.

## 4g. Mixed-mode arithmetic — the specificity rule (DECIDED 2026-08-02)

S4 shipped `Number`/`Integer` protocol that fails whenever the receiver reaches
**argument position**: `squared` (`self * self`), `factorial`, `gcd:`, `lcm:`. The
proximate cause is that §4e's "project SELF ONLY" correction lets a leaf receive a
non-leaf operand it cannot ground. The root cause is deeper, and this section states
it, the contract that was missing, and the rule that replaces it.

### Root cause — two reconstruction mechanisms, and a missing contract

The tower has **two** places that can decide a result's type: `withValue:` at the
abstract layer (§4e) and `adaptTo…andSend:` at the leaf layer (§4f). Squeak, Python
(`__rop__`) and Julia (`promote_rule`) each have exactly **one**, and the operator
returns its answer directly without post-processing. S3 layered an *unconditional*
`self withValue: (…)` on top of a mechanism that already answers finished values, so a
wrapper argument that reconstructs gets wrapped twice — violating `withValue:`'s own
*Requires: `aValue` is an encoded leaf*.

Compounding it, **`adaptToInteger:andSend:` was never given a contract.** §4f states
why the edge must exist but no `Requires:`/`Ensures:`, and the lone implementation
(`Float >> adaptToInteger:andSend:`) is ambiguous where it matters: it answers a
`Float`, which is *both* "an encoded leaf" *and* "a number of the adapter's own kind".
Those readings coincide for `Float` and diverge for every wrapper.

### Why mixed mode must be handled, not outlawed (the Haskell option, REJECTED)

OCaml/SML/Haskell refuse mixed operands outright: `(+) :: Num a => a -> a -> a` requires
both sides to already be the same type, with extension via `instance Num MyType`. Worth
recording because it shows **extensibility and implicit coercion are independent axes** —
Haskell is fully open *because* it refuses mixed mode. Unavailable to us: we are
dynamically typed, so a mixed send cannot be *prevented*, only given a runtime meaning;
requiring explicit conversion (`float_of_int`) is unenforceable for the same reason; and
wrappers cannot be forbidden without forfeiting the §4c openness the tower exists to
provide.

### The rule — specificity, not position

An encoded leaf carries **no domain**; a wrapper carries one. The wrapper's domain wins
**regardless of operand position**, so `3 * boxed` and `boxed * 3` agree. (Discriminating
on *same class* instead does not achieve this: it answers a leaf for `boxed * 3` and a
wrapper for `3 * boxed`, losing the wrapper in exactly the case where it is the
receiver.)

| receiver | argument | mechanism | result |
|---|---|---|---|
| wrapper | leaf | project arg, op on leaves, rewrap | wrapper |
| leaf | wrapper | leaf declines → `adaptTo…` → wrapper rewraps | wrapper |
| wrapper | same wrapper | project both, rewrap | wrapper |
| wrapper | *different* wrapper | domain-named double dispatch (below) | argument's choice, else `NoCoercion` |
| leaf | leaf | primitive | leaf |

The first three rows collapse into one branch — the **original pre-S3 form**,
restricted to "the argument is not a different domain":

```smalltalk
Number >> * aNumber [
    ^(aNumber isEncodedNumber or: [aNumber class == self class])
        ifTrue:  [ self withValue: (self value * aNumber value) ]
        ifFalse: [ self pvtAdapt: aNumber send: #* ] ]
```

`withValue:`'s *Requires: an encoded leaf* now holds **by construction** — the only
branch that calls it has already established that both operands project to leaves.

This closes all four S4 failures: `boxed squared` is `boxed * boxed` (same class →
rewrap); `boxed factorial` is `self - 1` (encoded arg → rewrap) then `boxed * boxed`;
`gcd:`/`lcm:` flip operands within one class. Comparison and `=` are unaffected — they
answer a `Bool` and never reconstruct.

**`isEncodedNumber` is promoted out of leaf-private — amends §4f**, which had
declared the branch mechanism a private detail of the leaves. "Is this operand a bare
encoding or a domain-carrying type?" is a genuine question at the abstract layer, and
is precisely the specificity test the rule needs.

### Domain-named double dispatch — generalizing `adaptTo…`

For two different wrappers, projecting the receiver to a leaf before consulting the
argument would **erase the receiver's type**, silently reducing to Python `__radd__`
semantics. Instead the adapt selector names the **receiver's domain** and the argument
implements it — which is what `adaptToInteger:`/`adaptToFloat:` already are; the idea
simply was never extended past the two leaf domains, which is why `Number` had nothing
to send. (Squeak has the same shape at larger N: `adaptToFraction:`, `adaptToComplex:`,
`adaptToPoint:`, ….)

```smalltalk
"The adapt selector names the receiver's DOMAIN, not its encoding — all three integer
 leaves present as #adaptToInteger:andSend: because they share one domain.
 The pins go on the LEAVES: pinning abstract Integer would make every user
 `Integer subclass:` inherit the claim and ask for the wrong coercion method."
Number           >> pvtAdaptSelector [ ^('adaptTo', self class name, ':andSend:') asSymbol ]
MachineInteger   >> pvtAdaptSelector [ ^#adaptToInteger:andSend: ]
ArbitraryInteger >> pvtAdaptSelector [ ^#adaptToInteger:andSend: ]
Float            >> pvtAdaptSelector [ ^#adaptToFloat:andSend: ]

Number >> pvtAdapt: aNumber send: aSymbol [
    | sel |
    sel := self pvtAdaptSelector.
    ^(aNumber respondsTo: sel)
        ifTrue:  [ aNumber perform: sel withArguments:
                     (Cons with: self rest: (Cons with: aSymbol rest: nil)) ]
        ifFalse: [ (NoCoercion receiver: self argument: aNumber selector: aSymbol) signal ] ]
```

`audited + money` now sends `money adaptToAudited: audited andSend: #+` with the
receiver **intact** — no projection, no erasure. This retroactively *explains* the
leaf selector naming (domain, not encoding) rather than contradicting it, and the
closed `vmSubclass:` leaves keep their existing hardcoded sends unchanged: only
`Number` gains the general path, so §4f's "no edit to the leaves" criterion holds.

**`Number` also carries default `adaptTo…` implementations (added S5).** They project
self to a leaf, let the leaf perform the operation with the **original operand order**
preserved (`anInteger` stays on the left, since the send was `anInteger op self`), and
reconstruct via `withValue:`. This is what makes §4c's "a wrapper implements *two*
methods and inherits the tower" literally true: `value` + `withValue:` is now sufficient
for full participation in *both* operand positions, with no `adaptTo…` of its own.

These defaults were **unsafe before S5 and are safe now**: under S3's unconditional
`self withValue: (…)` the wrapper they reconstruct would have been wrapped a second time
by the calling abstract op. The specificity guard removes that path entirely — when the
argument is an encoded leaf the guard takes the project-both branch and never reaches
coercion — so `withValue:`'s Requires holds. Structured numbers (Fraction, Complex) have
no single leaf to project onto and override these with real domain arithmetic.

**Contract, finally stated.**

`adaptTo<Domain>: aNumber andSend: aSymbol` — *Requires:* `aNumber` is a Number of the
domain named in the selector, and `aSymbol` is a binary arithmetic selector.
*Ensures:* answers the **finished result** of `aNumber aSymbol self`, reconstructed in
whichever domain the implementor judges correct. The caller must **not** post-process
the answer — this is the single point that decides the result's type, matching Squeak/
Python/Julia. `Object`'s default refuses (see below) rather than answering.

**Detection: `respondsTo:` guard, not DNU interception.** Asking before sending keeps
both operands intact at the refusal site and avoids condition-handling-as-control-flow
(which `docs/condition_system_guide.md` steers away from). DNU interception was
considered; recognising an adapt-shaped selector needs string predicates that do not
exist (no `startsWith:`/`endsWith:`), so it would have to match loosely on `Message`
shape.

*Implementation note:* `respondsTo:` is **not** universally available today despite
`Object` being the root class. It lives in `obj_prim_core` (`builtins_iterative.rs`),
reached only from the `Value::Instance` dispatch path; scalars (`Str`, `Symbol`, `Int`,
`Float`, `Bool`, `Nil`, `Block`) each have their own per-variant arm list and none
includes it — the tell is `=`, duplicated across every variant's arms. So
`3 respondsTo: #foo` currently MNUs, and this step must lift it to the shared value
paths. Instance of a broader latent inconsistency: "Object protocol" is universal in
Rust only where each variant happens to include it.

### `NoCoercion` — a typed condition

The refusing defaults call `self error:`, which signals a real `Error` but an
**untyped** one: the message is a string, so a handler cannot distinguish "cannot
coerce" from any other error, cannot inspect the operands, and cannot offer a restart.
Replaced by an `Error` subclass following the existing `TraitConflict` idiom (ivars +
class-side factory):

```smalltalk
Error subclass: #NoCoercion
    instanceVariableNames: 'receiver argument selector' …
```

Non-resumable in this step; a `use-value`-style restart is a natural later addition once
there is a caller that wants one. §4f's deferred `NumberCoercion` trait adopts this
condition wherever the refusing defaults eventually live.

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
  **Note (2026-08-01):** S1a introduced a `Magnitude` *class* on the spine; §2a
  later revised comparability to the `Ordered` trait, so **S2 dissolves that class**
  (`Number` reparents directly under `Value`).
- **S2.0 — seam pair `value`/`withValue:` (foundation for S2/S3).** Before any
  protocol moves to Smol, establish the seam that lets comparison (S2) and coercion
  (S3) be written against a representation-agnostic hook. Placement and naming are
  DECIDED — see §4e:
  ```
  Value  >> value             [ ^self ]                          "default (scope-lifted 2026-08-01)"
  Value  >> withValue: aValue [ ^aValue ]
  Number >> value             [ ^self subclassResponsibility ]   "override: open ⇒ project"
  MachineInteger >> value     [ ^self ]
  ArbitraryInteger >> value   [ ^self ]
  Float >> value              [ ^self ]
  ```
  **DONE 2026-08-01** (seam-pair committed `6fa4f7c`; scope-lifted to `Value` in S2 so
  String/Symbol answer `value`). The `<`-grounding (leaf `_primLt:`) was deferred to S2
  so it lands atomically with the leaf `<` methods and the `Ordered` trait. `_primLt:`
  is a single selector branching internally on the `Value::` variant (chosen over
  per-representation selectors).
- **S2 — Comparison protocol (`Ordered` trait). DONE 2026-08-01.** Dissolved the S1a
  `Magnitude` class (`Number` reparented directly under `Value`); defined `Ordered` as
  a trait (§2a) requiring `<` and providing `<= > >= max: min: between:and:` (derived
  from `<` alone via total-order negation). `Number uses: Ordered`; `String uses:
  Ordered`. The `<` grounding follows the corrected seam→prim pattern (§4e): each
  **leaf** sends `_primLt:` to `self` projecting only the arg
  (`MachineInteger >> < aNumber [ ^self _primLt: aNumber value ]`); abstract `Number`
  projects self and re-dispatches (`Number >> < aNumber [ ^self value < aNumber ]`).
  Added the `_primLt:` primitive (`numeric_lt`, single selector, internal variant
  branch, all Int/LargeInt/Float cross-types promoted in Rust — no Smol double-dispatch
  needed; that is S3 for user structured numbers) and `_primStrLt:` for lexicographic
  String order. Fixed cross-type `<` (`2 < 3.5` symmetric). Tests: same/cross-type
  numeric compare, derived protocol, `'a' < 'b'`, value-seam lift, membership without
  Magnitude. Two behavior changes recorded: comparing to an incomparable operand now
  surfaces as a `_primLt:` TypeError (Int receiver) or a MessageNotUnderstood signal
  (flipped `>` / non-Value arg like `nil`); `as_block_type_error` retired.
- **S3 — Arithmetic + coercion protocol. DONE 2026-08-01.** Double-dispatch
  `adaptToInteger:andSend:` / `adaptToFloat:andSend:` (§4 — NOT a central
  `generality` table); public `+ - * /` (and `// \\` on the Integer leaves only)
  rewritten in Smol over promoting primitives + the `adaptTo…` edge; `2 + 3.5`
  symmetry fixed. Six new single-selector primitives (`_primAdd: _primSub: _primMul:
  _primDiv: _primQuo: _primRem:`) share one promotion policy (`numeric_binop`),
  following the S2 `_primLt:` precedent: any `Float` ⇒ f64, else any `LargeInt` ⇒
  `BigInt`, else `i64` with overflow promoting and results demoting via
  `normalize_bigint`. See §4f for the branch-mechanism decision. Two behavior
  changes: a non-numeric argument now signals a *coercion `Error`* naming operand
  and operator (previously a Rust `TypeError`), because the operand reaches the
  coercion edge whose refusing default sits on `Object`; and `Float` no longer
  answers `//`/`\\` via a hidden path — they are Integer protocol and Float MNUs.
- **S4 — Integer protocol build-out in Smol. DONE 2026-08-01** (committed `7b80331`).
  `Number >> negated abs squared reciprocal isZero sign`; `Integer >> even odd gcd:
  lcm: factorial isPrime` (+ helper `pvtHasNoOddFactorFrom:`). All encoding-agnostic
  per §4c — pure protocol self-sends, no `_prim`, no named `Value` variant. `negated`
  is `self * -1` (NOT `0 - self`) so the receiver stays on the LEFT and enters via
  `Number >> *`; `isPrime` uses `k * k > self` tail recursion because the tower has no
  float `sqrt`; recursion throughout rather than loop accumulation (method temps can't
  be assigned inside blocks, and a `ValueHolder` would drag an entity into value
  arithmetic). Demonstrates the payoff: real numeric behavior authored in the language.
  **Shipped with two known gaps, addressed by S5 and S6 below** — the S4 wrapper test
  documents the limit rather than asserting the fix.
- **S5 — Mixed-mode arithmetic: the specificity rule (§4g). DONE 2026-08-02.** Fixes the
  S4 gap where a wrapper in *argument* position is not carried. **Closed `squared` and
  `factorial`; `gcd:`/`lcm:` remain blocked on S6** — they terminate on `anInteger
  isZero`, which is `^self = 0`, and with no `Number >> =` a wrapper's `Boxed(0) = 0` is
  false, so the base case never fires and the recursion divides by zero. **S6 is
  therefore a prerequisite for §4c's encoding-agnostic claim, not merely a correctness
  fix.** Also added: default `Number >> adaptToInteger:/adaptToFloat:andSend:` (§4g), so
  the seam really is two methods. Order as executed:
  1. Lift `respondsTo:` from `obj_prim_core` to the shared value dispatch paths so
     scalars answer it (§4g implementation note); it MNU'd on `3` before. Surfaced a
     pre-existing gap — it resolves by class-dictionary lookup and is blind to builtin
     selectors, so `'s' respondsTo: #size` is false. Sound for the coercion edge (adapt
     selectors are Smol-defined) but unsound generally; recorded as
     `docs/open_questions.md` **Q14** and pinned by a test.
  2. `Error subclass: #NoCoercion` (ivars `receiver argument selector`, class-side
     factory, `TraitConflict` idiom), replacing `self error:` in the two refusing
     defaults on `Object`.
  3. `Number >> pvtAdaptSelector` (+ `Integer`/`Float` overrides to the two existing
     leaf selectors) and `Number >> pvtAdapt:send:`.
  4. Rewrite the six abstract ops (`+ - * /` on `Number`, `// \\` on `Integer`) with
     the specificity guard. The closed `vmSubclass:` leaves are **not** touched.
  5. Tests: the five-case table of §4g, incl. `3 * boxed` ≡ `boxed * 3`, two distinct
     wrappers reaching `adaptTo<Domain>:`, and `NoCoercion` carrying both operands.
- **S6 — Numeric equality and hash.** Fixes the second S4 gap: there is no `Number >>
  =`, so `2 = 2.0` is false while `2 < 2.0` promotes — and the `Ordered` trait
  *requires* `=` and derives `<=`/`>=` assuming a consistent total order, so its
  contract is unmet. For a wrapper, `Boxed(0) = 0` is false, making `isZero`/`even`/
  `odd` answer silently WRONG and `isPrime` call composites prime. Mirrors `<`
  exactly: `numeric_eq` in Rust with `numeric_lt`'s promotion policy, exposed as
  `_primEq:` on the three leaf arms; `<leaf> >> = aNumber [ ^self _primEq: aNumber
  value ]`; `Number >> = aNumber [ ^self value = aNumber ]`; plus `~=`.
  **DECIDED: `hash` is aligned in the same step** — making `2 = 2.0` true otherwise
  breaks equal-implies-same-hash (`hash_value` gives `Int(2)` → 2 but `Float(2.0)` →
  its bit pattern), corrupting `Dictionary`. So `hash_value`'s `Value::Float` arm must
  hash a whole-valued float as the corresponding integer (Pharo's choice). `Dictionary`
  itself needs no change — `pvtSlotFor:` already routes through Smol `hash` and `=`.
  Kept a separate step from S5: it is a semantic change touching collections.
  Watch for: the Rust `"="`/`"~="` arms for `Int`/`LargeInt`/`Float` go dead once the
  kernel shadows them, and any test asserting `2 = 2.0` is false will flip.
- **S7 — Prune** now-dead Rust arms in `dispatch_int`/`dispatch_float` that Smol
  fully supersedes; audit that every remaining `_prim` is irreducible. Update
  `docs/primitives.md`.

Each step is independently committable; S1 alone fixes the reported bugs.

## 7. Open questions / deferred

- **OPEN — `_prim` self-only rule is broader than its justification (noted S2,
  2026-08-01).** The self-only restriction (chunk 21) is justified by *entity* data
  thread-ownership: mutable entity state is touched only by its owning thread, so no
  lock is needed. That argument does **not** apply to `Value`s — they are immutable and
  shareable, so a `_prim` sent to a *non-self Value* has no data-race concern. So the
  rule is stricter than it strictly needs to be for the Value case. Two *other* reasons
  still favor keeping it strict: (1) **encapsulation** — primitives should stay behind
  public protocol (the tower's leaf-grounding relies on this); (2) **static
  checkability** — the check is a cheap syntactic `is_self_or_super`; "self or any
  Value" is not decidable at method-install time, so relaxing trades a compile-time
  guarantee for a runtime one. **Consequence for the tower:** none — leaf-grounding
  (`_prim` on the leaf's `self`, abstract code projects+re-dispatches) is the right
  design either way. Recorded because the rule's scope vs. its rationale is a genuine
  latent gap worth revisiting if it resurfaces (e.g. a future Value protocol that wants
  a primitive on a projected receiver). Cross-ref: memory `chunk21_actor_decisions`.
- **RESOLVED (§3):** arbitrary-precision leaf named `ArbitraryInteger`; bare `Int`
  retired in favour of abstract `Integer` (landed S1a, `82e8c13`).
- **RESOLVED (§4e):** seam placement, naming, and shape — `value`/`withValue:`
  *defaults* on `Value` (scope-lifted S2 so String/Symbol answer `value`), with
  `Number` overriding `value` to `subclassResponsibility` (open-class demand, not on
  `Integer`); the `Ordered` trait's seam is `<`, it gets no `value`; `value` kept over
  `primitiveValue`/`encodedValue`. The `_prim` grounding lives on the leaves
  (self-only rule); abstract code projects with `value` and re-dispatches.
- **RESOLVED (§2a):** comparability is the `Ordered` trait (requires `<`/`=`), not a
  `Magnitude` class; `String uses: Ordered` (not a Number/Magnitude). `Magnitude` =
  future richer trait (`uses: Ordered` + metric `-`/zero), recorded, deferred.
- **RESOLVED (§4b):** closedness mechanism — `vmSubclass:` + representational-block
  rule; no user-facing modifier; abstract classes (incl. `Integer`) open.
- **`_prim` naming/rework** — kept a **separate thread** (2026-07-12); co-design
  vocabulary with `vmSubclass:` when it runs.
- **Fraction / Rational** — not in this chunk; slots above Float in generality when
  added. Design later. S3 built the edge it will plug into (§4f), and
  `s3_user_number_joins_the_tower_without_changing_the_kernel` is the executable
  proof that adding it requires no kernel change.
- **RESOLVED (S3 follow-up, 2026-08-01) — binary-operator symbol literals.** The lexer
  accepted `#word`, `#'quoted'` and `#(` but not `#+` / `#//`, so the kernel had to
  write `#'+'`. `#` now reads a binary selector via the same one-or-two-character rule
  as operator tokens (so `#//` is one symbol), and the operator-char set is shared as
  `is_binop_char`.
- **RESOLVED (S3 follow-up, 2026-08-01) — `perform:withArguments:` argument list.**
  `cons_to_vec` terminated its walk on a non-`Cons` node, so passing an `Array` (`{x}`)
  silently produced a *zero*-argument send and failed far from the mistake. A node that
  is neither `nil` nor a `Cons` is now a `TypeError` naming the offending class. Still
  open, deliberately: whether `withArguments:` should accept any sequenceable
  collection rather than `Cons` alone — that is collection-protocol design, not a
  numeric-tower question.
- **Float subtyping under Number only** — Float is a `Number` but not an `Integer`;
  confirm no code assumes Number == Integer.
- **RESOLVED — `Int` name retirement** — done in S1a: `Integer` is the abstract
  shared class, literals produce `MachineInteger`, bare `Int` is gone.
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
