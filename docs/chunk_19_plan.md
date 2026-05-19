# Chunk 19: Core Collections — Minimal Primitives + Smol Kernel

## Status

Chunk 18 delivered `super` dispatch, class-side methods, and the namespace
runtime — 84 interpreter tests passing, 142 triple-store tests passing. Between
chunks: workspace line coverage raised to ~91% (was 82%), and negative number
literals fixed in the lexer (`-3`, `-3.14` are now valid literal tokens per the
ANSI Smalltalk spec).

**Delivers:**

1. Four Rust primitives on any `Object` with indexed storage: `_primAt:`,
   `_primAt:put:`, `_primSize`, `_primAsString`. These are the only Rust
   additions to collection dispatch.
2. `error:` primitive on `Object` — the minimal signalling hook needed by
   kernel code.
3. A kernel loading mechanism: `Evaluator::new()` runs `kernel.smol` with a
   temporary bootstrap env before handing control to the REPL or test harness.
4. `kernel.smol` — the Smol source file that defines `Array`, `ByteArray`, and
   `OrderedCollection` using those primitives.
5. Nested array literal syntax: `#(1 #(2 3) 'hello')` — a recursive
   grammar rule, no arbitrary depth limit.

**Explicitly deferred:**

- `Dictionary` / `Set` — chunk 20.
- `Float64Array` protocol — chunk 20.
- Brace array syntax `{expr. expr}` (dynamic construction) — chunk 20.
- Stream protocol (`ReadStream`, `WriteStream`) — chunk 20.
- Traits / mixin composition — separate design chunk before implementation.
- Exception class hierarchy (`Error`, `Exception`, `resume`, `retry`) — chunk 20.
- Actor model and entity identity — chunk 21+.
- Triple store integration — chunk 21+.
- Strict `asUtf8String` (error on invalid bytes) — deferred; `asString` uses
  lossy UTF-8 for bootstrap.

---

## Key changes from the initial chunk 19 plan

The original plan proposed implementing `Array`, `OrderedCollection`, and
`ByteArray` entirely in Rust dispatch functions, guarded by a new
`PrimitiveKind` discriminant on `Class`. This revision replaces that approach
for the following reasons:

- **No throw-away code.** Full Rust dispatch for `collect:`, `select:`, `do:`,
  etc. would be rewritten once the system is self-hosting enough to load kernel
  code. The primitive approach writes each method once, in Smol.
- **Self-hosting validation.** Defining the standard library in the language
  itself is the eventual goal. Doing it now for collections proves the eval
  pipeline is expressive enough and exercises the kernel loading path.
- **Growable sequences need no new Rust.** `OrderedCollection` manages its own
  buffer reallocation in Smol (Vec-style doubling using `Array new:`), so no
  `_primAppend:` or `_primRemoveLast:` primitives are needed.
- **`_primBasicNew:` is redundant.** `new:` (already implemented in Rust) is
  sufficient for kernel code. The `basicNew:` / `new:` distinction only matters
  when `new:` is user-overridden, which is irrelevant during bootstrap.
- **`PrimitiveKind` is unnecessary.** `_primAt:` etc. already route through
  `dispatch_object_primitive`, which checks `obj.indexed`. No class-level
  discriminant is needed.

0-based indexing replaces 1-based throughout (see rationale below).

---

## Design decisions

### Primitive surface

Four primitives are added to `dispatch_object_primitive` in `builtins.rs`.
They require the receiver to have `indexed: Some(_)`:

| Primitive | Requires | Ensures |
|---|---|---|
| `_primAt: i` | `0 ≤ i < _primSize`; indexed storage present | element at 0-based index `i` |
| `_primAt: i put: v` | `0 ≤ i < _primSize`; indexed storage present; for Byte storage, `v` is `Int` in `0..=255` | sets index `i` to `v`; returns `v` |
| `_primSize` | indexed storage present | length of backing store as `Int` |
| `_primAsString` | `IndexedStorage::Byte` | bytes as `String` via UTF-8 lossy conversion |

Bounds violations and missing indexed storage raise `EvalError::TypeError` with
a descriptive message. These primitives are intentionally raw — bounds checking
at the Smol method level (e.g. `Array >> at:`) is optional since Rust already
enforces it, but the Smol methods may provide friendlier messages.

