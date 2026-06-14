use std::collections::HashMap;
use std::sync::Arc;

/// Cooperative yield interval: one yield every N completed message dispatches.
/// Matches the step-loop constant in eval_iterative so loop primitives and the
/// iterative evaluator yield at the same granularity.
const YIELD_INTERVAL: u32 = 1024;

use num_bigint::BigInt;
use num_traits::Zero;
use parking_lot::RwLock;

use crate::class::{Class, IndexedSlotKind};
use crate::entity::{
    EntityId, InstanceHandle, InstanceKind, InstanceState, send_to_entity, spawn_class_entity,
    spawn_entity_bare,
};
use crate::env::Env;
use crate::eval::EvalError;
use crate::eval_recursive::RecursiveEvaluator;
use crate::namespace::Namespace;
use crate::object::{IndexedStorage, Object};
use crate::value::{Block, Value};
use triple_store::exec_context::ExecContext;
use triple_store::fact::{Fact, StoredFact};
use triple_store::term::Term;

/// Entry point for all built-in method dispatch.
pub fn dispatch(
    ev: &RecursiveEvaluator,
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
        return force_value(recv);
    }
    // `isNil` / `notNil` are universal.
    if selector.as_ref() == "isNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(matches!(force_value(recv)?, Value::Nil)));
    }
    if selector.as_ref() == "notNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(!matches!(force_value(recv)?, Value::Nil)));
    }
    // `_primHash` is universal — handled here so all value kinds are covered.
    if selector.as_ref() == "_primHash" {
        expect_arity(&selector, 0, args.len())?;
        let v = force_value(recv)?;
        return Ok(Value::Int(hash_value(&v)));
    }
    // `_primClass` — maps any value to its kernel class entity handle.
    if selector.as_ref() == "_primClass" {
        expect_arity(&selector, 0, args.len())?;
        let recv = force_value(recv)?;
        return value_to_class_handle_rec(ev, &recv)
            .map(Value::Instance)
            .ok_or_else(|| mnu("Object", &selector));
    }
    // `_primIsKindOf:` — true iff receiver's class is aClass or a subclass of it.
    if selector.as_ref() == "_primIsKindOf:" {
        expect_arity(&selector, 1, args.len())?;
        let recv = force_value(recv)?;
        let arg = force_value(args.into_iter().next().unwrap())?;
        let recv_class = match value_to_class_handle_rec(ev, &recv) {
            Some(h) => h.0.class.clone(),
            None => return Ok(Value::Bool(false)),
        };
        let arg_class = match arg {
            Value::Instance(h) => h.0.class.clone(),
            _ => return Ok(Value::Bool(false)),
        };
        return Ok(Value::Bool(is_kind_of(&recv_class, &arg_class)));
    }
    // `hash` is universal for non-object types (primitive values don't route through
    // Object method lookup). For Value::Instance with Entity/ValueObject kind, the
    // type-specific dispatch runs instead to find any user-defined `hash` override.
    // Value::Future is excluded so the transparent-future arm below resolves the
    // future first; if it resolves to an Instance, hash is re-dispatched through
    // dispatch_entity so user-defined overrides are found.
    if selector.as_ref() == "hash" && !matches!(recv, Value::Instance(_) | Value::Future(_)) {
        expect_arity(&selector, 0, args.len())?;
        let v = force_value(recv)?;
        return Ok(Value::Int(hash_value(&v)));
    }
    // `->` is universal — any receiver can create an Association.
    // Implemented in Rust so primitive values (String, Int, …) work without an
    // Object-inherited method dispatch path.
    if selector.as_ref() == "->" {
        expect_arity(&selector, 1, args.len())?;
        let value = args.into_iter().next().unwrap();
        let assoc_class = ev
            .inner
            .registry
            .kernel()
            .lookup_class("Association")
            .ok_or_else(|| {
                EvalError::TypeError("'->': Association not yet defined in kernel".into())
            })?;
        return dispatch(
            ev,
            Value::Instance(assoc_class),
            Arc::from("key:value:"),
            vec![recv, value],
            env,
        );
    }
    // `ifNil:` / `ifNotNil:` family — universal nil-guards.
    if selector.as_ref() == "ifNil:" {
        expect_arity(&selector, 1, args.len())?;
        let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
        return if matches!(force_value(recv)?, Value::Nil) {
            ev.eval_block(&blk, vec![], env)
        } else {
            Ok(Value::Nil)
        };
    }
    if selector.as_ref() == "ifNotNil:" {
        expect_arity(&selector, 1, args.len())?;
        let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
        let forced = force_value(recv)?;
        return if !matches!(forced, Value::Nil) {
            ev.eval_block(&blk, vec![forced], env)
        } else {
            Ok(Value::Nil)
        };
    }
    if selector.as_ref() == "ifNil:ifNotNil:" {
        expect_arity(&selector, 2, args.len())?;
        let mut iter = args.into_iter();
        let nil_blk = as_block(iter.next().unwrap(), &selector)?;
        let not_nil_blk = as_block(iter.next().unwrap(), &selector)?;
        let forced = force_value(recv)?;
        return if matches!(forced, Value::Nil) {
            ev.eval_block(&nil_blk, vec![], env)
        } else {
            ev.eval_block(&not_nil_blk, vec![forced], env)
        };
    }
    if selector.as_ref() == "ifNotNil:ifNil:" {
        expect_arity(&selector, 2, args.len())?;
        let mut iter = args.into_iter();
        let not_nil_blk = as_block(iter.next().unwrap(), &selector)?;
        let nil_blk = as_block(iter.next().unwrap(), &selector)?;
        let forced = force_value(recv)?;
        return if matches!(forced, Value::Nil) {
            ev.eval_block(&nil_blk, vec![], env)
        } else {
            ev.eval_block(&not_nil_blk, vec![forced], env)
        };
    }
    // `then:` — chains a block after this value resolves.
    // Future: spawns an async continuation and returns a new Future.
    // Block: evaluates the receiver block synchronously (≡ `value`), then — if the
    //   result is a Future, chains the continuation asynchronously; otherwise runs it
    //   synchronously. This is the canonical `[loop] then: [result]` combinator: the
    //   receiver block runs first (queuing any actor messages), then the continuation
    //   is guaranteed to execute after it.
    // Any other value: the value is already resolved; the block is run synchronously.
    // The continuation block receives no arguments in any case.
    if selector.as_ref() == "then:" {
        expect_arity(&selector, 1, args.len())?;
        let blk_val = args.into_iter().next().unwrap();
        return match recv {
            Value::Future(future) => {
                let blk = as_block(blk_val, &selector)?;
                let result_future = crate::entity::FutureHandle::new();
                let result_reply = result_future.clone();
                let registry = ev.inner.registry.clone();
                crate::entity::init_coroutine_stack();
                may::go!(move || {
                    match future.wait() {
                        Err(e) => result_reply.resolve_err(e),
                        Ok(_) => {
                            let then_ev = RecursiveEvaluator::with_registry(registry);
                            let mut dummy_env = crate::env::Env::new();
                            let result = then_ev.eval_block(&blk, vec![], &mut dummy_env);
                            match result {
                                Ok(v) => match force_value(v) {
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
                let cont_blk = as_block(blk_val, &selector)?;
                let result = ev.eval_block(&recv_blk, vec![], env)?;
                match result {
                    Value::Future(future) => {
                        let result_future = crate::entity::FutureHandle::new();
                        let result_reply = result_future.clone();
                        let registry = ev.inner.registry.clone();
                        crate::entity::init_coroutine_stack();
                        may::go!(move || {
                            match future.wait() {
                                Err(e) => result_reply.resolve_err(e),
                                Ok(_) => {
                                    let then_ev = RecursiveEvaluator::with_registry(registry);
                                    let mut dummy_env = crate::env::Env::new();
                                    let r = then_ev.eval_block(&cont_blk, vec![], &mut dummy_env);
                                    match r {
                                        Ok(v) => match force_value(v) {
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
                    _ => ev.eval_block(&cont_blk, vec![], env),
                }
            }
            _ => {
                let blk = as_block(blk_val, &selector)?;
                ev.eval_block(&blk, vec![], env)
            }
        };
    }

    match recv {
        Value::Int(n) => dispatch_int(ev, n, &selector, args, env),
        Value::Float(f) => dispatch_float(ev, f, &selector, args, env),
        Value::Bool(b) => dispatch_bool(ev, b, &selector, args, env),
        Value::Nil => dispatch_nil(ev, &selector, args, env),
        Value::Str(s) => dispatch_str(ev, s, &selector, args, env),
        Value::Symbol(s) => dispatch_symbol(ev, s, &selector, args, env),
        Value::Block(blk) => dispatch_block(ev, blk, &selector, args, env),
        Value::LargeInt(n) => dispatch_largeint(ev, n, &selector, args, env),
        Value::Instance(handle) => match &handle.0.kind {
            InstanceKind::ValueObject(_) => dispatch_object_value(ev, handle, selector, args, env),
            InstanceKind::Entity(_) => dispatch_entity(ev, handle, selector, args, env),
            InstanceKind::ClassEntity(_) => dispatch_class(ev, handle, selector, args, env),
        },
        Value::Future(future) => {
            // Transparent future: wait for it to resolve, then re-dispatch to the result.
            let resolved = future.wait()?;
            dispatch(ev, resolved, selector, args, env)
        }
        Value::Namespace(ns) => dispatch_namespace(ev, ns, selector, args, env),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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
pub(crate) fn force_value(v: Value) -> Result<Value, EvalError> {
    match v {
        Value::Future(f) => force_value(f.wait()?),
        v => Ok(v),
    }
}

fn as_int(v: Value, selector: &Arc<str>) -> Result<i64, EvalError> {
    match force_value(v)? {
        Value::Int(n) => Ok(n),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be Int"
        ))),
    }
}

pub(crate) fn as_block(v: Value, selector: &Arc<str>) -> Result<Arc<Block>, EvalError> {
    match force_value(v)? {
        Value::Block(b) => Ok(b),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be a Block"
        ))),
    }
}

fn bigint_from_value(v: Value, sel: &Arc<str>) -> Result<BigInt, EvalError> {
    match force_value(v)? {
        Value::Int(n) => Ok(BigInt::from(n)),
        Value::LargeInt(b) => Ok((*b).clone()),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: argument must be Int or LargeInt"
        ))),
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
        Value::Float(f) => f.to_bits() as i64,
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
        Value::LargeInt(n) => {
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
        Value::Instance(h) => fmix64(Arc::as_ptr(&h.0) as usize as u64) as i64,
        Value::Future(f) => fmix64(Arc::as_ptr(&f.0) as usize as u64) as i64,
        Value::Block(b) => fmix64(Arc::as_ptr(b) as usize as u64) as i64,
        Value::Namespace(ns) => fnv1a_hash(&ns.name),
    }
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
    let Some(handle) = registry.kernel().lookup_class("Error") else {
        return Value::Str(Arc::from(message));
    };
    let error_class = handle.0.class.clone();
    let obj = Arc::new(crate::object::Object::new(error_class.clone()));
    obj.set_slot(Arc::from("message"), Value::Str(Arc::from(message)));
    Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: error_class,
        kind: InstanceKind::ValueObject(obj),
    })))
}

/// Fallback dispatch for primitive value types (Int, Float, Bool, Nil, Str,
/// Symbol, Block, LargeInt). Called from the `_` arm of each type-specific
/// dispatch function after type-specific cases are exhausted.
///
/// Handles selectors that are meaningful on every value but are not universal
/// enough to bypass user-defined method lookup (unlike the pre-checks in
/// `dispatch()`). Object/ValueObject instances never reach this function —
/// they have their own fallback in `dispatch_object_primitive`.
fn dispatch_primitive_fallback(
    ev: &RecursiveEvaluator,
    kind: &'static str,
    sel: &Arc<str>,
    mut args: Vec<Value>,
) -> Result<Value, EvalError> {
    // NOTE: primitive types (Int, Float, etc.) don't carry an Env — handler_chain
    // checking is skipped here. Use dispatch_object_primitive for Condition instances.
    match sel.as_ref() {
        "error:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match args.remove(0) {
                Value::Str(s) => s,
                other => Arc::from(format!("{other}").as_str()),
            };
            Err(EvalError::Signal(make_error_value(
                &ev.inner.registry,
                &msg,
            )))
        }
        _ => Err(mnu(kind, sel)),
    }
}

