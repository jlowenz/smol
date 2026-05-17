# Chunk 18: super, Class-Side Methods, and Namespace Runtime

## Status

Chunk 17 delivered user-defined classes, methods, instance/class variables,
LargeInt, and a multi-line REPL — 79 tests passing. Chunk 18 completes the
core object model and introduces the namespace system that was fully designed
but deferred in chunk 17.

**Delivers:**

1. `super` dispatch — method lookup starts from the superclass of the class
   that defined the currently-executing method, not from the receiver's class.
2. Class-side method dispatch — each class gains a separate method dictionary
   for class-side (class >> selector) methods; dispatch on `Value::Class`
   checks this dict before falling through to built-ins.
3. Class-side method definition syntax — `ClassName class >> pattern [ body ]`.
4. Class-side `self` binding — inside a class-side method `self` is the
   `Value::Class` object, not an instance.
5. `new` / `new:` become overridable — user-defined class-side `new` takes
   priority over the built-in fallback.
6. Namespace runtime — `Kernel` primordial namespace pre-created at bootstrap;
   `Namespace` class; user namespaces via
   `Namespace named: #ns imports: { Kernel }`.
7. `::` resolution — `Token::ColonColon` (already reserved) becomes
   operational: `ns::ClassName` resolves to the class named `ClassName` in
   namespace `ns`.
8. Namespace-scoped class registration — `subclass:` registers the new class
   in the active namespace (default: `Kernel`).

**Explicitly deferred:**

- Full `Behavior` / `Class` / `Metaclass` object hierarchy (bootstrap
  metaclasses use a simplified class-side dict on `Class`, not a separate
  metaclass object).
- Traits / multiple inheritance via trait composition — chunk 19.
- Actor model and entity identity.
- Triple store integration and `Package` runtime.
- Garbage collection (still using `Arc`).
- Exception class hierarchy beyond the existing `on:do:` built-in.
- Collection classes (`Array`, `OrderedCollection`).

---

## Design decisions

### `super` dispatch

`super` sends look up the selector starting from the superclass of the class
that *defined* the currently-executing method — not from the receiver's class
and not from the receiver's superclass. This is the standard Smalltalk rule.

**Change to `method_stack`**: replace `RefCell<Vec<Arc<Object>>>` with
`RefCell<Vec<MethodContext>>`:

```rust
struct MethodContext {
    receiver: Arc<Object>,
    /// Class in which the currently-executing method was defined.
    /// Drives super-send lookup: start from `defining_class.superclass`.
    defining_class: Arc<Class>,
}
```

`method_depth: Cell<usize>` and `self_sym` (`OnceLock`) are unchanged.

**`super` in the AST**: add `Token::Super` to the lexer (alongside the
existing `true` / `false` / `nil` keywords) and `Expr::Super` to the AST.
`Expr::Super` carries no payload; the receiver it denotes is always the
current method's receiver.

**Evaluation**: in `eval_expr`:

- `Expr::Super` → evaluate to `Value::Object(current_receiver)`.
- `Expr::Send { receiver: Expr::Super, message }` is detected before
  evaluating the receiver normally and routed to a `send_super` path.

`send_super`:

1. Get `defining_class` from `method_stack.last().defining_class`.
2. Let `sc = defining_class.superclass?` (error if no superclass).
3. Look up the selector in `sc` via `sc.lookup(selector)`.
4. Invoke the found method on the current receiver (not `super` — `self` and
   `super` always denote the same object, just different lookup starting
   points).

`Expr::Super` outside a message send (e.g., stored in a variable) is a
runtime error: `super must only appear as a message receiver`.

**Rationale**: detecting `Expr::Super` at the send site — rather than
introducing a `Value::Super` variant — keeps the value representation clean
and makes the constraint explicit at the site where it matters.

### Class-side method dispatch

Every `Class` struct gains a second method dictionary:

```rust
pub struct Class {
    // ... existing fields ...
    /// Methods defined on the class side (`ClassName class >> selector`).
    pub class_methods: RefCell<HashMap<Arc<str>, Arc<Method>>>,
}
```

Dispatch for `Value::Class(class)` in `dispatch_class` (builtins.rs):