`_primAt:` on `IndexedStorage::Byte` returns `Value::Int(byte as i64)`.
`_primAt:put:` on `IndexedStorage::Byte` requires a `Value::Int` in `0..=255`
and stores the low byte.

### `error:` on Object

```
Object >> error: aString
```

Added to `dispatch_object_primitive`. Raises `EvalError::TypeError(aString)`.
This is the minimal hook needed for kernel code to signal precondition
violations. It will be promoted to a proper exception once the exception
hierarchy lands in chunk 20.

### Kernel loading

`Evaluator::new()` loads `kernel.smol` as part of construction:

```rust
pub fn new() -> Self {
    let registry = Arc::new(ClassRegistry::new());
    let kernel = registry.kernel();
    let ev = Self { registry, method_stack: ..., method_depth: ..., active_namespace: RefCell::new(kernel) };

    let mut bootstrap_env = Env::new();
    ev.registry.populate_env(&mut bootstrap_env);
    let src = include_str!("kernel.smol");
    let prog = Parser::parse(src).expect("kernel.smol: parse error");
    ev.eval_program(&prog.stmts, &mut bootstrap_env)
        .expect("kernel.smol: eval error");

    ev
}
```

Bootstrap ordering: `populate_env` seeds `bootstrap_env` with `Object`. Kernel
code defines `Array`, `ByteArray`, and `OrderedCollection` via `subclass:`,
which registers each class in `Kernel` namespace and writes a global binding
into `bootstrap_env`. The bootstrap env is then discarded; the classes persist
in `registry.kernel()`. A subsequent `populate_env` call (from `main.rs` or a
test helper) picks them up.

`Evaluator::new_without_kernel()` is provided for low-level primitive tests
that do not need the kernel class definitions.

A kernel load failure (`panic!`) is intentional — a broken kernel is a
programmer error, not a recoverable runtime condition.

### 0-based indexing

All collection indexing uses 0-based convention throughout `kernel.smol` and
the four Rust primitives. Rationale: consistent with the implementation
language; natural for offset arithmetic and future bytecode work; eliminates a
`i - 1` translation layer in the Rust primitives. This departs from Smalltalk
convention intentionally — the project is inspired by Smalltalk but not bound
to it.

### OrderedCollection growth strategy

`OrderedCollection` manages a `storage` instance variable (an `Array`) and a
`size` counter. It does not use `IndexedStorage` directly — its `indexedSlots:`
is `#none`. Growth is handled entirely in Smol:

```
OrderedCollection >> pvtGrow [
    | newStorage i |
    newStorage := Array new: storage _primSize * 2.
    i := 0.
    [i < size] whileTrue: [
        newStorage _primAt: i put: (storage _primAt: i).
        i := i + 1
    ].
    storage := newStorage
]
```

Initial capacity is 8. Doubling amortises appends to O(1). `addFirst:` and
`removeFirst` shift elements and are O(n); a ring-buffer optimisation is
deferred.

### Collection hierarchy and future traits

The three collection classes are defined as direct subclasses of `Object` with
no intermediate `Collection` or `SequenceableCollection` superclass. This
avoids encoding a deep hierarchy before traits are designed. The shared
iteration protocol (`do:`, `collect:`, `select:`, etc.) is repeated across
`Array` and `OrderedCollection` — a deliberate temporary duplication. When
traits land, a `Sequenceable` trait will capture the shared protocol and the
repetition will be eliminated. `select:` and `reject:` return `OrderedCollection`
regardless of receiver type; a `species` protocol can refine this later.

### Nested array literals

```
array-literal ::= '#(' literal-element* ')'
literal-element ::= integer | float | string | symbol | boolean | 'nil'
                  | array-literal
```

The `LiteralValue` AST node is recursive:

```rust
pub enum LiteralValue {
    Int(i64),
    Float(f64),
    LargeInt(Arc<BigInt>),
    Str(Arc<str>),
    Symbol(Arc<str>),
    Bool(bool),
    Nil,
    Array(Vec<LiteralValue>),
}
```

`parse_primary` handles `Token::ArrayLiteralOpen` by consuming elements
recursively until `Token::RParen`. A nested `#(` re-enters the same rule.
`Expr::ArrayLiteral(Vec<LiteralValue>)` is added to the AST. Eval walks the
tree and constructs nested `Array` objects bottom-up.