// ── Int ───────────────────────────────────────────────────────────────────────

fn dispatch_int(
    ev: &RecursiveEvaluator,
    n: i64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // Note: user-defined Int methods are now routed via lookup_user_method in the
    // iterative evaluator. The old (recursive) evaluator reaches builtins directly.
    match sel.as_ref() {
        "+" => {
            expect_arity(sel, 1, args.len())?;
            let arg = force_value(args.remove(0))?;
            match arg {
                Value::Int(m) => match n.checked_add(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) + BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) + (*m).clone())),
                _ => Err(EvalError::TypeError(
                    "+: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "-" => {
            expect_arity(sel, 1, args.len())?;
            let arg = force_value(args.remove(0))?;
            match arg {
                Value::Int(m) => match n.checked_sub(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) - BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) - (*m).clone())),
                _ => Err(EvalError::TypeError(
                    "-: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "*" => {
            expect_arity(sel, 1, args.len())?;
            let arg = force_value(args.remove(0))?;
            match arg {
                Value::Int(m) => match n.checked_mul(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) * BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) * (*m).clone())),
                _ => Err(EvalError::TypeError(
                    "*: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Int(d) => {
                    if d == 0 {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(Value::Int(n / d))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(normalize_bigint(BigInt::from(n) / (*d).clone()))
                }
                _ => Err(EvalError::TypeError(
                    "/: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "//" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Int(d) => {
                    if d == 0 {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    let r = n % d;
                    Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) {
                        n / d - 1
                    } else {
                        n / d
                    }))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(normalize_bigint(bigint_floor_div(BigInt::from(n), &d)))
                }
                _ => Err(EvalError::TypeError(
                    "//: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "\\\\" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Int(d) => {
                    if d == 0 {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    let r = n % d;
                    Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) {
                        r + d
                    } else {
                        r
                    }))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(normalize_bigint(bigint_floor_rem(BigInt::from(n), &d)))
                }
                _ => Err(EvalError::TypeError(
                    "\\\\: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "<" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n < as_int(args.remove(0), sel)?))
        }
        ">" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n > as_int(args.remove(0), sel)?))
        }
        "<=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n <= as_int(args.remove(0), sel)?))
        }
        ">=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n >= as_int(args.remove(0), sel)?))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Int(n) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Int(n) != force_value(args.remove(0))?))
        }
        "negated" => {
            expect_arity(sel, 0, args.len())?;
            match n.checked_neg() {
                Some(r) => Ok(Value::Int(r)),
                None => Ok(normalize_bigint(-BigInt::from(n))),
            }
        }
        "abs" => {
            expect_arity(sel, 0, args.len())?;
            match n.checked_abs() {
                Some(r) => Ok(Value::Int(r)),
                None => {
                    // i64::MIN has no positive i64 representation; promote to BigInt.
                    use num_traits::Signed;
                    Ok(normalize_bigint(BigInt::from(n).abs()))
                }
            }
        }
        "max:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Int(n.max(as_int(args.remove(0), sel)?)))
        }
        "min:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Int(n.min(as_int(args.remove(0), sel)?)))
        }
        "asString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string())))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            let s = n.to_string();
            println!("{:?}", s);
            Ok(Value::Str(Arc::from(s)))
        }
        "asFloat" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(n as f64))
        }
        // `timesRepeat:` is the public selector; `_primTimesRepeat:` is the legacy
        // primitive name kept for backward compatibility. Both run the Rust loop
        // in the old (recursive) evaluator; the iterative evaluator routes through
        // the Smol recursive kernel.smol method via lookup_user_method instead.
        "timesRepeat:" | "_primTimesRepeat:" => {
            expect_arity(sel, 1, args.len())?;
            let blk = as_block(args.remove(0), sel)?;
            for i in 0..n.max(0) {
                ev.eval_block(&blk, vec![], env)?;
                if i > 0 && (i as u32).is_multiple_of(YIELD_INTERVAL) {
                    may::coroutine::yield_now();
                }
            }
            Ok(Value::Nil)
        }
        "to:do:" => {
            expect_arity(sel, 2, args.len())?;
            let stop = as_int(args.remove(0), sel)?;
            let blk = as_block(args.remove(0), sel)?;
            let mut i = n;
            while i <= stop {
                ev.eval_block(&blk, vec![Value::Int(i)], env)?;
                i += 1;
            }
            Ok(Value::Nil)
        }
        "to:by:do:" => {
            expect_arity(sel, 3, args.len())?;
            let stop = as_int(args.remove(0), sel)?;
            let step = as_int(args.remove(0), sel)?;
            let blk = as_block(args.remove(0), sel)?;
            if step == 0 {
                return Err(EvalError::TypeError(
                    "to:by:do: step must be non-zero".into(),
                ));
            }
            let mut i = n;
            while if step > 0 { i <= stop } else { i >= stop } {
                ev.eval_block(&blk, vec![Value::Int(i)], env)?;
                i += step;
            }
            Ok(Value::Nil)
        }
        _ => dispatch_primitive_fallback(ev, "Int", sel, args),
    }
}

