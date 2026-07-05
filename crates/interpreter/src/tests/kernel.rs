use crate::eval::EvalError;
use crate::test_helpers::{eval, eval_diff, eval_err_diff};
use crate::value::Value;
use std::sync::Arc;

#[test]
fn class_print_string() {
    // Tests that sending `printString` to a class returns a String matching the class name.
    // Note: The `println!` in `builtins.rs` for `printString` is intentionally kept
    // for now as per user instruction, even though it's a side-effect.
    assert_eq!(
        eval_diff("Object printString"),
        Value::Str(Arc::from("Object"))
    );
    assert_eq!(
        eval_diff("Array printString"),
        Value::Str(Arc::from("Array"))
    );
    assert_eq!(
        eval_diff("ValueHolder printString"),
        Value::Str(Arc::from("ValueHolder"))
    );
}

#[test]
fn value_holder_set() {
    // Tests ValueHolder >> set: method
    assert_eq!(
        eval_diff("| vh | vh := ValueHolder initialValue: 1. vh set: 5. vh get"),
        Value::Int(5)
    );
    assert_eq!(
        eval_diff("| vh | vh := ValueHolder initialValue: 'hello'. vh set: 'world'. vh get"),
        Value::Str(Arc::from("world"))
    );
}

#[test]
fn array_at_out_of_bounds_error() {
    // Tests Array >> at: out of bounds access
    let err = eval_err_diff("| a | a := Array new: 2. a at: 2");
    assert!(format!("{err}").contains("out of bounds"));
    let err = eval_err_diff("| a | a := Array new: 2. a at: -1");
    assert!(format!("{err}").contains("out of bounds"));
}

#[test]
fn array_at_put_out_of_bounds_error() {
    // Tests Array >> at:put: out of bounds access
    let err = eval_err_diff("| a | a := Array new: 2. a at: 2 put: 10");
    assert!(
        matches!(err, EvalError::TypeError(msg) if msg.contains("_primAt:put:: index 2 out of bounds (size 2)"))
    );
    let err = eval_err_diff("| a | a := Array new: 2. a at: -1 put: 10");
    assert!(
        matches!(err, EvalError::TypeError(msg) if msg.contains("_primAt:put:: index -1 out of bounds (size 2)"))
    );
}

// ── S2: ValueHolder + Array gap tests ─────────────────────────────────────

#[test]
fn value_holder_update_receives_old_value() {
    // update: must pass the current value to the block and store the result.
    assert_eq!(
        eval_diff("| vh | vh := ValueHolder initialValue: 10. vh update: [:old | old + 5]. vh get"),
        Value::Int(15)
    );
    // Chaining: each update sees the result of the previous one.
    assert_eq!(
        eval_diff(
            "| vh | vh := ValueHolder initialValue: 1. vh update: [:v | v * 2]. vh update: [:v | v * 3]. vh get"
        ),
        Value::Int(6)
    );
}

#[test]
fn array_copy_from_to_single_element() {
    // single-element range: start == stop; result is an Array of size 1
    let src = "| a r | a := Array new: 5. 0 to: 4 do: [:i | a at: i put: i * 10]. r := a copyFrom: 2 to: 2. r size";
    assert_eq!(eval_diff(src), Value::Int(1));
    let src2 = "| a r | a := Array new: 5. 0 to: 4 do: [:i | a at: i put: i * 10]. r := a copyFrom: 2 to: 2. r at: 0";
    assert_eq!(eval_diff(src2), Value::Int(20));
}

// ── S3: ByteArray + OrderedCollection gap tests ────────────────────────────

#[test]
fn oc_add_last_appends_element() {
    // addLast: is an alias for add: — verify it appends and returns the value.
    assert_eq!(
        eval_diff("| oc | oc := OrderedCollection new. oc addLast: 42. oc size"),
        Value::Int(1)
    );
    assert_eq!(
        eval_diff("| oc | oc := OrderedCollection new. oc addLast: 7. oc at: 0"),
        Value::Int(7)
    );
    // Multiple addLast: calls preserve insertion order.
    assert_eq!(
        eval_diff(
            "| oc | oc := OrderedCollection new. oc addLast: 1. oc addLast: 2. oc addLast: 3. oc last"
        ),
        Value::Int(3)
    );
}

