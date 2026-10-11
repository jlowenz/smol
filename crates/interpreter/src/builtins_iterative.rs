use std::collections::HashMap;
use std::sync::Arc;

/// Cooperative yield interval: one yield every N completed message dispatches.
/// Matches the step-loop constant in eval_iterative so loop primitives and the
/// iterative evaluator yield at the same granularity.
use num_bigint::BigInt;
use num_traits::{FromPrimitive, Zero};
use parking_lot::RwLock;

use crate::class::{Class, IndexedSlotKind};
use crate::entity::FrameCommitOutcome;
use crate::entity::{
    EntityId, FutureHandle, InstanceHandle, InstanceKind, InstanceState, send_to_entity,
    spawn_class_entity, spawn_entity_bare,
};
use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
use crate::namespace::Namespace;
use crate::object::{IndexedStorage, Object};
use crate::shared_tx::{GenTx, TxMode};
use crate::value::{Block, Value};
use triple_store::exec_context::ExecContext;
use triple_store::fact::{Fact, StoredFact};
use triple_store::term::Term;

/// Entry point for all built-in method dispatch.
pub fn dispatch(
    ev: &Evaluator,
    recv: Value,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // `yourself` is universal — every value returns itself.
    if selector.as_ref() == "yourself" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(recv);
    }
    // `await` is universal — blocks until a Future resolves; no-op on non-futures.
    if selector.as_ref() == "await" {
        expect_arity(&selector, 0, args.len())?;
        return force_value(ev, recv);
    }
    // `isNil` / `notNil` are universal.
    if selector.as_ref() == "isNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(matches!(force_value(ev, recv)?, Value::Nil)));
    }
    if selector.as_ref() == "notNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(!matches!(force_value(ev, recv)?, Value::Nil)));
    }
    // C31 S5c: deadlock-detection timeout (ms). Universal so `Transaction class`
    // sends reach it regardless of class-side vs instance dispatch. Stored in a VM
    // global read by the entity park loop.
    if selector.as_ref() == "_primSetDeadlockTimeoutMs:" {
        expect_arity(&selector, 1, args.len())?;
        let ms = as_int(ev, args.into_iter().next().unwrap(), &selector)?.max(0) as u64;
        crate::entity::set_deadlock_timeout_ms(ms);
        return Ok(recv);
    }
    if selector.as_ref() == "_primDeadlockTimeoutMs" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Int(crate::entity::deadlock_timeout_ms() as i64));
    }
    // `_primHash` is universal — handled here so all value kinds are covered.
    if selector.as_ref() == "_primHash" {
        expect_arity(&selector, 0, args.len())?;
        let v = force_value(ev, recv)?;
        return Ok(Value::Int(hash_value(&v)));
    }
    // `_primClass` — maps any value to its kernel class entity handle.
    if selector.as_ref() == "_primClass" {
        expect_arity(&selector, 0, args.len())?;
        let recv = force_value(ev, recv)?;
        return value_to_class_handle(ev, &recv)
            .map(Value::Instance)
            .ok_or_else(|| mnu("Object", &selector));
    }
    // `_primIsKindOf:` — true iff receiver's class is aClass or a subclass of it.
    if selector.as_ref() == "_primIsKindOf:" {
        expect_arity(&selector, 1, args.len())?;
        let recv = force_value(ev, recv)?;
        let arg = force_value(ev, args.into_iter().next().unwrap())?;
        let recv_class = match value_to_class_handle(ev, &recv) {
            Some(h) => h.0.class.clone(),
            None => return Ok(Value::Bool(false)),
        };
        let arg_class = match arg {
            Value::Instance(h) => h.0.class.clone(),
            _ => return Ok(Value::Bool(false)),
        };
        return Ok(Value::Bool(is_kind_of(&recv_class, &arg_class)));
    }
    // `respondsTo:` is universal for non-object types, mirroring `hash` below: scalars
    // do not route through Object method lookup, so without this arm `3 respondsTo:
    // #foo` would MNU even though `Object` is the root class. `Value::Instance` is
    // excluded so `obj_prim_core`'s arm keeps handling entities/value objects.
    // Answers false for a non-Symbol argument and for a receiver with no kernel class.
    // Required by the numeric tower's coercion edge, which asks an arbitrary operand
    // whether it implements the receiver's adapt selector (plan §4g).
    //
    // KNOWN GAP (pre-existing, shared with the `obj_prim_core` arm this mirrors):
    // resolution is by class-dictionary lookup, so only Smol-defined methods are
    // visible — builtin selectors are Rust match arms with no enumerable registry, and
    // `'s' respondsTo: #size` answers false even though `'s' size` works. Sound for the
    // coercion edge (adapt selectors are always Smol-defined) but not in general.
    // See `docs/open_questions.md` Q14.
    if selector.as_ref() == "respondsTo:" && !matches!(recv, Value::Instance(_)) {
        expect_arity(&selector, 1, args.len())?;
        let recv = force_value(ev, recv)?;
        let arg = force_value(ev, args.into_iter().next().unwrap())?;
        let Value::Symbol(s) = arg else {
            return Ok(Value::Bool(false));
        };
        return Ok(Value::Bool(match value_to_class_handle(ev, &recv) {
            Some(h) => h.0.class.lookup(&s).is_some(),
            None => false,
        }));
    }
    // `hash` is universal for non-object types (primitive values don't route through
    // Object method lookup). For Value::Instance with Entity/ValueObject kind, the
    // type-specific dispatch runs instead to find any user-defined `hash` override.
    // Value::Future is excluded so the transparent-future arm below resolves the
    // future first; if it resolves to an Instance, hash is re-dispatched through
    // dispatch_entity so user-defined overrides are found.
    if selector.as_ref() == "hash" && !matches!(recv, Value::Instance(_) | Value::Future(_)) {
        expect_arity(&selector, 0, args.len())?;
        let v = force_value(ev, recv)?;
        return Ok(Value::Int(hash_value(&v)));
    }
    // `->` is universal — any receiver can create an Association.
    // Implemented in Rust so primitive values (String, Int, …) work without an
    // Object-inherited method dispatch path.
    if selector.as_ref() == "->" {
        expect_arity(&selector, 1, args.len())?;
        let value = args.into_iter().next().unwrap();
        let assoc_class = ev
            .registry
            .kernel()
            .lookup_class("Association")
            .ok_or_else(|| {
                EvalError::TypeError("'->': Association not yet defined in kernel".into())
            })?;
        return crate::builtins::dispatch(
            ev,
            Value::Instance(assoc_class),
            Arc::from("key:value:"),
            vec![recv, value],
            env,
        );
    }
    // `then:orElse:` — chains a block after this value resolves, with an explicit
    // rejection arm (bare `then:` was removed: it did not cover the reject outcome).
    if selector.as_ref() == "then:orElse:" {
        expect_arity(&selector, 2, args.len())?;
        let mut it = args.into_iter();
        let then_val = it.next().unwrap();
        let else_val = it.next().unwrap();
        return dispatch_then_or_else(ev, recv, then_val, else_val, &selector, env);
    }

    match recv {
        Value::Int(n) => dispatch_int(ev, n, &selector, args, env),
        Value::Float(f) => dispatch_float(ev, f, &selector, args, env),
        Value::Bool(b) => dispatch_primitive_fallback(ev, Value::Bool(b), "Bool", &selector, args),
        Value::Nil => dispatch_primitive_fallback(ev, Value::Nil, "Nil", &selector, args),
        Value::Str(s) => dispatch_str(ev, s, &selector, args, env),
        Value::Symbol(s) => {
            dispatch_primitive_fallback(ev, Value::Symbol(s), "Symbol", &selector, args)
        }
        Value::Block(blk) => dispatch_block(ev, blk, &selector, args, env),
        Value::LargeInt(n) => dispatch_largeint(ev, n, &selector, args, env),
        Value::Instance(handle) => match &handle.0.kind {
            InstanceKind::ValueObject(_) => dispatch_object_value(ev, handle, selector, args, env),
            InstanceKind::Entity(_) => dispatch_entity(ev, handle, selector, args, env),
            InstanceKind::ClassEntity(_) => dispatch_class(ev, handle, selector, args, env),
        },
        Value::Future(_) => unreachable!(
            "dispatch() called with unresolved Future; dispatch_to_stack forces futures before \
             calling dispatch(), and then: intercepts Futures before force_value"
        ),
        Value::Namespace(ns) => dispatch_namespace(ev, ns, selector, args, env),
        Value::Handle(_) => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Handle",
            selector,
        }),
    }
}

/// Run the rejection arm of `then:orElse:` in the continuation coroutine, settling
/// `result_reply` accordingly. `else_val` is either a 1-arg Block (invoked with the
/// carried condition) or one of the symbol idioms:
///   - `#signal`  — re-raise the condition in this (replayed) handler chain; a
///     handler's value resolves the result, an unhandled re-raise rejects it.
///   - `#reject`  — propagate the rejection down the chain unchanged.
///   - `#value`   — resolve the result with the condition as an ordinary value.
///
/// See `docs/enrollment_provenance_design.md` (Stage 1: async errors → conditions).
fn run_reject_arm(ev: &Evaluator, else_val: Value, err: EvalError, result_reply: &FutureHandle) {
    let cond = match &err {
        EvalError::Signal(c) => c.clone(),
        // Control-flow variants never reach a reject arm; synthesize defensively.
        _ => make_error_value(&ev.registry, &format!("{err}")),
    };
    let settle = |r: Result<Value, EvalError>, reply: &FutureHandle| match r {
        Ok(v) => match force_value(ev, v) {
            Ok(f) => reply.resolve_ok(f),
            Err(e) => reply.resolve_err(e),
        },
        // `^`-return and a handler's `return:` both mean "this (discarded) chain
        // result is v" — a clean completion, not a rejection.
        Err(EvalError::Return(v, _)) | Err(EvalError::ReturnFromBlock(v)) => reply.resolve_ok(v),
        Err(e) => reply.resolve_err(e),
    };
    match else_val {
        Value::Symbol(s) if &*s == "reject" => result_reply.resolve_err(err),
        Value::Symbol(s) if &*s == "value" => result_reply.resolve_ok(cond),
        Value::Symbol(s) if &*s == "signal" => {
            let mut env = Env::new();
            settle(signal_condition(ev, cond, &mut env), result_reply);
        }
        Value::Block(b) => {
            let mut env = Env::new();
            settle(ev.eval_block(&b, vec![cond], &mut env), result_reply);
        }
        // Unknown symbol / non-block else arg: fail loudly rather than silently drop.
        other => result_reply.resolve_err(EvalError::TypeError(format!(
            "then:orElse: expects a 1-arg block or #signal/#reject/#value, got {}",
            other.kind()
        ))),
    }
}

/// `then:orElse:` — total async chaining: run `then_blk` on resolution, `else_val`
/// on rejection (a 1-arg block or a `#signal`/`#reject`/`#value` symbol). Returns a
/// composable future. Mirrors `dispatch_then`'s receiver handling; the else arm
/// only fires on a genuine (async) future rejection.
#[inline(never)]
fn dispatch_then_or_else(
    ev: &Evaluator,
    recv: Value,
    then_val: Value,
    else_val: Value,
    selector: &Arc<str>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match recv {
        Value::Future(future) => {
            let blk = as_block(ev, then_val, selector)?;
            let result_future = crate::entity::FutureHandle::new();
            let result_reply = result_future.clone();
            let ctx = ev.fork_context();
            crate::entity::init_coroutine_stack();
            may::go!(move || {
                let then_ev = ctx.into_evaluator();
                match future.wait() {
                    Err(e) => run_reject_arm(&then_ev, else_val, e, &result_reply),
                    Ok(v) => {
                        let mut dummy_env = crate::env::Env::new();
                        match then_ev.eval_block(&blk, vec![v], &mut dummy_env) {
                            Ok(v) => match force_value(&then_ev, v) {
                                Ok(forced) => result_reply.resolve_ok(forced),
                                Err(e) => result_reply.resolve_err(e),
                            },
                            Err(EvalError::Return(v, _)) => result_reply.resolve_ok(v),
                            Err(e) => result_reply.resolve_err(e),
                        }
                    }
                }
            });
            Ok(Value::Future(result_future))
        }
        Value::Block(recv_blk) => {
            let then_blk = as_block(ev, then_val, selector)?;
            let result = ev.eval_block(&recv_blk, vec![], env)?;
            match result {
                Value::Future(future) => {
                    let result_future = crate::entity::FutureHandle::new();
                    let result_reply = result_future.clone();
                    let ctx = ev.fork_context();
                    crate::entity::init_coroutine_stack();
                    may::go!(move || {
                        let then_ev = ctx.into_evaluator();
                        match future.wait() {
                            Err(e) => run_reject_arm(&then_ev, else_val, e, &result_reply),
                            Ok(v) => {
                                let mut dummy_env = crate::env::Env::new();
                                match then_ev.eval_block(&then_blk, vec![v], &mut dummy_env) {
                                    Ok(v) => match force_value(&then_ev, v) {
                                        Ok(forced) => result_reply.resolve_ok(forced),
                                        Err(e) => result_reply.resolve_err(e),
                                    },
                                    Err(EvalError::Return(v, _)) => result_reply.resolve_ok(v),
                                    Err(e) => result_reply.resolve_err(e),
                                }
                            }
                        }
                    });
                    Ok(Value::Future(result_future))
                }
                // Receiver already resolved (no rejection possible): run then-block.
                other => ev.eval_block(&then_blk, vec![other], env),
            }
        }
        // Already-resolved value: run then-block; else arm cannot fire.
        already_resolved => {
            let blk = as_block(ev, then_val, selector)?;
            ev.eval_block(&blk, vec![already_resolved], env)
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Result of a user-method lookup for the iterative evaluator's step loop.
///
/// When `Some`, the step loop should push a `CallFrame` instead of calling
/// `builtins::dispatch`. When `None`, the dispatch falls through to builtins.
pub(crate) enum UserMethodFound {
    /// Instance or entity self-send: invoke `method` with `handle` as receiver.
    Instance {
        handle: InstanceHandle,
        method: Arc<crate::class::Method>,
        def_class: Arc<crate::class::Class>,
    },
    /// Primitive receiver (e.g. nil): invoke `method` with `self_value` as receiver.
    Primitive {
        self_value: Value,
        method: Arc<crate::class::Method>,
        def_class: Arc<crate::class::Class>,
    },
}

/// Look up a user-defined method that the step loop should dispatch as a `CallFrame`.
///
/// Requires: `ev` is the current evaluator.
/// Ensures: returns `Some` only for synchronous user-method calls — ValueObject
///   instance methods, entity self-sends, and nil methods. Cross-entity sends and
///   class-entity dispatch return `None` (handled by `builtins::dispatch`).
pub(crate) fn lookup_user_method(
    ev: &Evaluator,
    recv: &Value,
    selector: &Arc<str>,
) -> Option<UserMethodFound> {
    match recv {
        Value::Instance(handle) => match &handle.0.kind {
            InstanceKind::ValueObject(_) => {
                let (method, def_class) = handle.0.class.lookup(selector)?;
                Some(UserMethodFound::Instance {
                    handle: handle.clone(),
                    method,
                    def_class,
                })
            }
            InstanceKind::Entity(d) => {
                // Only intercept self-sends; cross-entity sends stay async via builtins.
                if ev.current_entity_id.get() != Some(d.id) {
                    return None;
                }
                let (method, def_class) = handle.0.class.lookup(selector)?;
                Some(UserMethodFound::Instance {
                    handle: handle.clone(),
                    method,
                    def_class,
                })
            }
            InstanceKind::ClassEntity(_) => None, // class-side dispatch handled by builtins
        },
        Value::Nil => {
            let nil_handle = ev.registry.kernel().lookup_class("Nil")?;
            let (method, def_class) = nil_handle.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Nil,
                method,
                def_class,
            })
        }
        Value::Block(blk) => {
            let blk_cls = ev.registry.kernel().lookup_class("Block")?;
            let (method, def_class) = blk_cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Block(blk.clone()),
                method,
                def_class,
            })
        }
        Value::Int(n) => {
            let cls = ev.registry.kernel().lookup_class("MachineInteger")?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Int(*n),
                method,
                def_class,
            })
        }
        Value::LargeInt(n) => {
            let cls = ev.registry.kernel().lookup_class("ArbitraryInteger")?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::LargeInt(n.clone()),
                method,
                def_class,
            })
        }
        Value::Float(f) => {
            let cls = ev.registry.kernel().lookup_class("Float")?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Float(*f),
                method,
                def_class,
            })
        }
        Value::Str(s) => {
            let cls = ev.registry.kernel().lookup_class("String")?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Str(s.clone()),
                method,
                def_class,
            })
        }
        Value::Symbol(s) => {
            let cls = ev.registry.kernel().lookup_class("Symbol")?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Symbol(s.clone()),
                method,
                def_class,
            })
        }
        Value::Bool(b) => {
            let cls = ev
                .registry
                .kernel()
                .lookup_class(if *b { "True" } else { "False" })?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Bool(*b),
                method,
                def_class,
            })
        }
        _ => None, // Future, Namespace, Handle
    }
}

