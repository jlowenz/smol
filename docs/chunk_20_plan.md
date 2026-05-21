# Chunk 20: Signals, Dictionary, and Actor Prerequisites

## Status — COMPLETE

Chunk 20 delivered `EvalError::Signal`, `_primSignal:`, `_primHash` (universal),
`Error`, `Association`, `->` (universal Rust primitive), and `Dictionary` with full
open-addressing hash map. 362 interpreter tests pass; workspace line coverage 95%.

**Implementation note:** `Object >> ->` was implemented as a universal Rust handler
in `dispatch()` (like `yourself`) rather than as a Smol method on Object, so that
primitive value types (String, Int, Symbol, etc.) work as receivers without requiring
a new `SelfBinding::Primitive` dispatch path.

Previous state: Chunk 19 delivered Array, ByteArray, and OrderedCollection in Smol, proper non-local
returns (depth-tagged `EvalError::Return`), nested array literals, and five Rust
primitives. 241 interpreter tests pass; workspace line coverage 91%.

**Scope note:** This chunk was revised from its original form (which also included
`Float64Array`, `species`, `ReadStream`, and `WriteStream`) to focus on the
prerequisites that unblock chunk 21's async actor model MVP — the most
architecturally novel and highest-risk component of the system. The deferred items
have no ordering dependency on the actor model and will land in chunk 22.

**Delivers:**

1. `EvalError::Signal` — a new Rust error variant for Smol-raised conditions,
   distinct from `TypeError`.
2. `_primSignal:` and `_primHash` — two Rust primitives on `Object`.
3. `Error` class in Smol — thin wrapper that makes signaling a message send rather
   than a Rust escape hatch.
4. `Association` — `key → value` pair used by `Dictionary` and as a general-purpose
   tuple.
5. `Dictionary` — open-addressing hash map implemented in Smol over a flat `Array`
   of `Association` pairs; needed by chunk 21 for entity registries and dispatch tables.

**Explicitly deferred:**

- `Float64Array` — no actor model dependency; chunk 22.
- `species` hook on collection classes — no actor model dependency; chunk 22.
- `ReadStream` / `WriteStream` — no actor model dependency; chunk 22.
- `Set` — straightforward once `Dictionary` exists; chunk 22.
- Brace array syntax `{expr. expr}` — chunk 22.
- Full exception hierarchy (`Exception`, `resume`, `retry`, `ensure:`) — chunk 21
  will clarify what the actor error model needs; full hierarchy chunk 22+.
- `asUtf8String` (strict, error on invalid bytes) — deferred; `asString` stays lossy.
- Traits / mixin composition — requires a separate design chunk.

---

## Design decisions

### Exception model

The goal is a minimal but honest exception hook. `on:do:` already dispatches any
`EvalError` to the handler block; what is missing is the ability for Smol code to
deliberately raise a condition.

One Rust primitive is added:

| Primitive | Semantics |
|---|---|
| `_primSignal: aString` on Object | Raises `EvalError::Signal(aString)` — a new variant distinct from `TypeError`. |

A new `EvalError` variant:

```rust
#[error("signal: {0}")]
Signal(Arc<str>),
```

`Signal` propagates identically to `TypeError` through `eval_block` and
`invoke_method`, but carries a clean message string that `on:do:` can present
to the handler without the `"type error: "` prefix. Chunk 21 will extend the
actor scheduler to treat unhandled `Signal` as an entity-termination event (fault
isolation) rather than a process-wide panic.

In kernel.smol, `Error` is a thin Smol class:

```smalltalk
Object subclass: #Error
    instanceVariableNames: 'message'
    classVariableNames: ''
    indexedSlots: #none.

Error class >> signal: aString [
    | e |
    e := self new.
    e pvtSetMessage: aString.
    e signal
]

Error >> pvtSetMessage: s [ message := s ]
Error >> message [ ^message ]
Error >> signal [ self _primSignal: message ]
Error >> messageText [ ^message ]
```

`error:` on Object (added in chunk 19) becomes:

```smalltalk
Object >> error: aString [ Error signal: aString ]
```

This replaces the current Rust implementation of `error:` (which raises `TypeError`)
with a Smol delegation that raises `Signal`. The `on:do:` handler already catches all
non-Return errors, so existing tests are unaffected.