// ── Duration ──────────────────────────────────────────────────────────────────

#[test]
fn duration_ms_factory() {
    assert_eq!(
        eval_diff("(Duration ms: 500) asMilliseconds"),
        Value::Int(500)
    );
    assert_eq!(eval_diff("(Duration ms: 0) asMilliseconds"), Value::Int(0));
}

#[test]
fn duration_s_factory() {
    assert_eq!(
        eval_diff("(Duration s: 2) asMilliseconds"),
        Value::Int(2000)
    );
    assert_eq!(eval_diff("(Duration s: 0) asMilliseconds"), Value::Int(0));
}

#[test]
fn duration_minutes_factory() {
    assert_eq!(
        eval_diff("(Duration minutes: 1) asMilliseconds"),
        Value::Int(60000)
    );
}

#[test]
fn duration_print_string() {
    assert_eq!(
        eval_diff("(Duration ms: 250) printString"),
        Value::Str(Arc::from("250ms"))
    );
}

// ── Timer ─────────────────────────────────────────────────────────────────────

#[test]
fn timer_is_pending_true_before_fire() {
    // Timer created with a large delay; isPending must return true immediately.
    let src = r#"
        Object subclass: #TA1 instanceVariableNames: ''.
        TA1 >> go [
            | t |
            t := Timer after: (Duration ms: 9999) do: [nil].
            ^t isPending
        ]
        | a |
        a := TA1 new.
        (a go) await
    "#;
    assert_eq!(eval(src), Value::Bool(true));
}

#[test]
fn timer_cancel_makes_not_pending() {
    // After cancel, isPending must return false.
    let src = r#"
        Object subclass: #TA2 instanceVariableNames: ''.
        TA2 >> go [
            | t |
            t := Timer after: (Duration ms: 9999) do: [nil].
            t cancel.
            ^t isPending
        ]
        | a |
        a := TA2 new.
        (a go) await
    "#;
    assert_eq!(eval(src), Value::Bool(false));
}

#[test]
fn timer_fires_in_entity() {
    // A 50 ms timer fires into the home entity and executes the block.
    // After sleeping 300 ms the result ivar must be 99.
    const STACK: usize = 0x4_0000; // 256 KB — matches test_helpers::eval
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    crate::entity::init_coroutine_stack();
    let _ = may::go_with!(STACK, move || {
        let setup = r#"
            Object subclass: #TA3 instanceVariableNames: 'result'.
            TA3 >> initialize [ result := ValueHolder initialValue: 0 ]
            TA3 >> go [ Timer after: (Duration ms: 50) do: [ result set: 99 ] ]
            TA3 >> getResult [ ^result get ]
            | a |
            a := TA3 new.
            (a go) await.
            a
        "#;
        let prog = crate::parser::Parser::parse(setup).unwrap();
        let ev = crate::eval::Evaluator::new();
        let mut env = crate::env::Env::new();
        ev.registry.populate_env(&mut env);
        // actor is the TA3 instance
        let actor = crate::builtins_iterative::force_value(
            &ev,
            ev.eval_program(&prog.stmts, &mut env).unwrap(),
        )
        .unwrap();
        // Give the 50 ms timer ample time to fire
        may::coroutine::sleep(std::time::Duration::from_millis(300));
        // Query result — cross-entity send returns a Future chain
        let result = crate::builtins_iterative::force_value(
            &ev,
            crate::builtins::dispatch(&ev, actor, Arc::from("getResult"), vec![], &mut env)
                .unwrap(),
        )
        .unwrap();
        // ValueHolder::get is also cross-entity, so result may be a nested Future
        let final_result = crate::builtins_iterative::force_value(&ev, result).unwrap();
        ev.shutdown();
        tx.send(final_result).unwrap();
    });
    assert_eq!(rx.recv().unwrap(), Value::Int(99));
}