/// The fixed set of VM-encoded value-class names — classes whose instances are a
/// native `Value` encoding (docs/numeric_tower_plan.md §4b). These are defined via
/// `vmSubclass:` and are **closed**: `subclass:` on them is rejected. The set mirrors
/// the encodings `value_to_class_handle` produces for scalar `Value` variants.
pub(crate) fn is_encoded_leaf_name(name: &str) -> bool {
    matches!(
        name,
        "MachineInteger" | "ArbitraryInteger" | "Float" | "String" | "Symbol" | "True" | "False"
    )
}

fn mnu(recv_kind: &'static str, selector: &Arc<str>) -> EvalError {
    EvalError::MessageNotUnderstood {
        receiver_kind: recv_kind,
        selector: selector.clone(),
    }
}

pub(crate) fn expect_arity(
    _selector: &Arc<str>,
    expected: usize,
    got: usize,
) -> Result<(), EvalError> {
    if got != expected {
        Err(EvalError::ArityMismatch { expected, got })
    } else {
        Ok(())
    }
}

/// Resolves a `Value::Future` to its concrete value at the point it is needed.
///
/// Requires: called from a may coroutine so that `FutureHandle::wait()` parks
///   cooperatively rather than blocking the OS thread.
/// Ensures: returns the first non-Future value reachable by following the chain,
///   or propagates any error from the future.
/// Resolve `v` to a non-future value, blocking on any pending future.
///
/// A rejected future's condition flows through the ONE error channel — the
/// condition system. On rejection we `signal_condition` the carried condition at
/// this (live) await site, so a `HandlerContext` handler in the current/replayed
/// chain can handle it (`resume:` the await with a substitute value, `retry`, or
/// reject/unwind). `signal_condition` re-raises unhandled `Error`s, preserving the
/// prior unwinding behavior when no handler matches. A `resume:` value may itself
/// be a future, so it is re-forced. See `docs/enrollment_provenance_design.md`
/// (Stage 1: async errors through the condition system).
///
/// Non-condition errors (control-flow variants; not-yet-promoted raw errors)
/// propagate unchanged — Stage 2 routes genuine raw errors through here too.
pub(crate) fn force_value(ev: &Evaluator, v: Value) -> Result<Value, EvalError> {
    match v {
        Value::Future(f) => match f.wait() {
            Ok(inner) => force_value(ev, inner),
            Err(EvalError::Signal(cond)) => {
                let mut env = Env::new();
                let handled = signal_condition(ev, cond, &mut env)?;
                force_value(ev, handled)
            }
            Err(e) => Err(e),
        },
        v => Ok(v),
    }
}

pub(crate) fn as_int(ev: &Evaluator, v: Value, selector: &Arc<str>) -> Result<i64, EvalError> {
    match force_value(ev, v)? {
        Value::Int(n) => Ok(n),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be Int"
        ))),
    }
}

/// Coerce a value to an opaque OS handle (from `Value::Handle`), forcing futures.
pub(crate) fn as_handle(
    ev: &Evaluator,
    v: Value,
    selector: &Arc<str>,
) -> Result<crate::os_interface::OsHandle, EvalError> {
    match force_value(ev, v)? {
        Value::Handle(h) => Ok(h),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be an OS Handle"
        ))),
    }
}

pub(crate) fn as_block(
    ev: &Evaluator,
    v: Value,
    selector: &Arc<str>,
) -> Result<Arc<Block>, EvalError> {
    match force_value(ev, v)? {
        Value::Block(b) => Ok(b),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be a Block"
        ))),
    }
}

/// Collect a nil-terminated `Cons` list into a `Vec`, head-to-tail.
///
/// Used by `perform:withArguments:` to unpack the argument list. The spine is
/// forced (to distinguish `Cons` from the terminating `nil`), but each `head`
/// element is left unforced so a `Value::Future` argument stays transparent.
///
/// Requires: `list` is `Value::Nil` or a chain of `Cons` ValueObjects.
/// Ensures: returns elements in list order; a node that is neither `nil` nor a
/// `Cons` is a `TypeError`. Terminating the walk silently on such a node (the
/// earlier behavior) turned `perform: #foo withArguments: anArray` into a
/// zero-argument send, surfacing as a confusing arity or coercion failure far from
/// the actual mistake.
pub(crate) fn cons_to_vec(ev: &Evaluator, list: Value) -> Result<Vec<Value>, EvalError> {
    let mut out = Vec::new();
    let mut node = force_value(ev, list)?;
    loop {
        match node {
            Value::Nil => return Ok(out),
            Value::Instance(ref h) => {
                let obj = match &h.0.kind {
                    InstanceKind::ValueObject(o) if &*h.0.class.name == "Cons" => o.clone(),
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "withArguments: expects a Cons list or nil, got {}",
                            h.0.class.name
                        )));
                    }
                };
                out.push(obj.get_slot("head").unwrap_or(Value::Nil));
                node = force_value(ev, obj.get_slot("tail").unwrap_or(Value::Nil))?;
            }
            other => {
                return Err(EvalError::TypeError(format!(
                    "withArguments: expects a Cons list or nil, got {}",
                    other.kind()
                )));
            }
        }
    }
}

fn normalize_bigint(b: BigInt) -> Value {
    use num_traits::ToPrimitive;
    if let Some(n) = b.to_i64() {
        Value::Int(n)
    } else {
        Value::LargeInt(Arc::new(b))
    }
}

/// Widen a numeric leaf (`Int`/`LargeInt`/`Float`) to `f64` for a mixed comparison.
///
/// Requires: `v` is a numeric leaf. Ensures: the `f64` nearest `v` (`LargeInt`
/// out of `f64` range saturates to ±∞, preserving order at the extremes).
fn leaf_to_f64(v: &Value, sel: &Arc<str>) -> Result<f64, EvalError> {
    use num_traits::ToPrimitive;
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::LargeInt(b) => Ok(b
            .to_f64()
            .unwrap_or(if b.sign() == num_bigint::Sign::Minus {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            })),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: operand must be numeric"
        ))),
    }
}

/// Widen an integer leaf (`Int`/`LargeInt`) to `BigInt` for a mixed integer compare.
///
/// Requires: `v` is `Int` or `LargeInt` (Float is handled on the f64 path before
/// this is called). Ensures: the exact `BigInt` value.
fn leaf_to_bigint(v: &Value, sel: &Arc<str>) -> Result<BigInt, EvalError> {
    match v {
        Value::Int(n) => Ok(BigInt::from(*n)),
        Value::LargeInt(b) => Ok((**b).clone()),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: operand must be integer"
        ))),
    }
}

/// Numeric equality over the leaf encodings, using the SAME promotion policy as
/// [`numeric_lt`] so that `<`, `=` and `>` stay mutually consistent: any `Float`
/// operand ⇒ `f64`; else any `LargeInt` ⇒ `BigInt`; else `i64`. This is the single
/// Rust grounding the tower's `=` routes to via `_primEq:`
/// (docs/numeric_tower_plan.md §4g/S6), making `2 = 2.0` true and satisfying the
/// `Ordered` trait's requirement that `=` agree with the order `<` induces.
///
/// Unlike `<`, equality is **total**: a non-numeric operand is not an error, it is
/// simply not equal. The Smol side already declines non-`Number` arguments before
/// reaching here, so this arm covers a hand-sent primitive.
///
/// Requires: nothing. Ensures: `true` iff both operands are numeric leaves equal
/// under the promoted comparison; `false` otherwise. Never errors.
fn numeric_eq(ev: &Evaluator, a: Value, b: Value, sel: &Arc<str>) -> Result<bool, EvalError> {
    let a = force_value(ev, a)?;
    let b = force_value(ev, b)?;
    Ok(match (&a, &b) {
        (Value::Float(_), _) | (_, Value::Float(_)) => {
            match (leaf_to_f64(&a, sel), leaf_to_f64(&b, sel)) {
                (Ok(x), Ok(y)) => x == y,
                _ => false,
            }
        }
        (Value::LargeInt(_), _) | (_, Value::LargeInt(_)) => {
            match (leaf_to_bigint(&a, sel), leaf_to_bigint(&b, sel)) {
                (Ok(x), Ok(y)) => x == y,
                _ => false,
            }
        }
        (Value::Int(x), Value::Int(y)) => x == y,
        _ => false,
    })
}

/// Total-order `<` over the numeric leaf encodings, promoting as needed: any `Float`
/// operand ⇒ `f64` compare; else any `LargeInt` ⇒ `BigInt` compare; else `i64`. This
/// is the single Rust grounding the tower's `<` routes to via `_primLt:`
/// (docs/numeric_tower_plan.md §2a). Cross-type numeric comparison (`2 < 3.5`) is
/// fixed entirely here; user structured numbers override `<` and never reach it.
///
/// Requires: `a`, `b` force to numeric leaves. Ensures: `a < b` under the promoted
/// comparison, or a `TypeError` when an operand is non-numeric.
fn numeric_lt(ev: &Evaluator, a: Value, b: Value, sel: &Arc<str>) -> Result<bool, EvalError> {
    let a = force_value(ev, a)?;
    let b = force_value(ev, b)?;
    match (&a, &b) {
        (Value::Float(_), _) | (_, Value::Float(_)) => {
            Ok(leaf_to_f64(&a, sel)? < leaf_to_f64(&b, sel)?)
        }
        (Value::LargeInt(_), _) | (_, Value::LargeInt(_)) => {
            Ok(leaf_to_bigint(&a, sel)? < leaf_to_bigint(&b, sel)?)
        }
        (Value::Int(x), Value::Int(y)) => Ok(x < y),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: operands must be numeric"
        ))),
    }
}

/// The arithmetic operations grounded by the tower's `_prim…` selectors (S3).
///
/// One variant per public operator; the promotion policy is shared
/// (`numeric_binop`) rather than written per representation, mirroring the
/// single-selector `_primLt:` precedent from S2.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumOp {
    Add,
    Sub,
    Mul,
    /// `/` — truncating division between integer leaves, true division once a
    /// `Float` is involved.
    Div,
    /// `//` — floor division. Integer leaves only.
    Quo,
    /// `\\` — modulo with the sign of the divisor. Integer leaves only.
    Rem,
}

/// Arithmetic over the numeric leaf encodings with the tower's promotion policy:
/// any `Float` operand ⇒ `f64`; else any `LargeInt` ⇒ `BigInt`; else `i64` with
/// overflow promoting to `BigInt`. Results demote back to `Int` whenever they fit
/// a machine word (`normalize_bigint`), so representation is chosen by magnitude
/// rather than by operand history.
///
/// This is the single Rust grounding the tower's `+ - * / // \\` route to via
/// `_primAdd:` and friends (docs/numeric_tower_plan.md §4). Cross-type arithmetic
/// (`2 + 3.5`) is fixed entirely here — the Smol `adaptTo…andSend:` double
/// dispatch is reached only for operands that are *not* encoded numbers.
///
/// Requires: `a` and `b` force to numeric leaves; `op` is `Quo`/`Rem` only when
/// both are integer leaves. Ensures: the promoted result, or a `TypeError` for a
/// non-numeric operand, a `Float` operand to `//`/`\\`, or division by zero.
fn numeric_binop(
    ev: &Evaluator,
    op: NumOp,
    a: Value,
    b: Value,
    sel: &Arc<str>,
) -> Result<Value, EvalError> {
    let a = force_value(ev, a)?;
    let b = force_value(ev, b)?;

    if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
        let (x, y) = (leaf_to_f64(&a, sel)?, leaf_to_f64(&b, sel)?);
        return match op {
            NumOp::Add => Ok(Value::Float(x + y)),
            NumOp::Sub => Ok(Value::Float(x - y)),
            NumOp::Mul => Ok(Value::Float(x * y)),
            NumOp::Div => Ok(Value::Float(x / y)),
            // `//` and `\\` are integer protocol; Float never reaches them from
            // the kernel (they are defined on the Integer leaves only), so this
            // is a guard against a hand-sent primitive.
            NumOp::Quo | NumOp::Rem => Err(EvalError::TypeError(format!(
                "{sel}: operands must be integers"
            ))),
        };
    }

    // Both operands are integer leaves. The i64 fast path covers the common case;
    // anything that overflows or involves a LargeInt goes through BigInt.
    if let (Value::Int(x), Value::Int(y)) = (&a, &b) {
        let (x, y) = (*x, *y);
        let fast = match op {
            NumOp::Add => x.checked_add(y),
            NumOp::Sub => x.checked_sub(y),
            NumOp::Mul => x.checked_mul(y),
            NumOp::Div | NumOp::Quo | NumOp::Rem => {
                if y == 0 {
                    return Err(EvalError::TypeError("division by zero".into()));
                }
                // `checked_rem` (not `%`) because `i64::MIN % -1` overflows and
                // would panic in debug; `None` falls through to the BigInt path.
                match (op, x.checked_rem(y)) {
                    (_, None) => None,
                    (NumOp::Div, Some(_)) => x.checked_div(y),
                    (NumOp::Quo, Some(r)) => {
                        let borrow = r != 0 && (r < 0) != (y < 0);
                        x.checked_div(y).map(|q| if borrow { q - 1 } else { q })
                    }
                    (_, Some(r)) => Some(if r != 0 && (r < 0) != (y < 0) {
                        r + y
                    } else {
                        r
                    }),
                }
            }
        };
        if let Some(r) = fast {
            return Ok(Value::Int(r));
        }
    }

    let x = leaf_to_bigint(&a, sel)?;
    let y = leaf_to_bigint(&b, sel)?;
    if matches!(op, NumOp::Div | NumOp::Quo | NumOp::Rem) && y.is_zero() {
        return Err(EvalError::TypeError("division by zero".into()));
    }
    Ok(normalize_bigint(match op {
        NumOp::Add => x + y,
        NumOp::Sub => x - y,
        NumOp::Mul => x * y,
        NumOp::Div => x / y,
        NumOp::Quo => bigint_floor_div(x, &y),
        NumOp::Rem => bigint_floor_rem(x, &y),
    }))
}