// ── LargeInt ──────────────────────────────────────────────────────────────────

fn dispatch_largeint(
    ev: &RecursiveEvaluator,
    n: Arc<BigInt>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    use num_traits::ToPrimitive;
    match sel.as_ref() {
        "+" | "-" | "*" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            let result = match sel.as_ref() {
                "+" => (*n).clone() + rhs,
                "-" => (*n).clone() - rhs,
                "*" => (*n).clone() * rhs,
                _ => unreachable!(),
            };
            Ok(normalize_bigint(result))
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            if rhs.is_zero() {
                return Err(EvalError::TypeError("division by zero".into()));
            }
            Ok(normalize_bigint((*n).clone() / rhs))
        }
        "//" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            if rhs.is_zero() {
                return Err(EvalError::TypeError("division by zero".into()));
            }
            Ok(normalize_bigint(bigint_floor_div((*n).clone(), &rhs)))
        }
        "\\\\" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            if rhs.is_zero() {
                return Err(EvalError::TypeError("division by zero".into()));
            }
            Ok(normalize_bigint(bigint_floor_rem((*n).clone(), &rhs)))
        }
        "<" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n < rhs))
        }
        ">" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n > rhs))
        }
        "<=" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n <= rhs))
        }
        ">=" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n >= rhs))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::LargeInt(n) == force_value(args.remove(0))?,
            ))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::LargeInt(n) != force_value(args.remove(0))?,
            ))
        }
        "negated" => {
            expect_arity(sel, 0, args.len())?;
            Ok(normalize_bigint(-(*n).clone()))
        }
        "abs" => {
            expect_arity(sel, 0, args.len())?;
            use num_traits::Signed;
            Ok(normalize_bigint(n.abs()))
        }
        "asString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string())))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string().as_str())))
        }
        "asFloat" => {
            expect_arity(sel, 0, args.len())?;
            let f = n.to_f64().unwrap_or(f64::INFINITY);
            Ok(Value::Float(f))
        }
        _ => dispatch_primitive_fallback(ev, "LargeInt", sel, args),
    }
}

