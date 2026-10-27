# Namespaces, bindings and class scope — design exploration

**Status:** DRAFT. §1–§8 are the original exploration, kept as the record. §9
(2026-10-26) revises the direction: namespaces hold definitions only, which
retires the binding variants of §5. Read §9 first.
**Started:** 2026-10-26. Follows the class-variables discussion opened in
`vm_compiler_plan.md` §3.1 ("Open: class variables").

The purpose of this document is to show *the same intent* written under each
open choice, so the choices can be judged by how the code reads and behaves.
All selectors marked † are placeholders; naming is deferred to the naming and
style conventions pass.

---

## 1. Decided so far (user, 2026-10-25/26)

1. **Namespaces are objects.**
2. **A namespace is an entity** that owns its contents (definitions and
   bindings) and publishes an **immutable snapshot** for reading. Writes are
   messages to the entity (serialized, recorded, transactional); name resolution
   reads the current snapshot (no Future, no lock), guarded by a version tag as
   for classes (VD6).
3. **Classes are not namespaces.**
4. **No class variables; no per-class mutable state.** A class is an immutable,
   versioned value with an implicit metaclass (`Foo class` answers it and it is
   inspectable). Shared mutable state is an explicit entity reachable through a
   binding.
5. **A binding names a value, not a cell** (extending the `let` rule of
   `perf_env_frame_redesign.md` §2.1). To change state, message the entity the
   binding names; to change the binding itself, redefine it.
6. **`let` stays local** (method/block locals; workspace temps at top level,
   §11.7a). It is not used for namespace bindings.
7. **Redefinition:** lazy migration at the entity's next dispatch, with a
   do-nothing-by-default opt-in hook (`migrateFrom:discarded:`†). No "stay on the
   old version" option.
8. **Binding right-hand sides are eager, in statement order, and fail fast** on
   undefined names. No lazy first-read initialization (it would recreate the
   `class_variable_shared` race).

## 2. Where the code is today

- Namespaces are Rust structs (`namespace.rs:14`) with three selectors: `do:`,
  `name`, `asString` (`builtins_iterative.rs:2885-2910`). There is no Smol form
  to create one; only Rust does (`ClassRegistry::register_namespace`).
- The registry holds a *forest* of root namespaces (`class_registry.rs:22`);
  `Kernel` is one root. A free name resolves: own entries → imports → Kernel →
  root namespace names (`class_registry.rs:262-276`). So root names are visible
  everywhere.
- A workspace has an anonymous namespace that is not a root
  (`class_registry.rs:282`).
- A method's free names already resolve through its **defining class's
  namespace** (env-frame d2a). Variant A below needs no new resolution rule
  because of this.
- Definitions inside `Ns do: [ ... ]` already parse and register into `Ns`
  (`tests/parser.rs:591`).

---

## 3. Question: one `Namespace` class, or a class per namespace?

Leaning (user): every namespace is an instance of the single `Namespace` class.

