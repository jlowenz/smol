# Chunk 23: Kernel Coverage Audit and Minor Gap Fixes

## Goals

- Improve Rust test line coverage as much as possible. There are several files with
  test coverage under 90, and we should aim for keeping things as high as reasonably
  possible.
- Systematic method-by-method audit of `kernel.smol`. Every method body and every
  conditional branch must be exercised by at least one test in `tests/kernel.rs`. Add
  tests for any gap found. Fix a small number of latent bugs discovered during audit.

This chunk has no language changes — it is a quality gate before Chunk 24
(class-as-singleton-entity), which is a large structural change that benefits from
a clean, fully-exercised baseline.

---

## S1 status: complete

`tests/kernel.rs` has been created with a `class_print_string` test that verifies
`printString` on a class returns a `String` matching the class name. This covers the
intended behavior.

The `println!` calls in the `printString` arms of `builtins.rs` (lines ~988, ~1271,
~1594) are **intentional debug aids**, not bugs. The `true`/`false` println! calls
at lines ~904/906 are similar. Removing them and designing a proper printString API
is deferred to a later chunk.

---

## Audit scope

All classes and methods currently in `kernel.smol`, in order:

### ValueHolder
- `set:`, `get`, `update:` — check update: receives old value as block arg

### Array
- `at:`, `at:put:`, `size`, `isEmpty`, `notEmpty`, `first`, `last`
- `do:`, `collect:`, `select:`, `reject:`
- `detect:` — **both** the found branch AND the "no element" error branch
- `detect:ifNone:` — both branches
- `inject:into:`
- `includes:` — found and not-found
- `copyFrom:to:` — normal range and single-element range
- `=` — equal and unequal (size mismatch, element mismatch, class mismatch)
- `~=`
- `printString`

### ByteArray
- `at:`, `at:put:`, `size`, `isEmpty`, `notEmpty`
- `do:`, `copyFrom:to:`, `asString`, `printString`

### OrderedCollection
- `initialize`, `size`, `isEmpty`, `notEmpty`, `first`, `last`
- `at:` — in-bounds and **out-of-bounds error**
- `at:put:` — in-bounds and **out-of-bounds error**
- `add:`, `addLast:`, `addFirst:` — normal case and **growth path** (`pvtGrow`)
- `removeFirst` — normal and **empty collection error**
- `removeLast` — normal and **empty collection error**
- `remove:ifAbsent:` — found and absent branches
- `do:`, `collect:`, `select:`, `reject:`
- `detect:` — found and **error branch**
- `detect:ifNone:` — both branches
- `inject:into:`
- `includes:` — found and not-found
- `copyFrom:to:`
- `=` — equal and unequal cases
- `~=`, `printString`

### Error
- `Error class >> signal:` — verify it signals and the message is accessible
- `Error >> pvtSetMessage:`, `Error >> message`, `Error >> messageText`
- `Error >> signal`

### Object >> error: (override)
- Test that `anObject error: 'msg'` raises a signal caught by `on:do:`

### Object >> hash
- Verify `hash` returns an Int; verify `a = b → a hash = b hash` for instances

### Object >> initialize (default no-op)
- Verify it returns without error (called by `new`)

### Object class >> new / Object class >> new:
- `new` — verify that `initialize` is called (use a subclass that sets an ivar in `initialize`)
- `new:` — verify indexed instance is properly sized

### Association
- `Association class >> key:value:`
- `key`, `value`, `value:` — getter, getter, setter
- `=` — same key/class, different key, different class
- `printString`

### Dictionary
- `initialize`, `size`, `isEmpty`, `notEmpty`
- `at:put:` — insert, update, **rehash trigger** (fill past 75% load)
- `at:` — found and **absent error**
- `at:ifAbsent:` — found and absent
- `includesKey:` — true and false
- `removeKey:ifAbsent:` — found and absent
- `do:`, `keysDo:`, `associationsDo:`
- `keys`, `values`
- `=` — equal, size mismatch, value mismatch
- `printString`

---

## Approach

Work through the audit list section by section, running `cargo test` after each
section to confirm no regressions. New kernel method tests go in `tests/kernel.rs`
using the `eval` / `eval_err` helpers. Builtin dispatch tests go in `tests/builtins.rs`.

For error branches, use `eval_err` and assert the error message contains the expected
substring.

---

## Steps

- **S0** Run `just cov` to get per-file Rust coverage numbers. For each interpreter
         source file under 90%, identify uncovered paths and add targeted tests to the
         appropriate test file (`tests/builtins.rs`, `tests/eval.rs`, etc.). Re-run
         `just cov` to confirm improvement before proceeding.
- **S1** ~~Fix `println!` bug~~; add `class_print_string` test to `tests/kernel.rs`. **Done.**
- **S2** Audit ValueHolder + Array; add gap tests to `tests/kernel.rs`.
- **S3** Audit ByteArray + OrderedCollection; add gap tests to `tests/kernel.rs`.
- **S4** Audit Error, Object extensions (error:, hash, initialize, new, new:); add gap tests to `tests/kernel.rs`.
- **S5** Audit Association + Dictionary; add gap tests to `tests/kernel.rs`.
- **S6** `cargo fmt`, `cargo clippy`, full test run; commit.

**Test placement rule:** Kernel method behavior tests → `tests/kernel.rs`. Builtin
dispatch tests (testing primitive dispatch directly) → `tests/builtins.rs`. Kernel
tests that currently live in `eval.rs` can migrate to `kernel.rs` when their methods
are audited.
