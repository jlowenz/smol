# Chunk 19: Core Collections — Array, OrderedCollection, and ByteArray

## Status

Chunk 18 delivered `super` dispatch, class-side methods, and the namespace
runtime — 84 interpreter tests passing, 142 triple-store tests passing. Between
chunks two maintenance items were completed: workspace-wide line coverage was
raised to ~91% (was 82%), and negative number literals were fixed in the lexer
(`-3`, `-3.14` are now valid literal tokens per the ANSI Smalltalk spec; `-`
adjacent to a digit folds the sign into the `Int`/`Float`/`LargeInt` token
directly).

**Delivers:**

1. `PrimitiveKind` discriminant on `Class` — a clean routing mechanism for
   built-in collection dispatch that avoids matching on class name strings.
2. `Array` — fixed-size, pointer-indexed collection; backed by
   `IndexedStorage::Pointer`. Full protocol: `at:`, `at:put:`, `size`,
   `isEmpty`, `notEmpty`, `first`, `last`, `copyFrom:to:`, `printString`, and
   the shared iteration protocol.
3. `OrderedCollection` — dynamically growable sequence; backed by the same
   pointer-indexed storage. Adds `add:`, `addFirst:`, `addLast:`,
   `removeFirst`, `removeLast`, `remove:ifAbsent:`.
4. `ByteArray` built-in protocol (backlog from chunk 17): `at:`, `at:put:`,
   `size`, `isEmpty`, `notEmpty`, `do:`, `asString`, `copyFrom:to:`,
   `printString`.
5. Shared iteration protocol across all three classes: `do:`, `collect:`,
   `select:`, `reject:`, `detect:`, `detect:ifNone:`, `inject:into:`,
   `includes:`.
6. Array literal syntax: `#(literal ...)` — a sequence of literals (integers,
   floats, strings, symbols, booleans, nil) parsed at compile time into a
   fixed-size `Array`. Arbitrary expressions inside `#(...)` are deferred.

**Explicitly deferred:**

- `Dictionary` / `Set` — chunk 20.
- `Float64Array` protocol — chunk 20.
- Brace array syntax `{expr. expr}` (dynamic construction) — chunk 20.
- Stream protocol (`ReadStream`, `WriteStream`) — chunk 20.
- Traits / mixin composition — separate design chunk before implementation.
- Exception class hierarchy (`Error`, `Exception`, `resume`, `retry`) — chunk 20.
- Actor model and entity identity — chunk 21+.
- Triple store integration — chunk 21+.

---

## Design decisions

### Built-in vs. Smol-defined collection classes

`Array`, `OrderedCollection`, and `ByteArray` are implemented in Rust and
registered in `Kernel` at bootstrap. Their instance protocol is dispatched in
`builtins.rs`. Rationale: the bootstrap interpreter cannot implement collections
in Smol code without already having collections (circular dependency); and
performance of these fundamental types matters for the interpreter's own
operation. They can be promoted to Smol-defined classes once the system is
self-hosting enough.

### PrimitiveKind discriminant on Class

Dispatching collection methods by matching `obj.class.name` as a string is
fragile and allocates. Instead, `Class` gains:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum PrimitiveKind {
    Array,
    ByteArray,
    OrderedCollection,
}

pub struct Class {
    // existing fields...
    pub primitive_kind: Option<PrimitiveKind>,
}

impl Class {
    /// Walk the superclass chain for the nearest `primitive_kind`.
    ///
    /// Ensures: subclasses of a primitive collection class inherit its kind,
    ///          so user-defined `Array` subclasses still get collection dispatch.
    pub fn effective_primitive_kind(&self) -> Option<&PrimitiveKind>;
}
```

Bootstrap registers `Array`, `ByteArray`, and `OrderedCollection` with their
respective `PrimitiveKind`. `handle_subclass` always sets `primitive_kind: None`
for user-defined classes.

`dispatch_object_value` routing:

```rust
fn dispatch_object_value(ev, obj, selector, args, env) {
    // User-defined method takes priority.
    if let Some((method, def_class)) = obj.class.lookup(&selector) {
        return ev.invoke_method(obj.clone(), def_class, method, args, env);
    }
    match obj.class.effective_primitive_kind() {
        Some(PrimitiveKind::Array) =>
            dispatch_array(ev, obj, &selector, args, env),
        Some(PrimitiveKind::ByteArray) =>
            dispatch_byte_array(ev, obj, &selector, args, env),
        Some(PrimitiveKind::OrderedCollection) =>
            dispatch_ordered_collection(ev, obj, &selector, args, env),
        None =>
            dispatch_object_primitive(obj, &selector, args),
    }
}
```

### Storage for Array and OrderedCollection

Both use `IndexedStorage::Pointer(RefCell<Vec<Value>>)`.

- **Array**: `new: n` allocates a Vec of `n` nils. `size` is always `n`.
  `at:put:` replaces in place; the Vec never grows or shrinks.
- **OrderedCollection**: `new` produces `Object::new_with_indexed(class, 0)`
  (an empty Vec). `add:` / `addFirst:` push into the Vec; `removeFirst` /
  `removeLast` drain from the ends. `size` returns the current `len()`.
  Sending `new:` to `OrderedCollection` is a `TypeError` — capacity
  pre-allocation is deferred.

### Indexing convention

1-based (Smalltalk convention). `at: 1` returns the first element. Index out of
bounds (`i < 1` or `i > size`) raises `EvalError::TypeError` with a descriptive
message.

### select: / reject: return type

`select:` and `reject:` do not know the result size in advance. Both return a
new `OrderedCollection` regardless of the receiver type. This is pragmatic for
the bootstrap; a proper `species` protocol can fix it later.

### Equality for collections

`=` on two `Array` or `OrderedCollection` values uses structural equality:
same `size` and all elements pairwise `=`. This matches Smalltalk convention
for collections (contrast with `Object =` which uses identity).

### Array literal syntax

`#(1 2 'hello' #sym true nil)` — a parenthesized sequence of literals following
`#(`. Produces a fixed `Array` at eval time.