1. Check `class.class_methods` for the selector.
2. If found, invoke via a new `invoke_class_method` path that binds
   `self → Value::Class(class)` (not an object instance).
3. Fall through to built-in `new`, `new:`, `name`, `printString`.

`invoke_class_method` is the class-side analogue of `invoke_method`:

- Push a `ClassMethodContext { class: Arc<Class>, defining_class: Arc<Class> }`
  onto a separate `class_method_stack` (or reuse `method_stack` with a
  `SelfBinding` enum — see below).
- Bind `self → Value::Class(class)` in the method frame.
- Variable lookup in a class-side method:
  1. Local frame.
  2. Class variables from `class.lookup_class_var_owner(name)` (no instance
     vars — there is no instance).
  3. Outer env (globals / class names).

**`SelfBinding` enum** (alternative to two stacks):

```rust
enum SelfBinding {
    Instance { receiver: Arc<Object>, defining_class: Arc<Class> },
    ClassSide { class: Arc<Class>, defining_class: Arc<Class> },
}
```

`method_stack: RefCell<Vec<SelfBinding>>`. This keeps a single stack and
naturally generalises `current_receiver()` into `current_self_value()` →
`Value`. `assign_var` checks `SelfBinding::Instance` for ivar writes (class-
side methods have no ivars) and falls through to class vars or env for both
variants.

**`super` in class-side methods**: `defining_class` in the `ClassSide`
variant drives super lookup. `super new` in a class-side method finds `new`
starting from `defining_class.superclass`.

### Class-side method definition syntax

```smalltalk
Counter class >> fromInteger: n [
    | c |
    c := self new.
    c setCount: n.
    ^c
]
```

Parser detection in `parse_stmt`: after consuming `IDENT '>>'` and seeing
that the next token after `IDENT` is `Token::Ident("class")` and then `'>>'`,
parse as a class-side `MethodDef`. OR, simpler: treat `class` as a unary
selector in the `>>` lookahead:

```
class_method_def ::= IDENT 'class' '>>' method_pattern '[' body ']'
```

Concretely, the parser already detects `IDENT '>>'` for instance-side defs.
Extend the lookahead: if the token sequence is `IDENT IDENT(="class") '>>'`,
parse as a class-side method def.

AST change: extend `MethodDef`:

```rust
pub struct MethodDef {
    pub class_name: Arc<str>,
    pub pattern: MethodPattern,
    pub locals: Vec<Arc<str>>,
    pub body: Vec<Stmt>,
    pub is_class_side: bool,   // NEW: true for `Foo class >> ...`
}
```

Evaluator: `Stmt::MethodDef` with `is_class_side = true` installs the method
into `class.class_methods`.

### `new` / `new:` overridability

`dispatch_class` checks `class.class_methods` for `"new"` / `"new:"` *before*
the existing built-in code. If a user-defined class-side `new` exists it runs
in its method frame with `self = Value::Class(class)`. The built-in `new` /
`new:` remain as fallbacks.

### Namespace runtime

**`src/namespace.rs`** — new file:

```rust
pub struct Namespace {
    pub name: Arc<str>,
    /// Classes registered in this namespace.
    classes: RefCell<HashMap<Arc<str>, Arc<Class>>>,
    /// Namespaces whose exports are visible here (import list).
    imports: RefCell<Vec<Arc<Namespace>>>,
}

impl Namespace {
    pub fn new(name: Arc<str>) -> Self;
    pub fn register(&self, class: Arc<Class>);
    /// Look up a class by name in this namespace, then in imports.
    pub fn lookup_class(&self, name: &str) -> Option<Arc<Class>>;
}
```

**`ClassRegistry` becomes a thin wrapper** over the `Kernel` namespace:
`ClassRegistry::register` and `ClassRegistry::get` delegate to the `Kernel`
namespace. The registry holds `Arc<Namespace>` for each named namespace and
exposes a `get_namespace(name)` lookup.

**Bootstrap sequence** (in `ClassRegistry::new` / `Evaluator::new`):

1. Create the `Kernel` namespace object.
2. Create the root `Object` class and register it in `Kernel`.
3. Create the `Namespace` class (with `name: Arc<str>`, `imports: OrderedCollection`
   instance vars) and register it in `Kernel`.
