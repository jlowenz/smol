# Q24 v0 plan: `^` as the method answer only, and the `Reducible` protocol

**Date:** 2026-11-11. **Status:** draft, not started.
**Decisions:** `docs/open_questions.md` Q24, "Decision (user, 2026-11-10)" and
"Protocol design (2026-11-10)". **Probe:** `docs/experiments/q24_protocols_v0.smol`
(17/17 on workspace stand-ins).

## Goal

The kernel and tests no longer use `^` inside a block. The parser rejects it, and the
VM plan no longer has `NonLocalReturn`. Early exits from iteration go through the
stopping fold `reduceWith:initial:` and the `Reducible` queries.

Every step leaves the suite green. Steps 0–4 only add or migrate; step 5 turns the
rule on.

## Out of scope

- `AsyncReducible` / `reduceAllWith:`, and cheap composition (deferred together).
- Multi-way conditional, tuples, records, sum types, named loops.
- Q18 (names in class extensions) and Q13 (standalone `uses:`).
- The combinator slowdown (`q24_calculator_results.md` finding 7).

## Step 0: parser warnings

The parser can only fail today (`Result<Chunk, ParseError>`). Add a channel for
warnings, so the migration can be measured as it goes.

- `Diagnostic { severity, kind, span, message }`, with `Severity::{Warning, Error}`.
  This is bootstrap plumbing. A Smol compiler would signal resumable warning
  conditions instead, as CL (`style-warning`, `with-compilation-unit`) and Pharo do,
  and this type would not survive. Keep it minimal.
- The parser collects warnings into a `Vec<Diagnostic>`. A new entry point answers
  the chunk together with its warnings. `Parser::parse` and `parse_source` keep their
  signatures (15 callers, mostly test helpers).
