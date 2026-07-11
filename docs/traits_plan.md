# Traits plan

**Status:** design settled for an MVP; implementation is the next work chunk.
Deferred to a later version: public-protocol markers (§8), class-side trait methods
(§8). Disallowed for now: `super` in trait methods (§4).

Traits are the prerequisite that several things wait on: the extensibility story
for non-subclassable scalars ([[design_scalar_representation]] / behavioral reuse via
`StringLike`), shallower class hierarchies, and stdlib composition
(`web_server_readiness.md`). So this lands **before** the scalar/String cleanup.

---

## 1. Model (MVP)

- **Stateless.** A trait provides *methods only* — no instance variables. It reaches
  host state through **required methods** (accessors), which are *derived* (§3), not
  declared. Stateless composition has no state-conflict problems and, crucially, a
  trait used by a `Value` class cannot smuggle in mutability.
- **Flattened at composition.** When a class composes a trait, the trait's methods are
  copied into the class's method dict, with **provenance** recorded per method (for
  reflection/tooling). Dispatch is unchanged — one method-dict lookup up the
  superclass chain, no runtime trait search, no diamonds.
- **Precedence:** `class-defined  >  flattened trait methods  >  superclass`.
- **Seamless for Value and Object classes** (stateless ⇒ safe for both).
- **Instance-side only** for MVP; **no `super`** inside trait methods (a trait has no
  superclass).

## 2. Traits are first-class objects; the algebra is messages

A `Trait` is an object. A **trait expression** is an ordinary Smol expression that
evaluates to a `Trait`, built with binary-message operators:

```
T1 + T2                     "sum / union (conflicts → conflict markers)"
T - { #a. #b }              "exclusion of an explicit selector set"
T1 - T2                     "exclusion of T2's PROVIDED selectors from T1"
T @ { #new -> #old. ... }   "aliasing: add entry points (shallow)"
```

- Binary-message precedence: left-associative, equal precedence — parenthesize to
  group. No new parser rules; `+`/`-`/`@` are just selectors on `Trait`.
- `-` is polymorphic on its right operand: a **selector set** or a **Trait**.
- `T1 - T2` enables **asymmetric override**: `(T1 - T2) + T2` = "union, but T2 wins
  every conflict," derived from T2's current surface (never drifts).
- **Aliasing is shallow**: `T @ {#fooB -> #foo}` adds a `#fooB` entry point with
  `#foo`'s body but does **not** redirect T's internal `self foo` sends. It gives a
  second name to reach a behavior; it does not remove a conflict by itself.

## 3. Requirements — derived, not declared