// ── Float ────────────────────────────────────────────────────────────────────

fn as_float_coerce(v: Value, sel: &Arc<str>) -> Result<f64, EvalError> {
    match force_value(v)? {
        Value::Float(f) => Ok(f),
        Value::Int(n) => Ok(n as f64),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: argument must be Float or Int"
        ))),
    }
}

fn dispatch_float(
    ev: &RecursiveEvaluator,
    f: f64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "+" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f + as_float_coerce(args.remove(0), sel)?))
        }
        "-" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f - as_float_coerce(args.remove(0), sel)?))
        }
        "*" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f * as_float_coerce(args.remove(0), sel)?))
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f / as_float_coerce(args.remove(0), sel)?))
        }
        "<" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f < as_float_coerce(args.remove(0), sel)?))
        }
        ">" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f > as_float_coerce(args.remove(0), sel)?))
        }
        "<=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f <= as_float_coerce(args.remove(0), sel)?))
        }
        ">=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f >= as_float_coerce(args.remove(0), sel)?))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Float(f) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Float(f) != force_value(args.remove(0))?))
        }
        "negated" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(-f))
        }
        "abs" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(f.abs()))
        }
        "truncated" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.trunc() as i64))
        }
        "rounded" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.round() as i64))
        }
        "ceiling" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.ceil() as i64))
        }
        "floor" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.floor() as i64))
        }
        "asInt" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f as i64))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("{f:?}").as_str())))
        }
        _ => dispatch_primitive_fallback(ev, "Float", sel, args),
    }
}

// ── Bool ─────────────────────────────────────────────────────────────────────

fn dispatch_bool(
    ev: &RecursiveEvaluator,
    b: bool,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "ifTrue:" => {
            expect_arity(sel, 1, args.len())?;
            let blk = as_block(args.remove(0), sel)?;
            if b {
                ev.eval_block(&blk, vec![], env)
            } else {
                Ok(Value::Nil)
            }
        }
        "ifFalse:" => {
            expect_arity(sel, 1, args.len())?;
            let blk = as_block(args.remove(0), sel)?;
            if !b {
                ev.eval_block(&blk, vec![], env)
            } else {
                Ok(Value::Nil)
            }
        }
        "ifTrue:ifFalse:" => {
            expect_arity(sel, 2, args.len())?;
            let true_blk = as_block(args.remove(0), sel)?;
            let false_blk = as_block(args.remove(0), sel)?;
            if b {
                ev.eval_block(&true_blk, vec![], env)
            } else {
                ev.eval_block(&false_blk, vec![], env)
            }
        }
        "ifFalse:ifTrue:" => {
            expect_arity(sel, 2, args.len())?;
            let false_blk = as_block(args.remove(0), sel)?;
            let true_blk = as_block(args.remove(0), sel)?;
            if b {
                ev.eval_block(&true_blk, vec![], env)
            } else {
                ev.eval_block(&false_blk, vec![], env)
            }
        }
        "&" => {
            expect_arity(sel, 1, args.len())?;
            if b {
                force_value(args.remove(0))
            } else {
                Ok(Value::Bool(false))
            }
        }
        "|" => {
            expect_arity(sel, 1, args.len())?;
            if b {
                Ok(Value::Bool(true))
            } else {
                force_value(args.remove(0))
            }
        }
        "and:" => {
            expect_arity(sel, 1, args.len())?;
            if !b {
                return Ok(Value::Bool(false));
            }
            let blk = as_block(args.remove(0), sel)?;
            ev.eval_block(&blk, vec![], env)
        }
        "or:" => {
            expect_arity(sel, 1, args.len())?;
            if b {
                return Ok(Value::Bool(true));
            }
            let blk = as_block(args.remove(0), sel)?;
            ev.eval_block(&blk, vec![], env)
        }
        "not" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Bool(!b))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Bool(b) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Bool(b) != force_value(args.remove(0))?))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            if b {
                println!("true")
            } else {
                println!("false")
            }
            Ok(Value::Str(Arc::from(if b { "true" } else { "false" })))
        }
        _ => dispatch_primitive_fallback(ev, "Bool", sel, args),
    }
}

// ── Nil ───────────────────────────────────────────────────────────────────────

fn dispatch_nil(
    ev: &RecursiveEvaluator,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // User-defined methods on the Nil class take priority over builtins.
    if let Some(nil_handle) = ev.inner.registry.kernel().lookup_class("Nil")
        && let Some((method, def_class)) = nil_handle.0.class.lookup(sel)
    {
        return ev.invoke_primitive_method(Value::Nil, def_class, method, args, env);
    }
    match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(matches!(
                force_value(args.remove(0))?,
                Value::Nil
            )))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(!matches!(
                force_value(args.remove(0))?,
                Value::Nil
            )))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from("nil")))
        }
        "isEmpty" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Bool(true))
        }
        _ => dispatch_primitive_fallback(ev, "Nil", sel, args),
    }
}

// ── String ────────────────────────────────────────────────────────────────────

