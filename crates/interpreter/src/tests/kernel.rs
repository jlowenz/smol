use crate::eval::EvalError;
use crate::test_helpers::{eval_diff, eval_err_diff};
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
