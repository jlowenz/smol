use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use num_bigint::BigInt;
use num_traits::Zero;

use crate::class::{Class, IndexedSlotKind};
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
    // `isNil` / `notNil` are universal.
    if selector.as_ref() == "isNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(matches!(recv, Value::Nil)));
    }
    if selector.as_ref() == "notNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(!matches!(recv, Value::Nil)));
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
        Value::Object(obj) => dispatch_object_value(ev, obj, selector, args, env),
        Value::Class(class) => dispatch_class(ev, class, selector, args, env),
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

fn as_int(v: Value, selector: &Arc<str>) -> Result<i64, EvalError> {
    match v {
        Value::Int(n) => Ok(n),
        _ => Err(EvalError::TypeError(format!("{selector}: argument must be Int"))),
    }
}

fn as_block(v: Value, selector: &Arc<str>) -> Result<Arc<Block>, EvalError> {
    match v {
        Value::Block(b) => Ok(b),
        _ => Err(EvalError::TypeError(format!("{selector}: argument must be a Block"))),
    }
}

fn bigint_from_value(v: Value, sel: &Arc<str>) -> Result<BigInt, EvalError> {
    match v {
        Value::Int(n) => Ok(BigInt::from(n)),
        Value::LargeInt(b) => Ok((*b).clone()),
        _ => Err(EvalError::TypeError(format!("{sel}: argument must be Int or LargeInt"))),
    }
}

fn normalize_bigint(b: BigInt) -> Value {
    use num_traits::ToPrimitive;
    if let Some(n) = b.to_i64() { Value::Int(n) } else { Value::LargeInt(Arc::new(b)) }
}

/// Smalltalk `//`: floor division (truncates toward negative infinity).
fn bigint_floor_div(n: BigInt, d: &BigInt) -> BigInt {
    use num_traits::Signed;
    let q = &n / d;
    let r = n % d;
    if !r.is_zero() && r.is_negative() != d.is_negative() { q - 1 } else { q }
}

/// Smalltalk `\\`: modulo with the sign of the divisor.
fn bigint_floor_rem(n: BigInt, d: &BigInt) -> BigInt {
    use num_traits::Signed;
    let r = &n % d;
    if !r.is_zero() && r.is_negative() != d.is_negative() { r + d } else { r }
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
            let arg = args.remove(0);
            match arg {
                Value::Int(m) => match n.checked_add(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) + BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) + (*m).clone())),
                _ => Err(EvalError::TypeError(format!("+: argument must be Int or LargeInt"))),
            }
        }
        "-" => {
            expect_arity(sel, 1, args.len())?;
            let arg = args.remove(0);
            match arg {
                Value::Int(m) => match n.checked_sub(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) - BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) - (*m).clone())),
                _ => Err(EvalError::TypeError(format!("-: argument must be Int or LargeInt"))),
            }
        }
        "*" => {
            expect_arity(sel, 1, args.len())?;
            let arg = args.remove(0);
            match arg {
                Value::Int(m) => match n.checked_mul(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) * BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) * (*m).clone())),
                _ => Err(EvalError::TypeError(format!("*: argument must be Int or LargeInt"))),
            }
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Int(d) => {
                    if d == 0 { return Err(EvalError::TypeError("division by zero".into())); }
                    Ok(Value::Int(n / d))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() { return Err(EvalError::TypeError("division by zero".into())); }
                    Ok(normalize_bigint(BigInt::from(n) / (*d).clone()))
                }
                _ => Err(EvalError::TypeError(format!("/: argument must be Int or LargeInt"))),
            }
        }
        "//" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Int(d) => {
                    if d == 0 { return Err(EvalError::TypeError("division by zero".into())); }
                    let r = n % d;
                    Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) { n / d - 1 } else { n / d }))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() { return Err(EvalError::TypeError("division by zero".into())); }
                    Ok(normalize_bigint(bigint_floor_div(BigInt::from(n), &d)))
                }
                _ => Err(EvalError::TypeError(format!("//: argument must be Int or LargeInt"))),
            }
        }
        "\\\\" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Int(d) => {
                    if d == 0 { return Err(EvalError::TypeError("division by zero".into())); }
                    let r = n % d;
                    Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) { r + d } else { r }))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() { return Err(EvalError::TypeError("division by zero".into())); }
                    Ok(normalize_bigint(bigint_floor_rem(BigInt::from(n), &d)))
                }
                _ => Err(EvalError::TypeError(format!("\\\\: argument must be Int or LargeInt"))),
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
            Ok(Value::Bool(Value::Int(n) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Int(n) != args.remove(0)))
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
        _ => Err(mnu("Int", sel)),
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
            Ok(Value::Bool(Value::LargeInt(n) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::LargeInt(n) != args.remove(0)))
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
        _ => Err(mnu("LargeInt", sel)),
    }
}