fn dispatch_str(
    ev: &RecursiveEvaluator,
    s: Arc<str>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "size" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(s.chars().count() as i64))
        }
        "at:" => {
            expect_arity(sel, 1, args.len())?;
            let idx = as_int(args.remove(0), sel)? as usize;
            // 1-based index (Smalltalk convention).
            s.chars()
                .nth(idx.wrapping_sub(1))
                .map(|c| Value::Str(Arc::from(c.to_string().as_str())))
                .ok_or_else(|| EvalError::TypeError(format!("{sel}: index {idx} out of bounds")))
        }
        "," => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Str(other) => Ok(Value::Str(Arc::from(format!("{s}{other}").as_str()))),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Str(s) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Str(s) != force_value(args.remove(0))?))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            let s: Arc<str> = Arc::from(format!("'{s}'").as_str());
            println!("{:?}", s);
            Ok(Value::Str(s))
        }
        "asSymbol" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Symbol(s))
        }
        "reversed" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(
                s.chars().rev().collect::<String>().as_str(),
            )))
        }
        "copyFrom:to:" => {
            expect_arity(sel, 2, args.len())?;
            let from = (as_int(args.remove(0), sel)? as usize).saturating_sub(1);
            let to = as_int(args.remove(0), sel)? as usize;
            let chars: Vec<char> = s.chars().collect();
            let slice: String = chars.get(from..to).unwrap_or(&[]).iter().collect();
            Ok(Value::Str(Arc::from(slice.as_str())))
        }
        "includesSubString:" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Str(sub) => Ok(Value::Bool(s.contains(sub.as_ref()))),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        _ => dispatch_primitive_fallback(ev, "String", sel, args),
    }
}

// ── Symbol ────────────────────────────────────────────────────────────────────

fn dispatch_symbol(
    ev: &RecursiveEvaluator,
    s: Arc<str>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::Symbol(s) == force_value(args.remove(0))?,
            ))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::Symbol(s) != force_value(args.remove(0))?,
            ))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("#{s}").as_str())))
        }
        "asString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(s))
        }
        "asSymbol" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Symbol(s))
        }
        _ => dispatch_primitive_fallback(ev, "Symbol", sel, args),
    }
}

// ── Block ─────────────────────────────────────────────────────────────────────

