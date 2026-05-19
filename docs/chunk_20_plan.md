# Chunk 20: Exceptions, Dictionary, and Stream Protocol

## Status

Chunk 19 delivered Array, ByteArray, and OrderedCollection in Smol, proper non-local
returns (depth-tagged `EvalError::Return`), nested array literals, and five Rust
primitives. 241 interpreter tests pass; workspace line coverage 91%.

**Delivers:**

1. A minimal exception hierarchy (`Error` class) and `signal` / `signal:` in Smol,
   wiring up the existing `on:do:` block handler.
2. `Dictionary` — an identity-keyed hash map implemented in Smol over a flat
   `Array` of association pairs, with open-addressing or a simple bucket strategy.
3. `Association` — `key → value` pair used internally by `Dictionary` and as a
   general-purpose tuple.
4. `ReadStream` and `WriteStream` over `SequenceableCollection` — cursor-based
   access to `Array`, `ByteArray`, and `OrderedCollection`.
5. `Float64Array` — the indexed-float collection protocol deferred from chunk 19.
6. `species` hook on collection classes — allows `collect:` and `copyFrom:to:` to
   return the same concrete type as the receiver rather than always `OrderedCollection`
   or `Array`.

**Explicitly deferred:**

- `Set` — straightforward once `Dictionary` exists; chunk 21.
- Brace array syntax `{expr. expr}` (dynamic construction) — chunk 21.
- Traits / mixin composition — requires a separate design chunk.
- Actor model and entity identity — chunk 21+.
- Triple store integration — chunk 21+.
- Full exception hierarchy (`Exception`, `resume`, `retry`, `ensure:`) — chunk 21.
- `asUtf8String` (strict, error on invalid bytes) — deferred; `asString` stays lossy.

---

## Design decisions

### Exception model

The goal is a minimal but honest exception hook. `on:do:` already dispatches any
`EvalError` to the handler block; what is missing is the ability for Smol code to
deliberately raise a condition.

Two Rust primitives are added:

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
to the handler without the `"type error: "` prefix.

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
hierarchy and is deferred to chunk 21.

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

### Float64Array

```smalltalk
Object subclass: #Float64Array
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #float64.

Float64Array >> at: i       [ ^self _primAt: i ]
Float64Array >> at: i put: v [ self _primAt: i put: v. ^v ]
Float64Array >> size        [ ^self _primSize ]
Float64Array >> isEmpty     [ ^self _primSize = 0 ]
Float64Array >> notEmpty    [ ^self _primSize > 0 ]

Float64Array >> do: aBlock [
    | i |
    i := 0.
    [i < self _primSize] whileTrue: [
        aBlock value: (self _primAt: i).
        i := i + 1
    ]
]
```

`_primAt:put:` on `Float64` storage already coerces `Int` to `Float` (implemented
in chunk 19). No new Rust is needed.

### `species` hook

`collect:` and `copyFrom:to:` on `Array` currently return `Array`; on
`OrderedCollection` they return `OrderedCollection`. Adding `species` makes this
explicit and extensible:

```smalltalk
Array >> species [ ^Array ]
OrderedCollection >> species [ ^OrderedCollection ]

Array >> collect: aBlock [
    | result i |
    result := self species new: self _primSize.
    ...
]
```

User subclasses that override `species` get correct return types from inherited
collection methods for free.

### Stream protocol

```smalltalk
Object subclass: #ReadStream
    instanceVariableNames: 'collection position limit'
    classVariableNames: ''
    indexedSlots: #none.

ReadStream class >> on: aCollection [
    | s |
    s := self new.
    s pvtOn: aCollection from: 0 to: aCollection size - 1.
    ^s
]

ReadStream class >> on: aCollection from: start to: stop [ ... ]

ReadStream >> pvtOn: c from: s to: e [
    collection := c. position := s. limit := e
]

ReadStream >> next         [ ... "returns element at position, advances" ]
ReadStream >> next: n      [ ... "returns Array of n elements" ]
ReadStream >> peek         [ ... ]
ReadStream >> atEnd        [ ^position > limit ]
ReadStream >> position     [ ^position ]
ReadStream >> position: n  [ position := n ]
ReadStream >> reset        [ position := 0 ]
ReadStream >> do: aBlock   [ [self atEnd] whileFalse: [aBlock value: self next] ]
ReadStream >> upToEnd      [ ... "returns remaining elements as Array" ]
```

```smalltalk
Object subclass: #WriteStream
    instanceVariableNames: 'collection position'
    classVariableNames: ''
    indexedSlots: #none.

WriteStream class >> on: aCollection [ ... ]
WriteStream class >> with: aCollection [ ... "starts after last element" ]

WriteStream >> nextPut: v      [ collection at: position put: v. position := position + 1. ^v ]
WriteStream >> nextPutAll: c   [ c do: [:e | self nextPut: e] ]
WriteStream >> position        [ ^position ]
WriteStream >> reset           [ position := 0 ]
WriteStream >> contents        [ ^collection copyFrom: 0 to: position - 1 ]
```

`WriteStream on: (Array new: n)` is the idiomatic buffer builder. No growable
`WriteStream` in this chunk — that requires a more complex `collection` swap, deferred.

---

## New Rust surface