/// Maps an arithmetic `_prim…` selector to its operation, or `None` if `sel` is
/// not one of them. Lets each leaf's dispatch table carry a single arm for all
/// six operators.
fn arith_prim_op(sel: &str) -> Option<NumOp> {
    match sel {
        "_primAdd:" => Some(NumOp::Add),
        "_primSub:" => Some(NumOp::Sub),
        "_primMul:" => Some(NumOp::Mul),
        "_primDiv:" => Some(NumOp::Div),
        "_primQuo:" => Some(NumOp::Quo),
        "_primRem:" => Some(NumOp::Rem),
        _ => None,
    }
}

/// Smalltalk `//`: floor division (truncates toward negative infinity).
fn bigint_floor_div(n: BigInt, d: &BigInt) -> BigInt {
    use num_traits::Signed;
    let q = &n / d;
    let r = n % d;
    if !r.is_zero() && r.is_negative() != d.is_negative() {
        q - 1
    } else {
        q
    }
}

/// Smalltalk `\\`: modulo with the sign of the divisor.
fn bigint_floor_rem(n: BigInt, d: &BigInt) -> BigInt {
    use num_traits::Signed;
    let r = &n % d;
    if !r.is_zero() && r.is_negative() != d.is_negative() {
        r + d
    } else {
        r
    }
}

// ── Hashing helpers ───────────────────────────────────────────────────────────

/// MurmurHash3 bit-mixer — avalanches bits to reduce clustering from
/// pointer-aligned addresses.
pub(crate) fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51afd7ed558ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ceb9fe1a85ec53);
    k ^= k >> 33;
    k
}

/// FNV-1a over the UTF-8 bytes of a string.
pub(crate) fn fnv1a_hash(s: &str) -> i64 {
    let mut h: u64 = 14695981039346656037;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h as i64
}

/// Returns a stable hash for any `Value`. Used by `_primHash`.
///
/// - Strings / Symbols: FNV-1a over bytes (content-based, value-equal strings hash equal).
/// - Integers: the value itself (or low 64 bits for LargeInt).
/// - Float: bit-pattern of the `f64`.
/// - Bool / Nil: fixed constants.
/// - Object / Class / Block / Namespace: `fmix64` of the heap pointer (identity hash).
pub(crate) fn hash_value(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        // A whole-valued Float hashes as the integer it equals, because S6 made
        // `2 = 2.0` true and equal values MUST hash equally or `Dictionary` breaks
        // (`pvtSlotFor:` probes by `hash` then confirms by `=`). Non-whole floats keep
        // the bit pattern — no integer can equal them, so no agreement is required.
        // The out-of-i64-range case routes through `hash_bigint` so it agrees with an
        // `ArbitraryInteger` of the same value, not merely with `Int`.
        Value::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 {
                if *f >= -(2f64.powi(63)) && *f < 2f64.powi(63) {
                    *f as i64
                } else {
                    match BigInt::from_f64(*f) {
                        Some(b) => hash_bigint(&b),
                        None => f.to_bits() as i64,
                    }
                }
            } else {
                f.to_bits() as i64
            }
        }
        Value::Str(s) => fnv1a_hash(s),
        Value::Symbol(s) => fnv1a_hash(s),
        Value::Bool(b) => {
            if *b {
                1
            } else {
                0
            }
        }
        Value::Nil => 0,
        Value::LargeInt(n) => hash_bigint(n),
        Value::Instance(h) => fmix64(Arc::as_ptr(&h.0) as usize as u64) as i64,
        // Futures are resolved by force_value before hash_value is called.
        Value::Future(_) => unreachable!("hash_value called on unresolved Future"),
        Value::Block(b) => fmix64(Arc::as_ptr(b) as usize as u64) as i64,
        Value::Namespace(ns) => fnv1a_hash(&ns.name),
        Value::Handle(h) => fmix64(Arc::as_ptr(h) as *const () as usize as u64) as i64,
    }
}

/// Hash a `BigInt` by its low 64 bits, negating for a negative sign.
///
/// Shared by the `LargeInt` and whole-valued-`Float` arms of [`hash_value`] so that
/// numerically equal values hash equally across encodings (S6).
///
/// Requires: nothing. Ensures: equal `BigInt`s hash equally.
fn hash_bigint(n: &BigInt) -> i64 {
    use num_bigint::Sign;
    let (sign, bytes) = n.to_bytes_le();
    let mut h: u64 = 0;
    for (i, &b) in bytes.iter().take(8).enumerate() {
        h |= (b as u64) << (i * 8);
    }
    if sign == Sign::Minus {
        h = h.wrapping_neg();
    }
    h as i64
}

/// Construct a bare `Error` instance without calling kernel methods (`initialize`, `pvtInit`).
///
/// Requires: None — if the `Error` class is unavailable (e.g. pre-bootstrap), returns
///   `Value::Str(message)` as a degenerate fallback.
/// Ensures: returns a `Value::Instance` of class `Error` with `message` ivar set;
///   `restarts` ivar is nil (no restarts — internal signals are not resumable).
pub(crate) fn make_error_value(
    registry: &crate::class_registry::ClassRegistry,
    message: &str,
) -> Value {
    make_condition_value(registry, "Error", message)
}

/// Build a condition instance of `class_name` (a `Condition` subclass) carrying
/// `message`. Falls back to `Error`, then to a bare `Str`, if the class is
/// missing. Used to construct typed conditions (e.g. `Deadlock`) from Rust.
pub(crate) fn make_condition_value(
    registry: &crate::class_registry::ClassRegistry,
    class_name: &str,
    message: &str,
) -> Value {
    let handle = registry
        .kernel()
        .lookup_class(class_name)
        .or_else(|| registry.kernel().lookup_class("Error"));
    let Some(handle) = handle else {
        return Value::Str(Arc::from(message));
    };
    let cond_class = handle.0.class.clone();
    let obj = Arc::new(crate::object::Object::new(cond_class.clone()));
    obj.set_slot(Arc::from("message"), Value::Str(Arc::from(message)));
    Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: cond_class,
        kind: InstanceKind::ValueObject(obj),
    })))
}

/// Allocate a fresh, **unfrozen** Value-subclass instance for `_primNewValue:`.
///
/// Requires: `class.inherits_is_value()`. `indexed_size` is `Some(n)` for indexed
///   classes (n slots), `None` for plain ones.
/// Ensures: returns a `ValueObject` instance the caller mutates via an init block
///   and then freezes with [`freeze_value_instance`].
pub(crate) fn make_value_instance(
    class: Arc<Class>,
    indexed_size: Option<usize>,
) -> InstanceHandle {
    let obj = match indexed_size {
        Some(n) => Arc::new(Object::new_with_indexed(class.clone(), n)),
        None => Arc::new(Object::new(class.clone())),
    };
    InstanceHandle(Arc::new(InstanceState {
        class,
        kind: InstanceKind::ValueObject(obj),
    }))
}

/// Freeze a Value-subclass instance built by [`make_value_instance`] once its
/// init block has run. No-op if `inst` is not a `ValueObject`.
pub(crate) fn freeze_value_instance(inst: &InstanceHandle) {
    if let InstanceKind::ValueObject(obj) = &inst.0.kind {
        obj.freeze();
    }
}

/// Read the native [`TraitData`] payload from a `Trait` value object (`data` ivar).
///
/// Ensures: returns `Some` iff `v` is a `Value::Instance` whose `data` ivar holds a
/// `Value::Handle` downcastable to `TraitData`.
pub(crate) fn trait_data_of(v: &Value) -> Option<Arc<crate::traits::TraitData>> {
    let Value::Instance(h) = v else { return None };
    let InstanceKind::ValueObject(obj) = &h.0.kind else {
        return None;
    };
    let Some(Value::Handle(any)) = obj.get_slot("data") else {
        return None;
    };
    any.downcast::<crate::traits::TraitData>().ok()
}

/// Wrap a [`TraitData`] as a frozen `Trait` value object (kernel `Trait` class).
pub(crate) fn make_trait_value(
    ev: &Evaluator,
    data: Arc<crate::traits::TraitData>,
) -> Result<Value, EvalError> {
    let cls = ev
        .registry
        .kernel()
        .lookup_class("Trait")
        .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Trait")))?
        .0
        .class
        .clone();
    let inst = make_value_instance(cls, None);
    if let InstanceKind::ValueObject(obj) = &inst.0.kind {
        let handle: Arc<dyn std::any::Any + Send + Sync> = data;
        obj.set_slot(Arc::from("data"), Value::Handle(handle));
        obj.freeze();
    }
    Ok(Value::Instance(inst))
}

/// Bind a trait's name, mirroring class-name binding in [`handle_subclass`]: the
/// trait becomes a definition of the active namespace (so methods resolve it), and
/// top-level code sees it through the env — a global binding for Kernel traits,
/// the current frame otherwise.
fn bind_trait_name(ev: &Evaluator, name: Arc<str>, trait_val: Value, env: &mut Env) {
    let active_ns = ev.active_namespace();
    active_ns.bind(name.clone(), trait_val.clone());
    if Arc::ptr_eq(&active_ns, &ev.registry.kernel()) {
        env.define_global(name, trait_val);
    } else {
        env.define(name, trait_val);
    }
}

/// Build the sentinel method for an unresolved trait conflict on `sel`. Its body
/// signals a `TraitConflict` carrying the selector; it keeps `pattern` so the
/// method arity matches any real send. Replaced when the class defines `sel`.
fn conflict_sentinel_method(
    pattern: &crate::class::MethodPattern,
    sel: &str,
) -> Result<crate::class::Method, EvalError> {
    let src = format!("self pvtTraitConflictFor: #'{sel}'");
    let prog = crate::parser::Parser::parse(&src)
        .map_err(|e| EvalError::TypeError(format!("trait conflict sentinel parse: {e:?}")))?;
    Ok(crate::class::Method {
        pattern: pattern.clone(),
        locals: vec![],
        body: prog.stmts,
    })
}

/// Extract the elements of a pointer-indexed value object (an `Array`), if `v` is one.
fn array_pointer_elements(v: &Value) -> Option<Vec<Value>> {
    use crate::object::IndexedStorage;
    let Value::Instance(h) = v else { return None };
    let InstanceKind::ValueObject(obj) = &h.0.kind else {
        return None;
    };
    match obj.indexed.as_ref()? {
        IndexedStorage::Pointer(slots) => Some(slots.lock().clone()),
        _ => None,
    }
}

/// Build a nil-terminated `Cons` list (a frozen `Value`) from `items`, in order.
///
/// Mirrors Array-literal construction (eval.rs): the interpreter instantiates a
/// Smol-declared value class directly. Used to reify `Message` arguments.
///
/// Requires: the kernel defines `Cons` with ivars `head`/`tail`.
/// Ensures: returns `Value::Nil` for an empty `items`, else a frozen `Cons` chain.
pub(crate) fn build_cons(ev: &Evaluator, items: Vec<Value>) -> Result<Value, EvalError> {
    let cons_cls = ev
        .registry
        .kernel()
        .lookup_class("Cons")
        .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Cons")))?
        .0
        .class
        .clone();
    let mut list = Value::Nil;
    for item in items.into_iter().rev() {
        let inst = make_value_instance(cons_cls.clone(), None);
        if let InstanceKind::ValueObject(obj) = &inst.0.kind {
            obj.set_slot(Arc::from("head"), item);
            obj.set_slot(Arc::from("tail"), list);
            obj.freeze();
        }
        list = Value::Instance(inst);
    }
    Ok(list)
}

/// Reify a failed send as a frozen `Message` value (`selector` + `arguments`),
/// for the `doesNotUnderstand:` hook.
///
/// Requires: the kernel defines `Message` with ivars `selector`/`arguments`.
/// Ensures: `selector` is a Symbol; `arguments` is a `Cons` list (nil if empty).
pub(crate) fn build_message(
    ev: &Evaluator,
    selector: Arc<str>,
    args: Vec<Value>,
) -> Result<Value, EvalError> {
    let arg_list = build_cons(ev, args)?;
    let msg_cls = ev
        .registry
        .kernel()
        .lookup_class("Message")
        .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Message")))?
        .0
        .class
        .clone();
    let inst = make_value_instance(msg_cls, None);
    if let InstanceKind::ValueObject(obj) = &inst.0.kind {
        obj.set_slot(Arc::from("selector"), Value::Symbol(selector));
        obj.set_slot(Arc::from("arguments"), arg_list);
        obj.freeze();
    }
    Ok(Value::Instance(inst))
}

/// Fallback dispatch for primitive value types (Int, Float, Bool, Nil, Str,
/// Symbol, Block, LargeInt). Called from the `_` arm of each type-specific
/// dispatch function after type-specific cases are exhausted.
///
/// Handles selectors that are meaningful on every value but are not universal
/// enough to bypass user-defined method lookup (unlike the pre-checks in
/// `dispatch()`). Object/ValueObject instances never reach this function —
/// they have their own fallback in `dispatch_object_primitive`.
/// Shared tail for every primitive dispatcher: the selectors that are grounded
/// identically for *all* primitive receivers.
///
/// Requires: `self_val` is the receiver the caller dispatched on.
/// Ensures: answers the grounded value, or `MessageNotUnderstood` naming `kind`.
fn dispatch_primitive_fallback(
    ev: &Evaluator,
    self_val: Value,
    kind: &'static str,
    sel: &Arc<str>,
    mut args: Vec<Value>,
) -> Result<Value, EvalError> {
    // NOTE: primitive types (Int, Float, etc.) don't carry an Env — handler_chain
    // checking is skipped here. Use dispatch_object_primitive for Condition instances.
    match sel.as_ref() {
        // Grounding for `Object >> ==` (C34 S2). Lives here rather than in each
        // per-variant dispatcher so there is exactly ONE identity implementation
        // for primitive receivers; `obj_prim_core` grounds the instance case.
        "_primIdentical:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(self_val == force_value(ev, args.remove(0))?))
        }
        // ── Encoding conversions (C34 S2/S3) ─────────────────────────────
        //
        // One `_prim` per OPERATION, branching internally on the variant (§4),
        // rather than a selector per representation family. They live here, in
        // the shared tail, for the same reason: `Symbol >> asString` and
        // `MachineInteger >> asString` are one operation over two encodings.
        //
        // Contrast the binary numeric prims (`_primLt:`, `_primAdd:`, …), which
        // stay in the per-leaf dispatchers: those promote across two operands and
        // are genuinely per-leaf grounding of a shared abstract method.
        "_primAsString" => {
            expect_arity(sel, 0, args.len())?;
            match self_val {
                Value::Symbol(s) => Ok(Value::Str(s)),
                Value::Int(n) => Ok(Value::Str(Arc::from(n.to_string().as_str()))),
                Value::LargeInt(n) => Ok(Value::Str(Arc::from(n.to_string().as_str()))),
                Value::Float(f) => Ok(Value::Str(Arc::from(format!("{f:?}").as_str()))),
                other => Err(EvalError::TypeError(format!(
                    "{sel}: no string encoding for {}",
                    other.kind()
                ))),
            }
        }
        "_primAsFloat" => {
            expect_arity(sel, 0, args.len())?;
            match self_val {
                Value::Int(n) => Ok(Value::Float(n as f64)),
                // A BigInt too large for f64 saturates rather than erroring.
                // `to_f64` already answers `Some(±inf)` out of range (verified), so
                // the fallback is defensive only — but S6 made it sign-correct
                // rather than leaving a latent wrong answer: the arm this replaced
                // answered `+inf` for a huge NEGATIVE value.
                Value::LargeInt(n) => {
                    use num_traits::{Signed, ToPrimitive};
                    Ok(Value::Float(n.to_f64().unwrap_or(if n.is_negative() {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    })))
                }
                other => Err(EvalError::TypeError(format!(
                    "{sel}: no float encoding for {}",
                    other.kind()
                ))),
            }
        }
        // Float -> Integer rounding modes. Each is a distinct operation, so each
        // gets its own selector rather than one prim taking a mode symbol.
        // `asInt` is deliberately absent: it is truncation, and says so in Smol.
        "_primTruncated" | "_primRounded" | "_primCeiling" | "_primFloor" => {
            expect_arity(sel, 0, args.len())?;
            match self_val {
                Value::Float(f) => {
                    let r = match sel.as_ref() {
                        "_primTruncated" => f.trunc(),
                        "_primRounded" => f.round(),
                        "_primCeiling" => f.ceil(),
                        _ => f.floor(),
                    };
                    Ok(Value::Int(r as i64))
                }
                other => Err(EvalError::TypeError(format!(
                    "{sel}: not a Float: {}",
                    other.kind()
                ))),
            }
        }
        _ => Err(mnu(kind, sel)),
    }
}