fn dispatch_block(
    ev: &RecursiveEvaluator,
    blk: Arc<Block>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // Note: user-defined Block methods are now routed via lookup_user_method in the
    // iterative evaluator. The old (recursive) evaluator reaches builtins directly.
    match sel.as_ref() {
        "value" => {
            expect_arity(sel, 0, args.len())?;
            ev.eval_block(&blk, vec![], env)
        }
        "value:" => {
            expect_arity(sel, 1, args.len())?;
            ev.eval_block(&blk, args, env)
        }
        "value:value:" => {
            expect_arity(sel, 2, args.len())?;
            ev.eval_block(&blk, args, env)
        }
        "value:value:value:" => {
            expect_arity(sel, 3, args.len())?;
            ev.eval_block(&blk, args, env)
        }
        // Public selectors are aliased with their legacy _prim* names.
        // The old (recursive) evaluator reaches these arms directly.
        // The iterative evaluator routes through the Smol recursive kernel.smol
        // methods via lookup_user_method + TCO, so these arms are not reached
        // from that path for whileTrue: and whileFalse:.
        "whileTrue:" | "_primWhileTrue:" => {
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;
            let mut iters: u32 = 0;
            loop {
                match ev.eval_block(&blk, vec![], env)? {
                    Value::Bool(true) => {
                        ev.eval_block(&body, vec![], env)?;
                    }
                    Value::Bool(false) => break,
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: condition block must return Bool"
                        )));
                    }
                }
                iters += 1;
                if iters.is_multiple_of(YIELD_INTERVAL) {
                    may::coroutine::yield_now();
                }
            }
            Ok(Value::Nil)
        }
        "whileFalse:" | "_primWhileFalse:" => {
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;
            let mut iters: u32 = 0;
            loop {
                match ev.eval_block(&blk, vec![], env)? {
                    Value::Bool(false) => {
                        ev.eval_block(&body, vec![], env)?;
                    }
                    Value::Bool(true) => break,
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: condition block must return Bool"
                        )));
                    }
                }
                iters += 1;
                if iters.is_multiple_of(YIELD_INTERVAL) {
                    may::coroutine::yield_now();
                }
            }
            Ok(Value::Nil)
        }
        "whileTrue" | "_primWhileTrue" => {
            expect_arity(sel, 0, args.len())?;
            let mut iters: u32 = 0;
            loop {
                match ev.eval_block(&blk, vec![], env)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) => break,
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: block must return Bool"
                        )));
                    }
                }
                iters += 1;
                if iters.is_multiple_of(YIELD_INTERVAL) {
                    may::coroutine::yield_now();
                }
            }
            Ok(Value::Nil)
        }
        "whileFalse" | "_primWhileFalse" => {
            expect_arity(sel, 0, args.len())?;
            let mut iters: u32 = 0;
            loop {
                match ev.eval_block(&blk, vec![], env)? {
                    Value::Bool(false) => {}
                    Value::Bool(true) => break,
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: block must return Bool"
                        )));
                    }
                }
                iters += 1;
                if iters.is_multiple_of(YIELD_INTERVAL) {
                    may::coroutine::yield_now();
                }
            }
            Ok(Value::Nil)
        }
        "on:do:" => {
            expect_arity(sel, 2, args.len())?;
            let catch_class_val = force_value(args.remove(0))?;
            let handler = as_block(args.remove(0), sel)?;
            let catch_class = match &catch_class_val {
                Value::Instance(h) => h.0.class.clone(),
                _ => {
                    return Err(EvalError::TypeError(
                        "on:do: first argument must be a class".into(),
                    ));
                }
            };
            loop {
                match ev.eval_block_sync(&blk, vec![], env) {
                    Ok(v) => return Ok(v),
                    Err(EvalError::Return(v, d)) => return Err(EvalError::Return(v, d)),
                    Err(EvalError::Signal(cond)) => {
                        let signal_class = match &cond {
                            Value::Instance(h) => h.0.class.clone(),
                            _ => return Err(EvalError::Signal(cond)),
                        };
                        if crate::class::Class::is_subclass_of(&signal_class, &catch_class) {
                            match ev.eval_block(&handler, vec![cond], env) {
                                Ok(v) => return Ok(v),
                                Err(EvalError::Resume(v)) => return Ok(v),
                                Err(EvalError::ReturnFromBlock(v)) => return Ok(v),
                                Err(EvalError::Retry) => continue,
                                Err(e) => return Err(e),
                            }
                        } else {
                            return Err(EvalError::Signal(cond));
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        "ensure:" => {
            expect_arity(sel, 1, args.len())?;
            let ensure_blk = as_block(args.remove(0), sel)?;
            let result = ev.eval_block_sync(&blk, vec![], env);
            let _ = ev.eval_block(&ensure_blk, vec![], env);
            result
        }
        "syncValue" => {
            expect_arity(sel, 0, args.len())?;
            ev.eval_block_sync(&blk, vec![], env)
        }
        _ => dispatch_primitive_fallback(ev, "Block", sel, args),
    }
}

// ── Object (user-defined instances) ──────────────────────────────────────────

fn dispatch_object_value(
    ev: &RecursiveEvaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let class = handle.0.class.clone();
    if let Some((method, def_class)) = class.lookup(&selector) {
        return ev.invoke_method(handle.clone(), def_class, method, args, env);
    }
    dispatch_object_primitive(ev, handle, &selector, args, env)
}

/// Dispatch a message to an actor entity. Checks for sync self-send
/// (target == CURRENT_ENTITY); otherwise enqueues the message and returns a Future.
///
/// Requires: caller may be any thread including the entity's own thread.
/// Ensures: self-send is dispatched synchronously (no deadlock); cross-entity
///   send returns `Value::Future(FutureHandle)` immediately.
fn dispatch_entity(
    ev: &RecursiveEvaluator,
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
    let current = ev.inner.current_entity_id.get();
    if current == Some(entity_id) {
        let class = handle.0.class.clone();
        if let Some((method, def_class)) = class.lookup(&selector) {
            return ev.invoke_method(handle.clone(), def_class, method, args, env);
        }
        return dispatch_object_primitive(ev, handle, &selector, args, env);
    }

    // Cross-entity or non-entity-context send: enqueue and return Future.
    // Snapshot the caller's handler_chain so the receiving entity can run inherited
    // HC handlers if it signals a condition.
    let chain_snapshot = {
        let chain = ev.inner.handler_chain.borrow();
        if chain.is_empty() {
            vec![]
        } else {
            chain.clone()
        }
    };
    let future = match &handle.0.kind {
        InstanceKind::Entity(d) => send_to_entity(d, selector, args, chain_snapshot),
        _ => unreachable!(),
    };
    Ok(Value::Future(future))
}

fn value_to_class_handle_rec(ev: &RecursiveEvaluator, v: &Value) -> Option<InstanceHandle> {
    let class_name = match v {
        Value::Bool(b) => {
            if *b {
                "True"
            } else {
                "False"
            }
        }
        Value::Int(_) | Value::LargeInt(_) => "Int",
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
    ev.inner.registry.kernel().lookup_class(class_name)
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

pub(crate) fn dispatch_object_primitive(
    ev: &RecursiveEvaluator,
    handle: InstanceHandle,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    use crate::object::IndexedStorage;
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
    match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(self_val == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(self_val != force_value(args.remove(0))?))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            let s: Arc<str> = Arc::from(format!("a {}", obj.class.name).as_str());
            println!("{:?}", s);
            Ok(Value::Str(s))
        }
        "class" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Instance(class_handle_of(&obj)?))
        }
        "isKindOf:" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Instance(h) => Ok(Value::Bool(is_kind_of(&obj.class, &h.0.class))),
                _ => Ok(Value::Bool(false)),
            }
        }
        "respondsTo:" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Symbol(s) => Ok(Value::Bool(obj.class.lookup(&s).is_some())),
                _ => Ok(Value::Bool(false)),
            }
        }
        "_primAt:" => {
            expect_arity(sel, 1, args.len())?;
            let n = as_int(args.remove(0), sel)?;
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
                    Ok(guard[n as usize].clone())
                }
                IndexedStorage::Byte(bytes) => {
                    let guard = bytes.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    Ok(Value::Int(guard[n as usize] as i64))
                }
                IndexedStorage::Float64(floats) => {
                    let guard = floats.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    Ok(Value::Float(guard[n as usize]))
                }
            }
        }
        "_primAt:put:" => {
            expect_arity(sel, 2, args.len())?;
            let n = as_int(args.remove(0), sel)?;
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
                        crate::store_bridge::buffer_indexed_slot(&ev.inner, eid, n as usize, &v);
                    }
                    Ok(v)
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
                            &ev.inner,
                            eid,
                            n as usize,
                            &Value::Int(b as i64),
                        );
                    }
                    Ok(v)
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
                            &ev.inner,
                            eid,
                            n as usize,
                            &Value::Float(f),
                        );
                    }
                    Ok(v)
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
            Ok(Value::Int(len as i64))
        }
        "_primAsString" => {
            expect_arity(sel, 0, args.len())?;
            match obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })? {
                IndexedStorage::Byte(bytes) => {
                    let s = String::from_utf8_lossy(&bytes.lock()).into_owned();
                    Ok(Value::Str(Arc::from(s.as_str())))
                }
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: requires Byte indexed storage"
                ))),
            }
        }
        "error:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match force_value(args.remove(0))? {
                Value::Str(s) => s,
                other => Arc::from(format!("error: argument was {}", other.kind()).as_str()),
            };
            signal_condition(ev, make_error_value(&ev.inner.registry, &msg), env)
        }
        "_primSignalCondition:" => {
            expect_arity(sel, 1, args.len())?;
            let condition = force_value(args.remove(0))?;
            if !matches!(&condition, Value::Instance(_)) {
                return Err(EvalError::TypeError(
                    "_primSignalCondition: requires a Condition instance".into(),
                ));
            }
            signal_condition(ev, condition, env)
        }
        "_primResume:" => {
            expect_arity(sel, 1, args.len())?;
            let v = args.remove(0);
            Err(EvalError::Resume(v))
        }
        "_primReturn:" => {
            expect_arity(sel, 1, args.len())?;
            let v = args.remove(0);
            Err(EvalError::ReturnFromBlock(v))
        }
        "_primRetry" => {
            expect_arity(sel, 0, args.len())?;
            Err(EvalError::Retry)
        }
        "_primHCRun:" => {
            // Run a block with this HandlerContext's handler pairs registered.
            //
            // Requires: args[0] is a Block; self's `pairs` ivar is an OrderedCollection
            //   of Associations (key = condition class, value = handler block).
            // Ensures: handler pairs are pushed onto ev.inner.handler_chain for the duration of
            //   body execution and popped when body completes (success, error, or retry).
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;

            // Extract pairs from self's `pairs` ivar.
            // HandlerContext is a Value subclass (ValueObject); Entity arm is kept as
            // a defensive fallback only.
            let pairs_val = match &handle.0.kind {
                InstanceKind::ValueObject(o) => o.get_slot("pairs").unwrap_or(Value::Nil),
                InstanceKind::Entity(d) => d.data.get_slot("pairs").unwrap_or(Value::Nil),
                InstanceKind::ClassEntity(_) => {
                    return Err(EvalError::TypeError(
                        "_primHCRun: requires a HandlerContext instance".into(),
                    ));
                }
            };

            // Walk the Cons-of-Association linked list:
            //   Cons(head: cls -> blk, tail: rest)
            // Newest pair is at the head; we collect then reverse so that
            // handler_chain is ordered oldest-first, making the reverse walk in
            // signal_condition match newest (innermost) first.
            let mut handler_pairs: Vec<(Arc<Class>, Arc<crate::value::Block>)> = Vec::new();
            let mut node = force_value(pairs_val)?;
            while let Value::Instance(node_h) = node {
                let node_obj = match &node_h.0.kind {
                    InstanceKind::ValueObject(o) => o.clone(),
                    InstanceKind::Entity(d) => d.data.clone(),
                    InstanceKind::ClassEntity(_) => break,
                };
                // head is an Association(key=cls, value=blk); tail is rest.
                let head_val = force_value(node_obj.get_slot("head").unwrap_or(Value::Nil))?;
                let tail = force_value(node_obj.get_slot("tail").unwrap_or(Value::Nil))?;

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
                let cls_val = force_value(assoc_obj.get_slot("key").unwrap_or(Value::Nil))?;
                let blk_val = force_value(assoc_obj.get_slot("value").unwrap_or(Value::Nil))?;

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
            // Reverse so oldest pair is pushed first; signal_condition's reverse
            // walk then matches the most-recently-registered handler first.
            handler_pairs.reverse();

            // Push handler pairs onto handler_chain.
            let chain_start_len = ev.inner.handler_chain.borrow().len();
            {
                let mut chain = ev.inner.handler_chain.borrow_mut();
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
            ev.inner
                .handler_chain
                .borrow_mut()
                .truncate(chain_start_len);
            result
        }
        "historyOf:" => {
            expect_arity(sel, 1, args.len())?;
            let eid = entity_id.ok_or_else(|| {
                EvalError::TypeError("historyOf: requires an entity receiver".into())
            })?;
            let pred_name = match force_value(args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "historyOf: argument must be a Symbol, got {}",
                        other.kind()
                    )));
                }
            };
            let pred = ev.inner.registry.store.lookup(&pred_name).ok_or_else(|| {
                EvalError::TypeError(format!("historyOf: unknown predicate '{pred_name}'"))
            })?;
            let ctx = ExecContext::Entity(eid);
            let facts = ev.inner.store.history(&ctx, eid, pred, None).map_err(|e| {
                EvalError::Signal(make_error_value(
                    &ev.inner.registry,
                    &format!("historyOf: store error: {e:?}"),
                ))
            })?;
            history_facts_to_array(ev, &facts)
        }
        "historyOf:from:to:" => {
            expect_arity(sel, 3, args.len())?;
            let eid = entity_id.ok_or_else(|| {
                EvalError::TypeError("historyOf:from:to: requires an entity receiver".into())
            })?;
            let pred_name = match force_value(args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "historyOf:from:to: first argument must be a Symbol, got {}",
                        other.kind()
                    )));
                }
            };
            let from_ms = as_int(args.remove(0), sel)?;
            let to_ms = as_int(args.remove(0), sel)?;
            let pred = ev.inner.registry.store.lookup(&pred_name).ok_or_else(|| {
                EvalError::TypeError(format!(
                    "historyOf:from:to: unknown predicate '{pred_name}'"
                ))
            })?;
            let ctx = ExecContext::Entity(eid);
            let all_facts = ev.inner.store.history(&ctx, eid, pred, None).map_err(|e| {
                EvalError::Signal(make_error_value(
                    &ev.inner.registry,
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
            history_facts_to_array(ev, &facts)
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Object",
            selector: sel.clone(),
        }),
    }
}