4. Retroactively set `Kernel`'s class pointer to `Namespace` (the
   boot-cycle pattern: `Kernel` is initially a plain struct, becomes a
   first-class `Value::Namespace(...)` once the `Namespace` class exists).

For the bootstrap interpreter, `Value::Namespace(Arc<Namespace>)` is a new
value variant. `populate_env` binds namespace names as
`Value::Namespace(...)`.

**User namespace creation**:

```smalltalk
Namespace named: #mylib imports: { Kernel }.
```

`Namespace` responds to `named:imports:` as a class-side method (class-side
method defined in Smol code during bootstrap, not built-in). The method
creates a new `Namespace` object, registers it in the registry, binds it in
the global env.

**`::` resolution** in `parse_primary`:

```
ns_ref  ::= IDENT '::' IDENT
```

When `parse_primary` sees `Token::Ident(ns) Token::ColonColon Token::Ident(class)`,
parse as `Expr::NamespaceRef { namespace: ns, name: class }`.

Evaluation of `Expr::NamespaceRef`:

1. Look up `namespace` in env → expect `Value::Namespace(ns_obj)`.
2. Call `ns_obj.lookup_class(name)` → `Value::Class(...)`.
3. Error if namespace or class is not found.

Namespace-scoped `subclass:` — when `handle_subclass` registers the new class,
it checks the current namespace context. The evaluator carries
`active_namespace: Arc<Namespace>` (default: `Kernel`). A `Namespace do: [...]`
block sets the active namespace for its body.

---

## New types

### `src/namespace.rs`

```rust
pub struct Namespace {
    pub name: Arc<str>,
    classes: RefCell<HashMap<Arc<str>, Arc<Class>>>,
    imports: RefCell<Vec<Arc<Namespace>>>,
}
```

### `src/ast.rs`

```rust
pub enum Expr {
    // existing ...
    Super,
    /// `ns::ClassName` — resolved at eval time.
    NamespaceRef { namespace: Arc<str>, name: Arc<str> },
}

pub struct MethodDef {
    pub class_name: Arc<str>,
    pub pattern: MethodPattern,
    pub locals: Vec<Arc<str>>,
    pub body: Vec<Stmt>,
    pub is_class_side: bool,
}
```

### `src/value.rs`

```rust
pub enum Value {
    // existing ...
    Namespace(Arc<Namespace>),
}
```

### `src/eval.rs`

```rust
pub enum SelfBinding {
    Instance { receiver: Arc<Object>, defining_class: Arc<Class> },
    ClassSide { class: Arc<Class>, defining_class: Arc<Class> },
}

pub struct Evaluator {
    pub registry: Arc<ClassRegistry>,
    method_stack: RefCell<Vec<SelfBinding>>,
    method_depth: Cell<usize>,
    active_namespace: RefCell<Arc<Namespace>>,
}
```

---

## Changes to existing types

### `src/class.rs`

Add `class_methods: RefCell<HashMap<Arc<str>, Arc<Method>>>` to `Class`.

### `src/class_registry.rs`

