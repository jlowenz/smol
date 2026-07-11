# Scalar value representation & String plan

**Status:** design largely settled; implementation deferred to a later session.
One naming question open (§7) and one sequencing dependency: **traits should land
before or alongside this** (§8). Character is a follow-up (§6).

---

## 1. Governing principle — `Value` variants are *encodings*, not separate things

The `Value` enum variants (`Int`, `Str`, `Bool`, `Float`, `LargeInt`, `Symbol`,
later `Char`) are **unboxed encodings of instances of value classes**, not a
concept separate from "an object of that class." `Value::Str("hi")` *is* an instance
of `String`. The class object exists for dispatch, identity, and reflection; the
encoding *is* the instance. There is exactly **one** representation of a string, so
"a String value object indistinguishable from `Value::Str`" is automatic — there is
nothing to distinguish it from.

Analogy: Smalltalk immediates (`SmallInteger`, `Character`) vs. boxed objects — the
immediate is an encoding, not a different kind of thing.

## 2. Reparent scalars under `Value` (correctness fix)

**Bug:** `Int`/`Str`/`Nil`/`Block` are bootstrapped as subclasses of `Object`
(`is_value: false`), so `42 isKindOf: Value` answers **false** — wrong, given Int is
an immutable value.

**Fix:** reparent the clear scalar value classes under `Value` (`is_value: true`):
`Int`, `Float`, `LargeInt`, `String`, `Symbol`, `True`, `False`. (`Nil`, `Block`,
`Future`, `Namespace` are separate judgment calls — deferred; `Nil` is value-ish,
`Block`/`Future` debatable.)

- Files: `class_registry.rs` bootstrap.
- Add `lookup_user_method` arms for `Str`/`Float`/`Symbol`/`LargeInt` (currently
  **missing** — only `Nil`/`Block`/`Int`/`True`/`False` have them) so these classes
  can host kernel-defined methods at all.
- Note: the `is_value` flag is largely **inert** for encoded types — their instances
  are never built through the generic construction machinery (§4).

## 3. `String` surface (kernel methods on a real `String` class)

Once `String` has a class object + a `lookup_user_method` arm:
- **Existing** hardcoded selectors in `dispatch_str` stay as the primitive layer:
  `size`, `at:`, `,`, `=`, `~=`, `printString`, `asSymbol`, `reversed`,
  `copyFrom:to:`, `includesSubString:`. (Optionally re-expose as kernel methods over
  `_prim*` for uniformity.)
- **New** string operations (`split`, `indexOf:`, `trimmed`, `startsWith:`,
  `asUppercase`, `replaceAll:with:`, `linesDo:`, …) are written in **Smol** on the
  `String` class — the payoff of giving `String` a real class.
- **Indexing** is char-based (Unicode scalar), 1-based (as today). `at:` returns a
  `Character` once §6 lands; until then a 1-char `String`.
- **Byte-orientation caveat:** HTTP and sockets are byte-oriented; `String` is
  UTF-8 chars. Keep the `String` ↔ `ByteArray` distinction; server reads may prefer
  `ByteArray`. (Cross-ref `web_server_readiness.md`.)

## 4. Construction contract (encoded types never use generic construction)

Encoded value classes must **not** route `new`/`new:` through the generic paths
(`_primNewValue:` → frozen `ValueObject`, or `_primNew` → `Entity`) — that would
produce a class-`String` `ValueObject` that is *not* a `Value::Str` and breaks
`dispatch_str`. Every constructor is a **primitive returning the encoding**:

- `String class >> new` → `''` (empty `Value::Str`).
- `String class >> new: n withAll: aChar` → an immutable filled string (primitive).
- `new`/`new:` are **overridden per encoded class** so `is_value: true` never reaches
  the generic construction path.

## 5. Immutability & incremental building (no mutable strings)

`String` is an **immutable** `Value`. Pharo's mutable `String` (allocate `new: n`,
poke bytes) is **rejected** — it contradicts value immutability.

Incremental construction lives in a **separate mutable `Object`**:
- `StringBuilder` (Object) and/or `WriteStream on: String` — accumulates, and yields
  an **immutable** `String` via `contents` / `asString`.
- This is the **transients** idea (see `web_server_readiness.md`): mutability lives in
  Objects; values stay immutable. It *uses* the Value/Object distinction rather than
  fighting it, and is the general answer for building immutable collections too.
- Design of `StringBuilder`/streams: separate follow-up.

## 6. `Character` (follow-up session)

`Character` is another scalar value under the same encoding model: `Value::Char(char)`,
`Character < Value`.
- Lexer: `$x` character literal.
- Methods: `Character value: n`, `asInteger`, `asString`, `value`, comparisons,
  `isDigit`/`isLetter`/`isVowel`; constants `cr`/`lf`/`tab`/`space`.
- `String` integration: `at:` → `Character`; `String with: aChar`; `do:` yields
  `Character`s.
- Reconcile §3 control-char helpers: `String cr` becomes `String with: Character cr`.

## 7. Non-subclassable scalars (DECIDED) — terminology OPEN

**Decision:** encoded scalar value classes are **not subclassable** (no subtypes).

Rationale (representation *and* concept agree):
- Subtyping a scalar is a **restrictive-subtype (Liskov) category error**: a domain
  type (`Html`/`EmailAddress`/`Money`) *restricts* the carrier, so it cannot stand in
  for an arbitrary `String`/`Integer` — substitution breaks in the combinator
  direction (`html , rawString` loses the invariant).
- The unboxed encoding carries no subclass tag, so it *can't* represent a subclass
  instance anyway.

**Extensibility is preserved on two of three axes** — only subclassing is denied:
1. **Open classes** — adding methods to `String` is allowed (extension). ✔
2. **Traits** — behavioral conformance (`StringLike`) for wrappers. ✔
3. **Subclassing** (subtyping) — denied. ✘

Domain types → a **wrapper value object** (a `ValueObject` with a `String`/`Int`
ivar) + a **trait** that delegates string-like behavior to the inner carrier. This is
`can-act-as` (structural conformance) instead of a false `is-a` (nominal subtype).

**⚠ TERMINOLOGY (open):** "final" is wrong — it implies *no new methods*, but we want
**open to method addition, closed to subclassing**. Candidate terms:
- `leaf` / "leaf value class" — structural, bottom-of-hierarchy (leaning candidate).
- "subclass-closed / extension-open" — precise but verbose.
- `sealed` — **avoid**: in Java/Kotlin/Scala/C# it means *restricted permitted
  subclasses*, a different thing.
- `terminal`.
Pick a ubiquitous-language term before implementing (DDD: consistent language).

**Escape hatch (documented, NOT built):** a class-tagged encoding
(`Value::Str { class: ClassId, data: Arc<str> }`) could allow scalar subclasses, at
the cost of a word per value + a class-propagation decision at every combinator
(`html , html` → `Html`? `html , string` → ?). That propagation question *is* the LSP
problem resurfacing as an implementation choice — more evidence the clean answer is
"don't." Keep as a known escape hatch.

## 8. Sequencing — traits first

The finality decision's extensibility story **leans on traits** (`StringLike` for
wrappers); without traits, finality feels restrictive. Traits also gate the stdlib
build-out (`web_server_readiness.md`). Therefore: **traits → scalar/String cleanup →
Character.**

## Open questions
- §7 terminology.
- §2 parentage of `Nil`/`Block`/`Future`/`Namespace`.
- §3 `String` vs `ByteArray` for byte-oriented I/O (server reads).
- Reconciling closed-scalars with Smol's open/extensible ethos — decided
  non-subclassable, but the *felt* principledness depends on traits landing.