**Lexer**: when `#` is followed immediately by `(`, emit
`Token::ArrayLiteralOpen`. The existing comment in the `#` branch
(`// #( array literal — deferred`) is the target site.

**Parser**: `parse_primary` handles `Token::ArrayLiteralOpen` by consuming
tokens until `Token::RParen`. Only literal tokens are accepted inside:
`Int`, `Float`, `LargeInt`, `Str`, `Symbol`, `True`, `False`, `Nil`. Any
other token is a parse error.

**AST**:

```rust
pub enum LiteralValue {
    Int(i64),
    Float(f64),
    LargeInt(Arc<BigInt>),
    Str(Arc<str>),
    Symbol(Arc<str>),
    Bool(bool),
    Nil,
}

// added to Expr:
ArrayLiteral(Vec<LiteralValue>),
```

**Eval**: look up `Array` class from the registry, allocate
`Object::new_with_indexed(array_class, vals.len())`, fill indexed slots.

---

## Protocol tables

### Array and OrderedCollection (shared)

| Selector | Requires | Ensures |
|---|---|---|
| `at: i` | `1 ≤ i ≤ size` | element at 1-based index `i` |
| `at: i put: v` | `1 ≤ i ≤ size` | sets index `i` to `v`; returns `v` |
| `size` | — | number of elements |
| `isEmpty` | — | `size = 0` |
| `notEmpty` | — | `size > 0` |
| `first` | `notEmpty` | element at index 1 |
| `last` | `notEmpty` | element at last index |
| `copyFrom: s to: e` | `1 ≤ s ≤ e ≤ size` | new collection of elements `s..e` inclusive |
| `do: blk` | `blk` arity 1 | evaluates `blk` with each element in order; returns nil |
| `collect: blk` | `blk` arity 1 | new Array of `blk` results |
| `select: blk` | `blk` returns Bool | new OrderedCollection of matching elements |
| `reject: blk` | `blk` returns Bool | new OrderedCollection of non-matching elements |
| `detect: blk` | match exists | first element for which `blk` is true; TypeError if none |
| `detect: blk ifNone: absent` | — | first match or result of zero-arg `absent` block |
| `inject: init into: blk` | `blk` arity 2 | fold left; returns final accumulator |
| `includes: v` | — | true if any element `= v` |
| `printString` | — | see below |
| `=` | — | structural equality (same size, elements pairwise `=`) |
| `~=` | — | negation of `=` |

`Array printString` → `'(1 2 3 )'` (space before closing paren, Smalltalk style).
`OrderedCollection printString` → `'OrderedCollection (1 2 3 )'`.

### OrderedCollection (additional)

| Selector | Ensures |
|---|---|
| `add: v` | appends `v`; returns `v` |
| `addFirst: v` | prepends `v`; returns `v` |
| `addLast: v` | appends `v`; returns `v` (alias for `add:`) |
| `removeFirst` | removes and returns index-1 element; TypeError if empty |
| `removeLast` | removes and returns last element; TypeError if empty |
| `remove: v ifAbsent: blk` | removes first element `= v`; returns it; evaluates `blk` if absent |

### ByteArray

| Selector | Requires | Ensures |
|---|---|---|
| `at: i` | `1 ≤ i ≤ size` | returns `Int` in `0..=255` |
| `at: i put: v` | `v` is `Int` in `0..=255` | sets byte; returns `v` |
| `size` | — | number of bytes |
| `isEmpty` | — | `size = 0` |
| `notEmpty` | — | `size > 0` |
| `do: blk` | `blk` arity 1 | evaluates `blk` with each byte as `Int` |
| `copyFrom: s to: e` | `1 ≤ s ≤ e ≤ size` | new ByteArray slice |
| `asString` | — | bytes interpreted as UTF-8 (lossy) |
| `printString` | — | `'(n1 n2 ... )'` |

---

## Changes to existing types

### `src/class.rs`

Add `PrimitiveKind` enum and `primitive_kind: Option<PrimitiveKind>` field to
`Class`. Add `effective_primitive_kind(&self) -> Option<&PrimitiveKind>` walking
the superclass chain. All existing `Class` construction sites (in
`class_registry.rs` bootstrap and `handle_subclass` in `builtins.rs`) must set
the new field: `None` for user-defined classes, appropriate variant for
bootstrap collection classes.