// ── Int ───────────────────────────────────────────────────────────────────────

fn dispatch_int(
    ev: &Evaluator,
    n: i64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    // Note: user-defined Int methods are routed via lookup_user_method in the
    // evaluator; this dispatcher only grounds `_prim` selectors and the remaining
    // conversion protocol.
    match sel.as_ref() {
        "_primLt:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(numeric_lt(
                ev,
                Value::Int(n),
                args.remove(0),
                sel,
            )?))
        }
        "_primEq:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(numeric_eq(
                ev,
                Value::Int(n),
                args.remove(0),
                sel,
            )?))
        }
        _ if arith_prim_op(sel).is_some() => {
            expect_arity(sel, 1, args.len())?;
            let op = arith_prim_op(sel).expect("guarded by the match arm");
            numeric_binop(ev, op, Value::Int(n), args.remove(0), sel)
        }
        _ => dispatch_primitive_fallback(ev, Value::Int(n), "MachineInteger", sel, args),
    }
}

// ── LargeInt ──────────────────────────────────────────────────────────────────

fn dispatch_largeint(
    ev: &Evaluator,
    n: Arc<BigInt>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "_primLt:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(numeric_lt(
                ev,
                Value::LargeInt(n),
                args.remove(0),
                sel,
            )?))
        }
        "_primEq:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(numeric_eq(
                ev,
                Value::LargeInt(n),
                args.remove(0),
                sel,
            )?))
        }
        _ if arith_prim_op(sel).is_some() => {
            expect_arity(sel, 1, args.len())?;
            let op = arith_prim_op(sel).expect("guarded by the match arm");
            numeric_binop(ev, op, Value::LargeInt(n), args.remove(0), sel)
        }
        _ => dispatch_primitive_fallback(ev, Value::LargeInt(n), "ArbitraryInteger", sel, args),
    }
}

// ── Float ────────────────────────────────────────────────────────────────────

fn dispatch_float(
    ev: &Evaluator,
    f: f64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "_primLt:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(numeric_lt(
                ev,
                Value::Float(f),
                args.remove(0),
                sel,
            )?))
        }
        "_primEq:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(numeric_eq(
                ev,
                Value::Float(f),
                args.remove(0),
                sel,
            )?))
        }
        _ if arith_prim_op(sel).is_some() => {
            expect_arity(sel, 1, args.len())?;
            let op = arith_prim_op(sel).expect("guarded by the match arm");
            numeric_binop(ev, op, Value::Float(f), args.remove(0), sel)
        }
        _ => dispatch_primitive_fallback(ev, Value::Float(f), "Float", sel, args),
    }
}

// ── Nil ───────────────────────────────────────────────────────────────────────

// ── String ────────────────────────────────────────────────────────────────────

fn dispatch_str(
    ev: &Evaluator,
    s: Arc<str>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    // The public String protocol lives in `kernel.smol` (C34 S4); what remains here
    // is the irreducible grounding it sends to. Indexing is 0-based and char-based
    // (Unicode scalar), matching every other collection.
    match sel.as_ref() {
        "_primSize" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(s.chars().count() as i64))
        }
        "_primAt:" => {
            expect_arity(sel, 1, args.len())?;
            let idx = as_int(ev, args.remove(0), sel)?;
            usize::try_from(idx)
                .ok()
                .and_then(|i| s.chars().nth(i))
                .map(|c| Value::Str(Arc::from(c.to_string().as_str())))
                .ok_or_else(|| EvalError::TypeError(format!("{sel}: index {idx} out of bounds")))
        }
        "_primConcat:" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(ev, args.remove(0))? {
                Value::Str(other) => Ok(Value::Str(Arc::from(format!("{s}{other}").as_str()))),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        // Lexicographic `<`, grounding `String >> <` (the Ordered trait's requirement).
        "_primStrLt:" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(ev, args.remove(0))? {
                Value::Str(other) => Ok(Value::Bool(*s < *other)),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        "_primAsSymbol" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Symbol(s))
        }
        "_primReversed" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(
                s.chars().rev().collect::<String>().as_str(),
            )))
        }
        // HALF-OPEN and 0-based, so it is the same primitive shape the collections
        // use (C34 S3a): the length is `until - from` with no adjustment, and
        // `String >> copyFrom:to:` derives from it in Smol. Out-of-range answers an
        // empty string rather than erroring, as the arm this replaced did.
        "_primCopyFrom:until:" => {
            expect_arity(sel, 2, args.len())?;
            let from = as_int(ev, args.remove(0), sel)?;
            let until = as_int(ev, args.remove(0), sel)?;
            let chars: Vec<char> = s.chars().collect();
            let slice: String = if from < 0 || until <= from {
                String::new()
            } else {
                let lo = from as usize;
                let hi = (until as usize).min(chars.len());
                chars.get(lo..hi).unwrap_or(&[]).iter().collect()
            };
            Ok(Value::Str(Arc::from(slice.as_str())))
        }
        // Programmatic construction WITHOUT mutability (C34 S4). The repetition
        // happens in a host buffer and the Smol value is born complete, so this is
        // not the allocate-then-poke construction the retired plan's §5 rejects.
        // Instance-side because a `_prim` may only be sent to `self`: the
        // class-side `new:withAll:` factory is pure Smol delegating to this.
        "_primRepeated:" => {
            expect_arity(sel, 1, args.len())?;
            let n = as_int(ev, args.remove(0), sel)?;
            let count = usize::try_from(n).map_err(|_| {
                EvalError::TypeError(format!("{sel}: count must not be negative, got {n}"))
            })?;
            Ok(Value::Str(Arc::from(s.repeat(count).as_str())))
        }
        // Console output (C34 S5). The ONLY channel by which Smol writes to the
        // console; rendering selectors (`asString`/`asDebugString`) are pure.
        // Instance-side on String for the self-only rule: `Object >> print`
        // converts and delegates to `String >> print`.
        "_primWrite" => {
            expect_arity(sel, 0, args.len())?;
            print!("{s}");
            use std::io::Write;
            std::io::stdout().flush().ok();
            Ok(Value::Str(s))
        }
        "_primWriteLine" => {
            expect_arity(sel, 0, args.len())?;
            println!("{s}");
            Ok(Value::Str(s))
        }
        "_primIncludesSubString:" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(ev, args.remove(0))? {
                Value::Str(sub) => Ok(Value::Bool(s.contains(sub.as_ref()))),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        _ => dispatch_primitive_fallback(ev, Value::Str(s), "String", sel, args),
    }
}

// ── Symbol ────────────────────────────────────────────────────────────────────

// ── Block ─────────────────────────────────────────────────────────────────────

fn dispatch_block(
    ev: &Evaluator,
    blk: Arc<Block>,
    sel: &Arc<str>,
    args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    // The iterative evaluator intercepts value/value:/whileTrue:/whileFalse:/etc. in
    // dispatch_to_stack (via push_block_frame or lookup_user_method + TCO) before
    // dispatch() is called, so only unknown selectors reach here.
    dispatch_primitive_fallback(ev, Value::Block(blk), "Block", sel, args)
}

// ── Object (user-defined instances) ──────────────────────────────────────────

fn dispatch_object_value(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // User-defined methods are intercepted by lookup_user_method in dispatch_to_stack
    // before dispatch() is called, so class.lookup() here would always return None.
    dispatch_object_primitive(ev, handle, &selector, args, env)
}

/// Dispatch a message to an actor entity. Checks for sync self-send
/// (target == CURRENT_ENTITY); otherwise enqueues the message and returns a Future.
///
/// Requires: caller may be any thread including the entity's own thread.
/// Ensures: self-send is dispatched synchronously (no deadlock); cross-entity
///   send returns `Value::Future(FutureHandle)` immediately.
fn dispatch_entity(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let entity_id = match &handle.0.kind {
        InstanceKind::Entity(d) => d.id,
        _ => unreachable!("dispatch_entity called on non-Entity instance"),
    };

    // Self-send: dispatch synchronously on the current thread to avoid deadlock.
    // User-defined methods for self-sends are intercepted by lookup_user_method
    // in dispatch_to_stack before dispatch() is called, so class.lookup() would
    // always return None here; only builtins remain.
    let current = ev.current_entity_id.get();
    if current == Some(entity_id) {
        return dispatch_object_primitive(ev, handle, &selector, args, env);
    }

    // Cross-entity or non-entity-context send: enqueue and return Future.
    // Snapshot the caller's handler_chain so the receiving entity can run inherited
    // HC handlers if it signals a condition.
    let chain_snapshot = {
        let chain = ev.handler_chain.borrow();
        if chain.is_empty() {
            vec![]
        } else {
            chain.clone()
        }
    };
    let tx_tag = ev.current_tx_ctx.borrow().clone();
    let future = match &handle.0.kind {
        InstanceKind::Entity(d) => send_to_entity(d, selector, args, chain_snapshot, tx_tag),
        _ => unreachable!(),
    };
    Ok(Value::Future(future))
}

/// Returns the canonical `InstanceHandle` for the class of `obj`.
///
/// Requires: `obj.class.class_object` is set (always true after class registration).
fn class_handle_of(obj: &Arc<Object>) -> Result<InstanceHandle, EvalError> {
    obj.class
        .class_object
        .get()
        .and_then(|w| w.upgrade())
        .map(InstanceHandle)
        .ok_or_else(|| {
            EvalError::TypeError(format!("class {} has no class object", obj.class.name))
        })
}

/// Commit a shared-transaction context buffer to the store, mapping failures to
/// a signal. `FactNotFound` (nil retract of a never-set ivar) is treated as
/// success, matching the per-message commit policy.
fn commit_shared_buffer(
    ev: &Evaluator,
    tx: triple_store::store::OwnedStoreTransaction,
) -> Result<(), EvalError> {
    match tx.commit() {
        Ok(_) | Err(triple_store::store::StoreError::FactNotFound) => Ok(()),
        Err(e) => Err(EvalError::Signal(make_error_value(
            &ev.registry,
            &format!("transaction commit failed: {e:?}"),
        ))),
    }
}

/// Begin a new transaction level of `requested` discipline as a child of the
/// current context (or a fresh root if none). Root allocates a new shared TxId;
/// a child reuses the tree's. `GenTx::begin` applies contagious-atomic coercion.
fn begin_tx_level(ev: &Evaluator, requested: TxMode) -> GenTx {
    let parent = ev.current_tx_ctx.borrow().clone();
    let tx_id = match &parent {
        Some(p) => p.tx_id(),
        None => ev.store.alloc_tx_id(),
    };
    let buf = ev.store.begin_owned_with_id(ExecContext::System, tx_id);
    GenTx::begin(requested, tx_id, buf, parent)
}

/// Extract the `GenTx` carried by a transaction handle value.
///
/// Requires: `v` is the `Value::Handle` produced by `_primBeginAtomicTx`/`Stm`.
fn tx_ctx_from_handle(v: Value) -> Result<Arc<GenTx>, EvalError> {
    match v {
        Value::Handle(h) => h
            .downcast::<GenTx>()
            .map_err(|_| EvalError::TypeError("expected a transaction handle".into())),
        other => Err(EvalError::TypeError(format!(
            "expected a transaction handle, got {}",
            other.kind()
        ))),
    }
}

/// Shared receiver context for the `dispatch_object_primitive` chunk handlers.
///
/// `dispatch_object_primitive`'s `match sel` was one ~900-line function whose
/// debug frame summed *every* arm's locals — and that frame is live at the
/// signal/eval_block recursion point, dominating the per-nesting-level Rust
/// stack (C31.5: ~52 KB of the ~60 KB/level). Splitting the arms into
/// `#[inline(never)]` chunk handlers means only the matched chunk's frame is
/// live at any recursion, not all arms'.
struct ObjPrim<'a> {
    ev: &'a Evaluator,
    handle: &'a InstanceHandle,
    obj: &'a Arc<Object>,
    entity_id: Option<EntityId>,
    self_val: &'a Value,
}

