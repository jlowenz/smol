use std::collections::HashMap;
use std::sync::Arc;

use num_bigint::BigInt;
use num_traits::Zero;
use parking_lot::RwLock;

use crate::class::{Class, IndexedSlotKind};
use crate::entity::{
    InstanceHandle, InstanceKind, InstanceState, send_to_entity, spawn_entity_bare,
};
use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
use crate::namespace::Namespace;
use crate::object::Object;
use crate::value::{Block, Value};

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
    // `hash` is universal for non-object types (primitive values don't route through
    // Object method lookup). For Value::Instance with Entity/ValueObject kind, the
    // type-specific dispatch runs instead to find any user-defined `hash` override.
    if selector.as_ref() == "hash" && !matches!(recv, Value::Instance(_)) {
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
            .registry
            .kernel()
            .lookup_class("Association")
            .ok_or_else(|| {
                EvalError::TypeError("'->': Association not yet defined in kernel".into())
            })?;
        return ev.dispatch(
            Value::Instance(assoc_class),
            Arc::from("key:value:"),
            vec![recv, value],
            env,
        );
    }
    // `ifNil:` / `ifNotNil:` family — universal nil-guards that work on every value,
    // including futures (force_value resolves them before the nil check).
    // Defined here so no type-specific dispatch function needs to repeat them.
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
    // Any other value: the value is already resolved; the block is run synchronously.
    // The block receives no arguments in either case — use `await` + a regular send
    // when the resolved value is needed inside the block.
    if selector.as_ref() == "then:" {
        expect_arity(&selector, 1, args.len())?;
        let blk_val = args.into_iter().next().unwrap();
        return match recv {
            Value::Future(future) => {
                let blk = as_block(blk_val, &selector)?;
                let result_future = crate::entity::FutureHandle::new();
                let result_reply = result_future.clone();
                let registry = ev.registry.clone();
                may::config().set_stack_size(0x4000);
                may::go!(move || {
                    match future.wait() {
                        Err(e) => result_reply.resolve_err(e),
                        Ok(_) => {
                            let then_ev = Evaluator::with_registry(registry);
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
            InstanceKind::ClassObject => dispatch_class(ev, handle, selector, args, env),
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

fn expect_arity(_selector: &Arc<str>, expected: usize, got: usize) -> Result<(), EvalError> {
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

fn as_block(v: Value, selector: &Arc<str>) -> Result<Arc<Block>, EvalError> {
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
fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51afd7ed558ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ceb9fe1a85ec53);
    k ^= k >> 33;
    k
}

/// FNV-1a over the UTF-8 bytes of a string.
fn fnv1a_hash(s: &str) -> i64 {
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
fn hash_value(v: &Value) -> i64 {
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
        Value::Namespace(ns) => fmix64(Arc::as_ptr(ns) as usize as u64) as i64,
    }
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
    kind: &'static str,
    sel: &Arc<str>,
    mut args: Vec<Value>,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "error:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match args.remove(0) {
                Value::Str(s) => s,
                other => Arc::from(format!("{other}").as_str()),
            };
            Err(EvalError::Signal(msg))
        }
        "_primSignal:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match args.remove(0) {
                Value::Str(s) => s,
                other => Arc::from(format!("{other}").as_str()),
            };
            Err(EvalError::Signal(msg))
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
    env: &mut Env,
) -> Result<Value, EvalError> {
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
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string().as_str())))
        }
        "asFloat" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(n as f64))
        }
        "timesRepeat:" => {
            expect_arity(sel, 1, args.len())?;
            let blk = as_block(args.remove(0), sel)?;
            for _ in 0..n.max(0) {
                ev.eval_block(&blk, vec![], env)?;
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
        _ => dispatch_primitive_fallback("Int", sel, args),
    }
}

// ── LargeInt ──────────────────────────────────────────────────────────────────

fn dispatch_largeint(
    _ev: &Evaluator,
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
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string().as_str())))
        }
        "asFloat" => {
            expect_arity(sel, 0, args.len())?;
            let f = n.to_f64().unwrap_or(f64::INFINITY);
            Ok(Value::Float(f))
        }
        _ => dispatch_primitive_fallback("LargeInt", sel, args),
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
    _ev: &Evaluator,
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
        _ => dispatch_primitive_fallback("Float", sel, args),
    }
}

// ── Bool ─────────────────────────────────────────────────────────────────────

fn dispatch_bool(
    ev: &Evaluator,
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
            match force_value(args.remove(0))? {
                Value::Bool(b2) => Ok(Value::Bool(b && b2)),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be Bool"
                ))),
            }
        }
        "|" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Bool(b2) => Ok(Value::Bool(b || b2)),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be Bool"
                ))),
            }
        }
        "and:" => {
            expect_arity(sel, 1, args.len())?;
            if !b {
                return Ok(Value::Bool(false));
            }
            let blk = as_block(args.remove(0), sel)?;
            let f = ev.eval_block(&blk, vec![], env)?;
            match f {
                Value::Bool(b2) => Ok(Value::Bool(b2)),
                Value::Future(_) => force_value(f),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: block must return Bool"
                ))),
            }
        }
        "or:" => {
            expect_arity(sel, 1, args.len())?;
            if b {
                return Ok(Value::Bool(true));
            }
            let blk = as_block(args.remove(0), sel)?;
            let f = ev.eval_block(&blk, vec![], env)?;
            match f {
                Value::Bool(b2) => Ok(Value::Bool(b2)),
                Value::Future(_) => force_value(f),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: block must return Bool"
                ))),
            }
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
        _ => dispatch_primitive_fallback("Bool", sel, args),
    }
}

