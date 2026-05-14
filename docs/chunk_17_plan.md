# Chunk 17: User-Defined Classes and Methods

## Status

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
5. `self` binding in method execution context.
6. `new` / `initialize` protocol.
7. A primitive `Object` root class with a small set of universal methods.
8. Multi-line REPL input (required for multi-statement method bodies).
9. `LargeInt` support via `num-bigint` (carried over from chunk 16 backlog).

**Explicitly deferred:**

- `super` — requires threading the "lookup class" through the evaluator; chunk 18.
- Metaclasses and true class-side methods — `new` is built-in in this chunk.
- Class variables (`classVariableNames:`).
- Module / namespace declarations.
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

### Instance variable access

Within a method body, instance variable names resolve through a
`MethodContext` carried in the evaluator (not through the lexical `Env`).
Variable lookup order:

1. Local frame (temporaries, parameters).
2. `MethodContext.receiver` instance variable slots (by name).
3. Outer lexical environment (enclosing `Env` at method-definition time — not
   present for class methods in chunk 17, but preserved for future closures
   within methods).

Assignment to an instance variable name inside a method body writes to the
object's slot, not to the lexical frame.

**Rationale for name-keyed slots over index-keyed:** the bootstrap does not
benefit from the performance of indexed slots and the extra complexity of
maintaining slot-index maps across the class hierarchy. Migration to indexed
slots is a later optimisation.

### `new` and `initialize`

`new` is a built-in handled directly in the evaluator when the receiver is a
`Value::Class`:

1. Allocate a new `Object` with the class's instance variable names, all bound
   to `nil`.
2. If the class (or any superclass) defines `initialize`, send it to the new
   object.
3. Return the object.

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
/// A user-defined class.
pub struct Class {
    pub name: Arc<str>,
    /// `None` only for the root `Object` class.
    pub superclass: Option<Arc<Class>>,
    /// Instance variable names, own only (not inherited).
    pub own_instance_vars: Vec<Arc<str>>,
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
/// A user-defined object instance.
pub struct Object {
    pub class: Arc<Class>,
    /// Instance variable storage, keyed by name. Includes inherited vars.
    pub slots: RefCell<HashMap<Arc<str>, Value>>,
}

impl Object {
    /// Requires: `class.all_instance_vars()` matches the intended slot set.
    /// Ensures: all slots initialised to `Value::Nil`.
    pub fn new(class: Arc<Class>) -> Self;

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
instance variable names are checked against `receiver.slots` after local
frames but before outer env.

`Stmt::ClassDef` evaluation: instantiate `Class`, bind the name as
`Value::Class(...)` in the global env, register in the registry.

`Stmt::MethodDef` evaluation: look up the class by name, add the `Method` to
its method dictionary.

Dispatch for `Value::Object`: look up selector in the class hierarchy via
`Class::lookup`, create a new method frame, bind `self` and parameters, run
the body via `eval_program` with a `MethodContext`.

Dispatch for `Value::Class` — `new` only in chunk 17: allocate, send
`initialize` if defined, return.

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

Class definition is already parseable as a keyword send statement
(`Object subclass: #Counter instanceVariableNames: 'count'.`); the evaluator
recognises the pattern.

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
2. `src/class.rs`: `Class`, `Method`, `MethodPattern`. Unit tests for
   `lookup` walking the superclass chain.
3. `src/object.rs`: `Object`. Unit tests for slot get/set.
4. `src/class_registry.rs`: `ClassRegistry` with pre-registered `Object` root.
5. `src/value.rs`: add `Value::Object`, `Value::Class`. Update `kind()`,
   `Display`, `PartialEq`, `Hash`.
6. `src/ast.rs`: add `ClassDef`, `MethodDef`, `MethodPattern`.
7. `src/parser.rs`: extend `parse_stmt` to detect and parse `>>` method defs.
8. `src/eval.rs`: wire up `ClassRegistry`; implement `Stmt::ClassDef`,
   `Stmt::MethodDef`, `MethodContext`, user-defined method dispatch,
   `Value::Class` `new` built-in.
9. `src/builtins.rs`: add `dispatch_object` for primitive `Object` methods.
10. `src/main.rs`: extend REPL to accumulate lines on `UnexpectedEof`.
11. Integration tests: define a class with multiple methods in a test string,
    exercise `new`, `initialize`, instance variable read/write, inheritance
    of `Object` methods.
12. Benchmarks: add a benchmark for object creation and method send throughput.

---

## Open questions

### Class definition as message vs keyword

`Object subclass: #Counter instanceVariableNames: 'count'.` requires the
parser/evaluator to recognise a specific message pattern as a class definition.
An alternative is to parse it as a normal keyword send on `Value::Class` and
let the evaluator handle it as a special built-in on class receivers. This
avoids a dedicated `Stmt::ClassDef` node. Evaluate which approach keeps the
evaluator cleaner before implementing.

### Instance variable inheritance and slot ordering

When a subclass adds instance variables, the slot map of instances must include
both own and inherited variables. With a name-keyed map this is trivially a
merged set. When we later migrate to indexed slots, slot ordering across the
inheritance chain must be stable. Record the ordering contract now even if
not enforced: superclass vars always precede subclass vars, in definition order.

### `printString` for user-defined objects

The default `Object >> printString` returns `'a ClassName'`. Many classes will
want to override it. Ensure the dispatch path checks user-defined method
dictionaries before falling back to the built-in `Object` implementation.

### Error class hierarchy

`on:do:` currently catches all errors under `#Error`. Chunk 17 can introduce
user-defined exception classes (subclasses of a built-in `Error`) but this
is not required. Defer unless a test case demands it.