`requirements(T)` = **every selector T's method bodies send to `self` that T does not
itself provide.** Computed from the ASTs; directly displayable ("T needs `asString`,
`size`").

- **Checked at composition (fail-fast):** `requirements(finalExpr) − (class methods ∪
  superclass chain ∪ other composed traits)` must be empty, else class-definition
  errors. Object's universal protocol is in the superclass chain, so `self printString`
  is a requirement that is always satisfied and never surfaces.
- **Safety net:** because requirements come from *self-sends*, excluding/renaming a
  selector that another composed trait self-sends leaves a requirement unmet →
  composition error. You cannot silently strand a collaborator.
- Optional (rare): an explicit `requires:` augmentation for requirements derivation
  can't see (a `perform:` with a computed selector). Derivation is primary.

## 4. Conflicts

`T1 + T2` where both provide selector `s` with **differing bodies** ⇒ the composite
carries a **conflict marker** for `s` (a sentinel method that raises `TraitConflict`
if ever sent). The **same** method inherited from a shared ancestor trait is **not** a
conflict (identical provenance).

Resolution options:
- **Exclude** a loser: `T1 + (T2 - {#s})`, or asymmetrically `(T1 - T2) + T2`.
- **Override** in the class (class-wins resolves the marker), optionally combining
  aliased entry points:
  ```
  Value subclass: #Html
      uses: { (T1 @ {#fooT1 -> #foo}) + (T2 @ {#fooT2 -> #foo}) }
      instanceVariableNames: 'raw'.
  Html >> foo [ ^self fooT1 , self fooT2 ]      "class owns the final #foo"
  Html >> asString [ ^raw ]
  ```
An **unresolved** conflict marker at class-definition time ⇒ error.

**Who owns the protocol:** a trait provides *behavior*; the composing class owns the
final *protocol*. External callers depend on the *class* answering a selector, not on
"T1's `#foo`." Exclusion/aliasing are deliberate surgery by the class author, who then
owns the result — the same responsibility as overriding an inherited method. (A
future public-protocol marker, §8, would let a trait flag selectors whose
exclusion/rename should warn.)

## 5. Algebra semantics

For each form, `provides` = set of `(selector ↦ method, provenance)` and
`requirements` as in §3:

| Form | provides | requirements |
|------|----------|--------------|
| `T` (atomic) | T's defined methods | self-sends not provided by T |
| `T - S` (set) | `provides(T)` minus `S` | `requirements(T)` (may now be unmet ⇒ caught at composition) |
| `T1 - T2` | `provides(T1)` minus `selectors(provides(T2))` | as above |
| `T @ A` | `provides(T)` ∪ { newSel ↦ body(oldSel) } | `requirements(T)` ∪ referenced oldSels |
| `T1 + T2` | union; differing bodies for a shared selector ⇒ conflict marker | `(requirements(T1) ∪ requirements(T2)) − provides(T1+T2)` |

`+` is associative and commutative. The algebra is **recursive**: a trait can be
*defined* from an expression, so sums/exclusions/aliases compose arbitrarily.

## 6. Defining and using traits

```
Trait named: #StringLike.                      "empty trait; add methods"
StringLike >> startsWith: p [ ^ ... self asString ... ].

Trait named: #Foo from: TA + (TB - TC).        "named trait from an expression"

Value  subclass: #Html uses: <TExpr> instanceVariableNames: 'raw' ...
Object subclass: #Widget uses: <TExpr> instanceVariableNames: '...' ...
```

`uses:` takes a `Trait` object — a named trait or an inline expression. Convention
prefers naming non-trivial expressions.

**Composition contract (`subclass:…uses:…`):**
- *Requires:* the `TExpr` evaluates to a `Trait`; every conflict marker in
  `provides(TExpr)` is resolved by a class-defined method; `requirements` are met by
  `class ∪ superclass chain ∪ TExpr`.
- *Ensures:* the class method dict = `flatten(provides(TExpr))` with class-defined
  methods overriding; per-method provenance recorded; dispatch precedence
  `class > trait > super`.

## 7. Implementation sketch (Rust) — for next session

- A `Trait` runtime type: `name`, method dict (`selector → Arc<Method>`), derived
  `requirements: HashSet<Arc<str>>`, and provenance. Trait objects are produced by
  `Trait named:` / `named:from:` and by the `+ - @` operators (which build new Trait
  objects).
- **Requirement derivation:** walk each `Method` body AST for self-sends
  (`Expr::Send` with a `self`/implicit-self receiver), collect selectors, subtract
  provided. (Blocks/`perform:` are the known blind spots → optional `requires:`.)
- **Conflict markers:** a sentinel `Method` that raises a `TraitConflict` condition
  when invoked; detected as leftover at composition time.
- **Composition:** extend the `subclass:…uses:…` class-creation path — evaluate the
  `TExpr`, flatten `provides` into the new `Class`'s `methods` dict (class-defined
  win), then validate (no leftover markers; requirements met) or raise.
- **Provenance:** a side table on `Class` (`selector → TraitName`) for reflection;
  the methods themselves live in the normal dict so dispatch is untouched.
- **Kernel/parser:** `Trait` class + `named:`/`named:from:`, binary `+`/`-`/`@` on
  `Trait`, and a `subclass:uses:instanceVariableNames:` (and siblings) class-def form.

## 8. Deferred / open

- **Public-protocol markers** (v2): a trait flags its intended public selectors;
  excluding/renaming one warns. (User likes this; deferred.)
- **Class-side trait methods** (v2).
- **Override sugar:** a named `T1 override: T2` for `(T1 - T2) + T2` — optional
  convenience; MVP uses the compositional form.
- **Tooling:** display a trait's derived requirements and per-method provenance
  (moldable development) — later.
