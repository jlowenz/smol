### Chunk 30 — TCO fix + Bool routing fix

This document records the bugs found during C30 S3-S4 work and the **correct** fixes
required before Bool can be migrated to kernel.smol. It must be resolved before any
other C30 steps proceed.

---

## The two bugs

### Bug 1 — TCO drains Instance binding that blocks need for ivar access

**Location:** `drain_tail_frames` in `eval_iterative.rs`, called from step (1) of
`dispatch_to_stack`.

**Root cause:** Blocks use `method_stack` traversal (not a captured self reference) for
ivar access. `drain_tail_frames` performs TCO by popping the enclosing method
frame — including its `SelfBinding::Instance` push. Any block argument to the
tail-called method then has no Instance binding in the method_stack to look up ivars
from.

**Concrete scenario:**

```smol
Point >> = other [
    self class = other class ifFalse: [^false].
    ^x = other x and: [y = other y]       " <-- tail call "
]
```

`^x = other x and: [y = other y]` is a `Stmt::Return`. After evaluating `x = other x`
→ `Bool(true)`, `dispatch_to_stack(Bool(true), "and:", [block])` is called. The stack
holds:

```
ReturnUnwind { D_eq }
Point>>= CallFrame { depth: D_eq, pending: [] }     ← empty, drained by TCO
```

Step (1) finds `True >> and: aBlock` via `lookup_user_method`, calls
`drain_tail_frames`, which drains the `ReturnUnwind` and the `Point>>=` CallFrame,
calling `ev.pop_self()`. The `SelfBinding::Instance{point}` is gone.

When the block `[y = other y]` later runs, `lookup_var("y")`:
- `method_stack.last()` = `SelfBinding::Primitive { True_class, Bool(true) }`
- No `SelfBinding::Instance` anywhere in the method_stack
- Returns `UndefinedVariable("y")`

**This same TCO bug exists for ANY Primitive method (not just Bool) called in tail
position inside an Instance method where block arguments reference ivars.** The Bool
work just surfaced it because the Bool → True/False Smol routing was the first such
case.

---

### Bug 2 — `invoke_primitive_method` is wrong for Bool (and for any dispatch_to_stack
caller)

**Location:** `dispatch_bool` in `builtins_iterative.rs`, class routing block added
this session.

```rust
// WRONG — this is called from step (8) of dispatch_to_stack, which is
// called from WITHIN step_loop. invoke_primitive_method starts a NEW
// step_loop → nested Rust stack frames → defeats the iterative evaluator.
return ev.invoke_primitive_method(Value::Bool(b), def_class, method, args, env);
```

`dispatch_to_stack` is designed to push continuations onto the **existing** `step_loop`
stack. Calling `invoke_primitive_method` from inside `dispatch_to_stack` creates a
second, nested `step_loop` invocation. Every `and:`, `or:`, `not` in a loop burns an
additional Rust stack frame. This is exactly the problem the iterative evaluator was
built to eliminate.

**Rule:** Nothing reachable from `dispatch_to_stack` may call `invoke_primitive_method`,
`eval_block`, `invoke_method`, or `invoke_class_method` — all of which start new
`step_loop` invocations. They may only manipulate `stack: &mut Vec<Cont>`.

---

## Correct fix — two parts, same chunk

### Part A: Fix TCO (`drain_tail_frames`)

`drain_tail_frames` must not drain a `SelfBinding::Instance` frame when the incoming
call would produce a `SelfBinding::Primitive` binding (i.e. the tail call crosses from
Instance self to a primitive self). The underlying reason is that blocks captured within
the Instance method may reference ivars through the method_stack, and the Instance
binding is their only ivar-lookup anchor.

**Correct fix: blocks must capture `home_self` explicitly.**

Blocks in Smalltalk always have access to `self`. Currently `Block` captures `env` and
`home_depth` but not self. Ivar access goes through the method_stack, which fails after
TCO removes the Instance frame. The fix makes blocks self-sufficient:

#### Step A1 — Add `home_self` to `Block`

In `crates/interpreter/src/value.rs`:

```rust
pub struct Block {
    pub params:      Vec<Arc<str>>,
    pub locals:      Vec<Arc<str>>,
    pub body:        Vec<Stmt>,
    pub env:         Env,
    pub home_depth:  usize,
    pub home_entity: Option<EntityId>,
    /// The instance `self` at the point the block was captured, if any.
    /// Used for ivar/classvar lookup after TCO has removed the enclosing
    /// Instance method frame from the method_stack.
    pub home_self:   Option<InstanceHandle>,
}
```

#### Step A2 — Populate `home_self` when block is captured

In `eval_iterative.rs`, `Cont::EvalExpr { expr: Expr::Block { .. } }`:

```rust
// Find the nearest enclosing Instance binding in method_stack.
// Also check ev.current_block_home_self so that blocks nested inside
// other blocks inherit the home_self even when the outer method frame
// has already been drained.
let home_self = {
    let ms = ev.method_stack.borrow();
    ms.iter().rev().find_map(|b| match b {
        SelfBinding::Instance { handle, .. } => Some(handle.clone()),
        _ => None,
    })
}
.or_else(|| ev.current_block_home_self.get());
```

#### Step A3 — Thread `home_self` through the evaluator

Add a new cell to `Evaluator` (analogous to `current_block_home`):

```rust
pub(crate) current_block_home_self: Cell<Option<InstanceHandle>>,
```

In `push_block_frame`:
- Save the current `current_block_home_self` into the `CallFrame`
- Set `ev.current_block_home_self.set(block.home_self.clone())`

When a block `CallFrame` is popped (in `step_loop` CallFrame exhaustion and in
`unwind_stack_to_depth`): restore the saved value.

#### Step A4 — Use `home_self` in `lookup_var` and `assign_var`

In `eval.rs`, `lookup_var` for `SelfBinding::Primitive` (and None for top-level
block context), after the existing stack walk and class-var check, before falling
through to `env.lookup_outer_frames`:

```rust
// Ivar access for blocks executing after TCO removed the Instance frame.
// Requires: `ev.current_block_home_self` is populated by `push_block_frame`.
if let Some(handle) = ev.current_block_home_self.get() {
    if let Some(v) = handle.ivar_storage().get_slot(name) {
        return Some(v);
    }
    if let Some(owner) = handle.0.class.lookup_class_var_owner(name)
        && let Some(v) = owner.class_var_storage.read().get(name).cloned()
    {
        return Some(v);
    }
}
```

Similarly in `assign_var` for `SelfBinding::Primitive`.

After Part A, TCO is correct for all tail calls including
`Instance → Primitive` (e.g. `Point >> =` calling `True >> and:`).

---

### Part B: Correct Bool routing in `dispatch_to_stack`

With TCO fixed, Bool can be routed through `lookup_user_method` exactly like any other
receiver type. No special path, no `invoke_primitive_method`, no nested step_loop.

#### Step B1 — Restore Bool to `lookup_user_method`

In `builtins_iterative.rs`, `lookup_user_method`:

```rust
Value::Bool(b) => {
    let class_name = if *b { "True" } else { "False" };
    let cls = ev.registry.kernel().lookup_class(class_name)?;
    let (method, def_class) = cls.0.class.lookup(selector)?;
    Some(UserMethodFound::Primitive {
        self_value: Value::Bool(*b),
        method,
        def_class,
    })
}
```

Step (1) of `dispatch_to_stack` finds Bool methods in True/False; `drain_tail_frames`
fires (now safe with Part A); `push_method_call_frame` pushes a Primitive CallFrame
onto the **existing** `step_loop` stack. No nested step_loop.

Step (6) inline handling for `ifTrue:`, `ifFalse:`, etc. is still reached ONLY when
the True/False class does NOT define those selectors — but since it does (from
kernel.smol), step (1) always catches them first. Step (6) becomes dead code and
**should be removed** once Part A is confirmed correct.

