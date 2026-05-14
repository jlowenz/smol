use std::sync::Arc;

use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
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

// ── Int ───────────────────────────────────────────────────────────────────────

fn dispatch_int(
    ev: &Evaluator,
    n: i64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "+" => { expect_arity(sel, 1, args.len())?; Ok(Value::Int(n.wrapping_add(as_int(args.remove(0), sel)?))) }
        "-" => { expect_arity(sel, 1, args.len())?; Ok(Value::Int(n.wrapping_sub(as_int(args.remove(0), sel)?))) }
        "*" => { expect_arity(sel, 1, args.len())?; Ok(Value::Int(n.wrapping_mul(as_int(args.remove(0), sel)?))) }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            let d = as_int(args.remove(0), sel)?;
            if d == 0 { return Err(EvalError::TypeError("division by zero".into())); }
            Ok(Value::Int(n / d))
        }
        "//" => {
            expect_arity(sel, 1, args.len())?;
            let d = as_int(args.remove(0), sel)?;
            if d == 0 { return Err(EvalError::TypeError("division by zero".into())); }
            // Smalltalk `//` is floor division.
            Ok(Value::Int(n.div_euclid(d) - if n % d != 0 && (n < 0) != (d < 0) { 1 } else { 0 }))
        }
        "\\\\" => {
            expect_arity(sel, 1, args.len())?;
            let d = as_int(args.remove(0), sel)?;
            if d == 0 { return Err(EvalError::TypeError("division by zero".into())); }
            // Smalltalk `\\` is modulo (result has sign of divisor).
            let r = n % d;
            Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) { r + d } else { r }))
        }
        "<"  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(n < as_int(args.remove(0), sel)?)) }
        ">"  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(n > as_int(args.remove(0), sel)?)) }
        "<=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(n <= as_int(args.remove(0), sel)?)) }
        ">=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(n >= as_int(args.remove(0), sel)?)) }
        "="  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Int(n) == args.remove(0))) }
        "~=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Int(n) != args.remove(0))) }
        "negated" => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(n.wrapping_neg())) }
        "abs"     => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(n.wrapping_abs())) }
        "max:" => { expect_arity(sel, 1, args.len())?; Ok(Value::Int(n.max(as_int(args.remove(0), sel)?))) }
        "min:" => { expect_arity(sel, 1, args.len())?; Ok(Value::Int(n.min(as_int(args.remove(0), sel)?))) }
        "printString" => { expect_arity(sel, 0, args.len())?; Ok(Value::Str(Arc::from(n.to_string().as_str()))) }
        "asFloat"     => { expect_arity(sel, 0, args.len())?; Ok(Value::Float(n as f64)) }
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
            if step == 0 { return Err(EvalError::TypeError("to:by:do: step must be non-zero".into())); }
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
        "+" => { expect_arity(sel, 1, args.len())?; Ok(Value::Float(f + as_float_coerce(args.remove(0), sel)?)) }
        "-" => { expect_arity(sel, 1, args.len())?; Ok(Value::Float(f - as_float_coerce(args.remove(0), sel)?)) }
        "*" => { expect_arity(sel, 1, args.len())?; Ok(Value::Float(f * as_float_coerce(args.remove(0), sel)?)) }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f / as_float_coerce(args.remove(0), sel)?))
        }
        "<"  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(f < as_float_coerce(args.remove(0), sel)?)) }
        ">"  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(f > as_float_coerce(args.remove(0), sel)?)) }
        "<=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(f <= as_float_coerce(args.remove(0), sel)?)) }
        ">=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(f >= as_float_coerce(args.remove(0), sel)?)) }
        "="  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Float(f) == args.remove(0))) }
        "~=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Float(f) != args.remove(0))) }
        "negated" => { expect_arity(sel, 0, args.len())?; Ok(Value::Float(-f)) }
        "abs"     => { expect_arity(sel, 0, args.len())?; Ok(Value::Float(f.abs())) }
        "truncated" => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(f.trunc() as i64)) }
        "rounded"   => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(f.round() as i64)) }
        "ceiling"   => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(f.ceil() as i64)) }
        "floor"     => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(f.floor() as i64)) }
        "asInt"     => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(f as i64)) }
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
            if b { ev.eval_block(&true_blk, vec![], env) } else { ev.eval_block(&false_blk, vec![], env) }
        }
        "ifFalse:ifTrue:" => {
            expect_arity(sel, 2, args.len())?;
            let false_blk = as_block(args.remove(0), sel)?;
            let true_blk = as_block(args.remove(0), sel)?;
            if b { ev.eval_block(&true_blk, vec![], env) } else { ev.eval_block(&false_blk, vec![], env) }
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
            if !b { return Ok(Value::Bool(false)); }
            let blk = as_block(args.remove(0), sel)?;
            match ev.eval_block(&blk, vec![], env)? {
                Value::Bool(b2) => Ok(Value::Bool(b2)),
                _ => Err(EvalError::TypeError(format!("{sel}: block must return Bool"))),
            }
        }
        "or:" => {
            expect_arity(sel, 1, args.len())?;
            if b { return Ok(Value::Bool(true)); }
            let blk = as_block(args.remove(0), sel)?;
            match ev.eval_block(&blk, vec![], env)? {
                Value::Bool(b2) => Ok(Value::Bool(b2)),
                _ => Err(EvalError::TypeError(format!("{sel}: block must return Bool"))),
            }
        }
        "not" => { expect_arity(sel, 0, args.len())?; Ok(Value::Bool(!b)) }
        "="  => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Bool(b) == args.remove(0))) }
        "~=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Bool(b) != args.remove(0))) }
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
        "printString" => { expect_arity(sel, 0, args.len())?; Ok(Value::Str(Arc::from("nil"))) }
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
        "size" => { expect_arity(sel, 0, args.len())?; Ok(Value::Int(s.chars().count() as i64)) }
        "at:" => {
            expect_arity(sel, 1, args.len())?;
            let idx = as_int(args.remove(0), sel)? as usize;
            // 1-based index (Smalltalk convention).
            s.chars().nth(idx.wrapping_sub(1))
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
        "=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Str(s) == args.remove(0))) }
        "~=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Str(s) != args.remove(0))) }
        "printString" => { expect_arity(sel, 0, args.len())?; Ok(Value::Str(Arc::from(format!("'{s}'").as_str()))) }
        "asSymbol" => { expect_arity(sel, 0, args.len())?; Ok(Value::Symbol(s)) }
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
        "=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Symbol(s) == args.remove(0))) }
        "~=" => { expect_arity(sel, 1, args.len())?; Ok(Value::Bool(Value::Symbol(s) != args.remove(0))) }
        "printString" => { expect_arity(sel, 0, args.len())?; Ok(Value::Str(Arc::from(format!("#{s}").as_str()))) }
        "asString" => { expect_arity(sel, 0, args.len())?; Ok(Value::Str(s)) }
        "asSymbol" => { expect_arity(sel, 0, args.len())?; Ok(Value::Symbol(s)) }
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
                    Value::Bool(true) => { ev.eval_block(&body, vec![], env)?; }
                    Value::Bool(false) => break,
                    _ => return Err(EvalError::TypeError(format!("{sel}: condition block must return Bool"))),
                }
            }
            Ok(Value::Nil)
        }
        "whileFalse:" => {
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;
            loop {
                match ev.eval_block(&blk, vec![], env)? {
                    Value::Bool(false) => { ev.eval_block(&body, vec![], env)?; }
                    Value::Bool(true) => break,
                    _ => return Err(EvalError::TypeError(format!("{sel}: condition block must return Bool"))),
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
                    _ => return Err(EvalError::TypeError(format!("{sel}: block must return Bool"))),
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
                    _ => return Err(EvalError::TypeError(format!("{sel}: block must return Bool"))),
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
