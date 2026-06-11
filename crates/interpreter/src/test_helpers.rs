use std::sync::Arc;

use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
use crate::eval_iterative::IEvaluator;
use crate::parser::Parser;
use crate::value::Value;

/// Extract the message text from an `EvalError::Signal(Value)` condition.
/// Returns `None` if the error is not a Signal.
pub(crate) fn signal_message(err: &EvalError) -> Option<String> {
    if let EvalError::Signal(v) = err {
        Some(crate::builtins::condition_message(v))
    } else {
        None
    }
}

/// Compare two `Value`s for the differential test.
///
/// Primitive/value types (Nil, Bool, Int, Float, Str, Symbol, LargeInt) are compared
/// by exact content — any divergence here is a real bug. Instances are compared by
/// class name: both evaluators create separate heap objects for the same class, so
/// pointer identity will always differ. Blocks and Futures are compared by discriminant
/// only — their identity is inherently pointer-based and differs across evaluators.
/// Namespaces are compared by name (hash_value also uses name-based hashing).
fn values_equivalent(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Symbol(a), Value::Symbol(b)) => a == b,
        (Value::LargeInt(a), Value::LargeInt(b)) => a == b,
        (Value::Instance(a), Value::Instance(b)) => a.0.class.name == b.0.class.name,
        (Value::Namespace(a), Value::Namespace(b)) => a.name == b.name,
        (Value::Block(_), Value::Block(_)) => true,
        (Value::Future(_), Value::Future(_)) => true,
        _ => false,
    }
}

/// Compare two `EvalError`s structurally for the differential test.
///
/// `Signal` errors are compared by their message text because two separate
/// evaluators produce different heap-allocated condition objects that are
/// never pointer-equal. All other error variants use `PartialEq` directly.
fn errors_equivalent(a: &EvalError, b: &EvalError) -> bool {
    match (a, b) {
        (EvalError::Signal(_), EvalError::Signal(_)) => signal_message(a) == signal_message(b),
        _ => a == b,
    }
}

/// Assert that an `EvalError` is a Signal whose message contains `substring`.
#[allow(dead_code)]
pub(crate) fn assert_signal_contains(err: &EvalError, substring: &str) {
    let msg = signal_message(err).unwrap_or_else(|| panic!("expected Signal, got {err:?}"));
    assert!(
        msg.contains(substring),
        "expected signal message to contain {substring:?}, got {msg:?}"
    );
}

/// Recursively resolves a `Value::Future` chain, propagating any error.
///
/// Requires: called from a may coroutine so that `FutureHandle::wait()` parks
///   the coroutine rather than blocking the underlying OS thread.
/// Ensures: returns the first non-Future value, or the first error in the chain.
fn resolve(v: Value) -> Result<Value, EvalError> {
    match v {
        Value::Future(f) => resolve(f.wait()?),
        v => Ok(v),
    }
}

/// Evaluate Smol source inside a may coroutine, auto-resolving any actor futures.
///
/// Runs both the recursive `Evaluator` and the iterative `IEvaluator` and asserts
/// they produce equal results, making every test an automatic differential test.
///
/// Requires: `src` is valid Smol; program must not error.
#[allow(clippy::arc_with_non_send_sync)]
pub(crate) fn eval(src: &str) -> Value {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");

        // Recursive evaluator.
        let ev_rec = Evaluator::new();
        let mut env_rec = Env::new();
        ev_rec.registry.populate_env(&mut env_rec);
        let v_rec = ev_rec
            .eval_program(&prog.stmts, &mut env_rec)
            .expect("recursive eval");
        let v_rec = resolve(v_rec).expect("recursive future resolution");
        ev_rec.shutdown();

        // Iterative evaluator.
        let ev_iter = IEvaluator::new(Arc::new(Evaluator::new()));
        let mut env_iter = Env::new();
        ev_iter.inner.registry.populate_env(&mut env_iter);
        let v_iter = ev_iter
            .eval_program(&prog.stmts, &mut env_iter)
            .expect("iterative eval");
        let v_iter = resolve(v_iter).expect("iterative future resolution");
        ev_iter.inner.shutdown();

        assert!(
            values_equivalent(&v_rec, &v_iter),
            "evaluator divergence on:\n{src}\n  recursive: {v_rec:?}\n  iterative: {v_iter:?}"
        );
        tx.send(v_rec).unwrap();
    });
    rx.recv().expect("eval coroutine panicked")
}

/// Evaluate Smol source inside a may coroutine and return the error.
///
/// Both evaluators must produce the same error (differential test).
///
/// Requires: `src` is valid Smol; program must produce an error.
#[allow(clippy::arc_with_non_send_sync)]
pub(crate) fn eval_err(src: &str) -> EvalError {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");

        // Recursive evaluator.
        let ev_rec = Evaluator::new();
        let mut env_rec = Env::new();
        ev_rec.registry.populate_env(&mut env_rec);
        let err_rec = match ev_rec.eval_program(&prog.stmts, &mut env_rec) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("recursive eval: expected error, got Ok"),
            },
        };
        ev_rec.shutdown();

        // Iterative evaluator.
        let ev_iter = IEvaluator::new(Arc::new(Evaluator::new()));
        let mut env_iter = Env::new();
        ev_iter.inner.registry.populate_env(&mut env_iter);
        let err_iter = match ev_iter.eval_program(&prog.stmts, &mut env_iter) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("iterative eval: expected error, got Ok"),
            },
        };
        ev_iter.inner.shutdown();

        assert!(
            errors_equivalent(&err_rec, &err_iter),
            "evaluator error divergence on:\n{src}\n  recursive: {err_rec:?}\n  iterative: {err_iter:?}"
        );
        tx.send(err_rec).unwrap();
    });
    rx.recv().expect("eval_err coroutine panicked")
}