---

## `kernel.smol` outline

Below is the authoritative sketch of the kernel source. The full file is the
implementation artifact; this captures structure and contracts.

### Array

```smalltalk
Object subclass: #Array
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #pointer.

"class side"
Array class >> new: n [ ^super new: n ]

"protocol"
Array >> at: i       [ ^self _primAt: i ]
Array >> at: i put: v [ self _primAt: i put: v. ^v ]
Array >> size        [ ^self _primSize ]
Array >> isEmpty     [ ^self _primSize = 0 ]
Array >> notEmpty    [ ^self _primSize > 0 ]
Array >> first       [ ^self _primAt: 0 ]
Array >> last        [ ^self _primAt: self _primSize - 1 ]

Array >> do: aBlock [
    | i |
    i := 0.
    [i < self _primSize] whileTrue: [
        aBlock value: (self _primAt: i).
        i := i + 1
    ]
]

Array >> collect: aBlock [
    | result i |
    result := Array new: self _primSize.
    i := 0.
    [i < self _primSize] whileTrue: [
        result _primAt: i put: (aBlock value: (self _primAt: i)).
        i := i + 1
    ].
    ^result
]

"select: and reject: return OrderedCollection (size unknown upfront)"
Array >> select: aBlock [
    | result |
    result := OrderedCollection new.
    self do: [:each | (aBlock value: each) ifTrue: [result add: each]].
    ^result
]

Array >> reject: aBlock [
    | result |
    result := OrderedCollection new.
    self do: [:each | (aBlock value: each) ifFalse: [result add: each]].
    ^result
]

Array >> detect: aBlock [
    self do: [:each | (aBlock value: each) ifTrue: [^each]].
    self error: 'detect: no element satisfies the condition'
]

Array >> detect: aBlock ifNone: noneBlock [
    self do: [:each | (aBlock value: each) ifTrue: [^each]].
    ^noneBlock value
]

Array >> inject: init into: aBlock [
    | acc |
    acc := init.
    self do: [:each | acc := aBlock value: acc value: each].
    ^acc
]

Array >> includes: v [
    self do: [:each | each = v ifTrue: [^true]].
    ^false
]

Array >> copyFrom: start to: stop [
    | result i |
    result := Array new: stop - start + 1.
    i := 0.
    [start + i <= stop] whileTrue: [
        result _primAt: i put: (self _primAt: start + i).
        i := i + 1
    ].
    ^result
]

Array >> = other [
    | n i |
    self class = other class ifFalse: [^false].
    n := self _primSize.
    n = other _primSize ifFalse: [^false].
    i := 0.
    [i < n] whileTrue: [
        (self _primAt: i) = (other _primAt: i) ifFalse: [^false].
        i := i + 1
    ].
    ^true
]

Array >> ~= other [ ^(self = other) not ]

Array >> printString [
    | s i |
    s := '('.
    i := 0.
    [i < self _primSize] whileTrue: [
        s := s , (self _primAt: i) printString , ' '.
        i := i + 1
    ].
    ^s , ')'
]
```

### ByteArray

```smalltalk
Object subclass: #ByteArray
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #byte.

ByteArray >> at: i       [ ^self _primAt: i ]
ByteArray >> at: i put: v [ self _primAt: i put: v. ^v ]
ByteArray >> size        [ ^self _primSize ]
ByteArray >> isEmpty     [ ^self _primSize = 0 ]
ByteArray >> notEmpty    [ ^self _primSize > 0 ]

ByteArray >> do: aBlock [
    | i |
    i := 0.
    [i < self _primSize] whileTrue: [
        aBlock value: (self _primAt: i).
        i := i + 1
    ]
]

ByteArray >> copyFrom: start to: stop [
    | result i |
    result := ByteArray new: stop - start + 1.
    i := 0.
    [start + i <= stop] whileTrue: [
        result _primAt: i put: (self _primAt: start + i).
        i := i + 1
    ].
    ^result
]

ByteArray >> asString [ ^self _primAsString ]

ByteArray >> printString [
    | s i |
    s := '('.
    i := 0.
    [i < self _primSize] whileTrue: [
        s := s , (self _primAt: i) printString , ' '.
        i := i + 1
    ].
    ^s , ')'
]
```

