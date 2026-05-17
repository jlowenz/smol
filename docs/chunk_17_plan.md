# Chunk 17: User-Defined Classes and Methods

## Status: COMPLETE

Chunk 17 implemented. 68 tests pass (63 from chunk 16 + 5 new integration tests).

New files: `class.rs`, `object.rs`, `class_registry.rs`.
Modified: `lexer.rs`, `ast.rs`, `value.rs`, `parser.rs`, `env.rs`, `eval.rs`,
`builtins.rs`, `lib.rs`, `main.rs`, `benches/interpreter.rs`.

Chunk 16 delivered a complete bootstrap interpreter with built-in dispatch for
seven `Value` kinds, a persistent REPL, and 57 passing tests. Chunk 17 adds
the first layer of the object model: user-defined classes and methods, instance
variables, and the `self` binding. After this chunk, non-trivial programs can
be structured as cooperating objects rather than flat scripts.

**Delivers:**

1. Class definition syntax and runtime representation.
2. Method definition syntax, runtime storage, and lookup.
3. `Value::Object` — heap-allocated user-defined object instances.
4. Instance variable read and write within method bodies.
5. Class variable read and write (`classVariableNames:`) — shared across all instances of a class and its subclasses.
6. `self` binding in method execution context.
7. `new` / `new:` / `initialize` protocol (`new:` allocates indexed slots).
8. Indexed slot declaration with three storage kinds: `Pointer` (any `Value`), `Byte` (`u8`), `Float64` (`f64`). Only `Pointer` slots are fully operational in this chunk; `Byte` and `Float64` kinds are declared and allocated but lack dedicated built-in methods (deferred to when `ByteArray` and `Float64Array` primitives are introduced).
9. A primitive `Object` root class with a small set of universal methods.
10. Multi-line REPL input (required for multi-statement method bodies).
11. `LargeInt` support via `num-bigint` (carried over from chunk 16 backlog).

**Namespace / package design direction (adopted, implementation deferred to chunk 18):**

Smol distinguishes two related concepts:

- **Namespace** — a lightweight, runtime object providing name-resolution scope. A class belongs to exactly one namespace. Namespaces can import other namespaces. Implemented as an instance of class `Namespace`.
- **Package** — a rich container object grouping classes, documentation, tests, and media for deployment and versioning. A package declares which namespace(s) its classes inhabit. Backed by the triple store as a subject with `contains:`, `dependsOn:`, and `documents:` predicates.

A primordial root namespace (`Kernel`) is pre-created at bootstrap before user code runs. `Object`, `Class`, `Behavior`, and `Namespace` itself live there. Once `Namespace` is defined, the root namespace is retroactively recognised as an instance of it (same boot-cycle pattern as `Class`/`Metaclass`). User namespaces are created via `Namespace named: #mylib imports: { Kernel }`.

Namespace qualification uses `::` (e.g., `mylib::Counter`). The `::` token is reserved in the lexer this chunk even though namespace resolution is not yet implemented.

**Explicitly deferred:**

- `super` — requires threading the "lookup class" through the evaluator; chunk 18.
- Metaclasses and true class-side methods — `new` / `new:` are built-in in this chunk.
- Namespace and package runtime implementation — chunk 18.
- Traits / multiple inheritance via trait composition — chunk 18.
- Actor model and entity identity.
- Triple store integration.
- Garbage collection (still using `Arc`).

---

## Design decisions before implementation

### Class definition syntax

Use the Smalltalk keyword-message form, parsed as a special statement:

```smalltalk
Object subclass: #Counter instanceVariableNames: 'count'.
```

This is a keyword send on the identifier `Object` with selector
`subclass:instanceVariableNames:`. The evaluator recognises this message on a
`Value::Class` receiver and registers the new class. No new AST node is
strictly required — the existing `Stmt::Expr(Expr::Send { ... })` covers it —
but a dedicated `Stmt::ClassDef` is cleaner and allows the evaluator to
distinguish class registration from ordinary message sends at a glance.

**Rationale for keyword-message form over bespoke syntax:** consistent with D12
(Smalltalk surface grammar as starting point). A bespoke `class Counter < Object
{ ... }` syntax buys nothing at this stage and diverges prematurely.