### `src/class_registry.rs`

Add three bootstrap helpers, called from `ClassRegistry::new`:

```rust
fn make_array_class(object_class: Arc<Class>) -> Arc<Class>;
fn make_byte_array_class(object_class: Arc<Class>) -> Arc<Class>;
fn make_ordered_collection_class(object_class: Arc<Class>) -> Arc<Class>;
```

Each registers the class in `Kernel` and binds it in the env via
`populate_env`. `make_byte_array_class` uses `indexed_slot_kind:
Some(IndexedSlotKind::Byte)`. The other two use `Pointer`.

### `src/lexer.rs`

In the `#` branch of `next_token`, before the `read_binop('#')` fallthrough:

```rust
Some('(') => { self.advance(); Ok(Token::ArrayLiteralOpen) }
```

### `src/ast.rs`

Add `LiteralValue` enum and `Expr::ArrayLiteral(Vec<LiteralValue>)`.

### `src/parser.rs`

In `parse_primary`, handle `Token::ArrayLiteralOpen`: consume literal tokens
until `Token::RParen`, building `Vec<LiteralValue>`. Produce
`Expr::ArrayLiteral`.

### `src/eval.rs`

Evaluate `Expr::ArrayLiteral`: look up `Array` from the class registry,
allocate via `Object::new_with_indexed`, populate slots from the literal values.

### `src/builtins.rs`

- Reroute `dispatch_object_value` through `effective_primitive_kind`.
- Add `dispatch_array`, `dispatch_byte_array`, `dispatch_ordered_collection`.
- `dispatch_class` `new:` for `OrderedCollection` returns a `TypeError`
  ("OrderedCollection does not support new: — use new").

---

## Implementation order

1. **`PrimitiveKind` on `Class`**: add enum, field, and
   `effective_primitive_kind`. Update bootstrap `Class` construction in
   `class_registry.rs` (`None` for `Object`). Update `handle_subclass` in
   `builtins.rs` (`None` for all user-defined classes). No behaviour change yet;
   tests: existing suite still passes.
2. **Bootstrap collection classes**: implement `make_array_class`,
   `make_byte_array_class`, `make_ordered_collection_class` in
   `class_registry.rs`. Register in `Kernel`; bind names in `populate_env`.
   Tests: `Array new: 3` returns an Object; `Array name` returns `'Array'`;
   `ByteArray new: 4` returns an Object; `OrderedCollection new` returns an
   Object.
3. **`dispatch_object_value` routing**: add the `effective_primitive_kind` arm.
   Tests: MNU on plain Object still works; unknown selector on Array produces MNU.
4. **`ByteArray` protocol**: implement `dispatch_byte_array`. Tests: `at:`,
   `at:put:`, `size`, `do:`, `asString`, `copyFrom:to:`, bounds errors.
5. **`Array` protocol**: implement `dispatch_array`. Tests: full protocol table
   coverage — `at:`, `at:put:`, `first`, `last`, `do:`, `collect:`,
   `select:`, `reject:`, `detect:ifNone:`, `inject:into:`, `includes:`,
   `copyFrom:to:`, `printString`, structural `=`, bounds errors.
6. **`OrderedCollection` protocol**: implement `dispatch_ordered_collection`.
   Tests: `add:` / `removeFirst` as queue, `addFirst:` / `removeFirst` as
   stack, `do:`, `collect:`, `select:`, `printString`, empty-collection errors.
7. **Array literal syntax**: `Token::ArrayLiteralOpen` in lexer, `LiteralValue`
   + `Expr::ArrayLiteral` in AST, parser rule, eval. Tests: `#(1 2 3) at: 2`
   → `2`; `#(#foo #bar) size` → `2`; `#() isEmpty` → `true`; mixed literal
   types; non-literal inside `#(...)` is a parse error.
8. **Integration tests**: OC as a queue, `collect:` chained with
   `inject:into:`, user-defined subclass of Array with an added method,
   Array literal passed to `do:`.
9. **Coverage check**: `just cov` — no file below 75%.

---

## Open questions

### `collect:` on Array returns Array or OrderedCollection?

`collect:` always knows the result size (same as receiver). It can return a new
`Array` of the same size. `select:` / `reject:` cannot know the size upfront and
return `OrderedCollection`. This asymmetry is intentional and matches Pharo's
`Array >> collect:` behaviour. Revisit with a `species` protocol later.

### OrderedCollection growth strategy

Rust's `Vec<Value>` doubles automatically. This is not exposed in the API.
`new: n` as a capacity hint is deferred.

### Nested `#(...)` literals

`#(#(1 2) #(3 4))` is not supported — the inner `#(` is not recognized inside
the literal context. Deferred.

### ByteArray `asString` encoding

`asString` uses Rust's `String::from_utf8_lossy`, replacing invalid sequences
with U+FFFD. This is pragmatic for bootstrap. A strict `asUtf8String` / error
variant can be added later.