### OrderedCollection

```smalltalk
Object subclass: #OrderedCollection
    instanceVariableNames: 'storage size'
    classVariableNames: ''
    indexedSlots: #none.

OrderedCollection class >> new [
    | oc |
    oc := super new.
    oc pvtInit.
    ^oc
]

OrderedCollection >> pvtInit [
    storage := Array new: 8.
    size := 0
]

OrderedCollection >> size        [ ^size ]
OrderedCollection >> isEmpty     [ ^size = 0 ]
OrderedCollection >> notEmpty    [ ^size > 0 ]
OrderedCollection >> first       [ ^self at: 0 ]
OrderedCollection >> last        [ ^self at: size - 1 ]

OrderedCollection >> at: i [
    (i < 0 or: [i >= size]) ifTrue: [self error: 'at: index out of bounds'].
    ^storage _primAt: i
]

OrderedCollection >> at: i put: v [
    (i < 0 or: [i >= size]) ifTrue: [self error: 'at:put: index out of bounds'].
    storage _primAt: i put: v.
    ^v
]

OrderedCollection >> add: v [
    size >= storage _primSize ifTrue: [self pvtGrow].
    storage _primAt: size put: v.
    size := size + 1.
    ^v
]

OrderedCollection >> addLast: v [ ^self add: v ]

OrderedCollection >> addFirst: v [
    | i |
    size >= storage _primSize ifTrue: [self pvtGrow].
    i := size.
    [i > 0] whileTrue: [
        storage _primAt: i put: (storage _primAt: i - 1).
        i := i - 1
    ].
    storage _primAt: 0 put: v.
    size := size + 1.
    ^v
]

OrderedCollection >> removeFirst [
    | v i |
    size = 0 ifTrue: [self error: 'removeFirst: collection is empty'].
    v := storage _primAt: 0.
    i := 1.
    [i < size] whileTrue: [
        storage _primAt: i - 1 put: (storage _primAt: i).
        i := i + 1
    ].
    size := size - 1.
    ^v
]

OrderedCollection >> removeLast [
    | v |
    size = 0 ifTrue: [self error: 'removeLast: collection is empty'].
    v := storage _primAt: size - 1.
    size := size - 1.
    ^v
]

OrderedCollection >> remove: v ifAbsent: aBlock [
    | i j |
    i := 0.
    [i < size] whileTrue: [
        (storage _primAt: i) = v ifTrue: [
            j := i + 1.
            [j < size] whileTrue: [
                storage _primAt: j - 1 put: (storage _primAt: j).
                j := j + 1
            ].
            size := size - 1.
            ^v
        ].
        i := i + 1
    ].
    ^aBlock value
]

OrderedCollection >> do: aBlock [
    | i |
    i := 0.
    [i < size] whileTrue: [
        aBlock value: (storage _primAt: i).
        i := i + 1
    ]
]

OrderedCollection >> collect: aBlock [
    | result i |
    result := OrderedCollection new.
    i := 0.
    [i < size] whileTrue: [
        result add: (aBlock value: (storage _primAt: i)).
        i := i + 1
    ].
    ^result
]

OrderedCollection >> select: aBlock [
    | result |
    result := OrderedCollection new.
    self do: [:each | (aBlock value: each) ifTrue: [result add: each]].
    ^result
]

OrderedCollection >> reject: aBlock [
    | result |
    result := OrderedCollection new.
    self do: [:each | (aBlock value: each) ifFalse: [result add: each]].
    ^result
]

OrderedCollection >> detect: aBlock [
    self do: [:each | (aBlock value: each) ifTrue: [^each]].
    self error: 'detect: no element satisfies the condition'
]

OrderedCollection >> detect: aBlock ifNone: noneBlock [
    self do: [:each | (aBlock value: each) ifTrue: [^each]].
    ^noneBlock value
]

OrderedCollection >> inject: init into: aBlock [
    | acc |
    acc := init.
    self do: [:each | acc := aBlock value: acc value: each].
    ^acc
]

OrderedCollection >> includes: v [
    self do: [:each | each = v ifTrue: [^true]].
    ^false
]

OrderedCollection >> copyFrom: start to: stop [
    | result i |
    result := OrderedCollection new.
    i := start.
    [i <= stop] whileTrue: [
        result add: (self at: i).
        i := i + 1
    ].
    ^result
]

OrderedCollection >> = other [
    | i |
    self class = other class ifFalse: [^false].
    size = other size ifFalse: [^false].
    i := 0.
    [i < size] whileTrue: [
        (self at: i) = (other at: i) ifFalse: [^false].
        i := i + 1
    ].
    ^true
]

OrderedCollection >> ~= other [ ^(self = other) not ]

OrderedCollection >> printString [
    | s i |
    s := 'OrderedCollection ('.
    i := 0.
    [i < size] whileTrue: [
        s := s , (storage _primAt: i) printString , ' '.
        i := i + 1
    ].
    ^s , ')'
]

OrderedCollection >> pvtGrow [
    | newStorage i |
    newStorage := Array new: storage _primSize * 2.
    i := 0.
    [i < size] whileTrue: [
        newStorage _primAt: i put: (storage _primAt: i).
        i := i + 1
    ].
    storage := newStorage
]
```