### Method definition syntax

Use the `>>` form from VisualWorks/Pharo file-in:

```smalltalk
Counter >> initialize [
    count := 0
]

Counter >> increment [
    count := count + 1
]

Counter >> count [
    ^count
]
```

`ClassName >> methodPattern [ body ]` is parsed as a new top-level statement
form (`Stmt::MethodDef`). The `>>` token is a new `BinOp` already lexed; the
parser recognises it in statement position as a method definition rather than a
binary send. The body is a `[...]` block without parameters (method arguments
are declared in the pattern).

**Method pattern forms:**

```
unary:   Counter >> count [ ^count ]
binary:  MyNum >> + other [ ... ]
keyword: Counter >> incrementBy: n [ count := count + n ]
```

### Instance variable and class variable access

Within a method body, variable names resolve through a `MethodContext` carried
in the evaluator (not through the lexical `Env`). Variable lookup order:

1. Local frame (temporaries, parameters).
2. `MethodContext.receiver` instance variable slots (by name).
3. Class variables — walk the superclass chain from `receiver.class` upward,
   checking each class's `class_var_storage` for the name.
4. Outer lexical environment (globals; enclosing `Env` at method-definition
   time, preserved for future closures within methods).

Assignment to an instance variable name writes to the object's own slot.
Assignment to a class variable name writes to the `class_var_storage` of the
class in the hierarchy that owns the variable (the class where the variable is
declared, not necessarily `receiver.class`).

Instance variable names must be unique across the entire inheritance chain
(including superclass vars). A class definition that introduces a name already
declared in a superclass is a registration-time error.

**Class variable name uniqueness:** Class variable names must be disjoint from
instance variable names within the same class but may shadow class variables in
unrelated branches. When the same class variable name appears at multiple levels
of the hierarchy, the highest (closest to root) declaration wins for lookup and
write.

**Rationale for name-keyed instance slots over index-keyed:** the bootstrap
does not benefit from indexed-slot performance. Stable slot-index ordering
(`superclass vars → own vars, in definition order`) is documented as the
contract for future migration but not enforced here.

### Class definition keywords

The full `ClassDef` surface syntax:

```smalltalk
Object subclass: #Counter
    instanceVariableNames: 'count total'
    classVariableNames: 'InstanceCount'
    indexedSlots: #none.

Object subclass: #Array
    instanceVariableNames: ''
    indexedSlots: #pointer.

Object subclass: #ByteArray
    instanceVariableNames: ''
    indexedSlots: #byte.
```

`indexedSlots:` accepts `#none` (default, no indexed region), `#pointer`
(general `Value` slots), `#byte` (`u8` slots), or `#float64` (`f64` slots).

### `new`, `new:`, and `initialize`

`new` and `new:` are built-ins handled directly in the evaluator when the
receiver is a `Value::Class`:

**`new`** (no indexed slots, or class declared `indexedSlots: #none`):
1. Allocate a new `Object` with all instance variable names (own + inherited)
   bound to `nil`. No indexed region.
2. If the class (or any superclass) defines `initialize`, send it.
3. Return the object.

**`new: n`** (class declared an indexed slot kind):
1. Allocate instance variables as above.
2. Allocate indexed region of size `n`:
   - `#pointer` → `Vec<Value>` of `n` `nil`s.
   - `#byte` → `Vec<u8>` of `n` zeros.
   - `#float64` → `Vec<f64>` of `n` zeros.
3. If `initialize` is defined, send it.
4. Return the object.

Sending `new:` to a class with `indexedSlots: #none` is a runtime error.
Sending `new` to a class with indexed slots allocates an empty (zero-length)
indexed region.

This sidesteps metaclasses for now. True class-side method dispatch arrives
in chunk 18.

### `LargeInt`

Add `Value::LargeInt(Arc<BigInt>)` using the `num-bigint` crate. Lexer
promotes integer literals that exceed `i64::MAX` / fall below `i64::MIN`.
Arithmetic on `Int` that overflows detects overflow via Rust's `checked_*`
methods and promotes to `LargeInt`. `LargeInt` arithmetic always uses
`num-bigint`. Both `Int` and `LargeInt` respond to the same selector set.

---

## New types

