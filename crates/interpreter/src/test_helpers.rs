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
/// The evaluation runs inside a `may::go!` coroutine so that `FutureHandle::wait()`
/// parks via may's scheduler (cooperative yield) rather than blocking the OS thread.
/// This allows entity coroutines on may worker threads to make progress and resolve
/// futures, avoiding the deadlock that occurs when the test thread blocks on
/// `may::sync::Condvar::wait()` while waiting for a coroutine-side `notify_all()`.
/// The result is returned to the OS test thread via a `std::sync::mpsc` channel,
/// whose OS-level send/recv correctly crosses the coroutine-to-thread boundary.
///
/// Requires: `src` is valid Smol; program must not error.
pub(crate) fn eval(src: &str) -> Value {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        tx.send(resolve(v).expect("future resolution")).unwrap();
        ev.shutdown();
    });
    rx.recv().expect("eval coroutine panicked")
}

/// Evaluate Smol source inside a may coroutine and return the error.
///
/// Requires: `src` is valid Smol; program must produce an error.
pub(crate) fn eval_err(src: &str) -> EvalError {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        let err = match ev.eval_program(&prog.stmts, &mut env) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("expected error, got Ok"),
            },
        };
        tx.send(err).unwrap();
        ev.shutdown();
    });
    rx.recv().expect("eval_err coroutine panicked")
}
