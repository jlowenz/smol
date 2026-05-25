# Chunk 23: Kernel Coverage Audit and Minor Gap Fixes

## Goals

- Improve Rust test line coverage as much as possible. There are several files with
  test coverage under 90, and we should aim for keeping things as high as reasonably
  possible.
- Systematic method-by-method audit of `kernel.smol`. Every method body and every
  conditional branch must be exercised by at least one test in `tests/eval.rs`. Add
  tests for any gap found. Fix a small number of latent bugs discovered during audit.

This chunk has no language changes — it is a quality gate before Chunk 24
(class-as-singleton-entity), which is a large structural change that benefits from
a clean, fully-exercised baseline.

---

## Known bugs to fix before audit

**Stray `println!` in `dispatch_class_builtin`** (`builtins.rs` ~line 1594):
```rust
"printString" => {
    let s: Arc<str> = class.name.clone();
    println!("{:?}", s);  // ← remove
    Ok(Value::Str(s))
}
```
Add a `tests/builtins.rs` test that sends `printString` to a class and asserts the
result is a String matching the class name, with no side effects.

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

Ask Claude Code to work through the list section by section, running `cargo test`
after each section to confirm no regressions. New tests go in `tests/eval.rs` using
the `eval(prog)` / `eval_err(prog)` helpers - OR - we could create a dedicated `tests/kernel.rs` 
test file to specifically contain tests that are covering the kernel methods we add -
I think we should prefer that, unless there's some reason not to. 

For error branches, use the `eval_err` helper and assert the error message contains
the expected substring.

---

## Steps

- **S1** Fix `println!` bug; add printString-to-class test.
- **S2** Audit ValueHolder + Array; add gap tests.
- **S3** Audit ByteArray + OrderedCollection; add gap tests.
- **S4** Audit Error, Object extensions (error:, hash, initialize, new, new:); add gap tests.
- **S5** Audit Association + Dictionary; add gap tests.
- **S6** `cargo fmt`, `cargo clippy`, full test run; commit.