---

## Protocol tables

### Array

| Selector | Requires | Ensures |
|---|---|---|
| `at: i` | `0 ≤ i < size` | element at 0-based index `i` |
| `at: i put: v` | `0 ≤ i < size` | sets index `i` to `v`; returns `v` |
| `size` | — | number of elements |
| `isEmpty` | — | `size = 0` |
| `notEmpty` | — | `size > 0` |
| `first` | `notEmpty` | element at index 0 |
| `last` | `notEmpty` | element at last index |
| `copyFrom: s to: e` | `0 ≤ s ≤ e < size` | new `Array` of elements `s..e` inclusive |
| `do: blk` | `blk` arity 1 | evaluates `blk` with each element in order; returns nil |
| `collect: blk` | `blk` arity 1 | new `Array` of `blk` results |
| `select: blk` | `blk` returns Bool | new `OrderedCollection` of matching elements |
| `reject: blk` | `blk` returns Bool | new `OrderedCollection` of non-matching elements |
| `detect: blk` | match exists | first element for which `blk` is true; `error:` if none |
| `detect: blk ifNone: absent` | — | first match or result of zero-arg `absent` block |
| `inject: init into: blk` | `blk` arity 2 | fold left; returns final accumulator |
| `includes: v` | — | true if any element `= v` |
| `= other` | — | structural equality (same size, elements pairwise `=`) |
| `~= other` | — | negation of `=` |
| `printString` | — | `'(e0 e1 ... )'` |

### OrderedCollection (shared protocol same as Array, plus:)

| Selector | Ensures |
|---|---|
| `add: v` | appends `v`; returns `v` |
| `addFirst: v` | prepends `v`; returns `v` |
| `addLast: v` | appends `v`; returns `v` (alias for `add:`) |
| `removeFirst` | removes and returns index-0 element; `error:` if empty |
| `removeLast` | removes and returns last element; `error:` if empty |
| `remove: v ifAbsent: blk` | removes first element `= v`; returns it; evaluates `blk` if absent |

`OrderedCollection printString` → `'OrderedCollection (e0 e1 ... )'`.

### ByteArray

| Selector | Requires | Ensures |
|---|---|---|
| `at: i` | `0 ≤ i < size` | returns `Int` in `0..=255` |
| `at: i put: v` | `v` is `Int` in `0..=255`; `0 ≤ i < size` | sets byte; returns `v` |
| `size` | — | number of bytes |
| `isEmpty` | — | `size = 0` |
| `notEmpty` | — | `size > 0` |
| `do: blk` | `blk` arity 1 | evaluates `blk` with each byte as `Int` |
| `copyFrom: s to: e` | `0 ≤ s ≤ e < size` | new `ByteArray` slice |
| `asString` | — | bytes as `String` via UTF-8 lossy; invalid sequences replaced with U+FFFD |
| `printString` | — | `'(b0 b1 ... )'` |

---

## Changes to existing source files

### `src/builtins.rs`

Add to `dispatch_object_primitive`:

```rust
"_primAt:" => { /* bounds check obj.indexed; return Value */ }
"_primAt:put:" => { /* bounds check; store; return v */ }
"_primSize" => { /* return Value::Int(len) */ }
"_primAsString" => { /* require Byte storage; from_utf8_lossy */ }
"error:" => { /* Err(EvalError::TypeError(msg)) */ }
```