### `src/class.rs`

```rust
/// Indexed storage kind declared at class definition time.
pub enum IndexedSlotKind {
    Pointer,  // Vec<Value> — any object reference
    Byte,     // Vec<u8>   — compact binary data
    Float64,  // Vec<f64>  — unboxed floating-point array
}

/// A user-defined class.
pub struct Class {
    pub name: Arc<str>,
    /// `None` only for the root `Object` class.
    pub superclass: Option<Arc<Class>>,
    /// Instance variable names, own only (not inherited).
    /// Requires: disjoint from all superclass instance variable names.
    pub own_instance_vars: Vec<Arc<str>>,
    /// Class variable names, own only.
    pub own_class_vars: Vec<Arc<str>>,
    /// Shared class variable storage. Keyed by own name; inherited class vars
    /// are accessed by walking the superclass chain.
    pub class_var_storage: RefCell<HashMap<Arc<str>, Value>>,
    /// `None` means no indexed region (normal named-slot-only object).
    pub indexed_slot_kind: Option<IndexedSlotKind>,
    pub methods: RefCell<HashMap<Arc<str>, Arc<Method>>>,
}

impl Class {
    /// All instance variable names in definition order: superclass vars first,
    /// then own vars.
    pub fn all_instance_vars(&self) -> Vec<Arc<str>>;

    /// Look up a method by selector, walking the superclass chain.
    /// Returns the method and the class it was found in (needed for `super`
    /// in chunk 18).
    pub fn lookup(&self, selector: &str) -> Option<(Arc<Method>, Arc<Class>)>;

    /// Look up a class variable by name, walking the superclass chain.
    /// Returns a reference to the class that owns the variable (for writes).
    pub fn lookup_class_var(&self, name: &str) -> Option<Arc<Class>>;
}

pub struct Method {
    pub pattern: MethodPattern,
    pub locals: Vec<Arc<str>>,
    pub body: Vec<Stmt>,
}

pub enum MethodPattern {
    Unary(Arc<str>),
    Binary { op: Arc<str>, param: Arc<str> },
    Keyword { parts: Vec<(Arc<str>, Arc<str>)> }, // (keyword, param_name)
}

impl MethodPattern {
    /// The full selector string: `count`, `+`, `at:put:`.
    pub fn selector(&self) -> Arc<str>;
    /// Parameter names in order.
    pub fn params(&self) -> Vec<Arc<str>>;
}
```

### `src/object.rs`

```rust
/// Indexed storage region for instances of variableSubclass: classes.
pub enum IndexedStorage {
    Pointer(RefCell<Vec<Value>>),
    Byte(RefCell<Vec<u8>>),
    Float64(RefCell<Vec<f64>>),
}

/// A user-defined object instance.
pub struct Object {
    pub class: Arc<Class>,
    /// Named instance variable storage (own + inherited). Keyed by name.
    pub slots: RefCell<HashMap<Arc<str>, Value>>,
    /// Present iff `class.indexed_slot_kind` is `Some(_)`.
    pub indexed: Option<IndexedStorage>,
}

impl Object {
    /// Requires: `class.all_instance_vars()` matches the intended slot set.
    /// Ensures: all named slots initialised to `Value::Nil`; no indexed region.
    pub fn new(class: Arc<Class>) -> Self;

    /// Requires: `class.indexed_slot_kind` is `Some(_)`.
    /// Ensures: all named slots initialised to `Value::Nil`; indexed region
    ///          allocated with `size` elements at their zero value.
    pub fn new_with_indexed(class: Arc<Class>, size: usize) -> Self;

    pub fn get_slot(&self, name: &str) -> Option<Value>;

    /// Requires: `name` is a known instance variable of this object's class.
    /// Ensures: slot is updated.
    pub fn set_slot(&self, name: &str, value: Value);
}
```

### `src/class_registry.rs`

```rust
/// Global table of named classes. Shared through the evaluator.
pub struct ClassRegistry {
    classes: RefCell<HashMap<Arc<str>, Arc<Class>>>,
}

impl ClassRegistry {
    pub fn new() -> Self;
    pub fn register(&self, class: Arc<Class>);
    pub fn get(&self, name: &str) -> Option<Arc<Class>>;
    /// Returns the root `Object` class (always present).
    pub fn object_class(&self) -> Arc<Class>;
}
```