pub(crate) fn dispatch_object_primitive(
    ev: &Evaluator,
    handle: InstanceHandle,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let entity_id = match &handle.0.kind {
        InstanceKind::Entity(d) => Some(d.id),
        _ => None,
    };
    let obj = match &handle.0.kind {
        InstanceKind::ValueObject(o) => o.clone(),
        InstanceKind::Entity(d) => d.data.clone(),
        InstanceKind::ClassEntity(_) => {
            return Err(EvalError::MessageNotUnderstood {
                receiver_kind: "Class",
                selector: sel.clone(),
            });
        }
    };
    let self_val = Value::Instance(handle.clone());
    let cx = ObjPrim {
        ev,
        handle: &handle,
        obj: &obj,
        entity_id,
        self_val: &self_val,
    };

    // Each chunk handles a disjoint set of selectors and returns Ok(None) for
    // the rest; chunks run sequentially so only the matched chunk's frame is
    // ever live at a recursion point inside it (e.g. signal_condition).
    if let Some(v) = obj_prim_core(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_traits(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_indexed(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_conditions(&cx, sel, &mut args, env)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_history(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_timers(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_tx(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_futures(&cx, sel, &mut args)? {
        return Ok(v);
    }
    // Generic instance rendering, grounding `Object >> asString` (C34 S5). LAST
    // on purpose: `_primAsString` is one operation ("render me as a String") whose
    // answer depends on the receiver, and a more specific chunk must get first
    // refusal — `ByteArray` decodes its bytes as UTF-8 rather than answering
    // "a ByteArray". The class name is host data, so this stays a primitive; a
    // user class overrides `asString` in Smol.
    if sel.as_ref() == "_primAsString" {
        expect_arity(sel, 0, args.len())?;
        return Ok(Value::Str(Arc::from(
            format!("a {}", cx.obj.class.name).as_str(),
        )));
    }
    Err(EvalError::MessageNotUnderstood {
        receiver_kind: "Object",
        selector: sel.clone(),
    })
}

/// `=`, `~=`, `printString`, `respondsTo:`.
#[inline(never)]
fn obj_prim_core(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        // Grounding for `Object >> ==` (C34 S2) on an instance receiver;
        // `dispatch_primitive_fallback` grounds every primitive receiver.
        "_primIdentical:" => {
            expect_arity(sel, 1, args.len())?;
            Value::Bool(*cx.self_val == force_value(ev, args.remove(0))?)
        }
        "respondsTo:" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Symbol(s) => Value::Bool(cx.obj.class.lookup(&s).is_some()),
                _ => Value::Bool(false),
            }
        }
        _ => return Ok(None),
    }))
}

/// Trait algebra primitives on a `Trait` value object: `+`/`-`/`@` builders plus
/// reflection (`name`, derived `requirements`, `conflicts`). Each expects the
/// receiver's `data` ivar to hold a [`TraitData`].
#[inline(never)]
fn obj_prim_traits(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    use crate::traits::TraitData;
    let ev = cx.ev;
    // Fast reject: only trait selectors reach here.
    if !sel.starts_with("_primTrait") {
        return Ok(None);
    }
    let Some(data) = trait_data_of(cx.self_val) else {
        return Err(EvalError::TypeError(format!(
            "{sel}: receiver is not a Trait"
        )));
    };
    Ok(Some(match sel.as_ref() {
        "_primTraitName" => {
            expect_arity(sel, 0, args.len())?;
            Value::Symbol(data.name.read().clone())
        }
        "_primTraitSum:" => {
            expect_arity(sel, 1, args.len())?;
            let other = force_value(ev, args.remove(0))?;
            let other = trait_data_of(&other)
                .ok_or_else(|| EvalError::TypeError(format!("{sel}: argument is not a Trait")))?;
            make_trait_value(ev, TraitData::sum(&data, &other))?
        }
        "_primTraitExcludeTrait:" => {
            expect_arity(sel, 1, args.len())?;
            let other = force_value(ev, args.remove(0))?;
            let other = trait_data_of(&other)
                .ok_or_else(|| EvalError::TypeError(format!("{sel}: argument is not a Trait")))?;
            make_trait_value(ev, TraitData::exclude_trait(&data, &other))?
        }
        "_primTraitExcludeSelectors:" => {
            expect_arity(sel, 1, args.len())?;
            let arr = force_value(ev, args.remove(0))?;
            let elems = array_pointer_elements(&arr).ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: argument must be an Array of Symbols"))
            })?;
            let mut set = std::collections::HashSet::new();
            for e in elems {
                match force_value(ev, e)? {
                    Value::Symbol(s) => {
                        set.insert(s);
                    }
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: selector-set elements must be Symbols"
                        )));
                    }
                }
            }
            make_trait_value(ev, TraitData::exclude_selectors(&data, &set))?
        }
        "_primTraitAlias:" => {
            // Argument: Array of Associations `#newSel -> #oldSel`.
            expect_arity(sel, 1, args.len())?;
            let arr = force_value(ev, args.remove(0))?;
            let elems = array_pointer_elements(&arr).ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: argument must be an Array of Associations"))
            })?;
            let mut aliases: Vec<(Arc<str>, Arc<str>)> = vec![];
            for e in elems {
                let assoc = force_value(ev, e)?;
                let (Value::Instance(_), Some(k), Some(v)) = (
                    &assoc,
                    assoc_slot(&assoc, "key"),
                    assoc_slot(&assoc, "value"),
                ) else {
                    return Err(EvalError::TypeError(format!(
                        "{sel}: alias elements must be Associations #new -> #old"
                    )));
                };
                match (k, v) {
                    (Value::Symbol(new_sel), Value::Symbol(old_sel)) => {
                        aliases.push((new_sel, old_sel))
                    }
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: alias key and value must be Symbols"
                        )));
                    }
                }
            }
            make_trait_value(ev, TraitData::alias(&data, &aliases))?
        }
        "_primTraitRequirements" => {
            expect_arity(sel, 0, args.len())?;
            let mut reqs: Vec<Value> = data.requirements().into_iter().map(Value::Symbol).collect();
            reqs.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
            build_array(ev, reqs)?
        }
        "_primTraitConflicts" => {
            expect_arity(sel, 0, args.len())?;
            let mut cs: Vec<Value> = data
                .conflict_selectors()
                .into_iter()
                .map(Value::Symbol)
                .collect();
            cs.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
            build_array(ev, cs)?
        }
        _ => return Ok(None),
    }))
}

/// Read a named slot from an `Association` value object (or any value object).
fn assoc_slot(v: &Value, name: &str) -> Option<Value> {
    let Value::Instance(h) = v else { return None };
    if let InstanceKind::ValueObject(obj) = &h.0.kind {
        obj.get_slot(name)
    } else {
        None
    }
}

/// Indexed-slot primitives: `_primAt:`, `_primAt:put:`, `_primSize`, `_primAsString`.
#[inline(never)]
fn obj_prim_indexed(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    use crate::object::IndexedStorage;
    let obj = cx.obj;
    let entity_id = cx.entity_id;
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primAt:" => {
            expect_arity(sel, 1, args.len())?;
            let n = as_int(ev, args.remove(0), sel)?;
            let indexed = obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })?;
            match indexed {
                IndexedStorage::Pointer(slots) => {
                    let guard = slots.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize].clone()
                }
                IndexedStorage::Byte(bytes) => {
                    let guard = bytes.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    Value::Int(guard[n as usize] as i64)
                }
                IndexedStorage::Float64(floats) => {
                    let guard = floats.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    Value::Float(guard[n as usize])
                }
            }
        }
        "_primAt:put:" => {
            expect_arity(sel, 2, args.len())?;
            let n = as_int(ev, args.remove(0), sel)?;
            let v = args.remove(0);
            let indexed = obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })?;
            match indexed {
                IndexedStorage::Pointer(slots) => {
                    let mut guard = slots.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize] = v.clone();
                    drop(guard);
                    if let Some(eid) = entity_id {
                        crate::store_bridge::buffer_indexed_slot(ev, eid, n as usize, &v);
                    }
                    v
                }
                IndexedStorage::Byte(bytes) => {
                    let b = match &v {
                        Value::Int(i) if *i >= 0 && *i <= 255 => *i as u8,
                        Value::Int(i) => {
                            return Err(EvalError::TypeError(format!(
                                "{sel}: byte value {i} out of range 0..=255"
                            )));
                        }
                        _ => {
                            return Err(EvalError::TypeError(format!(
                                "{sel}: Byte storage requires Int argument in 0..=255"
                            )));
                        }
                    };
                    let mut guard = bytes.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize] = b;
                    drop(guard);
                    if let Some(eid) = entity_id {
                        crate::store_bridge::buffer_indexed_slot(
                            ev,
                            eid,
                            n as usize,
                            &Value::Int(b as i64),
                        );
                    }
                    v
                }
                IndexedStorage::Float64(floats) => {
                    let f = match &v {
                        Value::Float(f) => *f,
                        Value::Int(i) => *i as f64,
                        _ => {
                            return Err(EvalError::TypeError(format!(
                                "{sel}: Float64 storage requires Float or Int argument"
                            )));
                        }
                    };
                    let mut guard = floats.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize] = f;
                    drop(guard);
                    if let Some(eid) = entity_id {
                        crate::store_bridge::buffer_indexed_slot(
                            ev,
                            eid,
                            n as usize,
                            &Value::Float(f),
                        );
                    }
                    v
                }
            }
        }
        "_primSize" => {
            expect_arity(sel, 0, args.len())?;
            let indexed = obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })?;
            let len = match indexed {
                IndexedStorage::Pointer(slots) => slots.lock().len(),
                IndexedStorage::Byte(bytes) => bytes.lock().len(),
                IndexedStorage::Float64(floats) => floats.lock().len(),
            };
            Value::Int(len as i64)
        }
        // `ByteArray >> asString` decodes the bytes as UTF-8. DECLINES (rather
        // than erroring) for any receiver without byte storage, so the generic
        // `a ClassName` rendering at the end of `dispatch_object_primitive` gets
        // it instead — `_primAsString` is one operation whose answer depends on
        // the receiver, and this chunk only owns the byte-storage case.
        "_primAsString" => match obj.indexed.as_ref() {
            Some(IndexedStorage::Byte(bytes)) => {
                expect_arity(sel, 0, args.len())?;
                let s = String::from_utf8_lossy(&bytes.lock()).into_owned();
                Value::Str(Arc::from(s.as_str()))
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    }))
}

/// Condition system: `error:`, `_primSignalCondition:`, `_primResume:`,
/// `_primReturn:`, `_primRetry`, and the `_primHCRun:` fallback. This chunk holds
/// the signal path; keeping it small bounds the Rust stack at the handler eval.
#[inline(never)]
fn obj_prim_conditions(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
    env: &mut Env,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primSignalCondition:" => {
            expect_arity(sel, 1, args.len())?;
            let condition = force_value(ev, args.remove(0))?;
            if !matches!(&condition, Value::Instance(_)) {
                return Err(EvalError::TypeError(
                    "_primSignalCondition: requires a Condition instance".into(),
                ));
            }
            signal_condition(ev, condition, env)?
        }
        "_primResume:" => {
            expect_arity(sel, 1, args.len())?;
            return Err(EvalError::Resume(args.remove(0)));
        }
        "_primReturn:" => {
            expect_arity(sel, 1, args.len())?;
            return Err(EvalError::ReturnFromBlock(args.remove(0)));
        }
        "_primRetry" => {
            expect_arity(sel, 0, args.len())?;
            return Err(EvalError::Retry);
        }
        "_primHCRun:" => {
            // Fallback path only. The iterative evaluator intercepts `_primHCRun:`
            // in `dispatch_to_stack` and runs the body on the heap `Cont` stack
            // (no recursive `step_loop`); see `eval_iterative::dispatch_to_stack`.
            // This arm preserves the recursive behavior for any direct
            // `builtins::dispatch` caller.
            expect_arity(sel, 1, args.len())?;
            let body = as_block(ev, args.remove(0), sel)?;
            let handler_pairs = extract_handler_pairs(cx.ev, cx.handle)?;

            // Push handler pairs onto handler_chain.
            let chain_start_len = ev.handler_chain.borrow().len();
            {
                let mut chain = ev.handler_chain.borrow_mut();
                for (cls, blk) in handler_pairs.iter() {
                    chain.push((cls.clone(), blk.clone()));
                }
            }

            // Execute body in a loop (for Retry). Always truncate chain on exit.
            let result = loop {
                match ev.eval_block(&body, vec![], env) {
                    Ok(v) => break Ok(v),
                    Err(EvalError::ReturnFromBlock(v)) => break Ok(v),
                    Err(EvalError::Retry) => continue,
                    Err(EvalError::Return(v, d)) => break Err(EvalError::Return(v, d)),
                    Err(other) => break Err(other),
                }
            };
            ev.handler_chain.borrow_mut().truncate(chain_start_len);
            result?
        }
        _ => return Ok(None),
    }))
}

/// Temporal queries: `historyOf:`, `historyOf:from:to:`.
#[inline(never)]
fn obj_prim_history(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    let entity_id = cx.entity_id;
    Ok(Some(match sel.as_ref() {
        "historyOf:" => {
            expect_arity(sel, 1, args.len())?;
            let eid = entity_id.ok_or_else(|| {
                EvalError::TypeError("historyOf: requires an entity receiver".into())
            })?;
            let pred_name = match force_value(ev, args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "historyOf: argument must be a Symbol, got {}",
                        other.kind()
                    )));
                }
            };
            let pred = ev.registry.store.lookup(&pred_name).ok_or_else(|| {
                EvalError::TypeError(format!("historyOf: unknown predicate '{pred_name}'"))
            })?;
            let ctx = ExecContext::Entity(eid);
            let facts = ev.store.history(&ctx, eid, pred, None).map_err(|e| {
                EvalError::Signal(make_error_value(
                    &ev.registry,
                    &format!("historyOf: store error: {e:?}"),
                ))
            })?;
            history_facts_to_array(ev, &facts)?
        }
        "historyOf:from:to:" => {
            expect_arity(sel, 3, args.len())?;
            let eid = entity_id.ok_or_else(|| {
                EvalError::TypeError("historyOf:from:to: requires an entity receiver".into())
            })?;
            let pred_name = match force_value(ev, args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "historyOf:from:to: first argument must be a Symbol, got {}",
                        other.kind()
                    )));
                }
            };
            let from_ms = as_int(ev, args.remove(0), sel)?;
            let to_ms = as_int(ev, args.remove(0), sel)?;
            let pred = ev.registry.store.lookup(&pred_name).ok_or_else(|| {
                EvalError::TypeError(format!(
                    "historyOf:from:to: unknown predicate '{pred_name}'"
                ))
            })?;
            let ctx = ExecContext::Entity(eid);
            let all_facts = ev.store.history(&ctx, eid, pred, None).map_err(|e| {
                EvalError::Signal(make_error_value(
                    &ev.registry,
                    &format!("historyOf:from:to: store error: {e:?}"),
                ))
            })?;
            let facts: Vec<StoredFact> = all_facts
                .into_iter()
                .filter(|f| {
                    let start_ms = (f.valid.start.0 / 1_000_000) as i64;
                    let end_ms = f
                        .valid
                        .end
                        .map(|t| (t.0 / 1_000_000) as i64)
                        .unwrap_or(i64::MAX);
                    start_ms <= to_ms && end_ms >= from_ms
                })
                .collect();
            history_facts_to_array(ev, &facts)?
        }
        _ => return Ok(None),
    }))
}