| Addition | Location | Notes |
|---|---|---|
| `EvalError::Signal(Arc<str>)` | `eval.rs` | New error variant for Smol-raised signals |
| `_primSignal: aString` | `dispatch_object_primitive` | Raises `EvalError::Signal` |
| `_primHash` | `dispatch_object_primitive` | Returns stable `Int` hash of receiver |

`error:` on Object moves from Rust (`dispatch_object_primitive`) to Smol delegation
(`Error signal: aString`). The old Rust arm is removed.

### `_primHash` design

Returned as `Value::Int(hash as i64)`. The hash is not cryptographic; stability
across process restarts is not guaranteed (pointer-based hashes will vary). Users
who need persistent keys should hash over content (`Str`, `Symbol`, numeric types)
rather than object identity.

Implementation in Rust:

```rust
"_primHash" => {
    expect_arity(sel, 0, args.len())?;
    // identity hash: rotate pointer bits for better distribution
    let addr = Arc::as_ptr(&obj) as usize;
    Ok(Value::Int(fmix64(addr as u64) as i64))
}
```

`_primHash` on primitive values (`Int`, `Str`, etc.) is handled in their respective
dispatch functions using the same scheme as `Value::Hash` (FNV-1a for strings,
numeric value for ints). It needs to be added to the universal dispatch in
`dispatch()` before routing to type-specific handlers.

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

- Add `Error`, `Association`, `Dictionary`, `Float64Array`, `ReadStream`,
  `WriteStream`.
- Add `species` to `Array` and `OrderedCollection`.
- Rewrite `Array >> collect:` and `Array >> copyFrom:to:` to use `self species`.
- Add `Object >> ->` for `Association` construction.
- Replace Rust `error:` with Smol delegation to `Error`.

---

## Protocol tables

### Error

| Selector | Ensures |
|---|---|
| `Error class >> signal: s` | Creates instance, signals with message `s` |
| `signal` | Raises `EvalError::Signal(message)` |
| `message` | Returns the message string |

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

### Float64Array

| Selector | Ensures |
|---|---|
| `at: i` | `Float` at 0-based index |
| `at: i put: v` | Stores `Float` (or `Int` coerced); returns `v` |
| `size`, `isEmpty`, `notEmpty`, `do:` | As Array |

### ReadStream

| Selector | Ensures |
|---|---|
| `ReadStream class >> on: c` | Stream over full collection |
| `next` | Next element; `Error signal:` if `atEnd` |
| `peek` | Next element without advancing |
| `atEnd` | True if no more elements |
| `position` / `position: n` | Read/set cursor |
| `reset` | Rewind to 0 |
| `do: blk` | Iterate remaining elements |
| `upToEnd` | `Array` of remaining elements |

### WriteStream

| Selector | Ensures |
|---|---|
| `WriteStream class >> on: c` | Stream starting at index 0 |
| `nextPut: v` | Stores `v` at current position, advances; returns `v` |
| `nextPutAll: c` | Appends all elements of `c` |
| `position` / `reset` | Read cursor / rewind |
| `contents` | `Array` from 0 to `position - 1` |

---

## Implementation order

1. **`EvalError::Signal`, `_primSignal:`** — new error variant and primitive. Tests:
   `Error signal: '...'` raises the error; `on:do:` catches it; message text round-trips.
2. **`_primHash`** — universal hash primitive. Tests: integers hash to themselves (mod);
   strings with same content hash equally; objects hash by identity.
3. **`Error`, `Association` in kernel.smol** — `Object >> ->` convenience. Tests:
   `'x' -> 42` produces an `Association`; `error:` delegates to `Error signal:`.
4. **`Dictionary`** — full protocol including rehash. Tests: at:put:/at: round-trip;
   missing key error; at:ifAbsent:; removeKey:ifAbsent:; growth beyond initial capacity;
   keys/values/associationsDo:.
5. **`Float64Array`** — trivial given existing Float64 storage primitives. Tests:
   at:put:/at: with Float and Int coercion; do:.
6. **`species`** — add to Array and OrderedCollection; update collect:/copyFrom:to:.
   Tests: `(Array new: 3) collect: [:e | e]` returns an Array; user subclass with
   overridden `species` returns correct type.
7. **`ReadStream` / `WriteStream`** — full protocol. Tests: read all elements; peek
   without advancing; atEnd; WriteStream round-trip via contents; nextPutAll:.
8. **Coverage check** — `just cov`; no file below 80%.

---

## Open questions

### Dictionary key equality

The current `=` on objects is identity (pointer equality). For `Dict at: 'hello'`
to find a key stored as `'hello'`, string equality must be value-based — which it
already is for `Value::Str`. Symbols, integers, and floats are also value-equal.
Objects use identity equality, which is the correct default.

No change needed; the existing `Value::PartialEq` impl is already correct for
dictionary use.

### Growable WriteStream

`WriteStream nextPut:` beyond the backing collection's size needs either a bounds
check + error, or silent growth. This chunk raises `Error signal:` on overflow;
a growable variant (backed by `OrderedCollection`) is deferred to chunk 21.

### `ensure:` / `ifCurtailed:`

Exception cleanup handlers are deferred. The missing piece is a way to guarantee
execution even when a signal propagates. Chunk 21 will add `EvalError::Signal`
as a proper catchable that `on:do:` can filter by class, which is the prerequisite
for `ensure:`.