// ── Nil ───────────────────────────────────────────────────────────────────────

fn dispatch_nil(
    _ev: &Evaluator,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
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
        _ => dispatch_primitive_fallback("Nil", sel, args),
    }
}

// ── String ────────────────────────────────────────────────────────────────────

fn dispatch_str(
    _ev: &Evaluator,
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
        _ => dispatch_primitive_fallback("String", sel, args),
    }
}

// ── Symbol ────────────────────────────────────────────────────────────────────

fn dispatch_symbol(
    _ev: &Evaluator,
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
        _ => dispatch_primitive_fallback("Symbol", sel, args),
    }
}

// ── Block ─────────────────────────────────────────────────────────────────────

fn dispatch_block(
    ev: &Evaluator,
    blk: Arc<Block>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
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
        "whileTrue:" => {
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;
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
            }
            Ok(Value::Nil)
        }
        "whileFalse:" => {
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;
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
            }
            Ok(Value::Nil)
        }
        "whileTrue" => {
            expect_arity(sel, 0, args.len())?;
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
            }
            Ok(Value::Nil)
        }
        "whileFalse" => {
            expect_arity(sel, 0, args.len())?;
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
            }
            Ok(Value::Nil)
        }
        "on:do:" => {
            expect_arity(sel, 2, args.len())?;
            let _condition = args.remove(0); // class-filtered dispatch deferred to chunk 21+
            let handler = as_block(args.remove(0), sel)?;
            match ev.eval_block(&blk, vec![], env) {
                Ok(v) => Ok(v),
                Err(EvalError::Return(v, d)) => Err(EvalError::Return(v, d)),
                Err(EvalError::Signal(msg)) => {
                    ev.eval_block(&handler, vec![Value::Str(msg.clone())], env)
                }
                Err(e) => {
                    let msg = Value::Str(Arc::from(e.to_string().as_str()));
                    ev.eval_block(&handler, vec![msg], env)
                }
            }
        }
        "syncValue" => {
            expect_arity(sel, 0, args.len())?;
            ev.eval_block_sync(&blk, vec![], env)
        }
        _ => dispatch_primitive_fallback("Block", sel, args),
    }
}

// ── Object (user-defined instances) ──────────────────────────────────────────

fn dispatch_object_value(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let class = handle.0.class.clone();
    if let Some((method, def_class)) = class.lookup(&selector) {
        return ev.invoke_method(handle.clone(), def_class, method, args, env);
    }
    dispatch_object_primitive(handle, &selector, args)
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
    let current = ev.current_entity_id.get();
    if current == Some(entity_id) {
        let class = handle.0.class.clone();
        if let Some((method, def_class)) = class.lookup(&selector) {
            return ev.invoke_method(handle.clone(), def_class, method, args, env);
        }
        return dispatch_object_primitive(handle, &selector, args);
    }

    // Cross-entity or non-entity-context send: enqueue and return Future.
    let future = match &handle.0.kind {
        InstanceKind::Entity(d) => send_to_entity(d, selector, args),
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

pub(crate) fn dispatch_object_primitive(
    handle: InstanceHandle,
    sel: &Arc<str>,
    mut args: Vec<Value>,
) -> Result<Value, EvalError> {
    use crate::object::IndexedStorage;
    let obj = match &handle.0.kind {
        InstanceKind::ValueObject(o) => o.clone(),
        InstanceKind::Entity(d) => d.data.clone(),
        InstanceKind::ClassObject => {
            return Err(EvalError::MessageNotUnderstood {
                receiver_kind: "Class",
                selector: sel.clone(),
            });
        }
    };
    let self_val = Value::Instance(handle);
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
        "_primSignal:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match force_value(args.remove(0))? {
                Value::Str(s) => s,
                other => Arc::from(format!("{other}").as_str()),
            };
            Err(EvalError::Signal(msg))
        }
        "error:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match force_value(args.remove(0))? {
                Value::Str(s) => s,
                other => Arc::from(format!("error: argument was {}", other.kind()).as_str()),
            };
            Err(EvalError::Signal(msg))
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Object",
            selector: sel.clone(),
        }),
    }
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
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let class = handle.0.class.clone();
    // Check user-defined class-side methods BEFORE built-ins.
    if let Some((method, def_class)) = class.lookup_class_method(&selector) {
        return ev.invoke_class_method(handle, def_class, method, args, env);
    }
    dispatch_class_builtin(ev, handle, selector, args, env)
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
                let obj = match indexed_size {
                    Some(n) => Arc::new(Object::new_with_indexed(class.clone(), n)),
                    None => Arc::new(Object::new(class.clone())),
                };
                Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
                    class,
                    kind: InstanceKind::ValueObject(obj),
                }))))
            } else {
                let mut global_env = Env::new();
                ev.registry.populate_env(&mut global_env);
                let inst = spawn_entity_bare(class, ev.registry.clone(), global_env, indexed_size);
                Ok(Value::Instance(inst))
            }
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
                Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
                    class,
                    kind: InstanceKind::ValueObject(obj),
                }))))
            } else {
                let mut global_env = Env::new();
                ev.registry.populate_env(&mut global_env);
                let inst = spawn_entity_bare(
                    class,
                    ev.registry.clone(),
                    global_env,
                    Some(n.max(0) as usize),
                );
                Ok(Value::Instance(inst))
            }
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
    let class_state = Arc::new(InstanceState {
        class: new_class.clone(),
        kind: InstanceKind::ClassObject,
    });
    new_class
        .class_object
        .set(Arc::downgrade(&class_state))
        .ok();
    let class_handle = InstanceHandle(class_state);

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