/// Timer primitives: `_primCancelTimer:`, `_primTimerPending:`.
#[inline(never)]
fn obj_prim_timers(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primCancelTimer:" => {
            // Requires: args[0] is a Value::Handle returned by _primScheduleTimer:block:.
            // Ensures: the timer callback will not be called if it has not already fired.
            //   No-op if already fired. Returns Value::Nil.
            expect_arity(sel, 1, args.len())?;
            match force_value(ev, args.remove(0))? {
                Value::Handle(h) => {
                    ev.os.cancel_timer(&h);
                    Value::Nil
                }
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primCancelTimer: argument must be a Handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        "_primTimerPending:" => {
            // Requires: args[0] is a Value::Handle returned by _primScheduleTimer:block:.
            // Ensures: returns Value::Bool(true) iff the timer has not fired and is not cancelled.
            expect_arity(sel, 1, args.len())?;
            match force_value(ev, args.remove(0))? {
                Value::Handle(h) => Value::Bool(ev.os.timer_pending(&h)),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primTimerPending: argument must be a Handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        // ── TCP sockets (C-web spike) ──────────────────────────────────────────
        // `accept`/`read`/`write` return a Future resolved from a fresh may
        // coroutine that performs the parking I/O, preserving async-everywhere.
        // `_primTcpListen:` is class-side (see dispatch_class_builtin); these four
        // are sent to `self` on the socket/listener *values*.
        "_primTcpAccept:" => {
            // Requires: args[0] is a listener Handle. Returns a Future of a stream Handle.
            expect_arity(sel, 1, args.len())?;
            let listener = as_handle(ev, args.remove(0), sel)?;
            let fh = FutureHandle::new();
            let (fh2, os, registry) = (fh.clone(), ev.os.clone(), ev.registry.clone());
            may::go!(move || match os.tcp_accept(&listener) {
                Ok(stream) => {
                    fh2.try_resolve_ok(Value::Handle(stream));
                }
                Err(e) => {
                    fh2.try_resolve_err(EvalError::Signal(make_error_value(
                        &registry,
                        &format!("tcp_accept failed: {e}"),
                    )));
                }
            });
            Value::Future(fh)
        }
        "_primTcpRead:" => {
            // Requires: args[0] is a stream Handle. Returns a Future of a Str
            // (utf8-lossy; empty string means EOF). Spike: single read, ≤64 KiB.
            expect_arity(sel, 1, args.len())?;
            let stream = as_handle(ev, args.remove(0), sel)?;
            let fh = FutureHandle::new();
            let (fh2, os, registry) = (fh.clone(), ev.os.clone(), ev.registry.clone());
            may::go!(move || match os.tcp_read(&stream, 65536) {
                Ok(bytes) => {
                    let s = String::from_utf8_lossy(&bytes);
                    fh2.try_resolve_ok(Value::Str(Arc::from(s.as_ref())));
                }
                Err(e) => {
                    fh2.try_resolve_err(EvalError::Signal(make_error_value(
                        &registry,
                        &format!("tcp_read failed: {e}"),
                    )));
                }
            });
            Value::Future(fh)
        }
        "_primTcpWrite:with:" => {
            // Requires: args[0] stream Handle, args[1] a Str. Returns a Future of the
            // Int byte count written.
            expect_arity(sel, 2, args.len())?;
            let stream = as_handle(ev, args.remove(0), sel)?;
            let data = force_value(ev, args.remove(0))?;
            let bytes: Vec<u8> = match data {
                Value::Str(s) => s.as_bytes().to_vec(),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primTcpWrite:with: expects a Str, got {}",
                        other.kind()
                    )));
                }
            };
            let fh = FutureHandle::new();
            let (fh2, os, registry) = (fh.clone(), ev.os.clone(), ev.registry.clone());
            may::go!(move || match os.tcp_write(&stream, &bytes) {
                Ok(n) => {
                    fh2.try_resolve_ok(Value::Int(n as i64));
                }
                Err(e) => {
                    fh2.try_resolve_err(EvalError::Signal(make_error_value(
                        &registry,
                        &format!("tcp_write failed: {e}"),
                    )));
                }
            });
            Value::Future(fh)
        }
        "_primTcpClose:" => {
            // Requires: args[0] is a stream Handle. Synchronous shutdown; answers nil.
            expect_arity(sel, 1, args.len())?;
            let stream = as_handle(ev, args.remove(0), sel)?;
            ev.os.tcp_close(&stream);
            Value::Nil
        }
        _ => return Ok(None),
    }))
}

/// Multi-entity transaction primitives (C31).
#[inline(never)]
fn obj_prim_tx(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primBeginAtomicTx" => {
            // Begin a pessimistic (`#atomic`) transaction level. Root (no active tx)
            // allocates a fresh shared TxId; a nested level reuses the tree's TxId.
            // `GenTx::begin` picks root vs. child from `parent`. Sets
            // `current_tx_ctx` and returns the level as an opaque handle.
            expect_arity(sel, 0, args.len())?;
            let new_ctx = begin_tx_level(ev, TxMode::Atomic);
            *ev.current_tx_ctx.borrow_mut() = Some(new_ctx.clone());
            Value::Handle(Arc::new(new_ctx))
        }
        "_primBeginStmTx" => {
            // Begin an optimistic (`#stm`) transaction level (C32). If the requested
            // level is genuinely optimistic (not coerced to atomic by an atomic
            // ancestor — contagious-atomic), arm the version-bump gate for its
            // lifetime; the matching disarm is in commit/rollback.
            expect_arity(sel, 0, args.len())?;
            let new_ctx = begin_tx_level(ev, TxMode::Stm);
            if new_ctx.discipline() == TxMode::Stm {
                crate::entity::stm_active_enter();
            }
            *ev.current_tx_ctx.borrow_mut() = Some(new_ctx.clone());
            Value::Handle(Arc::new(new_ctx))
        }
        "_primStmPrepare" => {
            // STM two-phase commit, phase 1 (C32): the participant try-locks and
            // validates itself for the tagged transaction context, voting
            // commit/abort. The context comes from the tag on this message (set as
            // `current_tx_ctx` for the dispatch), exactly like `_primExitTxMode:`.
            expect_arity(sel, 0, args.len())?;
            let d = match &cx.handle.0.kind {
                InstanceKind::Entity(d) => d.as_ref(),
                _ => {
                    return Err(EvalError::TypeError(
                        "_primStmPrepare: receiver must be an entity".into(),
                    ));
                }
            };
            let ctx_id = match ev.current_tx_ctx.borrow().as_ref() {
                Some(ctx) => ctx.ctx_id(),
                None => {
                    return Err(EvalError::TypeError(
                        "_primStmPrepare: no active transaction context".into(),
                    ));
                }
            };
            Value::Bool(d.stm_try_prepare(ctx_id))
        }
        "_primCommitSharedTx:" => {
            // Finalize a transaction level. Root: commit the level buffer to the
            // store and clear `current_tx_ctx`. Child: absorb the child buffer into
            // its parent and restore the parent as the active context.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(ev, args.remove(0))?)?;
            let was_stm = ctx.discipline() == TxMode::Stm;
            match ctx.parent() {
                None => {
                    let buf = ctx.buffer().lock().unwrap().take();
                    if let Some(tx) = buf {
                        commit_shared_buffer(ev, tx)?;
                    }
                    *ev.current_tx_ctx.borrow_mut() = None;
                }
                Some(parent) => {
                    if let Some(child_buf) = ctx.buffer().lock().unwrap().take()
                        && let Some(parent_buf) = parent.buffer().lock().unwrap().as_mut()
                    {
                        parent_buf.absorb(child_buf);
                    }
                    *ev.current_tx_ctx.borrow_mut() = Some(parent);
                }
            }
            // Per-level disarm of the STM version-bump gate (armed per-level at
            // begin iff the level is genuinely optimistic).
            if was_stm {
                crate::entity::stm_active_exit();
            }
            Value::Nil
        }
        "_primRollbackSharedTx:" => {
            // Abort a transaction level. Root: drop the level buffer (its ops never
            // commit) and clear `current_tx_ctx`. Child: drop the child buffer and
            // restore the parent as the active context.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(ev, args.remove(0))?)?;
            let was_stm = ctx.discipline() == TxMode::Stm;
            ctx.buffer().lock().unwrap().take(); // drop = rollback (ops never commit)
            *ev.current_tx_ctx.borrow_mut() = ctx.parent();
            if was_stm {
                crate::entity::stm_active_exit();
            }
            Value::Nil
        }
        "_primTxIsStm:" => {
            // Whether the transaction handle's *effective* discipline is optimistic
            // (`#stm`). A requested `#stm` coerced to atomic by an atomic ancestor
            // (contagious-atomic) reports `false`, so the coordinator runs the
            // pessimistic commit path — the coerced level behaves as nested atomic.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(ev, args.remove(0))?)?;
            Value::Bool(ctx.discipline() == TxMode::Stm)
        }
        "_primEnrolledParticipants:" => {
            // Return this level's participants as a Cons list (or nil). A Cons is a
            // Value (synchronous dispatch), so the coordinator iterates it in its
            // own coroutine and the pvtExitTx: sends it makes are tagged.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(ev, args.remove(0))?)?;
            let parts: Vec<Value> = ctx
                .participants()
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .map(Value::Instance)
                .collect();
            build_cons_list(ev, parts)?
        }
        "_primExitTxMode:" => {
            // Participant-side finalize of its current transaction frame, driven by
            // the coordinator. `outcome` is #commit or #rollback. The transaction
            // context is taken from the tag on this (pvtExitTx) message.
            expect_arity(sel, 1, args.len())?;
            let outcome = match force_value(ev, args.remove(0))? {
                Value::Symbol(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primExitTxMode: expects #commit or #rollback, got {}",
                        other.kind()
                    )));
                }
            };
            let d = match &cx.handle.0.kind {
                InstanceKind::Entity(d) => d.as_ref(),
                _ => {
                    return Err(EvalError::TypeError(
                        "_primExitTxMode: receiver must be an entity".into(),
                    ));
                }
            };
            // The tx being finalized is the tag on this (pvtExitTx) message.
            let Some(ctx) = ev.current_tx_ctx.borrow().clone() else {
                return Ok(Some(Value::Nil)); // defensive: no active tx context
            };
            let ctx_id = ctx.ctx_id();
            if !d.has_frame_for(ctx_id) {
                return Ok(Some(Value::Nil)); // defensive: nothing to finalize here
            }
            match outcome.as_ref() {
                "commit" => {
                    let parent_ctx_id = ctx.parent().map(|p| p.ctx_id());
                    match d.tx_commit_frame(ctx_id, parent_ctx_id) {
                        FrameCommitOutcome::RootCommitted(buf) => {
                            if let Some(ctx_buf) = ctx.buffer().lock().unwrap().as_mut() {
                                ctx_buf.absorb(buf);
                            }
                        }
                        FrameCommitOutcome::PassedUpTo(_) => {
                            // Lock passed up: become a participant of the parent so
                            // its later commit/rollback reaches this entity.
                            if let Some(parent) = ctx.parent() {
                                parent
                                    .participants()
                                    .lock()
                                    .unwrap()
                                    .push(cx.handle.clone());
                            }
                        }
                        FrameCommitOutcome::MergedIntoAncestor => {}
                    }
                }
                "rollback" => d.tx_abort_frame(ctx_id),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primExitTxMode: unknown outcome #{other}"
                    )));
                }
            }
            Value::Nil
        }
        _ => return Ok(None),
    }))
}

/// Explicit Future reification (C31): `_primNewFutureHandle`,
/// `_primResolveFuture:value:`, `_primRejectFuture:error:`.
#[inline(never)]
fn obj_prim_futures(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primNewFutureHandle" => {
            expect_arity(sel, 0, args.len())?;
            Value::Future(FutureHandle::new())
        }
        "_primResolveFuture:value:" => {
            expect_arity(sel, 2, args.len())?;
            let h = args.remove(0);
            let v = force_value(ev, args.remove(0))?;
            match h {
                Value::Future(fh) => Value::Bool(fh.try_resolve_ok(v)),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primResolveFuture:value: expects a future handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        "_primRejectFuture:error:" => {
            expect_arity(sel, 2, args.len())?;
            let h = args.remove(0);
            let e = force_value(ev, args.remove(0))?;
            match h {
                Value::Future(fh) => Value::Bool(fh.try_resolve_err(EvalError::Signal(e))),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primRejectFuture:error: expects a future handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        _ => return Ok(None),
    }))
}

/// Extract a HandlerContext's handler pairs from its `pairs` ivar.
///
/// Requires: `handle` is a HandlerContext instance whose `pairs` ivar is a
///   Cons-of-Association linked list `Cons(head: cls -> blk, tail: rest)`, newest
///   at the head.
/// Ensures: returns `(condition class, handler block)` pairs ordered oldest-first,
///   ready to push onto `ev.handler_chain` so that `signal_condition`'s reverse
///   walk matches the most-recently-registered handler first.
pub(crate) fn extract_handler_pairs(
    ev: &Evaluator,
    handle: &InstanceHandle,
) -> Result<Vec<crate::eval::HandlerPair>, EvalError> {
    let pairs_val = match &handle.0.kind {
        InstanceKind::ValueObject(o) => o.get_slot("pairs").unwrap_or(Value::Nil),
        InstanceKind::Entity(d) => d.data.get_slot("pairs").unwrap_or(Value::Nil),
        InstanceKind::ClassEntity(_) => {
            return Err(EvalError::TypeError(
                "_primHCRun: requires a HandlerContext instance".into(),
            ));
        }
    };

    let mut handler_pairs: Vec<crate::eval::HandlerPair> = Vec::new();
    let mut node = force_value(ev, pairs_val)?;
    while let Value::Instance(node_h) = node {
        let node_obj = match &node_h.0.kind {
            InstanceKind::ValueObject(o) => o.clone(),
            InstanceKind::Entity(d) => d.data.clone(),
            InstanceKind::ClassEntity(_) => break,
        };
        // head is an Association(key=cls, value=blk); tail is rest.
        let head_val = force_value(ev, node_obj.get_slot("head").unwrap_or(Value::Nil))?;
        let tail = force_value(ev, node_obj.get_slot("tail").unwrap_or(Value::Nil))?;

        let assoc_h = match head_val {
            Value::Instance(h) => h,
            _ => {
                node = tail;
                continue;
            }
        };
        let assoc_obj = match &assoc_h.0.kind {
            InstanceKind::ValueObject(o) => o.clone(),
            InstanceKind::Entity(d) => d.data.clone(),
            InstanceKind::ClassEntity(_) => {
                node = tail;
                continue;
            }
        };
        let cls_val = force_value(ev, assoc_obj.get_slot("key").unwrap_or(Value::Nil))?;
        let blk_val = force_value(ev, assoc_obj.get_slot("value").unwrap_or(Value::Nil))?;

        let cls = match cls_val {
            Value::Instance(ch) => ch.0.class.clone(),
            _ => {
                node = tail;
                continue;
            }
        };
        let blk = match blk_val {
            Value::Block(b) => b,
            _ => {
                node = tail;
                continue;
            }
        };
        handler_pairs.push((cls, blk));
        node = tail;
    }
    // Reverse so oldest pair is pushed first.
    handler_pairs.reverse();
    Ok(handler_pairs)
}

/// Core signal dispatch: walk the handler_chain, then apply unhandled policy.
///
/// Requires: `condition` is a `Value::Instance` whose class is a subclass of Condition.
/// Ensures: if a matching non-unwinding handler is found its return value is returned;
///   if none match, applies unhandled policy (Condition → nil, Warning → log+nil,
///   Error → raise EvalError::Signal).
pub(crate) fn signal_condition(
    ev: &Evaluator,
    condition: Value,
    env: &mut Env,
) -> Result<Value, EvalError> {
    #[cfg(feature = "step_probe")]
    {
        let sp_probe = 0u8;
        ev.probe
            .sample_signal_condition(&sp_probe as *const u8 as usize);
    }
    let cond_class = match &condition {
        Value::Instance(h) => h.0.class.clone(),
        _ => return Err(EvalError::Signal(condition)),
    };

    // Innermost (highest-index) matching handler.
    let idx = ev
        .handler_chain
        .borrow()
        .iter()
        .rposition(|(hc, _)| crate::class::Class::is_subclass_of(&cond_class, hc));
    if let Some(i) = idx {
        // Common-Lisp handler-scoping: run the handler with itself and all inner
        // handlers disabled (only outer clusters active), so a re-signal from the
        // handler defers to an outer handler rather than re-matching this one.
        // Restore the disabled handlers afterward.
        let (handler_class, handler_block, removed) = {
            let mut chain = ev.handler_chain.borrow_mut();
            let removed = chain.split_off(i);
            (removed[0].0.clone(), removed[0].1.clone(), removed)
        };
        let resumable = class_is_resumable(ev, &handler_class);
        let outcome = ev.eval_block(&handler_block, vec![condition], env);
        ev.handler_chain.borrow_mut().extend(removed);
        // Disposition (docs/condition_system_guide.md §2): an explicit `resume:`
        // always resumes; `return:`/`retry`/re-signal propagate. On *normal*
        // handler completion the default depends on the *handler's declared class*:
        // a resumable class resumes (deliver the value to the signal point); a
        // non-resumable one (an Error handler) ABORTS — a local unwind, expressed
        // as an implicit `return:` of the handler's value.
        return match outcome {
            Err(EvalError::Resume(v)) => Ok(v),
            Ok(v) if resumable => Ok(v),
            Ok(v) => Err(EvalError::ReturnFromBlock(v)),
            Err(e) => Err(e),
        };
    }

    apply_unhandled_policy(ev, condition)
}