Hold `Arc<Namespace>` per namespace name in addition to the existing flat
class table (which becomes `Kernel`'s class dict). Add
`get_namespace(name) -> Option<Arc<Namespace>>` and
`register_namespace(ns: Arc<Namespace>)`.

### `src/lexer.rs`

Add `Token::Super`. In `read_ident_or_keyword`, map `"super"` → `Token::Super`
alongside the existing `"true"` / `"false"` / `"nil"` cases.

### `src/parser.rs`

- `parse_primary`: `Token::Super` → `Expr::Super`.
- `parse_primary`: `Token::Ident(ns)` followed by `Token::ColonColon` →
  consume both, expect `Token::Ident(name)`, produce
  `Expr::NamespaceRef { namespace: ns, name }`.
- `parse_stmt` lookahead for class-side method defs:
  `IDENT Ident("class") BinOp(">>")` → `parse_class_method_def`.
- `MethodDef` gains `is_class_side: bool`.

### `src/eval.rs`

- `method_stack` element type changes to `SelfBinding`.
- `current_receiver()` → `Option<Arc<Object>>` (returns `None` on class-side context).
- `current_self_value()` → `Value` (instance or class).
- `current_defining_class()` → `Option<Arc<Class>>` (for super lookup).
- `eval_expr` special-cases `Expr::Send` where receiver is `Expr::Super`.
- `assign_var` checks `SelfBinding::Instance` for ivar writes; skips ivar
  check on `SelfBinding::ClassSide`.
- `invoke_class_method` added alongside `invoke_method`.
- `Stmt::MethodDef` with `is_class_side = true` → install in `class_methods`.
- `Evaluator` carries `active_namespace: RefCell<Arc<Namespace>>`.

### `src/builtins.rs`

`dispatch_class`:

1. Check `class.class_methods` for the selector first.
2. If found, call `ev.invoke_class_method(class, method, args, env)`.
3. Else fall through to built-in `new`, `new:`, `name`, `printString`.

---

## Primitive `Namespace` class

Registered at bootstrap. Instance variable: `name` (Symbol), `imports`
(pointer-indexed slots holding namespace references). Methods:

| Selector | Behaviour |
|---|---|
| `name` | returns the namespace's name Symbol |
| `printString` | `'Namespace(name)'` |
| `includes:` | true if the named class is registered |
| `at:` | look up a class by name, MNU if absent |
| `do:` | eval block with this as the active namespace |

Class-side:

| Selector | Behaviour |
|---|---|
| `named:imports:` | allocate a new namespace, register in registry |

---

## Implementation order

1. **`super`**: Add `Token::Super`, `Expr::Super`. Change `SelfBinding` enum,
   update `method_stack`. Add `send_super` in `eval_expr`. Tests: super in
   a two-level hierarchy.
2. **Class-side method dict**: Add `class_methods` to `Class`. Add
   `is_class_side` to `MethodDef`. Extend parser. Update `dispatch_class`
   to check user-defined class-side methods first. Update `invoke_class_method`.
   Tests: class-side method overrides `new`, `fromInteger:` factory method.
3. **`src/namespace.rs`**: Implement `Namespace` struct.
4. **`Value::Namespace`**, `ClassRegistry` namespace support.
5. **Bootstrap sequence**: `Kernel` namespace, register `Object` and `Namespace`
   classes there, bind `Kernel` in env.
6. **`Expr::NamespaceRef`**: parser + evaluator. Tests: `Kernel::Object new`.
7. **`Namespace named:imports:`** as a class-side method defined in Smol
   bootstrap code (not a built-in), plus a built-in fallback for the earliest
   bootstrap phase.
8. **`active_namespace` on Evaluator**: `subclass:` registers in the active
   namespace. `Namespace do: [...]` swaps the active namespace for the block.
9. Integration tests: multi-namespace class lookup, `::` resolution, super
   in a three-class hierarchy, class-side `new` override.
10. Benchmarks: namespace lookup overhead (should be negligible — one extra
    HashMap lookup on class resolution), super dispatch overhead vs. direct.

---

## Open questions

### Full metaclass hierarchy vs. simplified class-side dict

The simplified approach (class-side dict on `Class`) is inconsistent with
Smalltalk's "everything is an object" — `Counter class` returns
`Value::Class(Counter)`, not a distinct metaclass object. `Counter class class`
would return `Class` (or `Value::Class(Class)`) rather than a proper metaclass.
This is acceptable for the bootstrap but will need to be revisited when the
system needs to reflect on its own class hierarchy. The decision to use
`is_class_side` on `MethodDef` rather than a separate metaclass object is
recorded here as intentional.

### Namespace as value vs. as registry-only

Making `Value::Namespace(Arc<Namespace>)` a first-class value allows
namespaces to be passed around, stored in variables, and queried. The
alternative — keeping namespaces purely in `ClassRegistry` and never exposing
them as `Value` — is simpler but prevents the `ns do: [...]` pattern and
`Namespace named:imports:` returning a usable object. First-class namespace
values are adopted.

### `active_namespace` scope

A `RefCell<Arc<Namespace>>` on `Evaluator` makes the active namespace a
global (per-evaluator) mutable cell. For the single-threaded bootstrap this
is fine. In a concurrent future the active namespace would move to a per-fiber
or per-method-frame slot. This is recorded as a known future migration point.