#### Step B2 — Remove `invoke_primitive_method` from `dispatch_bool`

Remove the class routing block added in this session from the top of `dispatch_bool` in
`builtins_iterative.rs`. `dispatch_bool` via step (8) is now only reached for Bool
selectors that are NOT defined in True/False (e.g. `=`, `~=`, `hash`, `&`, `|`,
`_primHash`). Leave those Rust arms intact.

#### Step B3 — Remove `class` and `isKindOf:` Rust arms from `dispatch_bool`

These currently live in `dispatch_bool`. After the True/False class routing via step (1)
in `dispatch_to_stack` handles them through kernel-defined methods (or inherits
`Object >> class` / `Object >> isKindOf:`), the Rust arms become dead code. Remove.

Verify with:
```rust
assert_eq!(eval("true class name"), Value::Str(Arc::from("True")));
assert_eq!(eval("true isKindOf: Boolean"), Value::Bool(true));
assert_eq!(eval("true isKindOf: True"),    Value::Bool(true));
assert_eq!(eval("true isKindOf: False"),   Value::Bool(false));
```

---

## Current broken state (as of this session)

The following changes are in the working tree and must be revised:

| File | Current (wrong) state | What to change |
|---|---|---|
| `builtins_iterative.rs` | `lookup_user_method` omits Bool (reverted to avoid drain bug) | Restore Bool arm after Part A |
| `builtins_iterative.rs` | `dispatch_bool` has `invoke_primitive_method` class routing | Remove entirely |
| `eval.rs` | Ivar fallthrough added to `SelfBinding::Primitive` in `lookup_var`/`assign_var` — walks method_stack | Replace with `current_block_home_self` mechanism from Part A |
| `tests/entity.rs` | `entity_stack_depth_probe` uses `acc get await` | Keep — this is correct |

The `eval.rs` ivar fallthrough (stack walk in `SelfBinding::Primitive`) was an attempt
to fix the symptom without fixing TCO. It partially works for non-tail-call cases but
fails when TCO drains the Instance frame. Part A replaces it with the correct fix.

---

## Order of work

1. **Part A** (TCO fix) — add `home_self` to Block, populate, thread through evaluator,
   use in `lookup_var`/`assign_var`. Run full test suite.

2. **Part B** (Bool routing) — restore Bool to `lookup_user_method`, remove
   `invoke_primitive_method` from `dispatch_bool`, remove now-dead step (6) inline
   Bool arms and Rust `class`/`isKindOf:` arms from `dispatch_bool`. Run full test
   suite.

3. Continue with remaining C30 steps (stack profiling, stress test, CLAUDE.md update).

---

## Test coverage to add alongside Part A

```rust
// Ivar access in block argument to Bool method in tail position of instance method.
#[test]
fn ivar_access_in_and_block_tail_position() {
    let src = r#"
        Object subclass: #P instanceVariableNames: 'x y'.
        P class >> x: anX y: aY [ ^self new pvtInit: anX y: aY ]
        P >> pvtInit: anX y: aY [ x := anX. y := aY ]
        P >> = other [
            self class = other class ifFalse: [^false].
            ^x = other x and: [y = other y]
        ]
        (P x: 1 y: 2) = (P x: 1 y: 2)
    "#;
    assert_eq!(eval(src), Value::Bool(true));
}

// Ivar write in block argument to timesRepeat: in tail position of instance method.
#[test]
fn ivar_assign_in_block_tail_position() {
    let src = r#"
        Object subclass: #Counter instanceVariableNames: 'n'.
        Counter class >> new [ ^super new init ]
        Counter >> init [ n := 0 ]
        Counter >> run [ ^3 timesRepeat: [n := n + 1] ]
        Counter >> n [ ^n ]
        | c | c := Counter new. c run. c n
    "#;
    assert_eq!(eval(src), Value::Int(3));
}
```