The registry is created once and held by the `Evaluator` (or threaded through
as a parameter). The root `Object` class is pre-registered at startup.

---

## Changes to existing types

### `src/value.rs`

Add two variants:

```rust
pub enum Value {
    // existing ...
    Object(Arc<Object>),
    Class(Arc<Class>),
    // LargeInt added here if num-bigint is included
    LargeInt(Arc<BigInt>),
}
```

`Value::Class` is how class names are looked up: `Object`, `Counter` etc. are
resolved as variables bound to `Value::Class(...)` in the global environment.

`kind()` returns `"Object"` for `Value::Object` (the class name is available
via `obj.class.name` but `kind()` is a bootstrap tag, not the class name).

### `src/ast.rs`

Add two statement variants:

```rust
pub enum Stmt {
    // existing ...
    ClassDef(ClassDef),
    MethodDef(MethodDef),
}

pub struct ClassDef {
    pub name: Arc<str>,
    pub superclass_name: Arc<str>,
    pub instance_var_names: Vec<Arc<str>>,
    pub class_var_names: Vec<Arc<str>>,
    /// `None` means no indexed region (`indexedSlots: #none` or omitted).
    pub indexed_slots: Option<IndexedSlotKind>,
    /// Forward-compatibility: unrecognised keyword arguments from the surface
    /// syntax are preserved here. The evaluator warns on unknown keys.
    pub extra_keywords: Vec<(Arc<str>, Expr)>,
}

pub struct MethodDef {
    pub class_name: Arc<str>,
    pub pattern: MethodPattern,     // reuse from class.rs via ast.rs re-export
    pub locals: Vec<Arc<str>>,
    pub body: Vec<Stmt>,
}
```

### `src/eval.rs`

Add `MethodContext` and thread it through method evaluation:

```rust
pub struct MethodContext {
    pub receiver: Arc<Object>,
}
```

`Evaluator` gains a `ClassRegistry` field. `eval_program` gains an
`Option<&MethodContext>` parameter (or an inner helper that carries it).
When `None`, variable lookup is purely lexical (REPL mode). When `Some`,
the lookup order is:
1. Local frame (temporaries, parameters).
2. `receiver.slots` — instance variables.
3. Class variable chain — `receiver.class.lookup_class_var(name)` walking
   the superclass chain; read/write via the owning class's `class_var_storage`.
4. Outer lexical environment (globals).

`Stmt::ClassDef` evaluation:
- Validate that `instance_var_names` are disjoint from all superclass instance
  variable names (registration-time error if not).
- Instantiate `Class`, initialise `class_var_storage` entries to `nil`.
- Bind the name as `Value::Class(...)` in the global env; register in the registry.

`Stmt::MethodDef` evaluation: look up the class by name, add the `Method` to
its method dictionary.

Dispatch for `Value::Object`: look up selector in the class hierarchy via
`Class::lookup`, create a new method frame, bind `self` and parameters, run
the body via `eval_program` with a `MethodContext`.

Dispatch for `Value::Class`:
- `new` → `Object::new(class)`, send `initialize` if defined.
- `new: n` → `Object::new_with_indexed(class, n)`, send `initialize` if defined.
  Error if `class.indexed_slot_kind` is `None`.

### `src/lexer.rs`

Reserve `::` as a new token `Token::ColonColon`. The lexer emits it when two
consecutive `:` appear without a trailing alphabetic character (disambiguating
from keyword tokens like `at:`). No parser rules consume `::` in chunk 17 — it
is reserved for namespace qualification in chunk 18.

### `src/parser.rs`

Extend `parse_stmt` to recognise method definition:

```
method_def  ::= IDENT '>>' method_pattern '[' local_decl? stmt* ']'
method_pattern ::= IDENT                        -- unary
                 | BINOP IDENT                  -- binary
                 | (KEYWORD IDENT)+             -- keyword