/// Core signal dispatch: walk the handler_chain, then apply unhandled policy.
///
/// Requires: `condition` is a `Value::Instance` whose class is a subclass of Condition.
/// Ensures: if a matching non-unwinding handler is found its return value is returned;
///   if none match, applies unhandled policy (Condition → nil, Warning → log+nil,
///   Error → raise EvalError::Signal).
pub(crate) fn signal_condition(
    ev: &RecursiveEvaluator,
    condition: Value,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let cond_class = match &condition {
        Value::Instance(h) => h.0.class.clone(),
        _ => return Err(EvalError::Signal(condition)),
    };

    // Walk handler_chain in reverse (most-recently-pushed first).
    let chain_snapshot = ev.inner.handler_chain.borrow().clone();
    let mut re_signaled = false;
    for (handler_class, handler_block) in chain_snapshot.iter().rev() {
        if crate::class::Class::is_subclass_of(&cond_class, handler_class) {
            match ev.eval_block(handler_block, vec![condition.clone()], env) {
                Ok(v) => return Ok(v),
                Err(EvalError::Resume(v)) => return Ok(v),
                Err(EvalError::ReturnFromBlock(v)) => return Err(EvalError::ReturnFromBlock(v)),
                Err(EvalError::Retry) => return Err(EvalError::Retry),
                Err(EvalError::Signal(_)) => {
                    re_signaled = true;
                    break;
                }
                Err(other) => return Err(other),
            }
        }
    }
    if re_signaled {
        return Err(EvalError::Signal(condition));
    }

    // No handler matched — apply unhandled policy by condition class.
    let kernel = ev.inner.registry.kernel();
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

/// Assert `(entity, __instance_of__, class_entity)` in a fresh one-shot transaction.
///
/// Requires: `inst` is an Entity kind handle.
/// Ensures: a `__instance_of__` fact is committed to the shared store. Silent no-op
///   if the class has no backing class-entity (e.g. during bootstrap).
fn assert_instance_of(ev: &RecursiveEvaluator, inst: &InstanceHandle, class: &Arc<Class>) {
    let entity_id = match &inst.0.kind {
        InstanceKind::Entity(d) => d.id,
        _ => return,
    };
    let Some(class_eid) = class_entity_id_of(class) else {
        return;
    };
    let mut tx = ev.inner.store.begin_owned(ExecContext::Entity(entity_id));
    let _ = tx.assert_single(Fact {
        subject: entity_id,
        predicate: ev.inner.registry.instance_of_pred,
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
fn build_array(ev: &RecursiveEvaluator, values: Vec<Value>) -> Result<Value, EvalError> {
    let array_class_handle = ev
        .inner
        .registry
        .kernel()
        .lookup_class("Array")
        .ok_or_else(|| EvalError::TypeError("Array not in kernel".into()))?;
    let array_class = array_class_handle.0.class.clone();

    let mut global_env = crate::env::Env::new();
    ev.inner.registry.populate_env(&mut global_env);
    let arr = spawn_entity_bare(
        array_class,
        ev.inner.registry.clone(),
        global_env,
        Some(values.len()),
        ev.inner.store.clone_handle(),
    );
    ev.inner.track_entity(&arr);

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

/// Convert a slice of `StoredFact` to an `Array` of `Association(ts_ms → value)`.
///
/// Each Association has `key = epoch-milliseconds (Int)` and `value = resolved object`.
fn history_facts_to_array(
    ev: &RecursiveEvaluator,
    facts: &[StoredFact],
) -> Result<Value, EvalError> {
    let assocs: Vec<Value> = facts
        .iter()
        .map(|f| {
            let ts_ms = Value::Int((f.valid.start.0 / 1_000_000) as i64);
            let val = ev.inner.resolve_term(&f.object);
            make_association(ev, ts_ms, val)
        })
        .collect::<Result<_, _>>()?;
    build_array(ev, assocs)
}

/// Build an Association ValueObject with `key` and `value` set.
fn make_association(ev: &RecursiveEvaluator, key: Value, value: Value) -> Result<Value, EvalError> {
    let assoc_class_handle = ev
        .inner
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
    ev: &RecursiveEvaluator,
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
        "printString" => {
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
    ev: &RecursiveEvaluator,
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
    let current = ev.inner.current_entity_id.get();
    if current == Some(entity_id)
        || selector.starts_with("subclass:")
        || handle.0.class.inherits_is_value()
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
        let chain = ev.inner.handler_chain.borrow();
        if chain.is_empty() {
            vec![]
        } else {
            chain.clone()
        }
    };
    let future = match &handle.0.kind {
        InstanceKind::ClassEntity(d) => send_to_entity(d, selector, args, chain_snapshot),
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
    ev: &RecursiveEvaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let class = handle.0.class.clone();
    let self_val = Value::Instance(handle);
    match selector.as_ref() {
        "=" => {
            expect_arity(&selector, 1, args.len())?;
            Ok(Value::Bool(self_val == force_value(args[0].clone())?))
        }
        "~=" => {
            expect_arity(&selector, 1, args.len())?;
            Ok(Value::Bool(self_val != force_value(args.remove(0))?))
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
                let mut global_env = Env::new();
                ev.inner.registry.populate_env(&mut global_env);
                let inst = spawn_entity_bare(
                    class.clone(),
                    ev.inner.registry.clone(),
                    global_env,
                    indexed_size,
                    ev.inner.store.clone_handle(),
                );
                ev.inner.track_entity(&inst);
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
            let blk = as_block(args.remove(0), &selector)?;
            let obj = if class.indexed_slot_kind.is_some() {
                Arc::new(Object::new_with_indexed(class.clone(), 0))
            } else {
                Arc::new(Object::new(class.clone()))
            };
            let inst = InstanceHandle(Arc::new(InstanceState {
                class,
                kind: InstanceKind::ValueObject(obj.clone()),
            }));
            ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?;
            obj.freeze();
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
            let n = as_int(args.remove(0), &selector)?;
            let blk = as_block(args.remove(0), &selector)?;
            let obj = Arc::new(Object::new_with_indexed(class.clone(), n.max(0) as usize));
            let inst = InstanceHandle(Arc::new(InstanceState {
                class,
                kind: InstanceKind::ValueObject(obj.clone()),
            }));
            ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?;
            obj.freeze();
            Ok(Value::Instance(inst))
        }
        "_primNew:" => {
            expect_arity(&selector, 1, args.len())?;
            let n = as_int(args.remove(0), &selector)?;
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
                let mut global_env = Env::new();
                ev.inner.registry.populate_env(&mut global_env);
                let inst = spawn_entity_bare(
                    class.clone(),
                    ev.inner.registry.clone(),
                    global_env,
                    Some(n.max(0) as usize),
                    ev.inner.store.clone_handle(),
                );
                ev.inner.track_entity(&inst);
                assert_instance_of(ev, &inst, &class);
                Ok(Value::Instance(inst))
            }
        }
        "_primInstancesOf:" => {
            expect_arity(&selector, 1, args.len())?;
            let target_handle = match force_value(args.remove(0))? {
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
                .inner
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
        "printString" => {
            expect_arity(&selector, 0, args.len())?;
            let s: Arc<str> = class.name.clone();
            println!("{:?}", s);
            Ok(Value::Str(s))
        }
        sel if sel.contains("subclass:") => {
            // Extract the handle back from self_val to pass the Arc<Class> as superclass.
            handle_subclass(ev, class, selector, args, env)
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Class",
            selector,
        }),
    }
}

fn handle_subclass(
    ev: &RecursiveEvaluator,
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

    for (kw, arg) in keywords.iter().zip(args.iter()) {
        match *kw {
            "subclass:" => match arg {
                Value::Symbol(s) => class_name = Some(s.clone()),
                _ => {
                    return Err(EvalError::TypeError(
                        "subclass: argument must be a Symbol".into(),
                    ));
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
    });
    let class_handle = spawn_class_entity(
        new_class.clone(),
        ev.inner.registry.clone(),
        Env::new(),
        ev.inner.store.clone_handle(),
    );
    new_class
        .class_object
        .set(Arc::downgrade(&class_handle.0))
        .ok();
    ev.inner.registry.register_class_entity(&class_handle);
    ev.inner.registry.register_class_predicates(&new_class);

    let active_ns = ev.inner.active_namespace();
    active_ns.register(class_handle.clone());
    // Kernel classes get a global binding so they're always in scope by simple name.
    // Non-Kernel classes are scoped to the current frame (e.g. inside `Namespace do: [...]`);
    // outside that scope they are only reachable via `Ns::ClassName`.
    if Arc::ptr_eq(&active_ns, &ev.inner.registry.kernel()) {
        env.define_global(name, Value::Instance(class_handle.clone()));
    } else {
        env.define(name, Value::Instance(class_handle.clone()));
    }
    Ok(Value::Instance(class_handle))
}
