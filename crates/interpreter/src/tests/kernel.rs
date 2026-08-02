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
        // Poll for the 50 ms timer to fire rather than sleeping a single fixed span:
        // under parallel test load the timer/entity coroutine can be starved well
        // past any fixed budget, which made a blind 300 ms sleep flaky. Retry the
        // (cross-entity) getResult until it reads 99 or a generous ~5 s deadline
        // elapses — fast in the common case, robust under contention.
        let mut final_result = Value::Nil;
        for _ in 0..100 {
            may::coroutine::sleep(std::time::Duration::from_millis(50));
            // getResult is cross-entity (Future chain); ValueHolder::get is too, so
            // the reply may be a nested Future — force twice.
            let result = crate::builtins_iterative::force_value(
                &ev,
                crate::builtins::dispatch(
                    &ev,
                    actor.clone(),
                    Arc::from("getResult"),
                    vec![],
                    &mut env,
                )
                .unwrap(),
            )
            .unwrap();
            final_result = crate::builtins_iterative::force_value(&ev, result).unwrap();
            if final_result == Value::Int(99) {
                break;
            }
        }
        ev.shutdown();
        tx.send(final_result).unwrap();
    });
    assert_eq!(rx.recv().unwrap(), Value::Int(99));
}

// ── C34 S1b: to:do: / to:by:do: are Smol methods on Number ────────────────────
//
// Before S1b these were not methods at all: `dispatch_to_stack` step (1c) matched
// the SELECTOR against a `Value::Int` receiver and pushed a `Cont::ToDoStep`
// continuation. Both the special-case and the continuation variant were deleted
// in the same commit as these methods, so nothing can silently fall back to them.

#[test]
fn to_do_iterates_ascending() {
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        1 to: 4 do: [:i | h update: [:a | a + i]]. h get value"
        ),
        Value::Int(10)
    );
}

#[test]
fn to_do_answers_nil_and_skips_an_empty_range() {
    assert_eq!(eval("5 to: 1 do: [:i | i]"), Value::Nil);
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        5 to: 1 do: [:i | h update: [:a | a + 1]]. h get value"
        ),
        Value::Int(0)
    );
}

#[test]
fn to_by_do_handles_both_directions() {
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        0 to: 6 by: 2 do: [:i | h update: [:a | a + i]]. h get value"
        ),
        Value::Int(12)
    );
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        10 to: 0 by: -3 do: [:i | h update: [:a | a + i]]. h get value"
        ),
        Value::Int(22)
    );
}

#[test]
fn to_do_is_now_visible_to_reflection() {
    // The Q14 observability complaint in its purest form: as an evaluator
    // special-case, `to:do:` answered false here.
    assert_eq!(eval("3 respondsTo: #to:do:"), Value::Bool(true));
    assert_eq!(eval("3 respondsTo: #to:by:do:"), Value::Bool(true));
}

#[test]
fn to_do_works_for_every_numeric_leaf() {
    // Defining on Number rather than on one encoding fixes the Float and
    // ArbitraryInteger gap for free: both used to answer doesNotUnderstand.
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        1.5 to: 3 do: [:i | h update: [:a | a + 1]]. h get value"
        ),
        Value::Int(2)
    );
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        (100 factorial) to: (100 factorial) do: [:i | h update: [:a | a + 1]].
        h get value"
        ),
        Value::Int(1)
    );
}

#[test]
fn to_do_can_be_overridden() {
    // Not because overriding iteration is common, but because the construct is
    // now describable in the language rather than privileged by the evaluator.
    let src = "
        Number subclass: #Countdown instanceVariableNames: ''
            classVariableNames: '' indexedSlots: #none.
        Countdown >> to: stop do: aBlock [ ^#overridden ]
        Countdown new to: 3 do: [:i | i]
    ";
    assert_eq!(eval(src), Value::Symbol(Arc::from("overridden")));
}

#[test]
fn to_do_awaits_futures_in_every_position() {
    // OQ-1b. Step (1c) called Rust `as_int` on the bound, which REJECTED a Future
    // rather than awaiting it — contradicting asynchronous-everywhere. A Smol
    // to:do: compares bounds with ordinary sends, which force futures.
    let limit = "| h | h := ValueHolder initialValue: 3.
        | c | c := ValueHolder initialValue: 0.
        1 to: h get do: [:i | c update: [:a | a + 1]]. c get value";
    assert_eq!(eval(limit), Value::Int(3));

    let receiver = "| h | h := ValueHolder initialValue: 1.
        | c | c := ValueHolder initialValue: 0.
        h get to: 3 do: [:i | c update: [:a | a + 1]]. c get value";
    assert_eq!(eval(receiver), Value::Int(3));

    let step = "| h | h := ValueHolder initialValue: 2.
        | c | c := ValueHolder initialValue: 0.
        0 to: 6 by: h get do: [:i | c update: [:a | a + 1]]. c get value";
    assert_eq!(eval(step), Value::Int(4));
}

#[test]
fn deep_to_do_is_o1_stack_and_linear() {
    // Acceptance criteria 1 and 3 (plan §6 OQ-1). 100k iterations exhaust the
    // 64 KB release coroutine stack if any per-iteration Rust frame survives,
    // and hang rather than fail subtly if the loop is not linear in time. The
    // body is deliberately PURE: the criteria are about stack depth and time
    // complexity, and a cross-entity `update:` per iteration would dominate both
    // the measurement and the runtime. Iteration COUNT is covered separately by
    // `to_do_iterates_ascending`.
    assert_eq!(eval("1 to: 100000 do: [:i | i]"), Value::Nil);
}

#[test]
fn nested_to_do_does_not_grow_the_stack() {
    // Nesting is the case Cont::ToDoStep existed for; TCO plus push_block_frame
    // carry it now. See also step_probe_nested_loops, which measures depth = 1.
    assert_eq!(
        eval("1 to: 300 do: [:i | 1 to: 300 do: [:j | j]]"),
        Value::Nil
    );
}

#[test]
fn nested_to_do_runs_the_inner_loop() {
    // Separated from the stack test above so the effect is verified without
    // paying 90k entity sends for it.
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
        1 to: 10 do: [:i | 1 to: 10 do: [:j | h update: [:a | a + 1]]].
        h get value"
        ),
        Value::Int(100)
    );
}

#[test]
fn to_do_works_for_a_number_wrapper() {
    // Defining on Number rather than on the leaves means a user wrapper inherits
    // iteration through the tower's value/withValue: seam (numeric_tower_plan §4e),
    // in both receiver and limit position, with no leaf edited.
    let boxed = "Number subclass: #Boxed instanceVariableNames: 'x'
            classVariableNames: '' indexedSlots: #none.
        Boxed class >> of: v [ ^self _primNewValue: [:w | w setX: v] ]
        Boxed >> setX: v [ x := v ]
        Boxed >> value [ ^x ]
        Boxed >> withValue: v [ ^Boxed of: v ]
        | h | h := ValueHolder initialValue: 0. ";

    assert_eq!(
        eval(&format!(
            "{boxed} (Boxed of: 1) to: 3 do: [:i | h update: [:a | a + 1]]. h get value"
        )),
        Value::Int(3)
    );
    assert_eq!(
        eval(&format!(
            "{boxed} 1 to: (Boxed of: 3) do: [:i | h update: [:a | a + 1]]. h get value"
        )),
        Value::Int(3)
    );
}
