use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
use crate::parser::Parser;
use crate::value::Value;

/// Stack size for test-helper coroutines in words (× `size_of::<usize>()`).
///
/// Test coroutines run `Evaluator::new()` which loads and evaluates
/// `kernel.smol` — this involves the recursive-descent parser and bootstraps
/// entity actors, both of which consume significant Rust stack in debug builds.
/// Production entity coroutines use `Evaluator::with_registry` (no kernel
/// loading) and are governed by the much smaller `COROUTINE_STACK_SIZE`.
const TEST_COROUTINE_STACK: usize = 0x8000; // 256 KB in bytes on 64-bit

/// Extract the message text from an `EvalError::Signal(Value)` condition.
/// Returns `None` if the error is not a Signal.
pub(crate) fn signal_message(err: &EvalError) -> Option<String> {
    if let EvalError::Signal(v) = err {
        Some(crate::builtins_iterative::condition_message(v))
    } else {
        None
    }
}

/// Assert that an `EvalError` is a Signal whose message contains `substring`.
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

/// Evaluate Smol source using the iterative evaluator only.
///
/// Requires: `src` is valid Smol; program must not error.
pub(crate) fn eval(src: &str) -> Value {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(TEST_COROUTINE_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        let v = resolve(v).expect("eval future resolution");
        ev.shutdown();
        tx.send(v).unwrap();
    });
    rx.recv().expect("eval coroutine panicked")
}

/// Evaluate `src` with coverage enabled on the evaluator's registry; return the
/// set of statement spans recorded as executed by the user program (coverage is
/// enabled *after* `Evaluator::new()`, so kernel-load execution is excluded).
///
/// Requires: `src` is valid Smol; program must not error.
pub(crate) fn eval_coverage(src: &str) -> std::collections::HashSet<crate::span::Span> {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(TEST_COROUTINE_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let cov = ev.registry.enable_coverage();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        let _ = resolve(v).expect("eval future resolution");
        let touched = cov.lock().touched().clone();
        ev.shutdown();
        tx.send(touched).unwrap();
    });
    rx.recv().expect("eval_coverage coroutine panicked")
}

/// Evaluate Smol source on an oversized coroutine stack and report the maximum
/// `step_loop` recursion depth reached plus `size_of::<Cont>()`.
///
/// Used by C31.5 to measure per-level stack cost on scenarios that overflow the
/// production `COROUTINE_STACK_SIZE` (e.g. nested synchronous `HandlerContext`
/// `run:`). The generous stack lets the scenario complete so the depth can be
/// recorded; it says nothing about whether the scenario fits in 256 KB.
///
/// Requires: `src` is valid Smol; program must not error. The `step_probe`
///   feature must be enabled.
/// Ensures: returns `ProbeResult` with the result value and the recorded marks.
#[cfg(feature = "step_probe")]
pub(crate) struct ProbeResult {
    pub value: Value,
    pub max_depth: usize,
    pub cont_size: usize,
    /// Total Rust stack span (bytes) across all `step_loop` entries.
    pub stack_span: usize,
    /// Largest parent→child `step_loop` entry delta (bytes) — per-level growth.
    pub level_delta: usize,
    /// Max stack from a `step_loop` entry down to a `signal_condition` entry.
    pub sigcond_delta: usize,
}

#[cfg(feature = "step_probe")]
pub(crate) fn eval_probe(src: &str) -> ProbeResult {
    const BIG_STACK: usize = 0x80000; // 4 MB — far above any scenario under test
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(BIG_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        ev.probe.reset();
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        let v = resolve(v).expect("eval future resolution");
        // Read the probe marks from this evaluator before shutdown.
        let result = ProbeResult {
            value: v,
            max_depth: ev.probe.max_depth(),
            cont_size: ev.probe.cont_size(),
            stack_span: ev.probe.stack_span(),
            level_delta: ev.probe.level_delta(),
            sigcond_delta: ev.probe.sigcond_delta(),
        };
        ev.shutdown();
        tx.send(result).unwrap();
    });
    rx.recv().expect("eval_probe coroutine panicked")
}

/// Evaluate Smol source using the iterative evaluator and return the error.
///
/// Requires: `src` is valid Smol; program must produce an error.
pub(crate) fn eval_err(src: &str) -> EvalError {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(TEST_COROUTINE_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        let err = match ev.eval_program(&prog.stmts, &mut env) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("eval: expected error, got Ok"),
            },
        };
        ev.shutdown();
        tx.send(err).unwrap();
    });
    rx.recv().expect("eval_err coroutine panicked")
}

/// Evaluate Smol source (iterative evaluator) and return the resolved value.
///
/// Formerly ran a second, recursive evaluator and asserted agreement; the
/// recursive evaluator was retired (its collections were entities that ran on the
/// iterative evaluator anyway, so the cross-check was largely illusory). Retained
/// as an alias of [`eval`] so existing call sites need not change.
///
/// Requires: `src` is valid Smol; program must not error.
pub(crate) fn eval_diff(src: &str) -> Value {
    eval(src)
}

/// Evaluate Smol source (iterative evaluator) and return the error.
///
/// Alias of [`eval_err`] after the recursive evaluator's retirement (see
/// [`eval_diff`]).
///
/// Requires: `src` is valid Smol; program must produce an error.
pub(crate) fn eval_err_diff(src: &str) -> EvalError {
    eval_err(src)
}
