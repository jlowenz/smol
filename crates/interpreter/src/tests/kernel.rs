use crate::eval::EvalError;
use crate::test_helpers::{assert_signal_contains, eval, eval_diff, eval_err, eval_err_diff};
use crate::value::Value;
use std::sync::Arc;

#[test]
fn class_print_string() {
    // Tests that sending `asString` to a class returns a String matching the class name.
    // Note: The `println!` in `builtins.rs` for `asString` is intentionally kept
    // for now as per user instruction, even though it's a side-effect.
    assert_eq!(
        eval_diff("Object asString"),
        Value::Str(Arc::from("Object"))
    );
    assert_eq!(eval_diff("Array asString"), Value::Str(Arc::from("Array")));
    assert_eq!(
        eval_diff("ValueHolder asString"),
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
        eval_diff("(Duration ms: 250) asString"),
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

// ── C34 S2: identity (== / ~~) and Object >> = / ~= ──────────────────────────
//
// `==` is the identity primitive and `=` is defined in terms of it, replacing a
// per-variant Rust `=`/`~=` pair for Nil, Symbol, String and instances.

#[test]
fn identity_is_structural_for_encoded_scalars() {
    // An encoded scalar has no identity apart from its content, so `==` is
    // structural. Pharo answers false for two distinct String objects; that is an
    // artifact of everything being a heap object, not something to copy.
    assert_eq!(eval("'abc' == 'abc'"), Value::Bool(true));
    assert_eq!(eval("3 == 3"), Value::Bool(true));
    assert_eq!(eval("#foo == #foo"), Value::Bool(true));
    assert_eq!(eval("nil == nil"), Value::Bool(true));
    assert_eq!(eval("true == true"), Value::Bool(true));
    // The two integer encodings are one value.
    assert_eq!(
        eval("(100 factorial) == (100 factorial)"),
        Value::Bool(true)
    );
}

#[test]
fn identity_and_equality_part_company_on_float() {
    // The reason both selectors exist: `==` asks whether these are the same value,
    // `Number >> =` promotes across encodings. 2 and 2.0 are different values.
    assert_eq!(eval("2 == 2.0"), Value::Bool(false));
    assert_eq!(eval("2 = 2.0"), Value::Bool(true));
}

#[test]
fn identity_is_by_allocation_for_value_objects() {
    // A ValueObject is semantically a Value but HAS a distinct allocation, so `==`
    // stays O(1) and does not recurse. Content equality is `=`'s job, which is
    // exactly why Association overrides it.
    assert_eq!(
        eval("(Association key: 1 value: 2) == (Association key: 1 value: 2)"),
        Value::Bool(false)
    );
    assert_eq!(
        eval("(Association key: 1 value: 2) = (Association key: 1 value: 2)"),
        Value::Bool(true)
    );
    assert_eq!(
        eval("| a | a := Association key: 1 value: 2. a == a"),
        Value::Bool(true)
    );
}

#[test]
fn tilde_tilde_is_the_negation_of_identity() {
    // `~~` lexed before S2 but nothing implemented it, so it could only ever
    // answer doesNotUnderstand.
    assert_eq!(eval("3 ~~ 4"), Value::Bool(true));
    assert_eq!(eval("3 ~~ 3"), Value::Bool(false));
    assert_eq!(eval("'a' ~~ 'a'"), Value::Bool(false));
}

#[test]
fn default_equality_reaches_object_for_every_receiver_kind() {
    // The point of the migration: one implementation instead of one Rust arm per
    // Value variant. Each of these used to be its own arm.
    assert_eq!(eval("nil = nil"), Value::Bool(true));
    assert_eq!(eval("nil = 3"), Value::Bool(false));
    assert_eq!(eval("#a = #a"), Value::Bool(true));
    assert_eq!(eval("#a = 'a'"), Value::Bool(false));
    assert_eq!(eval("'a' = 'a'"), Value::Bool(true));
    assert_eq!(eval("nil ~= 3"), Value::Bool(true));
    assert_eq!(eval("'a' ~= 'b'"), Value::Bool(true));
}

#[test]
fn class_identity_is_synchronous_not_a_future() {
    // A class object is an entity, so a behavioural send to it answers a Future.
    // `=`/`~=` were already special-cased as synchronous; S2 added `==`/`~~`
    // alongside them, since a Bool is expected where identity is asked.
    assert_eq!(eval("Array == Array"), Value::Bool(true));
    assert_eq!(eval("Array ~~ OrderedCollection"), Value::Bool(true));
    assert_eq!(eval("Array = Array"), Value::Bool(true));
    assert_eq!(eval("Array = OrderedCollection"), Value::Bool(false));
}

#[test]
fn symbol_conversions_are_smol() {
    assert_eq!(eval("#foo asString"), Value::Str(Arc::from("foo")));
    assert_eq!(eval("#foo asSymbol"), Value::Symbol(Arc::from("foo")));
    // asSymbol on a Symbol is the identity, so it needs no primitive at all.
    assert_eq!(eval("#foo asString asSymbol == #foo"), Value::Bool(true));
    assert_eq!(eval("#foo respondsTo: #asString"), Value::Bool(true));
}

#[test]
fn user_class_can_override_equality_without_touching_identity() {
    let src = "
        Value subclass: #Pt instanceVariableNames: 'x'
            classVariableNames: '' indexedSlots: #none.
        Pt class >> of: v [ ^self _primNewValue: [:p | p setX: v] ]
        Pt >> setX: v [ x := v ]
        Pt >> x [ ^x ]
        Pt >> = other [ ^(other isKindOf: Pt) and: [ other x = x ] ]
        | a b | a := Pt of: 1. b := Pt of: 1.
        Array new: 2
    ";
    // `=` is overridden to content equality; `==` still answers allocation identity.
    assert_eq!(
        eval(&src.replace("Array new: 2", "(a = b) and: [ (a == b) not ]")),
        Value::Bool(true)
    );
}

// ── C34 S3: numeric conversions ──────────────────────────────────────────────

#[test]
fn integer_conversions_are_smol() {
    assert_eq!(eval("3 asFloat"), Value::Float(3.0));
    assert_eq!(eval("3 asString"), Value::Str(Arc::from("3")));
    assert_eq!(eval("-7 asString"), Value::Str(Arc::from("-7")));
    // ArbitraryInteger shares the same two methods, grounded on the same prims.
    // 21! is the first factorial past i64, so this is genuinely the LargeInt path.
    assert_eq!(
        eval("(21 factorial) class asString"),
        Value::Str(Arc::from("ArbitraryInteger"))
    );
    assert_eq!(
        eval("(21 factorial) asString"),
        Value::Str(Arc::from("51090942171709440000"))
    );
    assert_eq!(eval("(100 factorial) asString size"), Value::Int(158));
}

#[test]
fn large_integer_as_float_saturates_rather_than_erroring() {
    // Behaviour preserved from the arm this replaced.
    assert_eq!(eval("(100 factorial) asFloat > 1.0e150"), Value::Bool(true));
    assert_eq!(
        eval("(1000 factorial) asFloat > 1.0e300"),
        Value::Bool(true)
    );
}

#[test]
fn float_rounding_modes_differ_on_negatives() {
    // Positive values agree often enough to hide a mistake; negatives separate
    // truncation (toward zero) from floor (toward -inf) and ceiling.
    assert_eq!(eval("2.7 truncated"), Value::Int(2));
    assert_eq!(eval("2.7 rounded"), Value::Int(3));
    assert_eq!(eval("2.2 ceiling"), Value::Int(3));
    assert_eq!(eval("2.7 floor"), Value::Int(2));

    assert_eq!(eval("-2.7 truncated"), Value::Int(-2));
    assert_eq!(eval("-2.7 rounded"), Value::Int(-3));
    assert_eq!(eval("-2.2 ceiling"), Value::Int(-2));
    assert_eq!(eval("-2.7 floor"), Value::Int(-3));
}

#[test]
fn as_int_is_truncation_and_needs_no_primitive() {
    // `asInt` was a fifth Rust arm doing the same `as i64` cast as `truncated`;
    // it is now one line of Smol saying so.
    assert_eq!(eval("2.7 asInt"), Value::Int(2));
    assert_eq!(eval("-2.7 asInt"), Value::Int(-2));
    assert_eq!(eval("2.7 asInt = 2.7 truncated"), Value::Bool(true));
    assert_eq!(eval("-2.7 asInt = -2.7 truncated"), Value::Bool(true));
}

#[test]
fn numeric_conversions_are_visible_to_reflection() {
    assert_eq!(eval("3 respondsTo: #asFloat"), Value::Bool(true));
    assert_eq!(eval("3 respondsTo: #asString"), Value::Bool(true));
    assert_eq!(eval("2.5 respondsTo: #rounded"), Value::Bool(true));
    assert_eq!(eval("2.5 respondsTo: #asInt"), Value::Bool(true));
    assert_eq!(
        eval("(100 factorial) respondsTo: #asFloat"),
        Value::Bool(true)
    );
}

// ── C34 S3a: to: / until: — the inclusive/exclusive range duality ────────────
//
// `to:` includes its bound, `until:` stops before it. Both readings are named
// rather than one being a convention you must remember — the same resolution
// Ruby (`a..b` / `a...b`) and Swift (`a...b` / `a..<b`) reached with syntax.

#[test]
fn until_do_excludes_its_bound() {
    let count = |src: &str| {
        eval(&format!(
            "| h | h := ValueHolder initialValue: 0. {src} h get value"
        ))
    };
    assert_eq!(
        count("0 until: 10 do: [:i | h update: [:a | a + 1]]."),
        Value::Int(10)
    );
    assert_eq!(
        count("0 to: 10 do: [:i | h update: [:a | a + 1]]."),
        Value::Int(11)
    );
    // The payoff (EWD831): an empty range is start = stop, needing no stop < start.
    assert_eq!(
        count("5 until: 5 do: [:i | h update: [:a | a + 1]]."),
        Value::Int(0)
    );
    // Adjacent ranges share a bound and cover exactly once.
    assert_eq!(
        count(
            "0 until: 5 do: [:i | h update: [:a | a + 1]]. 5 until: 10 do: [:i | h update: [:a | a + 1]]."
        ),
        Value::Int(10)
    );
}

#[test]
fn until_by_do_handles_both_directions() {
    let sum = |src: &str| {
        eval(&format!(
            "| h | h := ValueHolder initialValue: 0. {src} h get value"
        ))
    };
    // 0, 3, 6, 9 — stops before 10.
    assert_eq!(
        sum("0 until: 10 by: 3 do: [:i | h update: [:a | a + i]]."),
        Value::Int(18)
    );
    // 10, 7, 4, 1 — stops before 0.
    assert_eq!(
        sum("10 until: 0 by: -3 do: [:i | h update: [:a | a + i]]."),
        Value::Int(22)
    );
    // Contrast the inclusive form, which reaches its bound.
    assert_eq!(
        sum("0 until: 9 by: 3 do: [:i | h update: [:a | a + i]]."),
        Value::Int(9)
    );
    assert_eq!(
        sum("0 to: 9 by: 3 do: [:i | h update: [:a | a + i]]."),
        Value::Int(18)
    );
}

#[test]
fn until_by_do_rejects_a_zero_step() {
    assert_signal_contains(
        &eval_err("0 until: 5 by: 0 do: [:i | i]"),
        "step must be non-zero",
    );
}

#[test]
fn until_do_works_for_every_numeric_leaf_and_reflection() {
    assert_eq!(eval("3 respondsTo: #until:do:"), Value::Bool(true));
    assert_eq!(eval("3 respondsTo: #until:by:do:"), Value::Bool(true));
    assert_eq!(
        eval(
            "| h | h := ValueHolder initialValue: 0.
            1.5 until: 3 do: [:i | h update: [:a | a + 1]]. h get value"
        ),
        Value::Int(2)
    );
}

#[test]
fn deep_until_do_is_o1_stack_and_linear() {
    // Same acceptance criterion as to:do: — it is the same shape with `<` for `<=`.
    assert_eq!(eval("1 until: 100000 do: [:i | i]"), Value::Nil);
}

// ── C34 S4: construction of the encoded leaves ───────────────────────────────
//
// Absorbed from the retired string plan §4. Before this, every encoded leaf
// reached `Value class >> new` and answered a frozen ValueObject that CLAIMED to
// be a String/Integer/Float but was not the encoding.

#[test]
fn encoded_leaf_new_answers_the_encoding_not_a_value_object() {
    // The defect: `String new` answered `a String` whose `size` failed and for
    // which `String new = ''` was false.
    assert_eq!(eval("String new"), Value::Str(Arc::from("")));
    assert_eq!(eval("String new = ''"), Value::Bool(true));
    assert_eq!(eval("String new isEmpty"), Value::Bool(true));
    assert_eq!(eval("String new size"), Value::Int(0));
    assert_eq!(eval("MachineInteger new"), Value::Int(0));
    assert_eq!(eval("Float new"), Value::Float(0.0));
    assert_eq!(eval("True new"), Value::Bool(true));
    assert_eq!(eval("False new"), Value::Bool(false));
}

#[test]
fn encoded_leaf_new_satisfies_class_identity() {
    // The rule that decides which leaves may answer a value at all.
    for cls in ["String", "MachineInteger", "Float", "True", "False"] {
        assert_eq!(
            eval(&format!("{cls} new class == {cls}")),
            Value::Bool(true),
            "({cls} new) class must be {cls}"
        );
    }
}

#[test]
fn leaves_without_an_honest_identity_value_refuse() {
    // Refusals because the `(X new) class = X` constraint FAILS, not for lack of
    // imagination: 0 is a MachineInteger, so there is no ArbitraryInteger zero;
    // and a symbol is an interned name, so an "empty symbol" names nothing.
    assert_signal_contains(&eval_err("Symbol new"), "symbols are interned names");
    assert_signal_contains(&eval_err("ArbitraryInteger new"), "no zero of this class");
}

#[test]
fn encoded_leaves_refuse_indexed_construction() {
    // `new: n` presumes n mutable slots; the retired plan §5 rejects mutable
    // strings outright. The old error came from deep in the generic path
    // ("requires class to declare indexedSlots:") and explained none of this.
    for cls in [
        "String",
        "Symbol",
        "MachineInteger",
        "ArbitraryInteger",
        "Float",
        "True",
        "False",
    ] {
        assert_signal_contains(
            &eval_err(&format!("{cls} new: 3")),
            "an encoded scalar has no indexed slots",
        );
    }
}

#[test]
fn leaf_construction_refusals_are_catchable() {
    assert_eq!(
        eval("[Symbol new] on: Error do: [:e | #caught]"),
        Value::Symbol(Arc::from("caught"))
    );
}

#[test]
fn string_repeated_constructs_without_mutability() {
    // Immutability forbids mutating a value after it exists, not constructing one:
    // the repetition happens in a host buffer and the String is born complete.
    assert_eq!(eval("'ab' repeated: 3"), Value::Str(Arc::from("ababab")));
    assert_eq!(eval("'x' repeated: 0"), Value::Str(Arc::from("")));
    assert_eq!(eval("'ab' repeated: 1"), Value::Str(Arc::from("ab")));
    assert_eq!(
        eval("('ab' repeated: 3) class == String"),
        Value::Bool(true)
    );
}

#[test]
fn string_new_with_all_is_smol_over_repeated() {
    // Class-side cannot ground a _prim (self is the class, not the fill), so the
    // factory delegates to the instance-side primitive.
    assert_eq!(
        eval("String new: 20 withAll: 'X'"),
        Value::Str(Arc::from("XXXXXXXXXXXXXXXXXXXX"))
    );
    assert_eq!(eval("(String new: 5 withAll: '-') size"), Value::Int(5));
    assert_eq!(
        eval("String new: 0 withAll: 'z'"),
        Value::Str(Arc::from(""))
    );
}

#[test]
fn filled_construction_is_allowed_where_empty_slots_are_refused() {
    // The immutability principle stated precisely: n empty slots to fill in later
    // is refused; n already-filled ones is not.
    assert_signal_contains(
        &eval_err("String new: 20"),
        "an encoded scalar has no indexed slots",
    );
    assert_eq!(eval("(String new: 20 withAll: 'X') size"), Value::Int(20));
}

#[test]
fn string_repeated_rejects_a_negative_count() {
    assert!(matches!(
        eval_err("'ab' repeated: -1"),
        EvalError::TypeError(msg) if msg.contains("must not be negative")
    ));
}

// ── C34 S5: rendering (asString / asDebugString) and console output ──────────
//
// `printString` is gone: it was misnamed, and its Rust arms carried a `println!`
// that made every rendering send write to the console as a side effect.

#[test]
fn as_string_is_bare_and_as_debug_string_disambiguates() {
    assert_eq!(eval("'hi' asString"), Value::Str(Arc::from("hi")));
    assert_eq!(eval("'hi' asDebugString"), Value::Str(Arc::from("'hi'")));
    assert_eq!(eval("#s asString"), Value::Str(Arc::from("s")));
    assert_eq!(eval("#s asDebugString"), Value::Str(Arc::from("#s")));
    // Everything unambiguous renders the same either way, inheriting
    // asDebugString from Object.
    for (expr, want) in [("3", "3"), ("2.5", "2.5"), ("nil", "nil"), ("true", "true")] {
        assert_eq!(
            eval(&format!("{expr} asString")),
            Value::Str(Arc::from(want))
        );
        assert_eq!(
            eval(&format!("{expr} asDebugString")),
            Value::Str(Arc::from(want))
        );
    }
}

#[test]
fn rendering_no_longer_leaks_output_when_composed() {
    // The defect that decided OQ-4. `Duration >> asString` is `^ms asString , 'ms'`,
    // and the nested send used to emit a stray line for the milliseconds; an
    // Association emitted two. Rendering is pure now, so composing is silent.
    assert_eq!(
        eval("(Duration ms: 5) asString"),
        Value::Str(Arc::from("5ms"))
    );
    assert_eq!(
        eval("(Association key: 1 value: 2) asString"),
        Value::Str(Arc::from("1 -> 2"))
    );
}

#[test]
fn collections_render_elements_with_as_debug_string() {
    // Which is the whole reason two renderings exist: inside a collection you
    // must be able to tell a String from a Symbol from a number.
    let src = "| a | a := Array new: 3.
        a at: 0 put: 'hi'. a at: 1 put: #hi. a at: 2 put: 3. a asString";
    assert_eq!(eval(src), Value::Str(Arc::from("('hi' #hi 3 )")));
}

#[test]
fn print_answers_self_so_it_chains() {
    // printNl returns the receiver, not the rendered string.
    assert_eq!(eval("(3 printNl) + 1"), Value::Int(4));
    assert_eq!(eval("'hi' printNl"), Value::Str(Arc::from("hi")));
}

#[test]
fn decorated_output_is_conversion_first() {
    // print emits the BARE form, which is the only non-circular choice: if it
    // decorated by default there would be no way to print a plain string, since
    // converting first and printing would decorate the result again. So
    // `x asDebugString printNl` is how you get decorated output, and it composes.
    assert_eq!(
        eval("'hi' asDebugString"),
        Value::Str(Arc::from("'hi'")),
        "the debug text is itself an ordinary String, printed bare"
    );
    assert_eq!(
        eval("'hi' asDebugString printNl"),
        Value::Str(Arc::from("'hi'"))
    );
}

#[test]
fn rendering_is_visible_to_reflection() {
    for sel in ["#asString", "#asDebugString", "#print", "#printNl"] {
        assert_eq!(
            eval(&format!("3 respondsTo: {sel}")),
            Value::Bool(true),
            "should respondTo: {sel}"
        );
    }
    // printString is gone from the language entirely.
    assert_eq!(eval("3 respondsTo: #printString"), Value::Bool(false));
}

#[test]
fn class_rendering_is_synchronous_not_a_future() {
    // Same trap S2 hit with `==`: a class is an entity, so a behavioural send
    // answers a Future. asString and name are pure functions of the class, so
    // they joined dispatch_class's synchronous list.
    assert_eq!(eval("Array asString"), Value::Str(Arc::from("Array")));
    assert_eq!(eval("Array name"), Value::Str(Arc::from("Array")));
    assert_eq!(
        eval("'the ' , Array asString"),
        Value::Str(Arc::from("the Array"))
    );
}

// ── C34 S6: prune + audit ────────────────────────────────────────────────────

#[test]
fn error_is_catchable_on_every_receiver_kind() {
    // Before S6 `error:` had THREE Rust implementations and the primitive-receiver
    // one returned a bare EvalError::Signal that bypassed the handler chain, so
    // `[3 error: 'x'] on: Error do: [...]` did not catch. One Smol method replaces
    // all three.
    for recv in [
        "3",
        "2.5",
        "'s'",
        "#s",
        "nil",
        "true",
        "Object new",
        "(100 factorial)",
    ] {
        assert_eq!(
            eval(&format!(
                "[{recv} error: 'boom'] on: Error do: [:e | e messageText]"
            )),
            Value::Str(Arc::from("boom")),
            "error: on {recv} must be catchable"
        );
    }
}

#[test]
fn kernel_domain_errors_are_catchable() {
    // The concrete casualty of the old asymmetry: factorial's domain error was
    // raised on an encoded leaf, so it could not be caught.
    assert_eq!(
        eval("[-1 factorial] on: Error do: [:e | e messageText]"),
        Value::Str(Arc::from("factorial of a negative integer"))
    );
    assert_eq!(
        eval("[1 to: 5 by: 0 do: [:i | i]] on: Error do: [:e | e messageText]"),
        Value::Str(Arc::from("to:by:do: step must be non-zero"))
    );
}

#[test]
fn large_integer_as_float_saturates_with_the_right_sign() {
    assert_eq!(
        eval("(1000 factorial) asFloat"),
        Value::Float(f64::INFINITY)
    );
    assert_eq!(
        eval("(1000 factorial) negated asFloat"),
        Value::Float(f64::NEG_INFINITY)
    );
    assert_eq!(eval("(100 factorial) asFloat > 0.0"), Value::Bool(true));
    assert_eq!(
        eval("(100 factorial) negated asFloat < 0.0"),
        Value::Bool(true)
    );
}