`_primSignal:` stays as the single Rust escape hatch; everything above it is Smol.

### `on:do:` refinement

The existing `on:do:` in `dispatch_block` catches any `EvalError` other than
`Return`. After this chunk the handler block receives the message string regardless
of whether the error was a `Signal` or a `TypeError`. No Rust change is required
for the basic case.

A class-check argument (the first arg to `on:do:`) is currently ignored. This chunk
does **not** add class-filtered dispatch — that requires a proper `Exception`
hierarchy and is deferred pending the actor error model design in chunk 21.

### Association

```smalltalk
Object subclass: #Association
    instanceVariableNames: 'key value'
    classVariableNames: ''
    indexedSlots: #none.

Association class >> key: k value: v [
    | a |
    a := self new.
    a pvtKey: k value: v.
    ^a
]

Association >> pvtKey: k value: v [ key := k. value := v ]
Association >> key   [ ^key ]
Association >> value [ ^value ]
Association >> value: v [ value := v ]
Association >> = other [
    self class = other class ifFalse: [^false].
    ^key = other key
]
Association >> printString [
    ^key printString , ' -> ' , value printString
]
```

The `->` binary operator already lexes as `BinOp("->")`. A convenience method on
Object allows `k -> v` literal syntax:

```smalltalk
Object >> -> aValue [ ^Association key: self value: aValue ]
```

### Dictionary

`Dictionary` uses open addressing over a flat `Array` of `Association` objects, with
`nil` marking empty slots. Initial capacity 8; load factor threshold 0.75 triggers
a doubling rehash.

Hash is obtained via `_primHash` — a new Rust primitive on Object that returns a
stable `Int` based on the value's identity or content:

| Value kind | Hash strategy |
|---|---|
| `Int`, `LargeInt`, `Float` | numeric hash |
| `Str`, `Symbol` | FNV-1a over bytes |
| `Bool`, `Nil` | fixed constants |
| `Object`, `Class`, `Block` | pointer address |

```smalltalk
Object subclass: #Dictionary
    instanceVariableNames: 'table size'
    classVariableNames: ''
    indexedSlots: #none.

Dictionary class >> new [
    | d |
    d := super new.
    d pvtInit.
    ^d
]

Dictionary >> pvtInit [
    table := Array new: 8.
    size := 0
]

Dictionary >> at: key put: value [ ... ]
Dictionary >> at: key [ ... ]
Dictionary >> at: key ifAbsent: aBlock [ ... ]
Dictionary >> includesKey: key [ ... ]
Dictionary >> removeKey: key ifAbsent: aBlock [ ... ]
Dictionary >> size [ ^size ]
Dictionary >> isEmpty [ ^size = 0 ]
Dictionary >> notEmpty [ ^size > 0 ]
Dictionary >> do: aBlock [ ... "iterate values" ]
Dictionary >> keysDo: aBlock [ ... ]
Dictionary >> associationsDo: aBlock [ ... ]
Dictionary >> keys [ ... "returns OrderedCollection" ]
Dictionary >> values [ ... ]
Dictionary >> printString [ ... ]
Dictionary >> = other [ ... ]
```

`at:` raises `Error signal: 'key not found'` on miss. The Smol implementation uses
`_primHash` to locate the slot; collision resolution is linear probing.

Rehashing copies all live associations into a new double-sized table.

### `_primHash` design

Returned as `Value::Int(hash as i64)`. The hash is not cryptographic; stability
across process restarts is not guaranteed (pointer-based hashes will vary). Users
who need persistent keys should hash over content (`Str`, `Symbol`, numeric types)
rather than object identity.

Implementation in Rust:

```rust
"_primHash" => {
    expect_arity(sel, 0, args.len())?;
    let addr = Arc::as_ptr(&obj) as usize;
    Ok(Value::Int(fmix64(addr as u64) as i64))
}
```

`_primHash` on primitive values (`Int`, `Str`, etc.) is handled in their respective
dispatch functions using the same scheme as `Value::Hash` (FNV-1a for strings,
numeric value for ints). It is added to the universal dispatch in `dispatch()` before
routing to type-specific handlers.

---

## Changes to existing source files

### `src/eval.rs`

