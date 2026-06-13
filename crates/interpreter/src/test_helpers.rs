use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
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
/// Runs two independent `Evaluator` instances and asserts they produce equal results,
/// catching any global-state leakage between evaluator instances.
///
/// Requires: `src` is valid Smol; program must not error.
pub(crate) fn eval(src: &str) -> Value {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");

        // First evaluator instance.
        let ev_a = Evaluator::new();
        let mut env_a = Env::new();
        ev_a.registry.populate_env(&mut env_a);
        let v_a = ev_a.eval_program(&prog.stmts, &mut env_a).expect("eval_a");
        let v_a = resolve(v_a).expect("eval_a future resolution");
        ev_a.shutdown();

        // Second independent instance — catches global-state leakage.
        let ev_b = Evaluator::new();
        let mut env_b = Env::new();
        ev_b.registry.populate_env(&mut env_b);
        let v_b = ev_b.eval_program(&prog.stmts, &mut env_b).expect("eval_b");
        let v_b = resolve(v_b).expect("eval_b future resolution");
        ev_b.shutdown();

        assert!(
            values_equivalent(&v_a, &v_b),
            "evaluator instance divergence on:\n{src}\n  a: {v_a:?}\n  b: {v_b:?}"
        );
        tx.send(v_a).unwrap();
    });
    rx.recv().expect("eval coroutine panicked")
}

/// Evaluate Smol source inside a may coroutine and return the error.
///
/// Runs two independent `Evaluator` instances and asserts they produce the same error,
/// catching any global-state leakage between evaluator instances.
///
/// Requires: `src` is valid Smol; program must produce an error.
pub(crate) fn eval_err(src: &str) -> EvalError {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");

        // First evaluator instance.
        let ev_a = Evaluator::new();
        let mut env_a = Env::new();
        ev_a.registry.populate_env(&mut env_a);
        let err_a = match ev_a.eval_program(&prog.stmts, &mut env_a) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("eval_a: expected error, got Ok"),
            },
        };
        ev_a.shutdown();

        // Second independent instance.
        let ev_b = Evaluator::new();
        let mut env_b = Env::new();
        ev_b.registry.populate_env(&mut env_b);
        let err_b = match ev_b.eval_program(&prog.stmts, &mut env_b) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("eval_b: expected error, got Ok"),
            },
        };
        ev_b.shutdown();

        assert!(
            errors_equivalent(&err_a, &err_b),
            "evaluator instance error divergence on:\n{src}\n  a: {err_a:?}\n  b: {err_b:?}"
        );
        tx.send(err_a).unwrap();
    });
    rx.recv().expect("eval_err coroutine panicked")
}