```

Detection: if the current token is `Ident` and the next non-comment token is
`BinOp(">>")`, parse as `Stmt::MethodDef`.

Class definition is parseable as a keyword send statement; the evaluator
recognises `subclass:instanceVariableNames:classVariableNames:indexedSlots:`
(any subset, any order) on a `Value::Class` receiver as `Stmt::ClassDef`.
Unknown trailing keywords are collected into `extra_keywords`.

---

## Primitive `Object` class

Pre-registered at startup. Methods (all built-in, not user-defined):

| Selector | Behaviour |
|---|---|
| `=` | identity equality (`Arc::ptr_eq`) |
| `~=` | negation of `=` |
| `yourself` | returns `self` |
| `printString` | `'a ClassName'` |
| `isNil` | `false` |
| `notNil` | `true` |
| `class` | returns `Value::Class(self.class.clone())` |
| `isKindOf:` | walks superclass chain checking class identity |
| `respondsTo:` | checks method dictionary including inherited |

These are implemented in `builtins.rs` under a new `dispatch_object` function,
consulted last in the dispatch chain (after user-defined method lookup fails).

---

## Multi-line REPL

Method bodies span multiple lines. The REPL must detect an incomplete input and
prompt for continuation. Detection strategy: after parsing, if the error is
`UnexpectedEof` (not a real lex/parse error), prompt `.. ` and accumulate
another line before retrying.

```
smol 0.1.0
> Object subclass: #Counter instanceVariableNames: 'count'.
> Counter >> initialize [
..     count := 0
.. ]
> Counter >> increment [
..     count := count + 1
.. ]
> | c |
> c := Counter new.
> c increment. c increment. c increment.
> c count
3
```

---

## Implementation order

1. Add `num-bigint` workspace dependency. Implement `Value::LargeInt`, extend
   lexer promotion and `Int` arithmetic overflow detection. Tests for
   arithmetic overflow and large literal parsing.
2. `src/lexer.rs`: add `Token::ColonColon` (`::`) as a reserved token.
3. `src/class.rs`: `IndexedSlotKind`, `Class` (with `own_class_vars`,
   `class_var_storage`, `indexed_slot_kind`), `Method`, `MethodPattern`.
   Unit tests for `lookup` and `lookup_class_var` walking the superclass chain.
4. `src/object.rs`: `IndexedStorage`, `Object` (with `indexed` field),
   `new`, `new_with_indexed`. Unit tests for named slot get/set and indexed
   allocation.
5. `src/class_registry.rs`: `ClassRegistry` with pre-registered `Object` root.
6. `src/value.rs`: add `Value::Object`, `Value::Class`. Update `kind()`,
   `Display`, `PartialEq`, `Hash`.
7. `src/ast.rs`: add `ClassDef` (with `class_var_names`, `indexed_slots`,
   `extra_keywords`), `MethodDef`, `MethodPattern`.
8. `src/parser.rs`: extend `parse_stmt` to detect and parse `>>` method defs;
   map class definition keyword sends into `Stmt::ClassDef`.
9. `src/eval.rs`: wire up `ClassRegistry`; implement `Stmt::ClassDef`
   (including instance variable disjointness check), `Stmt::MethodDef`,
   `MethodContext` with four-level variable lookup, user-defined method
   dispatch, `Value::Class` `new` / `new:` built-ins.
10. `src/builtins.rs`: add `dispatch_object` for primitive `Object` methods.
11. `src/main.rs`: extend REPL to accumulate lines on `UnexpectedEof`.
12. Integration tests: define a class with instance variables, class variables,
    and indexed slots; exercise `new`, `new:`, `initialize`, instance variable
    read/write, class variable shared state across instances, `at:` / `at:put:`
    on pointer-indexed objects, and inheritance of `Object` methods.
13. Benchmarks: add a benchmark for object creation and method send throughput.

---

## Open questions

### Instance variable inheritance and slot ordering

Slot ordering across the inheritance chain must be stable for future migration
to indexed slots. Contract (documented, not yet enforced): superclass instance
vars always precede subclass vars, in definition order within each level.

### `printString` for user-defined objects

The default `Object >> printString` returns `'a ClassName'`. Ensure the
dispatch path checks user-defined method dictionaries before falling back to
the built-in `Object` implementation.

### Error class hierarchy

`on:do:` currently catches all errors under `#Error`. Defer user-defined
exception classes to chunk 18, when core class library bootstrap begins.