| | Instance of `Namespace` | Subclass per namespace (Newspeak-style modules) |
|---|---|---|
| Create | send a message: `parent namespace: #name`† | define a class, then instantiate it (or operate on the class through its metaclass) |
| Per-namespace behavior | none; behavior belongs to the class | each namespace can have its own methods |
| Parameterization | none | a module class is instantiated with its dependencies, so the same module can be instantiated twice against different platforms (Newspeak's main argument [N1]) |
| Concept cost | low | two objects per namespace (class + instance), plus the metaclass |

**What the single-class choice loses:**

- *Parameterized modules.* Newspeak gets dependency injection and sandboxing
  from "module = class instantiated with its imports". Smol's imports are
  explicit (Q17), which recovers part of this, but not "the same module twice
  with different dependencies".
- *Custom resolution or behavior per namespace* (a lazily loaded namespace, a
  remote namespace, a test double). This is recoverable without per-namespace
  classes: subclass `Namespace` for a *kind* of namespace
  (`RemoteNamespace`†), and make specific namespaces instances of it.

Nothing in the variants below depends on this choice; they assume instances.

---

## 4. Question: the root, and how namespaces are referred to

A namespace is an entity, so a parent refers to a child through an ordinary
binding: **a child namespace is a binding in its parent whose value is the
child**. Every option below keeps that; they differ only in how you get to the
root.

| Option | Root | Absolute path | Today's analogue |
|---|---|---|---|
| R1. Forest | none; several roots in the registry | `app::logging::Logger` (a root name is visible everywhere) | Smol today |
| R2. Single root, children visible everywhere | one root object, reached by message (`Namespace root`†) | `app::logging::Logger` (the root's children resolve last, as root names do today) | Smalltalk's `Smalltalk`; Pharo environments |
| R3. Single root with path syntax | one root object | `::app::logging::Logger` (a leading `::` means "from the root") | C++ `::x`; Rust `::crate` |

R2 has R1's surface syntax with a single inspectable object behind it (P3),
and it adds no syntax. R3 adds one token for the rare case where a local name
shadows a top-level namespace. **Kernel** becomes `kernel` (or stays `Kernel`;
see §6), a child of the root that every namespace imports implicitly, as now.

The workspace's anonymous namespace stays anonymous: not a child of the root,
reachable only through its workspace (Q19 still open).

---

## 5. The common scenario

Each variant below writes the same program:

1. Create namespace `app`, and a child namespace `app::logging`.
2. In `app::logging`, define class `Logger` with:
   - a **runtime-mutable setting** `policy` (default `#stderr`), changed by
     running code;
   - a **definition-time constant** `levels` (the known levels).
3. A `Logger` method reads both.
4. Code in another namespace, `app::web`, uses `Logger` and changes the
   policy.
5. A developer edits `levels` (redefinition).
6. The file is loaded again (reload).

Placeholders used throughout:

- `parent namespace: #name`† answers the existing child if there is one,
  otherwise creates it. It is idempotent, so a reload is safe.
- `ns bind: #name to: value`† (re)defines a binding.
- `ns bindOnce: #name to: [expr]`† evaluates `expr` now (eagerly, in order)
  **only if** `name` is unbound, as CL's `defvar` and Clojure's `defonce` do
  [N2][N3]. The block is not evaluated lazily on first read.
- `ValueHolder` is the existing kernel entity (`kernel.smol:2`: `get`, `set:`).

---

### Variant A — Flat: bindings live in the class's namespace

There is no class scope at all. `policy` and `levels` are ordinary bindings in
`app::logging`; `Logger`'s methods see them by bare name through today's
"defining class's namespace" rule.

```smol
let logging := (Namespace root namespace: #app) namespace: #logging.

logging do: [
    Object subclass: #Logger instanceVariableNames: ''.

    logging bindOnce: #policy to: [ValueHolder initialValue: #stderr].
    logging bind: #levels to: #(#debug #info #warn #error).

    Logger >> report: msg at: level [
        (levels includes: level) ifFalse: [^self error: 'unknown level'].
        ^policy get then: [:p | self pvtEmit: msg via: p]
    ]
].

"Another namespace uses it."
(app namespace: #web) do: [
    app::logging::policy set: #suppress.
    app::logging::Logger new report: 'started' at: #info
].

"A developer edits the constant: re-send the bind (one statement, or the whole file)."
app::logging bind: #levels to: #(#trace #debug #info #warn #error).
```

- **Reads:** short. Bare `levels` and `policy` work in `Logger`'s methods with
  no new mechanism.
- **Weakness:** the association between `policy` and `Logger` exists only by
  co-location. Two classes in one namespace that both want `policy` must use
  prefixes (`loggerPolicy`), which is class scoping by naming convention.
- **Reload:** `bindOnce:` keeps the existing `ValueHolder` and its state;
  `bind:` re-binds `levels`.

### Variant B — Explicit companion namespace

When a class needs its own scope, make one explicitly. The lowercase/CamelCase
convention (§6) lets `logger` (namespace) sit beside `Logger` (class) without
a clash.

```smol
let logging := (Namespace root namespace: #app) namespace: #logging.
let logger := logging namespace: #logger.

logger bindOnce: #policy to: [ValueHolder initialValue: #stderr].
logger bind: #levels to: #(#debug #info #warn #error).

logging do: [
    Object subclass: #Logger instanceVariableNames: ''.

    Logger >> report: msg at: level [
        (logger::levels includes: level) ifFalse: [^self error: 'unknown level'].
        ^logger::policy get then: [:p | self pvtEmit: msg via: p]
    ]
].

(app namespace: #web) do: [
    app::logging::logger::policy set: #suppress.
    app::logging::Logger new report: 'started' at: #info
].

app::logging::logger bind: #levels to: #(#trace #debug #info #warn #error).
```

- **Reads:** one extra `logger::` inside methods. The relationship is
  explicit and inspectable (`logger` is an object you can look at).
- **No new rule:** it is variant A applied one level down. A class gets a scope
  only when its author asks for one.
- **Weakness:** the link between `Logger` and `logger` is still a naming
  convention; nothing in the system knows they belong together. Long external
  paths (`app::logging::logger::policy`); imports (Q17) would shorten them.

### Variant C — Implicit companion namespace

Defining `Logger` implicitly creates a companion namespace, reached by putting
the class name in path position: `Logger::policy`. Inside `Logger`'s methods,
the companion is searched first, so bare names work.

```smol
let logging := (Namespace root namespace: #app) namespace: #logging.

logging do: [
    Object subclass: #Logger instanceVariableNames: ''.

    Logger scope bindOnce: #policy to: [ValueHolder initialValue: #stderr].
    Logger scope bind: #levels to: #(#debug #info #warn #error).

    Logger >> report: msg at: level [
        (levels includes: level) ifFalse: [^self error: 'unknown level'].
        ^policy get then: [:p | self pvtEmit: msg via: p]
    ]
].

(app namespace: #web) do: [
    app::logging::Logger::policy set: #suppress.
    app::logging::Logger new report: 'started' at: #info
].
```

- **Reads:** the shortest inside methods, and the association is structural.
- **Cost:** `Logger` means a class in expression position and a namespace in
  path position, and `Logger scope`† has to answer the companion. This is the
  "class is also a namespace" model by another route, which §1.3 rejects. It
  also adds a lookup step (companion → namespace → imports), and nested-scope
  lookup is where Ruby's constant resolution becomes hard to predict [N4].
- **Redefinition:** does the companion survive when `Logger` is redefined?
  It has to (it holds the `policy` entity), so its lifetime is not the class
  version's lifetime. That is a second object whose lifecycle is tied to a name,
  not to the class.

Included for comparison; it conflicts with decision §1.3.

### Variant D — Declarative "current namespace" instead of `do:` blocks

This is orthogonal to A–C: it changes how a file *enters* a namespace, not where
bindings live. Shown here with A.

```smol
in: app::logging.        "† the rest of this chunk/file defines into app::logging"

Object subclass: #Logger instanceVariableNames: ''.

app::logging bindOnce: #policy to: [ValueHolder initialValue: #stderr].
app::logging bind: #levels to: #(#debug #info #warn #error).

Logger >> report: msg at: level [
    (levels includes: level) ifFalse: [^self error: 'unknown level'].
    ^policy get then: [:p | self pvtEmit: msg via: p]
]
```

- **Reads:** no nesting or indentation for a file that is all one namespace,
  which is the common case: Clojure's `ns`, Java's `package`, C#'s file-scoped
  `namespace X;` [N5].
- **Cost:** `in:`† is either a new statement form (syntax) or a message to the
  workspace/chunk (`thisChunk in: …`?). It also introduces "current namespace"
  as state of the chunk, which is dynamic context. Clojure models it as the
  dynamic Var `*ns*` [N3], so it may fall out of the dynamic-scope work.
- `do:` blocks remain for nested or one-off use.

### Variant E — Capitalized namespaces (B rewritten)

The same code as variant B, to compare how it looks:

```smol
let Logging := (Namespace root namespace: #App) namespace: #Logging.
let LoggerScope := Logging namespace: #LoggerScope.

LoggerScope bindOnce: #policy to: [ValueHolder initialValue: #stderr].
...
App::Logging::LoggerScope::policy set: #suppress.
App::Logging::Logger new report: 'started' at: #info.
```

- With capitals, `Logger` (namespace) and `Logger` (class) collide, which forces
  the awkward `LoggerScope`. The case of a path segment no longer tells you
  what kind of thing it is.
- With lowercase (variant B), the case of each segment is informative:
  `app::logging::Logger` reads as two namespaces and a class.

---

## 6. Naming case (input to the conventions pass)

| | lowercase namespaces (Java, Clojure, C++ std, Rust modules) | Capitalized (Smalltalk globals, C#, today's `Kernel`) |
|---|---|---|
| Kind visible in a path | yes: `kernel::collections::Array` | no |
| Namespace beside class of the same word | yes (`logger` / `Logger`) | collision |
| Looks like a local variable | **yes**: bare `logging` could be a local, an ivar, or a namespace | no |

The lowercase cost needs a **shadowing rule**: a local or ivar named `logging`
shadows the namespace binding `logging` within its scope (the normal lexical
rule), with R3's leading `::` as the escape hatch. Whether a definition-time
warning on such shadowing is wanted is open.

---

## 7. Comparison

| | A Flat | B Explicit companion | C Implicit companion | D Declarative entry (with A) | E Capitalized |
|---|---|---|---|---|---|
| New concepts | none | none | companion scope | current namespace | none |
| Inside methods | `policy` | `logger::policy` | `policy` | `policy` | `LoggerScope::policy` |
| From outside | `app::logging::policy` | `app::logging::logger::policy` | `app::logging::Logger::policy` | as A | `App::Logging::LoggerScope::policy` |
| Class ↔ state link | co-location | naming convention | structural | co-location | naming convention |
| Consistent with §1.3 | yes | yes | no | yes | yes |

A and B combine naturally: flat by default, with an explicit companion when a
namespace holds several classes with conflicting needs. D is independent and
can be added to either.

---

## 8. Open issues raised by the variants

1. **Seeing your own definitions.** A namespace is an entity, so
   `logging bind: …` is a message. The next statement must still see the
   binding. Today `subclass:` has a synchronous carve-out for exactly this
   (`builtins_iterative.rs:2944-2946`). Options: definition sends are awaited by
   the sender; or the sender reads its own pending writes. Either way it must be
   one rule for `subclass:`, `bind:`, method definition and `namespace:`.
2. **Reload.** `bind:` versus `bindOnce:` (two forms), or one form that
   re-evaluates only when the statement's source changed (the statement spans
   from the coverage work make this detectable).
3. **Shadowing** between locals/ivars and lowercase namespace names (§6).
4. **Who may define.** Any holder of a namespace reference can send it
   `bind:`. That is a capability question (Newspeak has no ambient authority
   [N1]); not restricted for now.
5. **Transactions.** Definition messages sent inside `atomic:`: part of the
   transaction, or rejected? And the lazy-migration commit point
   (§1.7).
6. **Root handle** (R1/R2/R3) and **namespace creation** form (Q17).
7. **Dynamic scope.** `policy` may not want to be global at all; a dynamically
   scoped setting that travels with messages (as the handler chain and the
   transaction tag already do) is a separate thread [N6].

---

## 9. Revised direction (2026-10-26)

None of the §5 variants felt right (user). The diagnosis: namespaces were being
given two jobs, organizing *definitions* (code: versioned, redefinable) and
holding *runtime data* (settings, singletons, constants). Every problem in §5
came from the second job:

- **Encapsulation.** A binding is read by name, not by message, so
  `app::logging::policy` is in effect a public ivar of the namespace object.
- **Reload.** `bindOnce:`/`defonce` exists only because data sat in a container
  whose contents are re-evaluated on reload. Code wants re-evaluation on reload;
  data wants to survive it.
- **Mixed references.** Inside `logging do: [...]` the bindings still had to
  name `logging` explicitly.
- **A second kind of variable.** A binding that is changed by redefinition is a
  global variable with extra steps. With locals single-assignment, the only
  mutable named slots left in Smol should be an entity's ivars, changed by its
  own methods.

### 9.1 Decided (user, 2026-10-26)

1. **Namespaces hold definitions only** (classes, traits, child namespaces).
   Data stays encapsulated in objects and is reached by messages. This replaces
   §1.5 ("a binding names a value") and retires the binding variants A–C and E
   of §5. §1.1–§1.3 and §1.6–§1.8 stand.
2. **Classes are immutable, versioned values with an implicit metaclass.**
   `Foo class` answers the metaclass, which is inspectable. There are no class
   variables and no class-instance variables (§1.4 without its binding clause).
3. **Constants are class-side methods.** No `once` construct and no constant
   sugar:
   - a literal answer is constant-folded by the compiler;
   - a constructed answer *may* be cached by the VM as an optimization when it
     observes at run time that an evaluation had no effects (no ivar write, no
     entity send or spawn, no dynamic-variable read, no effectful primitive)
     and answered a Value. The cache is guarded by the version tags of the
     classes and methods the evaluation used (VD6), so redefinition
     invalidates it, as in incremental-computation systems [N7][N8];
   - semantics never depend on caching. Anything that must run exactly once
     (it has effects or creates an entity) is state and belongs in an entity.
   - Precondition: caching must not be observable. See open item 1.
4. **Naming case:** lowercase namespaces, CamelCase classes
   (`app::logging::Logger`). Recorded as input to the naming-conventions pass.
5. **Redefinition:** lazy migration at the entity's next dispatch with an opt-in
   hook (§1.7). No "stay on the old version" option.

What becomes of the §5 example under this direction:

- `levels` → a class-side method (`Logger class >> levels [ ^#(...) ]`), or,
  if sum types land, a `Level` family, which is a definition (Q20).
- `policy` → not on a class and not in a namespace. Either an ivar of a
  `Logger` instance that is passed to its users, or a dynamic variable when it
  is ambient configuration for a scope of execution (Q22).
- A truly global named service, if one is ever needed, is a runtime registry
  (an ordinary entity), separate from code namespaces, as Erlang separates
  modules from registered process names. Deferred until a real case appears.
- Parameterized modules (Newspeak) are not needed: their main gain, the same
  code under two configurations at once, comes from configuration living in
  globals. With configuration in ivars or the dynamic context, it does not.

### 9.2 Still open

| # | Item | Home |
|---|---|---|
| 1 | Value `==`: (a) identity of Values is unobservable and `==` on Values compares state, as Java value objects do [N9]; or (b) identity of Values is unspecified. Leaning (a). Needed before the VM may cache constructed constants. | `vm_compiler_plan.md` §3.1 |
| 2 | Root handle (R1/R2/R3, §4; leaning R2), namespace creation form, single `Namespace` class (leaning yes, §3) | Q17, this doc |
| 3 | One rule by which a sender sees its own definition messages (`subclass:`, method definition, `namespace:`), now that a namespace is an entity | §8.1 |
| 4 | Declarative entry (`in:`, §5 variant D) versus `do:` blocks | this doc |
| 5 | Redefinition details: migrating Values, the transaction commit point of a lazy migration, new versions of subclasses | Q4 |
| 6 | `levels`-style enumerations as sum types | Q20 |
| 7 | Dynamic variables and aspects; `Warning`'s logging policy is the first case | Q22 |
| 8 | Naming and style conventions | to be written |
| 9 | Who may send definition messages (capabilities) | §8.4 |

### 9.3 Migration work this implies (future milestone)

- Remove `InstanceKind::ClassEntity` and `dispatch_class`; class-side methods
  run in the caller's process. `new` on an entity class still answers a Future,
  because `initialize` is sent to the new instance, not because of a hop through
  the class.
- Remove `Class.class_var_storage` and `classVariableNames:`.
- `Warning`'s `LoggingPolicy` (`kernel.smol:838`) becomes a dynamic variable or
  an instance setting (Q22), and `apply_unhandled_policy`
  (`builtins_iterative.rs:2669`) stops reading class-variable storage from Rust.
- Reify namespaces as entities with snapshots, replacing the `RwLock` maps in
  `namespace.rs`.
- Implicit metaclasses replace the separate `class_methods` table and
  `lookup_class_method` path, so a class-side send is an ordinary send.

---

## References

- [N1] Bracha et al., "Modules as Objects in Newspeak", ECOOP 2010.
- [N2] Common Lisp HyperSpec, `defparameter`, `defvar`.
- [N3] Clojure reference, "Namespaces" and "Vars and the Global Environment"
  (`def`, `defonce`, `binding`, `*ns*`).
- [N4] Ruby documentation, constant lookup (`Module#const_get`, `Module.nesting`).
- [N5] C# language reference, "File-scoped namespace declaration" (C# 10).
- [N6] JEP 506 "Scoped Values"; Racket Guide §4.13 "Dynamic Binding: parameterize".
- [N7] Hammer et al., "Adapton: Composable, Demand-Driven Incremental
  Computation", PLDI 2014.
- [N8] Salsa (the incremental framework behind rust-analyzer), salsa-rs.github.io.
- [N9] JEP 401, "Value Classes and Objects (Preview)": `==` on value objects.