// ── Float ────────────────────────────────────────────────────────────────────

fn as_float_coerce(v: Value, sel: &Arc<str>) -> Result<f64, EvalError> {
    match v {
        Value::Float(f) => Ok(f),
        Value::Int(n) => Ok(n as f64),
        _ => Err(EvalError::TypeError(format!("{sel}: argument must be Float or Int"))),
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
            Ok(Value::Bool(Value::Float(f) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Float(f) != args.remove(0)))
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
        _ => Err(mnu("Float", sel)),
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
            if b { ev.eval_block(&blk, vec![], env) } else { Ok(Value::Nil) }
        }
        "ifFalse:" => {
            expect_arity(sel, 1, args.len())?;
            let blk = as_block(args.remove(0), sel)?;
            if !b { ev.eval_block(&blk, vec![], env) } else { Ok(Value::Nil) }
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
            match args.remove(0) {
                Value::Bool(b2) => Ok(Value::Bool(b && b2)),
                _ => Err(EvalError::TypeError(format!("{sel}: argument must be Bool"))),
            }
        }
        "|" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Bool(b2) => Ok(Value::Bool(b || b2)),
                _ => Err(EvalError::TypeError(format!("{sel}: argument must be Bool"))),
            }
        }
        "and:" => {
            expect_arity(sel, 1, args.len())?;
            if !b {
                return Ok(Value::Bool(false));
            }
            let blk = as_block(args.remove(0), sel)?;
            match ev.eval_block(&blk, vec![], env)? {
                Value::Bool(b2) => Ok(Value::Bool(b2)),
                _ => Err(EvalError::TypeError(format!("{sel}: block must return Bool"))),
            }
        }
        "or:" => {
            expect_arity(sel, 1, args.len())?;
            if b {
                return Ok(Value::Bool(true));
            }
            let blk = as_block(args.remove(0), sel)?;
            match ev.eval_block(&blk, vec![], env)? {
                Value::Bool(b2) => Ok(Value::Bool(b2)),
                _ => Err(EvalError::TypeError(format!("{sel}: block must return Bool"))),
            }
        }
        "not" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Bool(!b))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Bool(b) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Bool(b) != args.remove(0)))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(if b { "true" } else { "false" })))
        }
        _ => Err(mnu("Bool", sel)),
    }
}

// ── Nil ───────────────────────────────────────────────────────────────────────

fn dispatch_nil(
    ev: &Evaluator,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "ifNil:" => {
            expect_arity(sel, 1, args.len())?;
            let blk = as_block(args.remove(0), sel)?;
            ev.eval_block(&blk, vec![], env)
        }
        "ifNotNil:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Nil)
        }
        "ifNil:ifNotNil:" => {
            expect_arity(sel, 2, args.len())?;
            let nil_blk = as_block(args.remove(0), sel)?;
            let _ = args.remove(0);
            ev.eval_block(&nil_blk, vec![], env)
        }
        "ifNotNil:ifNil:" => {
            expect_arity(sel, 2, args.len())?;
            let _ = args.remove(0);
            let nil_blk = as_block(args.remove(0), sel)?;
            ev.eval_block(&nil_blk, vec![], env)
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(matches!(args.remove(0), Value::Nil)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(!matches!(args.remove(0), Value::Nil)))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from("nil")))
        }
        _ => Err(mnu("Nil", sel)),
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
            match args.remove(0) {
                Value::Str(other) => Ok(Value::Str(Arc::from(format!("{s}{other}").as_str()))),
                _ => Err(EvalError::TypeError(format!("{sel}: argument must be String"))),
            }
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Str(s) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Str(s) != args.remove(0)))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("'{s}'").as_str())))
        }
        "asSymbol" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Symbol(s))
        }
        "reversed" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(s.chars().rev().collect::<String>().as_str())))
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
            match args.remove(0) {
                Value::Str(sub) => Ok(Value::Bool(s.contains(sub.as_ref()))),
                _ => Err(EvalError::TypeError(format!("{sel}: argument must be String"))),
            }
        }
        _ => Err(mnu("String", sel)),
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
            Ok(Value::Bool(Value::Symbol(s) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Symbol(s) != args.remove(0)))
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
        _ => Err(mnu("Symbol", sel)),
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
                        )))
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
                        )))
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
                        )))
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
                        )))
                    }
                }
            }
            Ok(Value::Nil)
        }
        "on:do:" => {
            expect_arity(sel, 2, args.len())?;
            let _condition = args.remove(0); // only #Error recognised; ignore class for now
            let handler = as_block(args.remove(0), sel)?;
            match ev.eval_block(&blk, vec![], env) {
                Ok(v) => Ok(v),
                Err(EvalError::Return(v)) => Err(EvalError::Return(v)),
                Err(e) => {
                    let msg = Value::Str(Arc::from(e.to_string().as_str()));
                    ev.eval_block(&handler, vec![msg], env)
                }
            }
        }
        _ => Err(mnu("Block", sel)),
    }
}