/// Whether a handler that completes without an explicit restart resumes (`true`)
/// or aborts (`false`) by default.
///
/// Keyed on the **handler's declared class** — the class named at the handler site
/// (`on: SomeClass do:` / `HandlerContext on: SomeClass do:`), NOT the concrete
/// class of the signalled instance. This makes disposition static and predictable
/// for the handler author: a handler declared for `Error` always aborts, even when
/// the signalled condition is a resumable `Error` subclass. See
/// `docs/condition_system_guide.md` §2.
///
/// Reads the class-side `isResumable` predicate by **constant-folding** its body
/// (`^true` / `^false`) — pure AST inspection, so it never re-enters `step_loop`
/// and adds no Rust stack depth (conditions subclass `Value`, so a real send would
/// run inline). Falls back to the `Error`-hierarchy default when the predicate is
/// absent or its body is not a literal `^true`/`^false`.
pub(crate) fn class_is_resumable(ev: &Evaluator, handler_class: &Arc<Class>) -> bool {
    if let Some((method, _)) = handler_class.lookup_class_method("isResumable")
        && method.body.len() == 1
        && let crate::ast::Stmt::Return(crate::ast::Expr::Lit(crate::ast::Literal::Bool(b))) =
            &method.body[0].node
    {
        return *b;
    }
    // Structural fallback: `Error` (and subclasses) non-resumable, else resumable.
    !ev.registry
        .kernel()
        .lookup_class("Error")
        .map(|h| crate::class::Class::is_subclass_of(handler_class, &h.0.class))
        .unwrap_or(false)
}

/// Apply the unhandled-condition policy by class: `Error` → raise
/// `EvalError::Signal`; `Warning` → log per `LoggingPolicy` and answer nil;
/// any other `Condition` → answer nil.
pub(crate) fn apply_unhandled_policy(ev: &Evaluator, condition: Value) -> Result<Value, EvalError> {
    let cond_class = match &condition {
        Value::Instance(h) => h.0.class.clone(),
        _ => return Err(EvalError::Signal(condition)),
    };
    let kernel = ev.registry.kernel();
    let is_error = kernel
        .lookup_class("Error")
        .map(|h| crate::class::Class::is_subclass_of(&cond_class, &h.0.class))
        .unwrap_or(false);
    let is_warning = !is_error
        && kernel
            .lookup_class("Warning")
            .map(|h| crate::class::Class::is_subclass_of(&cond_class, &h.0.class))
            .unwrap_or(false);

    if is_error {
        Err(EvalError::Signal(condition))
    } else if is_warning {
        let policy = kernel
            .lookup_class("Warning")
            .and_then(|h| h.0.class.lookup_class_var_owner("LoggingPolicy"))
            .and_then(|owner| owner.class_var_storage.read().get("LoggingPolicy").cloned());
        let message = condition_message(&condition);
        match policy.as_ref() {
            Some(Value::Symbol(s)) if s.as_ref() == "suppress" => {}
            Some(Value::Symbol(s)) if s.as_ref() == "stdout" => {
                println!("WARNING: {message}");
            }
            _ => {
                eprintln!("WARNING: {message}");
            }
        }
        Ok(Value::Nil)
    } else {
        Ok(Value::Nil)
    }
}

/// Extract the `message` ivar from a condition instance, or fall back to a string representation.
pub(crate) fn condition_message(condition: &Value) -> String {
    if let Value::Instance(h) = condition {
        let obj = match &h.0.kind {
            InstanceKind::ValueObject(o) => o.get_slot("message"),
            InstanceKind::Entity(d) => d.data.get_slot("message"),
            InstanceKind::ClassEntity(_) => None,
        };
        if let Some(Value::Str(s)) = obj {
            return s.to_string();
        }
    }
    format!("{condition}")
}

fn is_kind_of(class: &Arc<Class>, target: &Arc<Class>) -> bool {
    if Arc::ptr_eq(class, target) {
        return true;
    }
    if let Some(sc) = &class.superclass {
        is_kind_of(sc, target)
    } else {
        false
    }
}

/// Maps a value to its kernel class entity handle for `_primClass` / `_primIsKindOf:`.
///
/// Requires: `v` is not a `Value::Future` (caller must force first).
/// Ensures: returns `Some(handle)` where handle is the class entity for `v`'s type,
///   or `None` if no kernel class is available (ClassEntity receiver, Namespace).
fn value_to_class_handle(ev: &Evaluator, v: &Value) -> Option<InstanceHandle> {
    let class_name = match v {
        Value::Bool(b) => {
            if *b {
                "True"
            } else {
                "False"
            }
        }
        Value::Int(_) => "MachineInteger",
        Value::LargeInt(_) => "ArbitraryInteger",
        Value::Float(_) => "Float",
        Value::Str(_) => "String",
        Value::Symbol(_) => "Symbol",
        Value::Block(_) => "Block",
        Value::Nil => "Nil",
        Value::Instance(h) => {
            return match &h.0.kind {
                InstanceKind::ValueObject(obj) => class_handle_of(obj).ok(),
                InstanceKind::Entity(d) => class_handle_of(&d.data).ok(),
                InstanceKind::ClassEntity(_) => None,
            };
        }
        _ => return None,
    };
    ev.registry.kernel().lookup_class(class_name)
}

/// Assert `(entity, __instance_of__, class_entity)` in a fresh one-shot transaction.
///
/// Requires: `inst` is an Entity kind handle.
/// Ensures: a `__instance_of__` fact is committed to the shared store. Silent no-op
///   if the class has no backing class-entity (e.g. during bootstrap).
fn assert_instance_of(ev: &Evaluator, inst: &InstanceHandle, class: &Arc<Class>) {
    let entity_id = match &inst.0.kind {
        InstanceKind::Entity(d) => d.id,
        _ => return,
    };
    let Some(class_eid) = class_entity_id_of(class) else {
        return;
    };
    let mut tx = ev.store.begin_owned(ExecContext::Entity(entity_id));
    let _ = tx.assert_single(Fact {
        subject: entity_id,
        predicate: ev.registry.instance_of_pred,
        object: Term::EntityRef(class_eid),
        asserter: entity_id,
        valid: None,
    });
    let _ = tx.commit();
}

/// Extract the EntityId of the class-entity backing `class`, if available.
fn class_entity_id_of(class: &Arc<Class>) -> Option<EntityId> {
    class
        .class_object
        .get()
        .and_then(|w| w.upgrade())
        .and_then(|s| match &s.kind {
            InstanceKind::ClassEntity(d) => Some(d.id),
            _ => None,
        })
}

/// Build an `Array` entity pre-filled with `values`.
///
/// Allocates a fresh Array entity, fills its indexed Pointer slots directly at
/// the Rust level (no cross-entity dispatch), tracks it with the evaluator, and
/// returns it. Used by `historyOf:` and `_primInstancesOf:`.
fn build_array(ev: &Evaluator, values: Vec<Value>) -> Result<Value, EvalError> {
    let array_class_handle = ev
        .registry
        .kernel()
        .lookup_class("Array")
        .ok_or_else(|| EvalError::TypeError("Array not in kernel".into()))?;
    let array_class = array_class_handle.0.class.clone();

    let arr = spawn_entity_bare(
        array_class,
        ev.registry.clone(),
        crate::env::Env::new(),
        Some(values.len()),
        ev.store.clone_handle(),
    );
    ev.track_entity(&arr);

    if let InstanceKind::Entity(d) = &arr.0.kind
        && let Some(IndexedStorage::Pointer(slots)) = &d.data.indexed
    {
        let mut guard = slots.lock();
        for (i, v) in values.into_iter().enumerate() {
            guard[i] = v;
        }
    }

    Ok(Value::Instance(arr))
}

/// Build a `Cons` linked list (head-first) from `items`, or `Value::Nil` if empty.
///
/// Returns a Value-object chain (synchronous dispatch), so a caller can iterate
/// it with `do:` in its own coroutine — used for transaction participant lists,
/// where the iterating coordinator must send tagged messages from its own
/// coroutine (an `Array` entity's `do:` would run in the array's coroutine, where
/// `current_tx_ctx` is unset and the sends would be untagged).
fn build_cons_list(ev: &Evaluator, items: Vec<Value>) -> Result<Value, EvalError> {
    let cons_class = ev
        .registry
        .kernel()
        .lookup_class("Cons")
        .ok_or_else(|| EvalError::TypeError("Cons not in kernel".into()))?
        .0
        .class
        .clone();
    let mut list = Value::Nil;
    for item in items.into_iter().rev() {
        let obj = Arc::new(Object::new(cons_class.clone()));
        obj.set_slot(Arc::from("head"), item);
        obj.set_slot(Arc::from("tail"), list);
        list = Value::Instance(InstanceHandle(Arc::new(InstanceState {
            class: cons_class.clone(),
            kind: InstanceKind::ValueObject(obj),
        })));
    }
    Ok(list)
}

/// Convert a slice of `StoredFact` to an `Array` of `Association(ts_ms → value)`.
///
/// Each Association has `key = epoch-milliseconds (Int)` and `value = resolved object`.
fn history_facts_to_array(ev: &Evaluator, facts: &[StoredFact]) -> Result<Value, EvalError> {
    let assocs: Vec<Value> = facts
        .iter()
        .map(|f| {
            let ts_ms = Value::Int((f.valid.start.0 / 1_000_000) as i64);
            let val = ev.resolve_term(&f.object);
            make_association(ev, ts_ms, val)
        })
        .collect::<Result<_, _>>()?;
    build_array(ev, assocs)
}

/// Build an Association ValueObject with `key` and `value` set.
fn make_association(ev: &Evaluator, key: Value, value: Value) -> Result<Value, EvalError> {
    let assoc_class_handle = ev
        .registry
        .kernel()
        .lookup_class("Association")
        .ok_or_else(|| EvalError::TypeError("Association not in kernel".into()))?;
    let assoc_class = assoc_class_handle.0.class.clone();
    let obj = Arc::new(Object::new(assoc_class.clone()));
    obj.set_slot(Arc::from("key"), key);
    obj.set_slot(Arc::from("value"), value);
    Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: assoc_class,
        kind: InstanceKind::ValueObject(obj),
    }))))
}

// ── Namespace ─────────────────────────────────────────────────────────────────

fn dispatch_namespace(
    ev: &Evaluator,
    ns: Arc<Namespace>,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match selector.as_ref() {
        "do:" => {
            expect_arity(&selector, 1, args.len())?;
            let block = match args.remove(0) {
                Value::Block(b) => b,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "do: requires a Block, got {}",
                        other.kind()
                    )));
                }
            };
            ev.with_namespace(ns, |ev| ev.eval_block(&block, vec![], env))
        }
        "name" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Symbol(ns.name.clone()))
        }
        // Still Rust: Namespace is routing-bound (§6 OQ-6). Renamed with the rest
        // of the family in S5 so the language has ONE rendering selector.
        "asString" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(Arc::from(
                format!("Namespace({})", ns.name).as_str(),
            )))
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Namespace",
            selector,
        }),
    }
}

// ── Class (class-side dispatch) ───────────────────────────────────────────────

fn dispatch_class(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let entity_id = match &handle.0.kind {
        InstanceKind::ClassEntity(d) => d.id,
        _ => unreachable!("dispatch_class called on non-ClassEntity instance"),
    };

    // Self-send or structural class-definition message: dispatch synchronously.
    // `subclass:*` must run in the caller's context so that env bindings and
    // namespace registration are immediately visible to the next statement.
    // Value-class methods (classes whose instances are passive ValueObjects) are also
    // dispatched synchronously: they carry no actor-mailbox semantics, so
    // cross-entity dispatch would be wrong.
    // `=`/`~=` and `==`/`~~` on class objects are pure identity checks — they must
    // be synchronous so callers don't receive a Future where a Bool is expected.
    // (`==`/`~~` joined the list in C34 S2, when the identity pair was introduced;
    // without them `Array == Array` answered a Future while `Array = Array`
    // answered a Bool.)
    let current = ev.current_entity_id.get();
    if current == Some(entity_id)
        || selector.starts_with("subclass:")
        || selector.starts_with("vmSubclass:")
        || handle.0.class.inherits_is_value()
        || matches!(
            selector.as_ref(),
            "=" | "~=" | "==" | "~~" | "asString" | "name"
        )
    {
        let class = handle.0.class.clone();
        if let Some((method, def_class)) = class.lookup_class_method(&selector) {
            return ev.invoke_class_method(handle, def_class, method, args, env);
        }
        return dispatch_class_builtin(ev, handle, selector, args, env);
    }

    // Cross-entity behavioral send: enqueue and return Future.
    // Snapshot the caller's handler_chain so inherited HC handlers propagate.
    let chain_snapshot = {
        let chain = ev.handler_chain.borrow();
        if chain.is_empty() {
            vec![]
        } else {
            chain.clone()
        }
    };
    let tx_tag = ev.current_tx_ctx.borrow().clone();
    let future = match &handle.0.kind {
        InstanceKind::ClassEntity(d) => send_to_entity(d, selector, args, chain_snapshot, tx_tag),
        _ => unreachable!(),
    };
    Ok(Value::Future(future))
}

