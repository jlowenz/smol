use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
use crate::eval_recursive::RecursiveEvaluator;
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
/// The iterative and recursive evaluators sometimes produce different *kinds* of
/// "program is invalid" errors for the same bad input (e.g. `TypeError` from a
/// Rust-level type check vs `MessageNotUnderstood` from letting dispatch fail
/// naturally). That is acceptable — the important properties are:
///
/// - Both raise a `Signal` with the same condition message (condition system).
/// - Both return the same control-flow error (Return/Resume/ReturnFromBlock/Retry).
/// - If one raises any of TypeError / MessageNotUnderstood / ArityMismatch /
///   UndefinedVariable the other raises one too (invalid program detected by both).
fn errors_equivalent(a: &EvalError, b: &EvalError) -> bool {
    match (a, b) {
        // Condition errors must carry the same message.
        (EvalError::Signal(_), EvalError::Signal(_)) => signal_message(a) == signal_message(b),
        // Control-flow errors must agree.
        (EvalError::Return(_, da), EvalError::Return(_, db)) => da == db,
        (EvalError::Resume(_), EvalError::Resume(_)) => true,
        (EvalError::ReturnFromBlock(_), EvalError::ReturnFromBlock(_)) => true,
        (EvalError::Retry, EvalError::Retry) => true,
        // "Invalid program" errors: both evaluators detect the error; exact kind may differ
        // because the iterative path uses Smol kernel methods (failing at dispatch) while
        // the recursive path checks argument types in Rust.
        (
            EvalError::TypeError(_)
            | EvalError::MessageNotUnderstood { .. }
            | EvalError::ArityMismatch { .. }
            | EvalError::UndefinedVariable(_),
            EvalError::TypeError(_)
            | EvalError::MessageNotUnderstood { .. }
            | EvalError::ArityMismatch { .. }
            | EvalError::UndefinedVariable(_),
        ) => true,
        // Different error categories = real divergence.
        _ => false,
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
    use crate::eval_iterative::probe;
    const BIG_STACK: usize = 0x80000; // 4 MB — far above any scenario under test
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(BIG_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        probe::reset();
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        let v = resolve(v).expect("eval future resolution");
        // Capture the probe marks on this thread *before* shutdown, which can
        // yield/migrate the coroutine (the marks are thread-local).
        let result = ProbeResult {
            value: v,
            max_depth: probe::max_depth(),
            cont_size: probe::cont_size(),
            stack_span: probe::stack_span(),
            level_delta: probe::level_delta(),
            sigcond_delta: probe::sigcond_delta(),
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

/// Evaluate Smol source, running both the iterative and recursive evaluators
/// and asserting they agree. Use for features supported by both paths.
///
/// Requires: `src` is valid Smol; program must not error.
pub(crate) fn eval_diff(src: &str) -> Value {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(TEST_COROUTINE_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");

        let ev_iter = Evaluator::new();
        let mut env_iter = Env::new();
        ev_iter.registry.populate_env(&mut env_iter);
        let v_iter = ev_iter
            .eval_program(&prog.stmts, &mut env_iter)
            .expect("eval_iter");
        let v_iter = resolve(v_iter).expect("eval_iter future resolution");
        ev_iter.shutdown();

        let ev_rec = RecursiveEvaluator::new();
        let mut env_rec = Env::new();
        ev_rec.inner.registry.populate_env(&mut env_rec);
        let v_rec = ev_rec
            .eval_program(&prog.stmts, &mut env_rec)
            .expect("eval_rec");
        let v_rec = resolve(v_rec).expect("eval_rec future resolution");
        ev_rec.shutdown();

        assert!(
            values_equivalent(&v_iter, &v_rec),
            "evaluator divergence on:\n{src}\n  iter: {v_iter:?}\n  rec:  {v_rec:?}"
        );
        tx.send(v_iter).unwrap();
    });
    rx.recv().expect("eval_diff coroutine panicked")
}

/// Evaluate Smol source and return the error, checking both evaluators agree.
///
/// Requires: `src` is valid Smol; program must produce an error.
pub(crate) fn eval_err_diff(src: &str) -> EvalError {
    let src = src.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(TEST_COROUTINE_STACK, move || {
        let prog = Parser::parse(&src).expect("parse");

        let ev_iter = Evaluator::new();
        let mut env_iter = Env::new();
        ev_iter.registry.populate_env(&mut env_iter);
        let err_iter = match ev_iter.eval_program(&prog.stmts, &mut env_iter) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("eval_iter: expected error, got Ok"),
            },
        };
        ev_iter.shutdown();

        let ev_rec = RecursiveEvaluator::new();
        let mut env_rec = Env::new();
        ev_rec.inner.registry.populate_env(&mut env_rec);
        let err_rec = match ev_rec.eval_program(&prog.stmts, &mut env_rec) {
            Err(e) => e,
            Ok(v) => match resolve(v) {
                Err(e) => e,
                Ok(_) => panic!("eval_rec: expected error, got Ok"),
            },
        };
        ev_rec.shutdown();

        assert!(
            errors_equivalent(&err_iter, &err_rec),
            "evaluator error divergence on:\n{src}\n  iter: {err_iter:?}\n  rec:  {err_rec:?}"
        );
        tx.send(err_iter).unwrap();
    });
    rx.recv().expect("eval_err_diff coroutine panicked")
}