- Add `EvalError::Signal(Arc<str>)` variant with `#[error("signal: {0}")]`.
- No change to `eval_block` or `invoke_method` — `Signal` propagates like `TypeError`.

### `src/builtins.rs`

- Add `_primSignal:` and `_primHash` to `dispatch_object_primitive`.
- Add universal `_primHash` dispatch in `dispatch()` for primitive values
  (`Int`, `Float`, `Str`, `Symbol`, `Bool`, `Nil`, `LargeInt`).
- Remove `error:` from `dispatch_object_primitive` (it moves to Smol).

### `src/kernel.smol`

- Add `Error`, `Association`, `Dictionary`.
- Add `Object >> ->` for `Association` construction.
- Replace Rust `error:` with Smol delegation to `Error`.

---

## New Rust surface

| Addition | Location | Notes |
|---|---|---|
| `EvalError::Signal(Arc<str>)` | `eval.rs` | New error variant for Smol-raised signals |
| `_primSignal: aString` | `dispatch_object_primitive` | Raises `EvalError::Signal` |
| `_primHash` | `dispatch_object_primitive` + universal `dispatch()` | Returns stable `Int` hash of receiver |

---

## Protocol tables

### Error

| Selector | Ensures |
|---|---|
| `Error class >> signal: s` | Creates instance, signals with message `s` |
| `signal` | Raises `EvalError::Signal(message)` |
| `message` | Returns the message string |
| `messageText` | Alias for `message` |

### Association

| Selector | Ensures |
|---|---|
| `Association class >> key: k value: v` | New association |
| `key` | Returns key |
| `value` | Returns value |
| `value: v` | Updates value |
| `=` | Equality by key |
| `printString` | `'key -> value'` |

### Dictionary

| Selector | Requires | Ensures |
|---|---|---|
| `at: k put: v` | — | Stores mapping; returns `v` |
| `at: k` | key present | Returns value; `Error signal:` if absent |
| `at: k ifAbsent: blk` | — | Returns value or block result |
| `includesKey: k` | — | Bool |
| `removeKey: k ifAbsent: blk` | — | Removes and returns value or block result |
| `size` | — | Number of entries |
| `do: blk` | — | Iterates values |
| `keysDo: blk` | — | Iterates keys |
| `associationsDo: blk` | — | Iterates `Association` pairs |
| `keys` | — | `OrderedCollection` of keys |
| `values` | — | `OrderedCollection` of values |
| `= other` | — | Same keys and values |
| `printString` | — | `'Dictionary (k->v ...)'` |

---

## Implementation order

1. **`EvalError::Signal`, `_primSignal:`** — new error variant and primitive. Tests:
   `Error signal: '...'` raises the error; `on:do:` catches it; message text
   round-trips; `error:` on Object delegates correctly.
2. **`_primHash`** — universal hash primitive. Tests: integers hash to themselves (mod);
   strings with same content hash equally; objects hash by identity (two distinct
   objects get different hashes).
3. **`Error`, `Association`** in kernel.smol — `Object >> ->` convenience. Tests:
   `'x' -> 42` produces an `Association`; `error:` delegates to `Error signal:`.
4. **`Dictionary`** — full protocol including rehash. Tests: `at:put:` / `at:`
   round-trip; missing key error; `at:ifAbsent:`; `removeKey:ifAbsent:`; growth
   beyond initial capacity; `keys` / `values` / `associationsDo:`.
5. **Coverage check** — `just cov`; no file below 80%.

---

## Open questions

### Dictionary key equality

The current `=` on objects is identity (pointer equality). For `dict at: 'hello'`
to find a key stored as `'hello'`, string equality must be value-based — which it
already is for `Value::Str`. Symbols, integers, and floats are also value-equal.
Objects use identity equality, which is the correct default.

No change needed; the existing `Value::PartialEq` impl is already correct for
dictionary use.

### Pointer-based hash stability across chunk 21

Chunk 21 will clone values crossing entity boundaries. If a `Value::Object` is
cloned before being stored as a dictionary key, the pointer-based hash will differ
from a fresh clone of the same object. This is expected behavior (object identity
is pointer identity) and is acceptable; it is documented here so chunk 21 can note
the implication for cross-entity dictionary use.