No other Rust dispatch is added for collection methods.

### `src/eval.rs`

In `Evaluator::new()`:
- After registry init, allocate `bootstrap_env`, call `populate_env`.
- Parse and eval `include_str!("kernel.smol")`.
- On failure, `panic!` with a clear message including the error location.

Add `Evaluator::new_without_kernel()` that performs only registry init (for
low-level primitive tests).

### `src/ast.rs`

Add `LiteralValue` enum (replacing any prior placeholder):

```rust
pub enum LiteralValue {
    Int(i64),
    Float(f64),
    LargeInt(Arc<BigInt>),
    Str(Arc<str>),
    Symbol(Arc<str>),
    Bool(bool),
    Nil,
    Array(Vec<LiteralValue>),   // nested literal
}
```

Add `Expr::ArrayLiteral(Vec<LiteralValue>)`.

### `src/lexer.rs`

In the `#` branch of `next_token`, before the `read_binop('#')` fallthrough:

```rust
Some('(') => { self.advance(); Ok(Token::ArrayLiteralOpen) }
```

### `src/parser.rs`

`parse_primary` handles `Token::ArrayLiteralOpen` by consuming
`parse_literal_element` tokens recursively until `Token::RParen`. A nested
`Token::ArrayLiteralOpen` re-enters the same rule, producing a
`LiteralValue::Array`. Any non-literal, non-`#(` token inside is a parse
error.

### `src/eval.rs` (Expr::ArrayLiteral)

```rust
Expr::ArrayLiteral(elements) => {
    let array_class = /* look up "Array" from registry */;
    let obj = Arc::new(Object::new_with_indexed(array_class, elements.len()));
    for (i, elem) in elements.iter().enumerate() {
        let v = eval_literal_value(elem, ev)?; // recursive for nested arrays
        obj.indexed_set(i, v);
    }
    Ok(Value::Object(obj))
}
```

### `src/interpreter/kernel.smol` (new file)

The full implementation of `Array`, `ByteArray`, and `OrderedCollection` per
the outline above.

---

## Implementation order

1. **`_primAt:`, `_primAt:put:`, `_primSize`, `_primAsString`, `error:`** in
   `dispatch_object_primitive`. Tests via `new_without_kernel()`: raw reads and
   writes work; bounds errors fire; `error:` raises TypeError; `_primAsString`
   on Byte storage.
2. **Kernel loading in `Evaluator::new()`** and `new_without_kernel()`. Test:
   `Evaluator::new()` does not panic; `Array` is bound in a freshly-populated
   env; `OrderedCollection` and `ByteArray` likewise.
3. **Array protocol tests** — full coverage of the protocol table.
4. **ByteArray protocol tests**.
5. **OrderedCollection protocol tests** — queue (add/removeFirst), stack
   (addFirst/removeLast), growth trigger, remove:ifAbsent:, structural =.
6. **Nested array literal syntax** — lexer token, AST node, recursive parser,
   eval. Tests: `#(1 2 3) at: 1` → `2`; `#(#(0 1) #(2 3)) at: 0` → `(0 1 )`;
   `#() isEmpty` → `true`; non-literal token in `#(...)` is a parse error.
7. **Integration tests** — OC as a queue, chained `collect:` + `inject:into:`,
   user-defined subclass of Array with an added method, nested array literal
   passed to `do:`.
8. **Coverage check** — `just cov`; no file below 75%.

---

## Open questions

### `collect:` on OrderedCollection returns OrderedCollection

`Array >> collect:` returns a new `Array` (size is known). `OrderedCollection >>
collect:` returns a new `OrderedCollection` (simplest; consistent). A `species`
protocol (where `collect:` asks the receiver what class to construct) can
unify this when traits land.

### `addFirst:` / `removeFirst` performance

The current implementation is O(n) due to element shifting. A ring-buffer (two
instance variables `head` and `tail` into a power-of-two `Array`) would make
both O(amortised 1). Deferred — correctness first.

### `new:` on OrderedCollection

`OrderedCollection new:` is currently not defined. Sending it falls through to
the built-in `dispatch_class` `new:`, which requires `indexedSlots:` to be set
and will raise a `TypeError`. This is acceptable for bootstrap; a
capacity-hint `new:` can be added to kernel.smol later without Rust changes.