/// Built-in class-side dispatch that bypasses user-defined class methods.
///
/// Called directly from `dispatch_class` (after the user-method check) and from
/// `send_super` when a class-side super send finds no user-defined method in the
/// superclass chain — allowing `super new` / `super new:` to reach Rust primitives
/// without triggering the receiver class's own `new` override.
pub(crate) fn dispatch_class_builtin(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let class = handle.0.class.clone();
    let self_val = Value::Instance(handle);
    match selector.as_ref() {
        // A class object has no content beyond its allocation, so all four are the
        // same identity test. `=`/`~=` stay Rust arms for now: unlike every other
        // receiver kind, a class does NOT reach `Object >> =` — `dispatch_class`
        // resolves against CLASS-side methods (`lookup_class_method`), so migrating
        // these needs `Object class >>` definitions and belongs with the class-side
        // work deferred to §6 OQ-6.
        "=" | "==" => {
            expect_arity(&selector, 1, args.len())?;
            Ok(Value::Bool(self_val == force_value(ev, args[0].clone())?))
        }
        "~=" | "~~" => {
            expect_arity(&selector, 1, args.len())?;
            Ok(Value::Bool(self_val != force_value(ev, args.remove(0))?))
        }
        "_primNew" => {
            expect_arity(&selector, 0, args.len())?;
            // Indexed classes get zero-size indexed storage; non-indexed get plain Object.
            let indexed_size = if class.indexed_slot_kind.is_some() {
                Some(0usize)
            } else {
                None
            };
            if class.inherits_is_value() {
                // Value subclasses: create and freeze immediately. Any post-construction
                // ivar write will fail with the frozen error. Correct code uses _primNewValue:.
                let obj = match indexed_size {
                    Some(n) => Arc::new(Object::new_with_indexed(class.clone(), n)),
                    None => Arc::new(Object::new(class.clone())),
                };
                obj.freeze();
                return Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
                    class,
                    kind: InstanceKind::ValueObject(obj),
                }))));
            }
            {
                let inst = spawn_entity_bare(
                    class.clone(),
                    ev.registry.clone(),
                    crate::env::Env::new(),
                    indexed_size,
                    ev.store.clone_handle(),
                );
                ev.track_entity(&inst);
                assert_instance_of(ev, &inst, &class);
                Ok(Value::Instance(inst))
            }
        }
        "_primNewValue:" => {
            // Construct a frozen Value-subclass instance via an initialization block.
            //
            // Requires: args[0] is a 1-arg Block; receiver class inherits is_value.
            // Ensures: creates a new (unfrozen) ValueObject (zero-size indexed storage
            //   for indexed classes), evaluates the block with the instance as argument
            //   (mutations permitted during block), freezes on block return, returns
            //   the frozen instance.
            expect_arity(&selector, 1, args.len())?;
            if !class.inherits_is_value() {
                return Err(EvalError::TypeError(
                    "_primNewValue: requires a Value subclass".into(),
                ));
            }
            let blk = as_block(ev, args.remove(0), &selector)?;
            let indexed = if class.indexed_slot_kind.is_some() {
                Some(0)
            } else {
                None
            };
            let inst = make_value_instance(class, indexed);
            ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?;
            freeze_value_instance(&inst);
            Ok(Value::Instance(inst))
        }
        "_primNewValueIndexed:init:" => {
            // Construct a frozen indexed Value-subclass instance via an initialization block.
            //
            // Requires: args[0] is an Int size; args[1] is a 1-arg Block;
            //   receiver class inherits is_value and declares indexedSlots:.
            // Ensures: creates an indexed ValueObject of the given size, evaluates
            //   the block with the instance as argument, freezes on block return.
            expect_arity(&selector, 2, args.len())?;
            if !class.inherits_is_value() {
                return Err(EvalError::TypeError(
                    "_primNewValueIndexed:init: requires a Value subclass".into(),
                ));
            }
            if class.indexed_slot_kind.is_none() {
                return Err(EvalError::TypeError(
                    "_primNewValueIndexed:init: requires class to declare indexedSlots:".into(),
                ));
            }
            let n = as_int(ev, args.remove(0), &selector)?;
            let blk = as_block(ev, args.remove(0), &selector)?;
            let inst = make_value_instance(class, Some(n.max(0) as usize));
            ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?;
            freeze_value_instance(&inst);
            Ok(Value::Instance(inst))
        }
        "_primNewTrait:" => {
            // Create a fresh empty trait named by the Symbol argument, binding the
            // name in the current scope (like `subclass:`). Methods are added
            // afterward via `aTrait >> selector [...]` (install_method_def).
            expect_arity(&selector, 1, args.len())?;
            let name = match force_value(ev, args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                _ => {
                    return Err(EvalError::TypeError(
                        "_primNewTrait: name must be a Symbol".into(),
                    ));
                }
            };
            let tv = make_trait_value(ev, crate::traits::TraitData::new_empty(name.clone()))?;
            bind_trait_name(ev, name, tv.clone(), env);
            Ok(tv)
        }
        "_primDefineTrait:from:" => {
            // Name a composed trait expression and bind it in the current scope.
            expect_arity(&selector, 2, args.len())?;
            let name = match force_value(ev, args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                _ => {
                    return Err(EvalError::TypeError(
                        "_primDefineTrait:from: name must be a Symbol".into(),
                    ));
                }
            };
            let src = force_value(ev, args.remove(0))?;
            let data = trait_data_of(&src).ok_or_else(|| {
                EvalError::TypeError("_primDefineTrait:from: source is not a Trait".into())
            })?;
            *data.name.write() = name.clone();
            bind_trait_name(ev, name, src.clone(), env);
            Ok(src)
        }
        "_primScheduleTimer:block:" => {
            // Requires: args[0] is Value::Int (≥ 0, milliseconds); args[1] is Value::Block.
            // Ensures: after ms milliseconds (if not cancelled), evaluates the block in a
            //   fresh may coroutine inheriting the current continuation context. The block
            //   is responsible for all Smol-level dispatch (e.g. Timer >> pvtTimerFired).
            //   Returns Value::Handle opaque to callers.
            expect_arity(&selector, 2, args.len())?;
            let ms = as_int(ev, args.remove(0), &selector)?.max(0) as u64;
            let blk = as_block(ev, args.remove(0), &selector)?;
            let ctx = ev.fork_context();
            let callback = Box::new(move || {
                may::go!(move || {
                    let ev = ctx.into_evaluator();
                    let mut env = blk.env.clone();
                    let _ = ev.eval_block(&blk, vec![], &mut env);
                });
            });
            let handle = ev.os.schedule_timer(ms, callback);
            Ok(Value::Handle(handle))
        }
        "_primTcpListen:" => {
            // Class-side (sent to `self` = the listener's class in `onPort:`).
            // Requires: args[0] is a port Int in [0, 65535]. Returns Value::Handle.
            expect_arity(&selector, 1, args.len())?;
            let port = as_int(ev, args.remove(0), &selector)?;
            let port = u16::try_from(port).map_err(|_| {
                EvalError::TypeError("_primTcpListen: port out of range 0..=65535".into())
            })?;
            match ev.os.tcp_listen(port) {
                Ok(h) => Ok(Value::Handle(h)),
                Err(e) => Err(EvalError::Signal(make_error_value(
                    &ev.registry,
                    &format!("tcp_listen failed: {e}"),
                ))),
            }
        }
        "_primNew:" => {
            expect_arity(&selector, 1, args.len())?;
            let n = as_int(ev, args.remove(0), &selector)?;
            if class.indexed_slot_kind.is_none() {
                return Err(EvalError::TypeError(
                    "_primNew: requires class to declare indexedSlots:".into(),
                ));
            }
            if class.inherits_is_value() {
                let obj = Arc::new(Object::new_with_indexed(class.clone(), n.max(0) as usize));
                obj.freeze();
                return Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
                    class,
                    kind: InstanceKind::ValueObject(obj),
                }))));
            }
            {
                let inst = spawn_entity_bare(
                    class.clone(),
                    ev.registry.clone(),
                    crate::env::Env::new(),
                    Some(n.max(0) as usize),
                    ev.store.clone_handle(),
                );
                ev.track_entity(&inst);
                assert_instance_of(ev, &inst, &class);
                Ok(Value::Instance(inst))
            }
        }
        "_primInstancesOf:" => {
            expect_arity(&selector, 1, args.len())?;
            let target_handle = match force_value(ev, args.remove(0))? {
                Value::Instance(h) => h,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primInstancesOf: argument must be a class, got {}",
                        other.kind()
                    )));
                }
            };
            let target_class = target_handle.0.class.clone();
            let instances: Vec<Value> = ev
                .registry
                .instance_entities
                .read()
                .values()
                .filter(|state| Arc::ptr_eq(&state.class, &target_class))
                .map(|state| Value::Instance(InstanceHandle(state.clone())))
                .collect();
            build_array(ev, instances)
        }
        "name" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(class.name.clone()))
        }
        // Still Rust: class-side dispatch is routing-bound (§6 OQ-6). Renamed with
        // the rest of the family in S5, and the `println!` dropped — rendering
        // never writes to the console now; `print`/`printNl` do that.
        "asString" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(class.name.clone()))
        }
        sel if sel.contains("subclass:") || sel.starts_with("vmSubclass:") => {
            // Extract the handle back from self_val to pass the Arc<Class> as superclass.
            // (`vmSubclass:` has a capital S, so `contains("subclass:")` misses it.)
            handle_subclass(ev, class, selector, args, env)
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Class",
            selector,
        }),
    }
}

fn handle_subclass(
    ev: &Evaluator,
    superclass: Arc<Class>,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // `selector` is something like "subclass:instanceVariableNames:classVariableNames:"
    // Split on ':' boundaries to get the individual keyword parts.
    // Each keyword ends with ':', so split_inclusive(':') gives us each kw fragment.
    let keywords: Vec<&str> = selector.split_inclusive(':').collect();

    let mut class_name: Option<Arc<str>> = None;
    let mut ivar_names: Vec<Arc<str>> = vec![];
    let mut class_var_names: Vec<Arc<str>> = vec![];
    let mut indexed_slot_kind: Option<IndexedSlotKind> = None;
    let mut used_trait: Option<Arc<crate::traits::TraitData>> = None;

    for (kw, arg) in keywords.iter().zip(args.iter()) {
        match *kw {
            "uses:" => {
                used_trait = Some(trait_data_of(arg).ok_or_else(|| {
                    EvalError::TypeError("uses: argument must be a Trait".into())
                })?);
            }
            "subclass:" | "vmSubclass:" => match arg {
                Value::Symbol(s) => class_name = Some(s.clone()),
                _ => {
                    return Err(EvalError::TypeError(format!(
                        "{kw} argument must be a Symbol"
                    )));
                }
            },
            "instanceVariableNames:" => match arg {
                Value::Str(s) => {
                    ivar_names = s
                        .split_whitespace()
                        .filter(|s| !s.is_empty())
                        .map(Arc::from)
                        .collect();
                }
                _ => {
                    return Err(EvalError::TypeError(
                        "instanceVariableNames: argument must be a String".into(),
                    ));
                }
            },
            "classVariableNames:" => match arg {
                Value::Str(s) => {
                    class_var_names = s
                        .split_whitespace()
                        .filter(|s| !s.is_empty())
                        .map(Arc::from)
                        .collect();
                }
                _ => {
                    return Err(EvalError::TypeError(
                        "classVariableNames: argument must be a String".into(),
                    ));
                }
            },
            "indexedSlots:" => match arg {
                Value::Symbol(s) => match s.as_ref() {
                    "none" => indexed_slot_kind = None,
                    "pointer" => indexed_slot_kind = Some(IndexedSlotKind::Pointer),
                    "byte" => indexed_slot_kind = Some(IndexedSlotKind::Byte),
                    "float64" => indexed_slot_kind = Some(IndexedSlotKind::Float64),
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "indexedSlots: unknown kind #{s}"
                        )));
                    }
                },
                _ => {
                    return Err(EvalError::TypeError(
                        "indexedSlots: argument must be a Symbol".into(),
                    ));
                }
            },
            _ => {} // unknown keywords silently ignored
        }
    }

    let name =
        class_name.ok_or_else(|| EvalError::TypeError("subclass: name is required".into()))?;

    // Encoded-leaf closedness (docs/numeric_tower_plan.md §4b). `vmSubclass:` defines
    // a VM-encoded value class (closed by construction); it is reserved for the fixed
    // set of names the VM actually has an encoding for — users cannot invent one.
    // Plain `subclass:` cannot subclass such a leaf: its fixed encoding carries no
    // room for a subclass tag. Abstract tower classes (Magnitude/Number/Integer) are
    // not leaves, so they stay open.
    let is_vm = selector.starts_with("vmSubclass:");
    if is_vm {
        if !is_encoded_leaf_name(&name) {
            return Err(EvalError::TypeError(format!(
                "vmSubclass: is reserved for VM-encoded value classes; #{name} has no VM encoding"
            )));
        }
    } else if is_encoded_leaf_name(&superclass.name) {
        return Err(EvalError::TypeError(format!(
            "cannot subclass the VM-encoded value class {}: encoded scalars are closed \
             (add methods or use a trait/wrapper instead)",
            superclass.name
        )));
    }

    // Validate ivar name uniqueness across the superclass chain.
    let super_ivars = superclass.all_instance_vars();
    for ivar in &ivar_names {
        if super_ivars.iter().any(|v| v == ivar) {
            return Err(EvalError::TypeError(format!(
                "instance variable '{ivar}' already declared in a superclass"
            )));
        }
    }

    let class_var_storage: HashMap<Arc<str>, Value> = class_var_names
        .iter()
        .map(|n| (n.clone(), Value::Nil))
        .collect();

    // Inherit is_value from the superclass chain. Mixed-hierarchy subclassing
    // (actor subclassing a value class or vice versa) is a compile-time error.
    let super_is_value = superclass.inherits_is_value();
    // The selector determines whether this is a value or actor declaration.
    // `Value subclass:` → is_value: true; `Object subclass:` (any non-Value) → inherits.
    // We detect this by checking if the superclass is the built-in Value class.
    let declared_is_value = super_is_value; // inherit by default

    let new_class = Arc::new(Class {
        name: name.clone(),
        superclass: Some(superclass),
        own_instance_vars: ivar_names,
        own_class_vars: class_var_names,
        class_var_storage: RwLock::new(class_var_storage),
        indexed_slot_kind,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: declared_is_value,
        class_object: std::sync::OnceLock::new(),
        namespace: std::sync::OnceLock::new(),
    });
    // Flatten a composed trait's provided methods into the new class's method dict
    // (docs/traits_plan.md §1). Class-defined methods arrive on later statements and
    // overwrite these entries, giving the required `class > trait > super` precedence.
    // A conflict marker is flattened as a sentinel that raises `TraitConflict` when
    // sent; a later class-defined method (or a non-conflicting override) replaces it.
    // NOTE (deferred): fail-fast validation of unresolved conflicts / unmet
    // requirements at composition time needs an active-environment feedback loop and
    // must account for cross-file method extensions — see docs/open_questions.md.
    if let Some(trait_data) = &used_trait {
        let mut dict = new_class.methods.write();
        for (sel, entry) in trait_data.methods.read().iter() {
            let method = if entry.is_conflict {
                Arc::new(conflict_sentinel_method(&entry.method.pattern, sel)?)
            } else {
                entry.method.clone()
            };
            dict.insert(sel.clone(), method);
        }
    }
    let class_handle = spawn_class_entity(
        new_class.clone(),
        ev.registry.clone(),
        Env::new(),
        ev.store.clone_handle(),
    );
    new_class
        .class_object
        .set(Arc::downgrade(&class_handle.0))
        .ok();
    ev.registry.register_class_entity(&class_handle);
    ev.registry.register_class_predicates(&new_class);

    let active_ns = ev.active_namespace();
    active_ns.register(class_handle.clone());
    // Kernel classes get a global binding so they're always in scope by simple name.
    // Non-Kernel classes are scoped to the current frame (e.g. inside `Namespace do: [...]`);
    // outside that scope they are only reachable via `Ns::ClassName`.
    if Arc::ptr_eq(&active_ns, &ev.registry.kernel()) {
        env.define_global(name, Value::Instance(class_handle.clone()));
    } else {
        env.define(name, Value::Instance(class_handle.clone()));
    }
    Ok(Value::Instance(class_handle))
}

#[cfg(test)]
#[path = "tests/builtins.rs"]
mod tests;