// ── Object (user-defined instances) ──────────────────────────────────────────

fn dispatch_object_value(
    ev: &Evaluator,
    obj: Arc<Object>,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // Look up user-defined method.
    if let Some((method, def_class)) = obj.class.lookup(&selector) {
        return ev.invoke_method(obj.clone(), def_class, method, args, env);
    }
    // Fall back to primitive Object methods.
    dispatch_object_primitive(obj, &selector, args)
}

fn dispatch_object_primitive(
    obj: Arc<Object>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Object(obj) == args.remove(0)))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Object(obj) != args.remove(0)))
        }
        "yourself" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Object(obj))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("a {}", obj.class.name).as_str())))
        }
        "isNil" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Bool(false))
        }
        "notNil" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Bool(true))
        }
        "class" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Class(obj.class.clone()))
        }
        "isKindOf:" => {
            expect_arity(sel, 1, args.len())?;
            let target = match args.remove(0) {
                Value::Class(c) => c,
                _ => {
                    return Err(EvalError::TypeError(format!(
                        "{sel}: argument must be a Class"
                    )))
                }
            };
            Ok(Value::Bool(is_kind_of(&obj.class, &target)))
        }
        "respondsTo:" => {
            expect_arity(sel, 1, args.len())?;
            let sym = match args.remove(0) {
                Value::Symbol(s) => s,
                _ => {
                    return Err(EvalError::TypeError(format!(
                        "{sel}: argument must be a Symbol"
                    )))
                }
            };
            Ok(Value::Bool(obj.class.lookup(&sym).is_some()))
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
                other => return Err(EvalError::TypeError(format!(
                    "do: requires a Block, got {}",
                    other.kind()
                ))),
            };
            ev.with_namespace(ns, |ev| ev.eval_block(&block, vec![], env))
        }
        "name" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Symbol(ns.name.clone()))
        }
        "printString" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("Namespace({})", ns.name).as_str())))
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
    class: Arc<Class>,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // Check user-defined class-side methods BEFORE built-ins.
    if let Some((method, def_class)) = class.lookup_class_method(&selector) {
        return ev.invoke_class_method(class, def_class, method, args, env);
    }

    match selector.as_ref() {
        "new" => {
            expect_arity(&selector, 0, args.len())?;
            let obj = if class.indexed_slot_kind.is_some() {
                Arc::new(Object::new_with_indexed(class.clone(), 0))
            } else {
                Arc::new(Object::new(class.clone()))
            };
            let obj = ev.send_initialize(obj, env)?;
            Ok(Value::Object(obj))
        }
        "new:" => {
            expect_arity(&selector, 1, args.len())?;
            if class.indexed_slot_kind.is_none() {
                return Err(EvalError::TypeError(
                    "new: requires class to declare indexedSlots:".into(),
                ));
            }
            let n = as_int(args.remove(0), &selector)?;
            let obj = Arc::new(Object::new_with_indexed(class.clone(), n.max(0) as usize));
            let obj = ev.send_initialize(obj, env)?;
            Ok(Value::Object(obj))
        }
        "name" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(class.name.clone()))
        }
        "printString" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(class.name.clone()))
        }
        sel if sel.contains("subclass:") => handle_subclass(ev, class, selector, args, env),
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
                    ))
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
                    ))
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
                    ))
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
                        )))
                    }
                },
                _ => {
                    return Err(EvalError::TypeError(
                        "indexedSlots: argument must be a Symbol".into(),
                    ))
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

    let class_var_storage: HashMap<Arc<str>, Value> =
        class_var_names.iter().map(|n| (n.clone(), Value::Nil)).collect();

    let new_class = Arc::new(Class {
        name: name.clone(),
        superclass: Some(superclass),
        own_instance_vars: ivar_names,
        own_class_vars: class_var_names,
        class_var_storage: RefCell::new(class_var_storage),
        indexed_slot_kind,
        methods: RefCell::new(HashMap::new()),
        class_methods: RefCell::new(HashMap::new()),
    });

    let active_ns = ev.active_namespace();
    active_ns.register(new_class.clone());
    // Kernel classes get a global binding so they're always in scope by simple name.
    // Non-Kernel classes are scoped to the current frame (e.g. inside `Namespace do: [...]`);
    // outside that scope they are only reachable via `Ns::ClassName`.
    if Arc::ptr_eq(&active_ns, &ev.registry.kernel()) {
        env.define_global(name, Value::Class(new_class.clone()));
    } else {
        env.define(name, Value::Class(new_class.clone()));
    }
    Ok(Value::Class(new_class))
}