- Two warning kinds, named in the language's terms (`^` is the method's *answer*;
  see `docs/glossary.md`, "Answer"):
  - `AnswerInBlock`: a `^` inside any block, checked beside the existing check at
    `parser.rs:162`. Message: "`^` marks the method's answer and cannot appear in a
    block (Smalltalk's non-local return is not supported)".
  - `AnswerNotLast`: a `^` statement followed by more statements in the same body.
- Where warnings appear:
  - The REPL prints them to stderr with line and column.
  - Kernel load collects them.
  - A `just` recipe (or an ignored test) lists the kernel's warnings. That list
    measures steps 2–3.
- Line and column come from the `Span` byte offsets. Reuse the coverage tooling's
  span-to-line mapping if it fits.

**Done when:** the REPL shows both warnings on a sample, the recipe lists the
kernel's current set (about 48), and the suite is unchanged.

## Step 1: add `Reduced` and `Reducible` (nothing removed)

- **Kernel order.** `Trait` is declared at `kernel.smol:507`, after Array, ByteArray
  and OrderedCollection, and `uses:` is a clause of the class declaration. Either move
  the Trait section and `Reducible` above the collections, or move the collections
  below. Decide when implementing. Check that nothing in the moved section depends on
  something defined later.
- **`Reduced`.** A kernel Value: `Reduced with: x`, accessor `value`. If `value`
  conflicts with `Value >> value`, rename the accessor (Q24).
- **Private markers** for the seedless `reduceWith:` and for `findIf:otherwise:`. The
  probe uses `Unseeded` and `NotFound`, recognized by an exact class test. Their names
  in the kernel are open: classes have no `pvt` convention yet.
- **`Reducible`** requires `reduceWith:initial:` and provides `do:`, `reduceWith:`,
  `findIf:otherwise:`, `findIf:`, `contains:`, `anySatisfy:`, `allSatisfy:`,
  `noneSatisfy:` and `countIf:`. Bodies as in the probe.
- **`reduceWith:initial:`** in each class. Contract:
  - Requires: `aBlock` takes 2 arguments (accumulator, element).
  - Ensures: answers the accumulator after the last element, or `r value` for the
    first step answer `r` with `r class = Reduced`. No step runs after that one.
  - Each step's answer is forced before the test, so a `Reduced` arriving in a
    Future is honored.

  Implementations:
  - Array and ByteArray: index walk over `_primAt:`.
  - OrderedCollection: index walk over `storage` up to `size`.
  - Dictionary: over values, skipping empty slots, matching `Dictionary >> do:`.
  - String: `size` and `at:`, so elements are one-character Strings.
  - Cons: head, then tail.
- **Nil**, the empty list, cannot take a trait. Give it direct methods:
  - `reduceWith:initial:` answers `init`; `reduceWith:` is an error.
  - `findIf:otherwise:` answers `alt value`; `findIf:` is an error.
  - `contains:` and `anySatisfy:` answer false; `allSatisfy:` and `noneSatisfy:`
    answer true.
  - `countIf:` answers 0.
- Classes that already define `do:` keep it. Class methods take precedence over
  trait methods (`traits_plan.md`), so the trait's `do:` only reaches String.

**Tests** (Rust, per the kernel audit rule in CLAUDE.md; `just cov-suite` must show
every new kernel method and both branches of each conditional covered):

- every provided method on every class, including the empty collection;
- a stop on the first element, on the last element, and never;
- `findIf:` finding `nil`;
- a `Reduced` answered through a Future from another entity;
- `nil` cases;
- String elements.

## Step 2: move kernel early exits onto the fold

Rewrite the 11 early-exit sites without `^` in blocks:

| Site | Rewrite |
|---|---|
| `Array`/`OrderedCollection >> detect:`, `detect:ifNone:`, `includes:` | delegate to `findIf:`, `findIf:otherwise:`, `contains:` (old selectors stay until step 4) |
| `Array >> =`, `OrderedCollection >> =` | same class and size, then an index walk, or `allSatisfy:` over indices |
| `Dictionary >> =` | `allSatisfy:` over associations (the `await`s from the I5 fix stay) |
| `Dictionary >> pvtSlotFor:` | tail-recursive probe method |

Kernel callers of `inject:into:` move to `reduceWith:initial:`. The current
Array/OrderedCollection `inject:into:` runs asynchronously (a ValueHolder plus
`then:`); the fold runs synchronously within the owner's turn. Check each caller
(for example `asDebugString`) for behavior that depended on that.

**Done when:** the step 0 recipe shows no early-exit sites, and the suite is green.

## Step 3: guards become expressions

About 36 sites of the form `cond ifFalse: [^x]. rest` become
`^cond ifTrue: [rest] ifFalse: [x]`. Examples: `to:do:` and its siblings,
`whileTrue:`, `timesRepeat:`, `factorial`, `isPrime`, `sign`, `gcd:`, the class
checks in `=`, `Dictionary >> at:ifAbsent:`, `Condition >> pvtFindRestart:in:` and
`Future >> pvtSettle:`.

- TCO must survive for a tail send inside an inlined `ifTrue:` block. The probe's
  `to2:do:` ran in constant space at 10⁶ iterations, at the same speed as `to:do:`.
  Re-measure the loop methods after rewriting.
- `OrderedCollection >> remove:ifAbsent:` has a `^` inside a `then:` continuation,
  which runs on a continuation entity (Q23 B2). Establish its current behavior with a
  test first (the 2026-11-10 probe was malformed), then rewrite it as find the index,
  then remove.

**Done when:** the step 0 recipe lists zero kernel warnings.

## Step 4: retire the old selectors

- Decide: remove `inject:into:`, `detect:`, `detect:ifNone:` and `includes:`, or
  keep the Pharo names as an opt-in alias trait.
- Migrate Rust tests: `inject:` (16 lines, 4 files), `detect:` (8 lines),
  `includes:` (5 lines). Also docs and examples, about 12 files.
- Consider `includesKey:` → `containsKey:` for consistency with `contains:`.

## Step 5: turn the rule on

- `AnswerInBlock` and `AnswerNotLast` become errors. The existing
  `NonLocalReturnOutsideMethod` is subsumed by `AnswerInBlock`.
- Rename `Stmt::Return` to `Stmt::Answer` and `EvalError::Return` to
  `EvalError::Answer` (44 sites), so the AST uses the language's term. The VM keeps
  `Return A` and `ReturnSelf` (step 6): in the VM's context they are frame
  mechanics.
- Fix the tests that break. There are at most 273 lines in 8 files with a `[ … ^`
  pattern, mostly `tests/eval.rs`. That count includes legal one-line method bodies,
  so it is an overestimate. Tests that exercised non-local return become parse-error
  tests.
- `tests/nonlocal_return.rs`: the 3 `red_*` and 4 `ignored_*` tests become
  parse-error tests or are deleted. Close I5 in `docs/issues.md`.
- Update `docs/Smol_Patterns.md`, whose Pattern 4 is "`^` as early exit", and the
  `.smol` files that use the pattern (up to 10).

## Step 6: VM plan

In `vm_compiler_plan.md`:

- remove `NonLocalReturn A` and its unwinding through `ensure:`;
- keep `Return A` and `ReturnSelf`;
- §3 item 5: a clean block is one with no captures;
- V2 scope: drop non-local returns.

## Open during implementation

- Kernel order for `uses: Reducible` (step 1).
- Names of the private marker classes (step 1).
- Remove or alias the old selectors (step 4).
- Warning display format (step 0).
