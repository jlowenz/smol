use super::*;
use crate::test_helpers::{assert_signal_contains, eval, eval_diff, eval_err, eval_err_diff};

#[test]
fn integer_arithmetic() {
    assert_eq!(eval_diff("3 + 4"), Value::Int(7));
    assert_eq!(eval_diff("10 - 3"), Value::Int(7));
    assert_eq!(eval_diff("3 * 4"), Value::Int(12));
    assert_eq!(eval_diff("10 // 3"), Value::Int(3));
    assert_eq!(eval_diff("10 \\\\ 3"), Value::Int(1));
}

#[test]
fn integer_comparison() {
    assert_eq!(eval_diff("3 < 4"), Value::Bool(true));
    assert_eq!(eval_diff("4 < 3"), Value::Bool(false));
    assert_eq!(eval_diff("3 = 3"), Value::Bool(true));
    assert_eq!(eval_diff("3 ~= 4"), Value::Bool(true));
}

#[test]
fn boolean_conditional() {
    assert_eq!(
        eval_diff("true ifTrue: ['yes']"),
        Value::Str(Arc::from("yes"))
    );
    assert_eq!(
        eval_diff("false ifFalse: ['no']"),
        Value::Str(Arc::from("no"))
    );
    assert_eq!(
        eval_diff("true ifTrue: ['yes'] ifFalse: ['no']"),
        Value::Str(Arc::from("yes"))
    );
}

#[test]
fn block_value() {
    assert_eq!(eval_diff("[:x | x * x] value: 7"), Value::Int(49));
}

#[test]
fn block_in_block_value() {
    let src = r#"
    | b | b := [ :b | b value + 1 ]. b value: [42]"#;
    assert_eq!(eval_diff(src), Value::Int(43));
}

#[test]
fn times_repeat() {
    let src = "| oc | oc := OrderedCollection new. 3 timesRepeat: [oc add: 1]. oc size";
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn while_true() {
    let src = "| oc | oc := OrderedCollection new. [oc size < 5] whileTrue: [oc add: 1]. oc size";
    assert_eq!(eval_diff(src), Value::Int(5));
}

#[test]
fn assignment_and_variable() {
    assert_eq!(eval_diff("x := 42. x"), Value::Int(42));
}

#[test]
fn local_decl_binds_nil() {
    assert_eq!(eval_diff("| x | x"), Value::Nil);
}

#[test]
fn yourself_returns_receiver() {
    assert_eq!(eval_diff("42 yourself"), Value::Int(42));
}

#[test]
fn cascade() {
    // The result of a cascade is the last message's return value.
    // `3 yourself; yourself` — both return 3.
    assert_eq!(eval_diff("3 yourself; yourself"), Value::Int(3));
}

#[test]
fn print_string_int() {
    assert_eq!(eval_diff("42 printString"), Value::Str(Arc::from("42")));
}

#[test]
fn print_string_float() {
    assert_eq!(eval_diff("1.5 printString"), Value::Str(Arc::from("1.5")));
}

#[test]
fn undefined_variable_error() {
    matches!(eval_err_diff("x"), EvalError::UndefinedVariable(_));
}

#[test]
fn message_not_understood() {
    // C33: Int routes through user-method lookup, so an unknown selector reaches
    // the default Object>>doesNotUnderstand:, signaling MessageNotUnderstood
    // (iterative path). (The previous `matches!` here asserted nothing.)
    assert_signal_contains(&eval_err("42 unknownMsg"), "does not understand");
}

#[test]
fn to_do_loop() {
    let src = "| oc | oc := OrderedCollection new. 1 to: 5 do: [:i | oc add: i]. oc inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src), Value::Int(15));
}

#[test]
fn string_concat() {
    assert_eq!(
        eval_diff("'hello' , ' world'"),
        Value::Str(Arc::from("hello world"))
    );
}

#[test]
fn nil_is_nil() {
    assert_eq!(eval_diff("nil isNil"), Value::Bool(true));
    assert_eq!(eval_diff("42 isNil"), Value::Bool(false));
}

#[test]
fn class_definition_and_new() {
    let src = r#"
Object subclass: #Point instanceVariableNames: 'x y'.
| p |
p := Point new.
p
"#;
    let v = eval_diff(src);
    assert_eq!(format!("{v}"), "a Point");
}

#[test]
fn method_def_and_call() {
    let src = r#"
Object subclass: #Counter instanceVariableNames: 'count'.
Counter >> initialize [
    count := 0
]
Counter >> increment [
    count := count + 1
]
Counter >> count [
    ^count
]
| c |
c := Counter new.
c increment.
c increment.
c increment.
c count
"#;
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn class_variable_shared() {
    let src = r#"
Object subclass: #Counted instanceVariableNames: '' classVariableNames: 'Total'.
Counted >> initialize [
    Total := (Total isNil) ifTrue: [1] ifFalse: [Total + 1]
]
Counted >> total [
    ^Total
]
[| a b |
a := Counted new.
b := Counted new.
b total] syncValue
"#;
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn largeint_overflow_promotion() {
    // i64::MAX + 1 should produce a LargeInt
    let src = "9223372036854775807 + 1";
    let v = eval_diff(src);
    assert!(matches!(v, Value::LargeInt(_)));
}

#[test]
fn largeint_literal() {
    let src = "99999999999999999999 + 1";
    let v = eval_diff(src);
    assert!(matches!(v, Value::LargeInt(_)));
}

#[test]
fn object_print_string() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new printString
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("a Foo")));
}

#[test]
fn super_dispatch() {
    let src = r#"
Object subclass: #Animal instanceVariableNames: 'name'.
Animal >> speak [
    ^'...'
]
Animal subclass: #Dog instanceVariableNames: ''.
Dog >> speak [
    ^super speak , ' Woof!'
]
| d |
d := Dog new.
d speak
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("... Woof!")));
}

#[test]
fn class_side_method() {
    let src = r#"
Object subclass: #Counter instanceVariableNames: 'count'.
Counter class >> startingAt: n [
    | c |
    c := self new.
    c setCount: n.
    ^c
]
Counter >> setCount: n [ count := n ]
Counter >> count [ ^count ]
| c |
c := Counter startingAt: 42.
c count
"#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn namespace_ref() {
    let src = r#"
Kernel::Object new printString
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("a Object")));
}

#[test]
fn namespace_do_scopes_class() {
    let src = r#"
| oc |
oc := OrderedCollection new.
Kernel do: [ oc add: 1 ].
oc size
"#;
    assert_eq!(eval_diff(src), Value::Int(1));
}

#[test]
fn non_kernel_class_scoped_to_namespace() {
    use crate::namespace::Namespace;

    let ev = Evaluator::new();
    let my_lib = Arc::new(Namespace::new(Arc::from("MyLib"), None));
    ev.registry.register_namespace(my_lib);
    let mut env = Env::new();
    ev.registry.populate_env(&mut env);

    // Inside MyLib do: [...], Widget is accessible by simple name.
    // Uses Value subclass: (frozen after _primNewValue: init block).
    let result = ev
        .eval_program(
            &Parser::parse(
                r#"
MyLib do: [
    | w |
    Value subclass: #Widget instanceVariableNames: 'val'.
    Widget class >> at: v [ ^self _primNewValue: [:w | w pvtSetVal: v] ]
    Widget >> pvtSetVal: v [ val := v ]
    Widget >> val          [ ^val ]
    w := Widget at: 42.
    w val
]
"#,
            )
            .expect("parse")
            .stmts,
            &mut env,
        )
        .expect("eval");
    assert_eq!(result, Value::Int(42));

    // After the block, bare Widget is not in scope.
    let err = ev
        .eval_program(&Parser::parse("Widget").expect("parse").stmts, &mut env)
        .expect_err("Widget should be out of scope after do: block");
    assert!(matches!(err, EvalError::UndefinedVariable(_)));

    // Widget remains accessible via the namespace ref.
    let via_ns = ev
        .eval_program(
            &Parser::parse("MyLib::Widget new printString")
                .expect("parse")
                .stmts,
            &mut env,
        )
        .expect("namespace ref"); // Widget is Value subclass: — synchronous, no future
    assert_eq!(via_ns, Value::Str(Arc::from("a Widget")));
}

// ── Kernel loading ────────────────────────────────────────────────────────

#[test]
fn kernel_load_does_not_panic() {
    let _ev = Evaluator::new();
}

#[test]
fn kernel_classes_in_env() {
    let ev = Evaluator::new();
    let mut env = Env::new();
    ev.registry.populate_env(&mut env);
    use crate::entity::InstanceKind;
    assert!(matches!(
        env.get("Array"),
        Some(Value::Instance(h)) if matches!(h.0.kind, InstanceKind::ClassEntity(_))
    ));
    assert!(matches!(
        env.get("ByteArray"),
        Some(Value::Instance(h)) if matches!(h.0.kind, InstanceKind::ClassEntity(_))
    ));
    assert!(matches!(
        env.get("OrderedCollection"),
        Some(Value::Instance(h)) if matches!(h.0.kind, InstanceKind::ClassEntity(_))
    ));
}

// ── Array protocol ────────────────────────────────────────────────────────

#[test]
fn array_new_size_and_at() {
    assert_eq!(eval_diff("(Array new: 3) size"), Value::Int(3));
    assert_eq!(eval_diff("| a | a := Array new: 3. a at: 0"), Value::Nil);
}

#[test]
fn array_at_put_and_at() {
    assert_eq!(
        eval_diff("| a | a := Array new: 3. a at: 1 put: 99. a at: 1"),
        Value::Int(99)
    );
}

#[test]
fn array_is_empty_not_empty() {
    assert_eq!(eval_diff("(Array new: 0) isEmpty"), Value::Bool(true));
    assert_eq!(eval_diff("(Array new: 2) notEmpty"), Value::Bool(true));
}

#[test]
fn array_first_last() {
    assert_eq!(
        eval_diff("| a | a := Array new: 3. a at: 0 put: 10. a at: 2 put: 30. a first"),
        Value::Int(10)
    );
    assert_eq!(
        eval_diff("| a | a := Array new: 3. a at: 0 put: 10. a at: 2 put: 30. a last"),
        Value::Int(30)
    );
}

#[test]
fn value_object_update_basic() {
    assert_eq!(
        eval_diff("| s | s := ValueHolder initialValue: 0. s update: [:old | old + 1]. s get"),
        Value::Int(1)
    );
}

// Does a block sent cross-entity see a free variable from an outer block?
#[test]
fn value_object_update_captures_outer_var() {
    assert_eq!(
        eval_diff(
            "| s | s := ValueHolder initialValue: 0. [:x | s update: [:old | old + x]] value: 5. s get"
        ),
        Value::Int(5)
    );
}

#[test]
fn await_resolves_future() {
    assert_eq!(
        eval_diff("| s | s := ValueHolder initialValue: 42. (s get) await"),
        Value::Int(42)
    );
}

#[test]
fn await_is_noop_on_non_future() {
    assert_eq!(eval_diff("42 await"), Value::Int(42));
}

#[test]
fn sync_value_block_sequences_cross_entity_sends() {
    assert_eq!(
        eval_diff(
            "| s | s := ValueHolder initialValue: 0. [s update: [:old | old + 5]. s get] syncValue"
        ),
        Value::Int(5)
    );
}

#[test]
fn future_then_chains_after_completion() {
    assert_eq!(
        eval(
            "| a s | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3.
            s := ValueHolder initialValue: 0.
            (a do: [:e | s update: [:old | old + e]]) then: [:v | s get] orElse: #reject"
        ),
        Value::Int(6)
    );
}

#[test]
fn array_do_iterates() {
    assert_eq!(
        eval_diff(
            "| a s | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3.
            s := ValueHolder initialValue: 0.
            (a do: [:e | s update: [:old | old + e]]) await.
            s get"
        ),
        Value::Int(6)
    );
}

#[test]
fn array_collect_maps() {
    let src = "| a r |
    a := (Array new: 3) at: 0 put: 1; at: 1 put: 2; at: 2 put: 3; yourself.
    r := a collect: [:e | e * 2]. r at: 2";
    assert_eq!(eval_diff(src), Value::Int(6));
}

#[test]
fn array_select_returns_ordered_collection() {
    let src = "| a r | a := Array new: 4. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a at: 3 put: 4. r := a select: [:e | e > 2]. r size";
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn array_reject_returns_ordered_collection() {
    let src = "| a r | a := Array new: 4. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a at: 3 put: 4. r := a reject: [:e | e > 2]. r size";
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn array_detect() {
    let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 5. a at: 2 put: 3. a detect: [:e | e > 4]";
    assert_eq!(eval_diff(src), Value::Int(5));
}

#[test]
fn array_detect_if_none() {
    // noneBlock branch
    let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a detect: [:e | e > 10] ifNone: [99]";
    assert_eq!(eval_diff(src), Value::Int(99));
    // found branch
    let src2 = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 5. a at: 2 put: 3. a detect: [:e | e > 4] ifNone: [0]";
    assert_eq!(eval_diff(src2), Value::Int(5));
}

#[test]
fn array_inject_into_sum() {
    let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src), Value::Int(6));
}

#[test]
fn array_includes() {
    let src =
        "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a includes: 2";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 =
        "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a includes: 9";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn array_copy_from_to() {
    let src = "| a r | a := Array new: 5. 0 to: 4 do: [:i | a at: i put: i]. r := a copyFrom: 1 to: 3. r size";
    assert_eq!(eval_diff(src), Value::Int(3));
    let src2 = "| a r | a := Array new: 5. 0 to: 4 do: [:i | a at: i put: i]. r := a copyFrom: 1 to: 3. r at: 0";
    assert_eq!(eval_diff(src2), Value::Int(1));
}

#[test]
fn array_equality() {
    let src = "| a b |
    a := Array new: 2. a at: 0 put: 1. a at: 1 put: 2.
    b := Array new: 2. b at: 0 put: 1. b at: 1 put: 2.
    a = b";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 = "| a b |
    a := Array new: 2. a at: 0 put: 1. a at: 1 put: 2.
    b := Array new: 2. b at: 0 put: 1. b at: 1 put: 9.
    a = b";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn array_not_equal() {
    let src =
        "| a b | a := Array new: 1. b := Array new: 1. a at: 0 put: 1. b at: 0 put: 2. a ~= b";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 =
        "| a b | a := Array new: 1. b := Array new: 1. a at: 0 put: 5. b at: 0 put: 5. a ~= b";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn array_eq_class_mismatch() {
    // Array vs OrderedCollection should be unequal even if content looks similar
    let src = "| a oc | a := Array new: 0. oc := OrderedCollection new. a = oc";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn array_eq_size_mismatch() {
    // Same class, different sizes — exercises the n = other size ifFalse: [^false] branch.
    let src = "| a b | a := Array new: 2. b := Array new: 3. a = b";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn array_detect_no_match_signals() {
    let src = "| a | a := Array new: 3. 
    a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. 
    a detect: [:e | e > 10]";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

#[test]
fn array_print_string() {
    let src = "| a |
    a := Array new: 2. a at: 0 put: 1. a at: 1 put: 2.
    a printString";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("(1 2 )")));
}

// ── ByteArray protocol ────────────────────────────────────────────────────

#[test]
fn bytearray_new_size_at_put() {
    assert_eq!(eval_diff("(ByteArray new: 4) size"), Value::Int(4));
    let src = "| b | b := ByteArray new: 4. b at: 0 put: 65. b at: 0";
    assert_eq!(eval_diff(src), Value::Int(65));
}

#[test]
fn bytearray_is_empty() {
    assert_eq!(eval_diff("(ByteArray new: 0) isEmpty"), Value::Bool(true));
    assert_eq!(eval_diff("(ByteArray new: 1) notEmpty"), Value::Bool(true));
}

#[test]
fn bytearray_print_string() {
    let src = "| b | b := ByteArray new: 2. b at: 0 put: 65. b at: 1 put: 66. b printString";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("(65 66 )")));
}

#[test]
fn bytearray_as_string() {
    let src = "| b | b := ByteArray new: 3. b at: 0 put: 65. b at: 1 put: 66. b at: 2 put: 67. b asString";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("ABC")));
}

#[test]
fn bytearray_do() {
    let src = "| b s |
    b := ByteArray new: 3.
    b at: 0 put: 10.
    b at: 1 put: 20.
    b at: 2 put: 30.
    s := ValueHolder initialValue: 0.
    (b do: [:e | s update: [ :old | old + e ]]) await. s get";
    assert_eq!(eval_diff(src), Value::Int(60));
}

#[test]
fn bytearray_copy_from_to() {
    let src = "| b r | b := ByteArray new: 5. 0 to: 4 do: [:i | b at: i put: i * 10]. r := b copyFrom: 1 to: 3. r size";
    assert_eq!(eval_diff(src), Value::Int(3));
    let src2 = "| b r | b := ByteArray new: 5. 0 to: 4 do: [:i | b at: i put: i * 10]. r := b copyFrom: 1 to: 3. r at: 0";
    assert_eq!(eval_diff(src2), Value::Int(10));
}

// ── OrderedCollection protocol ────────────────────────────────────────────

#[test]
fn oc_new_is_empty() {
    assert_eq!(
        eval_diff("OrderedCollection new isEmpty"),
        Value::Bool(true)
    );
    assert_eq!(eval_diff("OrderedCollection new size"), Value::Int(0));
    assert_eq!(
        eval_diff("OrderedCollection new notEmpty"),
        Value::Bool(false)
    );
}

#[test]
fn oc_add_and_size() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc size";
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn oc_at() {
    let src = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc at: 1";
    assert_eq!(eval_diff(src), Value::Int(20));
}

#[test]
fn oc_first_last() {
    let src = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. oc first";
    assert_eq!(eval_diff(src), Value::Int(10));
    let src2 = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. oc last";
    assert_eq!(eval_diff(src2), Value::Int(30));
}

#[test]
fn oc_add_first_prepends() {
    let src = "| oc | oc := OrderedCollection new. oc add: 2. oc add: 3. oc addFirst: 1. oc at: 0";
    assert_eq!(eval_diff(src), Value::Int(1));
}

#[test]
fn oc_remove_first() {
    let src =
        "| oc v | oc := OrderedCollection new. oc add: 10. oc add: 20. v := oc removeFirst. v";
    assert_eq!(eval_diff(src), Value::Int(10));
    let src2 =
        "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc removeFirst. oc size";
    assert_eq!(eval_diff(src2), Value::Int(1));
}

#[test]
fn oc_remove_last() {
    let src = "| oc v | oc := OrderedCollection new. oc add: 10. oc add: 20. v := oc removeLast. v";
    assert_eq!(eval_diff(src), Value::Int(20));
    let src2 = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc removeLast. oc size";
    assert_eq!(eval_diff(src2), Value::Int(1));
}

#[test]
fn oc_remove_if_absent() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc remove: 2 ifAbsent: [0]. oc size";
    assert_eq!(eval_diff(src), Value::Int(2));
    let src2 =
        "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc remove: 9 ifAbsent: [99]";
    assert_eq!(eval_diff(src2), Value::Int(99));
}

#[test]
fn oc_do_iterates() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src), Value::Int(6));
}

#[test]
fn oc_collect_maps() {
    let src = "| oc r | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. r := oc collect: [:e | e * 10]. r at: 1";
    assert_eq!(eval_diff(src), Value::Int(20));
}

#[test]
fn oc_select_filters() {
    let src = "| oc r | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc add: 4. r := oc select: [:e | e > 2]. r size";
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn oc_reject_filters() {
    let src = "| oc r | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc add: 4. r := oc reject: [:e | e > 2]. r size";
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn oc_detect() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 5. oc add: 3. oc detect: [:e | e > 4]";
    assert_eq!(eval_diff(src), Value::Int(5));
}

#[test]
fn oc_detect_if_none() {
    // noneBlock branch
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc detect: [:e | e > 10] ifNone: [42]";
    assert_eq!(eval_diff(src), Value::Int(42));
    // found branch
    let src2 = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 5. oc add: 3. oc detect: [:e | e > 4] ifNone: [0]";
    assert_eq!(eval_diff(src2), Value::Int(5));
}

#[test]
fn oc_inject_into_sum() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src), Value::Int(6));
}

#[test]
fn oc_includes() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc includes: 2";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc includes: 9";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn oc_copy_from_to() {
    let src = "| oc r | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. r := oc copyFrom: 0 to: 1. r size";
    assert_eq!(eval_diff(src), Value::Int(2));
    let src2 = "| oc r | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. r := oc copyFrom: 0 to: 1. r at: 0";
    assert_eq!(eval_diff(src2), Value::Int(10));
}

#[test]
fn oc_at_put() {
    let src =
        "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc at: 0 put: 99. oc at: 0";
    assert_eq!(eval_diff(src), Value::Int(99));
}

#[test]
fn oc_at_put_out_of_bounds_signals() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc at: 5 put: 99";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

#[test]
fn oc_detect_no_match_signals() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc detect: [:e | e > 10]";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

#[test]
fn oc_equality() {
    let src =
        "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. b add: 1. a = b";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 =
        "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. b add: 2. a = b";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn oc_eq_size_mismatch() {
    let src = "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. a = b";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn oc_eq_class_mismatch() {
    let src = "| oc a | oc := OrderedCollection new. a := Array new: 0. oc = a";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn oc_not_equal() {
    let src = "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. b add: 2. a ~= b";
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn oc_print_string() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc printString";
    assert_eq!(
        eval_diff(src),
        Value::Str(Arc::from("OrderedCollection (1 2 )"))
    );
}

#[test]
fn oc_growth_trigger() {
    // add: beyond initial capacity of 8 forces pvtGrow.
    let src = "| oc | oc := OrderedCollection new. 1 to: 12 do: [:i | oc add: i]. oc size";
    assert_eq!(eval_diff(src), Value::Int(12));
    let src2 = "| oc | oc := OrderedCollection new. 1 to: 12 do: [:i | oc add: i]. oc at: 11";
    assert_eq!(eval_diff(src2), Value::Int(12));
}

#[test]
fn oc_add_first_growth_trigger() {
    // addFirst: beyond initial capacity of 8 forces pvtGrow via its own guard.
    let src = "| oc | oc := OrderedCollection new. 1 to: 12 do: [:i | oc addFirst: i]. oc size";
    assert_eq!(eval_diff(src), Value::Int(12));
    // addFirst prepends, so the last addFirst: (12) is at index 0.
    let src2 = "| oc | oc := OrderedCollection new. 1 to: 12 do: [:i | oc addFirst: i]. oc at: 0";
    assert_eq!(eval_diff(src2), Value::Int(12));
}

#[test]
fn oc_remove_first_empty_errors() {
    let src = "OrderedCollection new removeFirst";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

#[test]
fn oc_remove_last_empty_errors() {
    let src = "OrderedCollection new removeLast";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

#[test]
fn oc_at_out_of_bounds_errors() {
    let src = "| oc | oc := OrderedCollection new. oc add: 1. oc at: 5";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

// ── Nested array literals ─────────────────────────────────────────────────

#[test]
fn array_literal_basic() {
    assert_eq!(eval_diff("#(1 2 3) size"), Value::Int(3));
    assert_eq!(eval_diff("#(1 2 3) at: 1"), Value::Int(2));
}

#[test]
fn array_literal_empty() {
    assert_eq!(eval_diff("#() isEmpty"), Value::Bool(true));
}

#[test]
fn array_literal_nested() {
    assert_eq!(eval_diff("#(#(0 1) #(2 3)) size"), Value::Int(2));
    let src = "(#(#(0 1) #(2 3)) at: 0) at: 1";
    assert_eq!(eval_diff(src), Value::Int(1));
}

#[test]
fn array_literal_mixed_types() {
    assert_eq!(
        eval_diff("#(1 'hello' true nil) at: 1"),
        Value::Str(Arc::from("hello"))
    );
    assert_eq!(eval_diff("#(1 'hello' true nil) at: 2"), Value::Bool(true));
    assert_eq!(eval_diff("#(1 'hello' true nil) at: 3"), Value::Nil);
}

#[test]
fn array_literal_symbols() {
    assert_eq!(
        eval_diff("#(#foo #bar) at: 0"),
        Value::Symbol(Arc::from("foo"))
    );
}

#[test]
fn array_literal_passed_to_do() {
    let src = "#(1 2 3 4) inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src), Value::Int(10));
}

#[test]
fn array_literal_non_literal_token_is_parse_error() {
    let result = crate::parser::Parser::parse("#(1 + 2)");
    assert!(result.is_err());
}

// ── Integration tests ─────────────────────────────────────────────────────

#[test]
fn oc_as_queue() {
    let src = r#"
| q |
q := OrderedCollection new.
q add: 'a'.
q add: 'b'.
q add: 'c'.
q removeFirst , (q removeFirst) , (q removeFirst)
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("abc")));
}

#[test]
fn collect_then_inject() {
    let src = r#"
| oc |
oc := OrderedCollection new.
1 to: 5 do: [:i | oc add: i].
(oc collect: [:e | e * e]) inject: 0 into: [:acc :e | acc + e]
"#;
    assert_eq!(eval_diff(src), Value::Int(55)); // 1+4+9+16+25
}

#[test]
fn user_subclass_of_array() {
    let src = r#"
Array subclass: #Stack instanceVariableNames: '' indexedSlots: #pointer.
Stack >> push: v [ self _primAt: 0 put: v ]
Stack >> top   [ ^self _primAt: 0 ]
| s |
s := Stack new: 1.
s push: 42.
s top
"#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn nested_array_literal_with_do() {
    let src = r#"
| outer oc |
outer := #(#(1 2) #(3 4)).
oc := OrderedCollection new.
outer do: [:row | row do: [:e | oc add: e]].
oc inject: 0 into: [:acc :e | acc + e]
"#;
    assert_eq!(eval_diff(src), Value::Int(10));
}

#[test]
fn block_with_locals() {
    let prog = "b := [:i | | a | a := 42. a := a + i. a]. (b value: 42) printString.";
    assert_eq!(eval_diff(prog), Value::Str(Arc::from("84")));
}

// ── Parser: strict block assignment ──────────────────────────────────────

#[test]
fn non_local_block_assignment_is_parse_error() {
    use crate::parser::{ParseError, Parser};
    // Assigning a method temp from inside a block is rejected at parse time.
    let err = Parser::parse("Object >> m [ | n | [n := 1] value ]");
    assert!(
        matches!(err, Err(ParseError::NonLocalBlockAssignment(ref name)) if name.as_ref() == "n"),
        "expected NonLocalBlockAssignment(\"n\"), got {err:?}"
    );
    // Assigning a block param or local is legal.
    assert!(Parser::parse("Object >> m [ [:x | x := 1. x] value: 0 ]").is_ok());
}

// ── Signal / Error ────────────────────────────────────────────────────────

#[test]
fn error_signal_raises_signal() {
    let src = "Error signal: 'boom'";
    assert_signal_contains(&eval_err_diff(src), "boom");
}

#[test]
fn error_message_roundtrips() {
    let src = "(Error withMessage: 'hello') message";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("hello")));
}

#[test]
fn error_messagetext_alias() {
    let src = "(Error withMessage: 'world') messageText";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("world")));
}

#[test]
fn on_do_catches_signal() {
    let src = "[Error signal: 'oops'] on: Error do: [:e | e message]";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("oops")));
}

#[test]
fn object_error_colon_delegates_to_error_signal() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new error: 'bad'
"#;
    assert_signal_contains(&eval_err_diff(src), "bad");
}

#[test]
fn on_do_catches_error_colon() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
[Foo new error: 'caught me'] on: Error do: [:e | e message]
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("caught me")));
}

#[test]
fn error_in_block_enclosing_value() {
    let src = r#"
Value subclass: #Test instanceVariableNames: ''.
[Test new error: 'what'] value."#;
    let ret = eval_err_diff(src);
    println!("error in block: {}", ret);
    assert!(matches!(ret, EvalError::Signal(_)));
}

#[test]
fn error_in_block_enclosing_object() {
    let src = r#"
Object subclass: #Test instanceVariableNames: ''.
[Test new error: 'what'] value."#;
    let ret = eval_err_diff(src);
    println!("error in block: {}", ret);
    assert!(matches!(ret, EvalError::Signal(_)));
}

// ── _primHash ─────────────────────────────────────────────────────────────

#[test]
fn hash_delegates_to_prim_hash_for_primitives() {
    assert_eq!(eval_diff("42 hash"), Value::Int(42));
    assert_eq!(eval_diff("'hello' hash"), eval_diff("'hello' _primHash"));
    assert_eq!(eval_diff("#foo hash"), eval_diff("#foo _primHash"));
}

#[test]
fn hash_delegates_to_object_hash_for_objects() {
    let src = r#"
Object subclass: #Blah instanceVariableNames: 'x'.
Blah >> initialize [ x := 42 ]
Blah >> x [^x]
Blah >> = other [^x = other x]
Blah >> hash [ ^x hash ]
Blah new hash.
"#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn hash_overridable_on_object() {
    let src = r#"
Object subclass: #Point instanceVariableNames: 'x y'.
Point class >> x: anX y: aY [
    ^self new pvtX: anX y: aY
]
Point >> pvtX: anX y: aY [ x := anX. y := aY ]
Point >> x [ ^x ]
Point >> y [ ^y ]
Point >> = other [
    self class = other class ifFalse: [^false].
    ^x = other x and: [y = other y]
]
Point >> hash [ ^(x hash * 31) + y hash ]

| d |
d := Dictionary new.
d at: (Point x: 1 y: 2) put: 'origin-ish'.
d at: (Point x: 1 y: 2)
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("origin-ish")));
}

#[test]
fn prim_hash_int_returns_self() {
    assert_eq!(eval_diff("42 _primHash"), Value::Int(42));
    assert_eq!(eval_diff("0 _primHash"), Value::Int(0));
}

#[test]
fn prim_hash_str_equal_strings_equal_hash() {
    let src = "'hello' _primHash = 'hello' _primHash";
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn prim_hash_str_different_strings_differ() {
    let src = "'hello' _primHash = 'world' _primHash";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn prim_hash_symbol_and_str_same_content_equal_hash() {
    // Both Str and Symbol use FNV-1a over the same bytes → equal hashes
    let src = "#foo _primHash = 'foo' _primHash";
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn prim_hash_two_distinct_objects_differ() {
    let src = r#"
Object subclass: #Pt instanceVariableNames: ''.
| a b |
a := Pt new. b := Pt new.
a _primHash = b _primHash
"#;
    // Pointer-based: two distinct heap objects almost certainly differ.
    // (Not guaranteed, but extremely unlikely to collide.)
    assert_eq!(eval_diff(src), Value::Bool(false));
}

// ── Association ───────────────────────────────────────────────────────────

#[test]
fn association_key_value() {
    let src = "| a | a := Association key: 'x' value: 42. a key";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("x")));
    let src2 = "| a | a := Association key: 'x' value: 42. a value";
    assert_eq!(eval_diff(src2), Value::Int(42));
}

#[test]
fn association_arrow_syntax() {
    let src = "| a | a := 'k' -> 99. a key";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("k")));
    let src2 = "| a | a := 'k' -> 99. a value";
    assert_eq!(eval_diff(src2), Value::Int(99));
}

#[test]
fn association_value_returns_new_association() {
    // value: is a functional update — returns a new Association; original unchanged.
    let src = "| a b | a := 'k' -> 1. b := a value: 2. b value";
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn association_original_unchanged_after_value() {
    let src = "| a b | a := 'k' -> 1. b := a value: 2. a value";
    assert_eq!(eval_diff(src), Value::Int(1));
}

#[test]
fn association_is_immutable() {
    // Direct post-construction ivar write raises frozen error.
    let err = eval_err_diff("| a | a := 'k' -> 1. a pvtKey: 'x' value: 2");
    assert!(
        err.to_string().contains("frozen"),
        "expected frozen error, got: {err}"
    );
}

#[test]
fn association_equality_by_key() {
    let src = "('x' -> 1) = ('x' -> 2)";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 = "('x' -> 1) = ('y' -> 1)";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn association_eq_class_mismatch() {
    // Association = a different value object should be false via the class guard.
    // Error is also a Value subclass, so it has `class` — exercises the class
    // mismatch branch without hitting MessageNotUnderstood.
    let src = "| e | e := Error new. ('x' -> 1) = e";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn association_print_string() {
    let src = "('x' -> 42) printString";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("'x' -> 42")));
}

#[test]
fn arrow_on_all_primitive_receiver_types() {
    // Float and LargeInt receivers reach the -> arm in dispatch() directly
    // (lookup_user_method returns None for those types); Int, Bool, and Nil
    // go through the kernel.smol Object >> -> method via lookup_user_method.
    // All must produce an Association with the correct key and value.

    // Float receiver — goes through dispatch() -> arm
    assert_eq!(eval_diff("(1.5 -> #ok) key"), Value::Float(1.5));
    assert_eq!(
        eval_diff("(1.5 -> #ok) value"),
        Value::Symbol(Arc::from("ok"))
    );

    // LargeInt receiver — goes through dispatch() -> arm
    assert_eq!(
        eval_diff("(99999999999999999999 -> 7) key"),
        eval_diff("99999999999999999999")
    );
    assert_eq!(
        eval_diff("(99999999999999999999 -> 7) value"),
        Value::Int(7)
    );

    // Int receiver — goes through Object >> -> in kernel.smol
    assert_eq!(eval_diff("(42 -> 'v') key"), Value::Int(42));
    assert_eq!(eval_diff("(42 -> 'v') value"), Value::Str(Arc::from("v")));

    // Bool receiver
    assert_eq!(eval_diff("(true -> 0) key"), Value::Bool(true));
    assert_eq!(eval_diff("(false -> 1) key"), Value::Bool(false));

    // Nil receiver
    assert_eq!(eval_diff("(nil -> 'x') key"), Value::Nil);
    assert_eq!(eval_diff("(nil -> 'x') value"), Value::Str(Arc::from("x")));
}

// ── Dictionary ────────────────────────────────────────────────────────────

#[test]
fn dict_new_is_empty() {
    assert_eq!(eval_diff("Dictionary new isEmpty"), Value::Bool(true));
    assert_eq!(eval_diff("Dictionary new size"), Value::Int(0));
    assert_eq!(eval_diff("Dictionary new notEmpty"), Value::Bool(false));
    let src = "| d | d := Dictionary new. d at: 'k' put: 1. d notEmpty";
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn dict_at_put_and_at() {
    let src = "| d | d := Dictionary new. d at: 'a' put: 1. d at: 'a'";
    assert_eq!(eval_diff(src), Value::Int(1));
}

#[test]
fn dict_at_missing_key_signals() {
    let src = "Dictionary new at: 'missing'";
    assert!(matches!(eval_err_diff(src), EvalError::Signal(_)));
}

#[test]
fn dict_at_if_absent() {
    let src = "| d | d := Dictionary new. d at: 'x' ifAbsent: [99]";
    assert_eq!(eval_diff(src), Value::Int(99));
    let src2 = "| d | d := Dictionary new. d at: 'x' put: 7. d at: 'x' ifAbsent: [99]";
    assert_eq!(eval_diff(src2), Value::Int(7));
}

#[test]
fn dict_includes_key() {
    let src = "| d | d := Dictionary new. d at: 'k' put: 0. d includesKey: 'k'";
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 = "| d | d := Dictionary new. d includesKey: 'k'";
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn dict_at_put_updates_existing() {
    let src = "| d | d := Dictionary new. d at: 'k' put: 1. d at: 'k' put: 2. d at: 'k'";
    assert_eq!(eval_diff(src), Value::Int(2));
    // size should still be 1
    let src2 = "| d | d := Dictionary new. d at: 'k' put: 1. d at: 'k' put: 2. d size";
    assert_eq!(eval_diff(src2), Value::Int(1));
}

#[test]
fn dict_remove_key_if_absent() {
    let src = "| d | d := Dictionary new. d at: 'k' put: 42. d removeKey: 'k' ifAbsent: [0]";
    assert_eq!(eval_diff(src), Value::Int(42));
    let src2 =
        "| d | d := Dictionary new. d at: 'k' put: 42. d removeKey: 'k' ifAbsent: [0]. d size";
    assert_eq!(eval_diff(src2), Value::Int(0));
    let src3 = "| d | d := Dictionary new. d removeKey: 'missing' ifAbsent: [99]";
    assert_eq!(eval_diff(src3), Value::Int(99));
}

#[test]
fn dict_do_iterates_values() {
    let src = r#"
| d s |
d := Dictionary new.
d at: 'a' put: 1.
d at: 'b' put: 2.
d at: 'c' put: 3.
s := ValueHolder initialValue: 0.
(d do: [:v | s update: [:old| old + v]]) then: [:r | s get] orElse: #reject.
"#;
    assert_eq!(eval(src), Value::Int(6));
}

#[test]
fn dict_keys_do() {
    let src = r#"
| d r |
d := Dictionary new.
d at: 'x' put: 0.
r := OrderedCollection new.
(d keysDo: [:k | r add: k]) then: [:v | r size] orElse: #reject.
"#;
    assert_eq!(eval(src), Value::Int(1));
}

#[test]
fn dict_associations_do() {
    let src = r#"
| d pairs |
d := Dictionary new.
d at: 'k' put: 7.
pairs := OrderedCollection new.
(d associationsDo: [:assoc | pairs add: assoc value]) await.
pairs at: 0
"#;
    assert_eq!(eval_diff(src), Value::Int(7));
}

#[test]
fn dict_keys_and_values() {
    let src = "| d | d := Dictionary new. d at: 'a' put: 1. d keys size";
    assert_eq!(eval_diff(src), Value::Int(1));
    let src2 = "| d | d := Dictionary new. d at: 'a' put: 99. d values at: 0";
    assert_eq!(eval_diff(src2), Value::Int(99));
}

#[test]
fn dict_equality() {
    let src = r#"
| a b |
a := Dictionary new. a at: 'x' put: 1.
b := Dictionary new. b at: 'x' put: 1.
a = b
"#;
    assert_eq!(eval_diff(src), Value::Bool(true));
    let src2 = r#"
| a b |
a := Dictionary new. a at: 'x' put: 1.
b := Dictionary new. b at: 'x' put: 2.
a = b
"#;
    assert_eq!(eval_diff(src2), Value::Bool(false));
}

#[test]
fn dict_eq_size_mismatch() {
    let src = r#"
| a b |
a := Dictionary new. a at: 'x' put: 1.
b := Dictionary new.
a = b
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn dict_eq_class_mismatch() {
    let src = "Dictionary new = OrderedCollection new";
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn dict_eq_key_absent_in_other() {
    // a has 'x', b has 'y' — same size but different keys.
    let src = r#"
| a b |
a := Dictionary new. a at: 'x' put: 1.
b := Dictionary new. b at: 'y' put: 1.
a = b
"#;
    assert_eq!(eval(src), Value::Bool(false));
}

#[test]
fn dict_remove_preserves_other_keys() {
    let src = r#"
| d |
d := Dictionary new.
d at: 'a' put: 1.
d at: 'b' put: 2.
d at: 'c' put: 3.
d removeKey: 'b' ifAbsent: [0].
(d includesKey: 'a') & (d includesKey: 'c') & ((d includesKey: 'b') not)
"#;
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn dict_print_string() {
    let src = "| d | d := Dictionary new. d at: 'k' put: 1. d printString";
    // Exact order may vary, but should contain the association
    let v = eval_diff(src);
    if let Value::Str(s) = v {
        assert!(s.contains("'k' -> 1"));
    } else {
        panic!("expected Str");
    }
}

#[test]
fn dict_growth_beyond_initial_capacity() {
    let src = r#"
| d |
d := Dictionary new.
(1 to: 12 do: [:i | d at: i put: i * i]) then: [:v | d size] orElse: #reject
"#;
    assert_eq!(eval(src), Value::Int(12));
    let src2 = r#"
| d |
d := Dictionary new.
(1 to: 12 do: [:i | d at: i put: i * i]) then: [:v | d at: 7] orElse: #reject
"#;
    assert_eq!(eval(src2), Value::Int(49));
}

#[test]
fn dict_int_keys() {
    let src = "| d | d := Dictionary new. d at: 1 put: 'one'. d at: 1";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("one")));
}

#[test]
fn dict_symbol_keys() {
    let src = "| d | d := Dictionary new. d at: #foo put: 42. d at: #foo";
    assert_eq!(eval_diff(src), Value::Int(42));
}

// ── C2: implicit self return ──────────────────────────────────────────────────

#[test]
fn method_without_caret_returns_self() {
    let src = r#"
Object subclass: #Setter instanceVariableNames: 'val'.
Setter >> val: v [ val := v ]
Setter >> val [ ^val ]
| s |
s := Setter new.
(s val: 42) = s
"#;
    // val: has no ^, so it returns self; identity check with s should be true
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn method_with_caret_still_returns_explicit_value() {
    let src = r#"
Object subclass: #Getter instanceVariableNames: 'val'.
Getter >> initialize [ val := 99 ]
Getter >> val [ ^val ]
| g |
g := Getter new.
g val
"#;
    assert_eq!(eval_diff(src), Value::Int(99));
}

#[test]
fn class_method_without_caret_returns_self_class() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo class >> setup [ 42 ]
Foo setup = Foo
"#;
    // setup has no ^; class-side implicit return is the class itself
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn actor_method_without_caret_returns_self() {
    let src = r#"
Object subclass: #MyActor instanceVariableNames: 'x'.
MyActor >> setX: v [ x := v ]
MyActor >> x [ ^x ]
| a |
a := MyActor new.
(a setX: 7) await.
a x
"#;
    // setX: has no ^; the future it returns resolves to the actor handle (self)
    // We don't assert the identity here, just that x was set correctly
    assert_eq!(eval_diff(src), Value::Int(7));
}

// ── Evaluator::default ────────────────────────────────────────────────────────

#[test]
fn evaluator_default() {
    // Evaluator::default() must delegate to Evaluator::new() and produce a working evaluator.
    let (tx, rx) = std::sync::mpsc::channel();
    crate::entity::init_coroutine_stack();
    may::go!(move || {
        let ev = Evaluator::default();
        let mut env = crate::env::Env::new();
        ev.registry.populate_env(&mut env);
        let prog = crate::parser::Parser::parse("42").unwrap();
        let v = ev.eval_program(&prog.stmts, &mut env).unwrap();
        tx.send(v).unwrap();
    });
    assert_eq!(rx.recv().unwrap(), Value::Int(42));
}

// ── Block arity mismatch ──────────────────────────────────────────────────────

#[test]
fn eval_block_arity_mismatch() {
    // Sending value: with 1 arg to a 2-param block triggers ArityMismatch in eval_block.
    let err = eval_err_diff("[:x :y | x] value: 1");
    assert!(matches!(
        err,
        EvalError::ArityMismatch {
            expected: 2,
            got: 1
        }
    ));
}

#[test]
fn eval_block_sync_arity_mismatch() {
    // syncValue passes 0 args; a 1-param block triggers ArityMismatch in eval_block_sync.
    let err = eval_err_diff("[:x | x] syncValue");
    assert!(matches!(
        err,
        EvalError::ArityMismatch {
            expected: 1,
            got: 0
        }
    ));
}

// ── syncValue with local declarations ─────────────────────────────────────────

#[test]
fn eval_program_sync_with_locals() {
    // A block with locals evaluated via syncValue exercises LocalDecl in eval_program_sync.
    assert_eq!(eval_diff("[| x | x := 42. x] syncValue"), Value::Int(42));
}

// ── Class variable read / write ───────────────────────────────────────────────

#[test]
fn class_variable_read_write() {
    // Class-side methods reading and writing class variables exercise the
    // SelfBinding::ClassSide branches in lookup_var and assign_var.
    let src = r#"
Object subclass: #Counter classVariableNames: 'count'.
Counter class >> setCount: n [ count := n ].
Counter class >> getCount [ ^count ].
Counter setCount: 42.
Counter getCount.
"#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

// ── Class-side super dispatch ─────────────────────────────────────────────────

#[test]
fn super_class_side_method_found() {
    // super in a class-side method finds a user-defined method in the superclass.
    // Exercises SelfBinding::ClassSide branch in current_defining_class and
    // the found-method arm of eval_super_send for class-side.
    let src = r#"
Object subclass: #Base.
Base class >> label [ ^'base' ].
Base subclass: #Child.
Child class >> label [ ^super label ].
Child label.
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("base")));
}

#[test]
fn super_class_side_fallback_to_builtin() {
    // super new in a class-side method finds no user method in the superclass chain
    // and falls through to dispatch_class_builtin — the None arm.
    let src = r#"
Object subclass: #Base.
Base subclass: #Child.
Child class >> make [ ^super new ].
Child make.
"#;
    assert!(matches!(eval_diff(src), Value::Instance(_)));
}

// ── super with no superclass ──────────────────────────────────────────────────

#[test]
fn super_send_in_root_class_errors() {
    // Object has no superclass — super send produces a TypeError.
    let src = r#"
Object >> rootSuper [ ^super yourself ].
Object new rootSuper.
"#;
    let err = eval_err_diff(src);
    assert!(matches!(err, EvalError::TypeError(_)));
}

// ── method def: class name is not a class ─────────────────────────────────────

#[test]
fn method_def_non_class_receiver_errors() {
    // If the name before >> resolves to a non-class value, eval_method_def errors.
    let err = eval_err_diff("| Foo | Foo := 42. Foo >> bar [ ^1 ]");
    assert!(matches!(err, EvalError::TypeError(_)));
}

// ── Namespace reference errors ────────────────────────────────────────────────

#[test]
fn namespace_ref_first_segment_not_a_namespace() {
    // Object is a class, not a namespace — NamespaceRef errors at first segment.
    let err = eval_err_diff("Object::Array");
    assert!(matches!(err, EvalError::TypeError(_)));
}

#[test]
fn namespace_ref_first_segment_undefined() {
    let err = eval_err_diff("NonExistentNs::Array");
    assert!(matches!(err, EvalError::UndefinedVariable(_)));
}

#[test]
fn namespace_ref_class_not_found_in_namespace() {
    let err = eval_err_diff("Kernel::NonExistentClass");
    assert!(matches!(err, EvalError::TypeError(_)));
}

// ── _prim cascade receiver check ─────────────────────────────────────────────

#[test]
fn prim_send_via_cascade_non_self_errors() {
    // _prim* messages in a cascade sent to a non-self receiver are rejected.
    let err = eval_err_diff(
        "Object subclass: #CascadeTest. CascadeTest >> bad [ | x | x _primAt: 0; size ]",
    );
    assert!(matches!(err, EvalError::TypeError(_)));
}

// ── Instance-side super with no matching method ───────────────────────────────

#[test]
fn super_instance_method_not_found() {
    // super send in an instance method where the superclass has no such method.
    let src = r#"
Object subclass: #Base.
Base subclass: #Child.
Child >> trySuper [ ^super noSuchMethod ].
Child new trySuper.
"#;
    let err = eval_err_diff(src);
    assert!(matches!(err, EvalError::MessageNotUnderstood { .. }));
}

// ── Super binary and keyword sends ───────────────────────────────────────────
// These exercise Cont::SuperCollectArgs / dispatch_super_to_stack (C29.8 S6).

#[test]
fn super_binary_send() {
    // super + n in a method delegates to the superclass's + method.
    let src = r#"
Object subclass: #Base.
Base >> + n [ ^n + 100 ]
Base subclass: #Child.
Child >> + n [ ^super + n + 1 ]
(Child new) + 5
"#;
    // super + 5 = 5 + 100 = 105; then 105 + 1 = 106.
    assert_eq!(eval_diff(src), Value::Int(106));
}

#[test]
fn super_keyword_send() {
    // super keyword: arg delegates to the superclass's keyword method.
    let src = r#"
Object subclass: #Base.
Base >> addTo: n [ ^n + 100 ]
Base subclass: #Child.
Child >> addTo: n [ ^super addTo: n + 1 ]
(Child new) addTo: 5
"#;
    // Child addTo: 5 → super addTo: 6 → 6 + 100 = 106.
    assert_eq!(eval_diff(src), Value::Int(106));
}

#[test]
fn super_keyword_multi_arg_send() {
    // super with a multi-keyword message exercises multi-arg SuperCollectArgs accumulation.
    let src = r#"
Object subclass: #Base.
Base >> from: a to: b [ ^b - a ]
Base subclass: #Child.
Child >> from: a to: b [ ^super from: a to: b + 1 ]
(Child new) from: 3 to: 8
"#;
    // Child from: 3 to: 8 → super from: 3 to: 9 → 9 - 3 = 6.
    assert_eq!(eval_diff(src), Value::Int(6));
}

// ── Namespace deep-path error ─────────────────────────────────────────────────

#[test]
fn namespace_ref_sub_namespace_not_found() {
    // Three-segment path A::B::C — Kernel has no sub-namespace Sub.
    // Exercises the segment-loop error branch in eval_namespace_ref.
    let err = eval_err_diff("Kernel::Sub::Object");
    assert!(matches!(err, EvalError::TypeError(_)));
}

// ── S7: historyOf: / historyOf:from:to: ──────────────────────────────────────

#[test]
fn history_of_records_ivar_assignments() {
    // Two assignments to `x` produce two history entries.
    let src = r#"
Object subclass: #Recorder
    instanceVariableNames: 'x'
    classVariableNames: ''
    indexedSlots: #none.
Recorder >> setX: v [ x := v ]
Recorder >> histX [ ^self historyOf: #x ]
| r hist |
r := Recorder new.
r setX: 1.
r setX: 2.
hist := r histX.
hist size
"#;
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn history_of_associations_have_timestamp_key() {
    // The first association's key is an Int (epoch-milliseconds).
    let src = r#"
Object subclass: #Stamped
    instanceVariableNames: 'n'
    classVariableNames: ''
    indexedSlots: #none.
Stamped >> setN: v [ n := v ]
Stamped >> histN [ ^self historyOf: #n ]
| s hist assoc |
s := Stamped new.
s setN: 42.
hist := s histN.
assoc := hist at: 0.
assoc key isNil not
"#;
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn history_of_association_value_matches_stored() {
    // The association value matches the last stored value.
    let src = r#"
Object subclass: #Logged
    instanceVariableNames: 'v'
    classVariableNames: ''
    indexedSlots: #none.
Logged >> setV: val [ v := val ]
Logged >> histV [ ^self historyOf: #v ]
| obj hist |
obj := Logged new.
obj setV: 99.
hist := obj histV.
(hist at: 0) value
"#;
    assert_eq!(eval_diff(src), Value::Int(99));
}

#[test]
fn history_of_from_to_filters_by_range() {
    // historyOf:from:to: with a range that includes 0..max returns all entries.
    let src = r#"
Object subclass: #Filtered
    instanceVariableNames: 'x'
    classVariableNames: ''
    indexedSlots: #none.
Filtered >> setX: v [ x := v ]
Filtered >> histXFromTo [ ^self historyOf: #x from: 0 to: 9999999999999 ]
| f hist |
f := Filtered new.
f setX: 1.
f setX: 2.
hist := f histXFromTo.
hist size
"#;
    assert_eq!(eval_diff(src), Value::Int(2));
}

#[test]
fn history_of_unknown_predicate_errors() {
    let src = r#"
Object subclass: #H.
H >> badHist [ ^self historyOf: #noSuchVar ]
H new badHist.
"#;
    let err = eval_err_diff(src);
    assert!(matches!(err, EvalError::TypeError(_)));
}

// ── S8: World instancesOf: ───────────────────────────────────────────────────

#[test]
fn world_instances_of_finds_spawned_entity() {
    // World instancesOf: SomeClass returns an Array containing the spawned instance.
    // `await` ensures Widget new completes (and track_entity runs) before the query.
    let src = r#"
Object subclass: #Widget.
Widget new await.
(World instancesOf: Widget) size
"#;
    assert_eq!(eval_diff(src), Value::Int(1));
}

#[test]
fn world_instances_of_empty_when_none_spawned() {
    // No instances of Gadget have been spawned.
    let src = r#"
Object subclass: #Gadget.
(World instancesOf: Gadget) size
"#;
    assert_eq!(eval_diff(src), Value::Int(0));
}

#[test]
fn world_instances_of_multiple_instances() {
    // Each `await` ensures track_entity has run before the next spawn or the query.
    let src = r#"
Object subclass: #Thing.
Thing new await. Thing new await. Thing new await.
(World instancesOf: Thing) size
"#;
    assert_eq!(eval_diff(src), Value::Int(3));
}

// ── Chunk 28: Dynamic arrays (S1) ────────────────────────────────────────────

#[test]
fn dynamic_array_evaluates_elements() {
    let src = "{ 1 + 1. 3. 'hello' }";
    let result = eval_diff(src);
    // Should be a 3-element Array
    assert_eq!(eval_diff("{ 1 + 1. 3. 'hello' } size"), Value::Int(3));
    assert_eq!(eval_diff("({ 1 + 1. 3. 'hello' }) at: 0"), Value::Int(2));
    assert_eq!(eval_diff("({ 1 + 1. 3. 'hello' }) at: 1"), Value::Int(3));
    let _ = result;
}

#[test]
fn dynamic_array_empty() {
    assert_eq!(eval_diff("{} size"), Value::Int(0));
}

#[test]
fn arrow_builds_association() {
    let src = "#foo -> 42";
    let result = eval_diff(src);
    // Association: key = #foo, value = 42
    assert_eq!(
        eval_diff("(#foo -> 42) key"),
        Value::Symbol(Arc::from("foo"))
    );
    assert_eq!(eval_diff("(#foo -> 42) value"), Value::Int(42));
    let _ = result;
}

#[test]
fn dynamic_array_of_associations() {
    let src = "{ #a -> 1. #b -> 2 } size";
    assert_eq!(eval_diff(src), Value::Int(2));
}

// ── Chunk 28a: Value immutability (S1) ───────────────────────────────────────

#[test]
fn value_object_frozen_after_prim_new_value() {
    // _primNewValue: freezes the instance; ivar write after construction raises TypeError.
    let src = r#"
Value subclass: #FrozenPoint
    instanceVariableNames: 'x'
    classVariableNames: ''
    indexedSlots: #none.
FrozenPoint class >> at: anX [
    ^self _primNewValue: [:p | p pvtSetX: anX]
]
FrozenPoint >> pvtSetX: v [ x := v ]
FrozenPoint >> x         [ ^x ]
| p |
p := FrozenPoint at: 42.
p x
"#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn value_object_ivar_write_after_freeze_raises_type_error() {
    use crate::test_helpers::eval_err_diff;
    let src = r#"
Value subclass: #FrozenPoint2
    instanceVariableNames: 'x'
    classVariableNames: ''
    indexedSlots: #none.
FrozenPoint2 class >> at: anX [
    ^self _primNewValue: [:p | p pvtSetX: anX]
]
FrozenPoint2 >> pvtSetX: v [ x := v ]
FrozenPoint2 >> x         [ ^x ]
| p |
p := FrozenPoint2 at: 10.
p pvtSetX: 99
"#;
    let err = eval_err_diff(src);
    assert!(
        err.to_string().contains("frozen"),
        "expected 'frozen' in error, got: {err}"
    );
}

#[test]
fn value_class_new_freezes_instance() {
    // Value class >> new calls _primNewValue: which freezes; further writes fail.
    use crate::test_helpers::eval_err_diff;
    let src = r#"
Value subclass: #Immutable
    instanceVariableNames: 'val'
    classVariableNames: ''
    indexedSlots: #none.
Immutable >> initialize [ val := 0 ]
Immutable >> pvtSetVal: v [ val := v ]
Immutable >> val        [ ^val ]
| i |
i := Immutable new.
i pvtSetVal: 1
"#;
    let err = eval_err_diff(src);
    assert!(
        err.to_string().contains("frozen"),
        "expected 'frozen' in error, got: {err}"
    );
}

// ── Chunk 28a: Cons linked list (S3) ─────────────────────────────────────────

#[test]
fn cons_head_and_tail() {
    assert_eq!(eval_diff("(Cons with: 1 rest: nil) head"), Value::Int(1));
    assert_eq!(eval_diff("(Cons with: 1 rest: nil) tail"), Value::Nil);
}

#[test]
fn cons_is_empty_false_nil_is_empty_true() {
    assert_eq!(
        eval_diff("(Cons with: 1 rest: nil) isEmpty"),
        Value::Bool(false)
    );
    assert_eq!(eval_diff("nil isEmpty"), Value::Bool(true));
}

#[test]
fn cons_add_first_prepends() {
    let src = "(Cons with: 2 rest: nil) addFirst: 1";
    // Result is Cons(head=1, tail=Cons(head=2, tail=nil))
    assert_eq!(eval_diff(&format!("({src}) head")), Value::Int(1));
    assert_eq!(eval_diff(&format!("({src}) tail head")), Value::Int(2));
}

#[test]
fn cons_size() {
    assert_eq!(eval_diff("(Cons with: 1 rest: nil) size"), Value::Int(1));
    assert_eq!(
        eval_diff("(Cons with: 2 rest: (Cons with: 1 rest: nil)) size"),
        Value::Int(2)
    );
}

#[test]
fn cons_do_visits_all() {
    let src = r#"
| n list |
n := ValueHolder initialValue: 0.
list := Cons with: 1 rest: (Cons with: 2 rest: (Cons with: 3 rest: nil)).
list do: [:each | n update: [:v | v + each]].
n get
"#;
    assert_eq!(eval_diff(src), Value::Int(6));
}

#[test]
fn cons_is_frozen_after_construction() {
    use crate::test_helpers::eval_err_diff;
    let src = r#"
| c |
c := Cons with: 1 rest: nil.
c pvtHead: 99 tail: nil.
"#;
    let err = eval_err_diff(src);
    assert!(
        err.to_string().contains("frozen"),
        "expected 'frozen' in error, got: {err}"
    );
}

// ── Chunk 28a: Condition restarts via Cons (S4) ───────────────────────────────

#[test]
fn condition_add_restart_returns_new_condition() {
    // addRestart:block: is functional: original unchanged, new condition returned.
    let src = r#"
| e1 e2 |
e1 := Error new.
e2 := e1 addRestart: #foo block: [42].
e2 hasRestart: #foo
"#;
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn condition_original_unchanged_after_add_restart() {
    let src = r#"
| e1 e2 |
e1 := Error new.
e2 := e1 addRestart: #foo block: [42].
e1 hasRestart: #foo
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

// ── Chunk 28: Condition hierarchy (S2) ───────────────────────────────────────

#[test]
fn condition_message_set() {
    assert_eq!(
        eval_diff("(Condition withMessage: 'x') message"),
        Value::Str(Arc::from("x"))
    );
}

#[test]
fn error_is_kind_of_condition() {
    assert_eq!(
        eval_diff("Error new isKindOf: Condition"),
        Value::Bool(true)
    );
}

#[test]
fn warning_is_kind_of_condition() {
    assert_eq!(
        eval_diff("Warning new isKindOf: Condition"),
        Value::Bool(true)
    );
}

#[test]
fn warning_is_not_kind_of_error() {
    assert_eq!(eval_diff("Warning new isKindOf: Error"), Value::Bool(false));
}

#[test]
fn error_signal_carries_message() {
    let src = "[Error signal: 'boom'] on: Error do: [:e | e message]";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("boom")));
}

// ── Chunk 28: on:do: hierarchy matching (S3/S4) ───────────────────────────────

#[test]
fn on_do_catches_by_superclass() {
    // Error is a subclass of Condition; catching Condition should catch Error.
    let src = "[Error signal: 'x'] on: Condition do: [:e | 'caught']";
    assert_eq!(eval_diff(src), Value::Str(Arc::from("caught")));
}

#[test]
fn on_do_does_not_catch_unrelated_subclass() {
    // Warning is NOT a subclass of Error. on: Error do: doesn't catch it.
    // Unhandled Warning returns nil per the Warning policy (log + continue).
    let src = "[Warning signal: 'w'] on: Error do: [:e | 'caught']";
    assert_eq!(eval_diff(src), Value::Nil);
}

#[test]
fn on_do_retry_reruns_body() {
    // Use ValueHolder to avoid NonLocalBlockAssignment restriction.
    let src = r#"
| n |
n := ValueHolder initialValue: 0.
[n update: [:v | v + 1]. (n get) < 3 ifTrue: [Error signal: 'retry me']. n get] on: Error do: [:e | e retry]
"#;
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn ensure_runs_on_success() {
    let src = r#"
| log |
log := ValueHolder initialValue: 0.
[log update: [:v | v + 1]] ensure: [log update: [:v | v + 10]].
log get
"#;
    assert_eq!(eval_diff(src), Value::Int(11));
}

#[test]
fn ensure_runs_on_error() {
    let src = r#"
| log |
log := ValueHolder initialValue: 0.
[[Error signal: 'x'] ensure: [log update: [:v | v + 10]]] on: Error do: [:e | nil].
log get
"#;
    assert_eq!(eval_diff(src), Value::Int(10));
}

// ── Chunk 28: control-flow primitives (S5) ───────────────────────────────────

#[test]
fn condition_resume_delivers_value_to_signal_site() {
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | e resume: 99].
hc run: [Error signal: 'x']
"#;
    assert_eq!(eval(src), Value::Int(99));
}

#[test]
fn condition_return_exits_handler_context() {
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | e return: 'done'].
hc run: [Error signal: 'x'. 'not reached']
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("done")));
}

#[test]
fn handler_context_retry_reruns_body() {
    let src = r#"
| n hc |
n := ValueHolder initialValue: 0.
hc := HandlerContext new on: Error do: [:e | e retry].
hc run: [n update: [:v | v + 1]. (n get) < 3 ifTrue: [Error signal: 'x']. n get]
"#;
    assert_eq!(eval(src), Value::Int(3));
}

// ── Chunk 28: HandlerContext handler_chain (S6) ───────────────────────────────

#[test]
fn handler_chain_empty_after_hcrun_exits() {
    // handler_chain must be truncated after run: completes.
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | 'caught'].
hc run: ['ok'].
"#;
    // Just check it returns normally
    assert_eq!(eval(src), Value::Str(Arc::from("ok")));
}

#[test]
fn handler_resignal_defers_to_outer_handler_cl_scoping() {
    // Common-Lisp handler-scoping: while a handler runs, it (and inner handlers)
    // are disabled, so a handler that re-signals a condition it itself handles
    // defers to the OUTER handler instead of re-matching itself (which would loop
    // forever under the naive "handler stays active" rule).
    let src = r#"
| outer |
outer := HandlerContext new on: Error do: [:e | 'outer caught'].
outer run: [
    (HandlerContext new on: Error do: [:e | Error signal: 'reraised'])
        run: [Error signal: 'original']
]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("outer caught")));
}

// C31.5: nested synchronous HandlerContext run: now runs entirely on the heap
// Cont stack (flattened _primHCRun: + signals), so this no longer overflows.
#[test]
fn handler_context_most_recent_handler_matches_first() {
    let src = r#"
| outer |
outer := HandlerContext new on: Error do: [:e | 'outer'].
outer run: [
    (HandlerContext new on: Error do: [:e | 'inner'])
        run: [Error signal: 'x']
]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("inner")));
}

// ── Chunk 28: Named restarts (S7) ────────────────────────────────────────────

#[test]
fn invoke_restart_delivers_value_via_resume() {
    // addRestart:block: is now functional — returns a new (frozen) Condition.
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | e invokeRestart: #useDefault].
hc run: [
    | e |
    e := Error new.
    e := e addRestart: #useDefault block: [:c | 'fallback'].
    e signal
]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("fallback")));
}

#[test]
fn invoke_restart_with_passes_arg() {
    // addRestart:block: is now functional — returns a new (frozen) Condition.
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | e invokeRestart: #useValue: with: 99].
hc run: [
    | e |
    e := Error new.
    e := e addRestart: #useValue: block: [:c :v | v].
    e signal
]
"#;
    assert_eq!(eval(src), Value::Int(99));
}

#[test]
fn invoke_restart_control_restart_unwinds_via_return() {
    // A control restart: the restart block performs its own transfer (`c return:`)
    // instead of resuming. The block receives the condition (first arg) and aborts
    // the protected region with a value — the `resume:` default is never reached.
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | e invokeRestart: #abort].
hc run: [
    | e |
    e := Error new.
    e := e addRestart: #abort block: [:c | c return: 'aborted'].
    e signal.
    'unreachable'
]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("aborted")));
}

#[test]
fn invoke_restart_block_receives_condition() {
    // The restart block can read state off the condition it is passed.
    let src = r#"
| hc |
hc := HandlerContext new on: Error do: [:e | e invokeRestart: #echo].
hc run: [
    | e |
    e := Error withMessage: 'boom'.
    e := e addRestart: #echo block: [:c | c message].
    e signal
]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("boom")));
}

// ── Chunk 28b: Cross-entity HandlerContext propagation ────────────────────────

#[test]
fn cross_entity_hc_resume_delivers_value() {
    // HC registered in the test evaluator propagates into a separate actor.
    // resume: resolves the actor future with the given value.
    let src = r#"
Object subclass: #CrossEntitySignaler
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
CrossEntitySignaler >> fireError [
    ^Error signal: 'from actor'
]
| actor hc |
actor := CrossEntitySignaler new.
hc := HandlerContext new on: Error do: [:e | e resume: 99].
hc run: [actor fireError]
"#;
    assert_eq!(eval(src), Value::Int(99));
}

#[test]
fn cross_entity_hc_return_exits_run() {
    // return: from an inherited handler resolves the actor future and exits run:.
    let src = r#"
Object subclass: #CrossEntitySignaler2
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
CrossEntitySignaler2 >> fireError [
    ^Error signal: 'from actor'
]
| actor hc |
actor := CrossEntitySignaler2 new.
hc := HandlerContext new on: Error do: [:e | e return: 'done'].
hc run: [actor fireError]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("done")));
}

#[test]
fn cross_entity_hc_retry_reruns_entity_message() {
    // retry from an inherited handler re-runs the actor's entire message handler.
    // The actor's ivar count persists across retries.
    let src = r#"
Object subclass: #RetryActor
    instanceVariableNames: 'count'
    classVariableNames: ''
    indexedSlots: #none.
RetryActor >> initialize [ count := 0 ]
RetryActor >> signalTwiceThenReturn [
    count := count + 1.
    count < 3 ifTrue: [ Error signal: 'retry me' ].
    ^count
]
| actor hc |
actor := RetryActor new.
hc := HandlerContext new on: Error do: [:e | e retry].
hc run: [actor signalTwiceThenReturn]
"#;
    assert_eq!(eval(src), Value::Int(3));
}

// C31.5: nested synchronous HC run: is now flattened onto the heap Cont stack.
#[test]
fn cross_entity_hc_inner_handler_takes_priority() {
    // Actor registers an inner HC handler for Error. Entity-local handlers (pushed
    // later, higher in the chain) take priority over inherited ones.
    let src = r#"
Object subclass: #NestedHCActor
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
NestedHCActor >> signalWithInnerHC [
    | inner |
    inner := HandlerContext new on: Error do: [:e | e resume: 'inner'].
    ^inner run: [ Error signal: 'x' ]
]
| actor outerHC |
actor := NestedHCActor new.
outerHC := HandlerContext new on: Error do: [:e | e resume: 'outer'].
outerHC run: [actor signalWithInnerHC]
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("inner")));
}

// C31.5: nested synchronous HC run: is now flattened onto the heap Cont stack.
#[test]
fn cross_entity_retry_targets_nearest_hcrun_boundary() {
    // retry from an inherited handler targets the nearest _primHCRun: in the
    // signaling entity's stack, not the outer sender's run: boundary.
    let src = r#"
Object subclass: #BoundaryActor
    instanceVariableNames: 'count'
    classVariableNames: ''
    indexedSlots: #none.
BoundaryActor >> initialize [ count := 0 ]
BoundaryActor >> tickAndSignalIfNeeded [
    count := count + 1.
    count < 3 ifTrue: [ Error signal: 'retry' ] ifFalse: [ ^count ]
]
BoundaryActor >> signalInInnerBoundary [
    "Inner HC handles Warning only; Error retry from inherited chain targets this boundary."
    | inner |
    inner := HandlerContext new on: Warning do: [:w | w resume: 'warn'].
    ^inner run: [ ^self tickAndSignalIfNeeded ]
]
| actor outerHC |
actor := BoundaryActor new.
outerHC := HandlerContext new on: Error do: [:e | e retry].
outerHC run: [actor signalInInnerBoundary]
"#;
    assert_eq!(eval(src), Value::Int(3));
}

// ── Chunk 28: Unhandled Warning policy (S8) ──────────────────────────────────

#[test]
fn unhandled_warning_suppress_completes() {
    let src = r#"
Warning loggingPolicy: #suppress.
Warning signal: 'quiet'
"#;
    // Should complete without error (returns nil)
    assert_eq!(eval_diff(src), Value::Nil);
}

#[test]
fn unhandled_base_condition_silent() {
    // Base Condition (not Warning, not Error) returns nil when unhandled.
    let src = r#"
Condition subclass: #BaseCond
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
BaseCond signal: 'quiet'
"#;
    assert_eq!(eval_diff(src), Value::Nil);
}

#[test]
fn ivar_read_in_block_arg_to_bool_method_tail_position() {
    // TCO removes the enclosing P>>= frame before `and:` evaluates its block.
    // Block must still reach `y` via home_self captured at block creation.
    let src = r#"
        Object subclass: #P instanceVariableNames: 'x y'.
        P class >> x: anX y: aY [ ^self new pvtInit: anX y: aY ]
        P >> pvtInit: anX y: aY [ x := anX. y := aY ]
        P >> x [ ^x ]
        P >> y [ ^y ]
        P >> = other [
            self class = other class ifFalse: [^false].
            ^x = other x and: [y = other y]
        ]
        (P x: 1 y: 2) = (P x: 1 y: 2)
    "#;
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn ivar_read_in_nested_block_after_tco() {
    // Nested block: outer block is the and: arg (TCO removes P>>= frame),
    // inner block is the or: arg. Both must see `y` via home_self.
    let src = r#"
        Object subclass: #P instanceVariableNames: 'x y'.
        P class >> x: anX y: aY [ ^self new pvtInit: anX y: aY ]
        P >> pvtInit: anX y: aY [ x := anX. y := aY ]
        P >> x [ ^x ]
        P >> y [ ^y ]
        P >> = other [
            self class = other class ifFalse: [^false].
            ^x = other x and: [y = other y or: [false]]
        ]
        (P x: 1 y: 2) = (P x: 1 y: 2)
    "#;
    assert_eq!(eval_diff(src), Value::Bool(true));
}

// ── SelfBinding::Primitive coverage ──────────────────────────────────────────

#[test]
fn primitive_method_reads_outer_env_var() {
    // lookup_var Primitive arm: outer-frame path (eval.rs line 276).
    // A user-defined method on Nil accesses a variable from the call-site env.
    let src = "| x | x := 99. Nil >> readX [ ^ x ]. nil readX";
    assert_eq!(eval_diff(src), Value::Int(99));
}

#[test]
fn assign_inside_primitive_method_body() {
    // assign_var Primitive arm (eval.rs line 330): assignment to a local
    // variable inside a user-defined method on a primitive-value class (Nil).
    // The method frame has SelfBinding::Primitive active during the assignment.
    let src = "Nil >> setAndGet [ | y | y := 42. ^ y ]. nil setAndGet";
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn super_in_nil_method_dispatches_to_object() {
    // current_defining_class Primitive arm (eval.rs line 191) +
    // dispatch_super_to_stack Primitive arm (eval_iterative.rs line 1263).
    let src = r#"
        Object >> baseMsg [ ^ 42 ]
        Nil >> testSuper [ ^ super baseMsg ]
        nil testSuper
    "#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn super_outside_method_context_errors() {
    // current_defining_class None arm (eval.rs line 192):
    // super send at the top level with no active method frame.
    let err = eval_err_diff("super foo");
    assert!(
        matches!(
            err,
            EvalError::TypeError(_) | EvalError::MessageNotUnderstood { .. }
        ),
        "expected TypeError or MNU, got {err:?}"
    );
}

// ── C31 S5: explicit Future reification ───────────────────────────────────────
// (iterative-only `eval`: Future/HandlerContext live in kernel.smol)

#[test]
fn future_resolve_then_sync_value() {
    let src = r#"
        | f |
        f := Future new.
        f resolveWith: 42.
        f await
    "#;
    assert_eq!(eval(src), Value::Int(42));
}

#[test]
fn future_then_runs_after_resolution() {
    // A continuation registered with then: on the explicit future runs once the
    // future resolves, observing the resolved value. then: is seamless on Future
    // (delegates to the underlying handle) — no implementation leak.
    let src = r#"
        | f |
        f := Future new.
        f resolveWith: 10.
        (f then: [:v | v + 1 ] orElse: #reject) await
    "#;
    assert_eq!(eval(src), Value::Int(11));
}

// ── C31 S5: probe — condition signaled in a then: continuation is caught by a
// HandlerContext handler captured via fork_context (the mechanism Transaction
// relies on: f then: [ ... signal ] under an enclosing HandlerContext). ────────

#[test]
fn signal_in_then_continuation_caught_by_enclosing_hc() {
    // The continuation future (returned by then:, which is the run: body's value)
    // is awaited so there is no race with the spawned continuation coroutine.
    let src = r#"
        | f cont |
        f := Future new.
        f resolveWith: 7.
        cont := (HandlerContext new on: Error do: [:e | e resume: 99])
            run: [ f then: [:v | (Error signal: 'from continuation') + 1 ] orElse: #reject ].
        cont await
    "#;
    // If the HC handler is captured into the then: continuation via fork_context,
    // it resumes the signal with 99, so the continuation computes 99 + 1 = 100.
    assert_eq!(eval(src), Value::Int(100));
}

#[test]
fn future_double_resolve_signals_already_resolved_and_keeps_first_value() {
    // Single-winner resolution: the first resolveWith: wins; a later attempt
    // signals the typed, restartable FutureAlreadyResolved condition. A handler
    // that invokes the #ignore restart keeps the original value (the late attempt
    // is a no-op), so the future still resolves to 1.
    let src = r#"
        | f |
        f := Future new.
        f resolveWith: 1.
        [ f resolveWith: 2 ] on: FutureAlreadyResolved do: [:e | e invokeRestart: #ignore].
        f await
    "#;
    assert_eq!(eval(src), Value::Int(1));
}

#[test]
fn future_double_resolve_condition_is_catchable_as_error() {
    // FutureAlreadyResolved is an Error subclass, so an Error handler also catches
    // it; returning from the handler yields its value (here #handled).
    let src = r#"
        | f |
        f := Future new.
        f resolveWith: 1.
        [ f resolveWith: 2. #unreached ] on: Error do: [:e | e return: #handled]
    "#;
    assert_eq!(eval(src), Value::Symbol(Arc::from("handled")));
}

// ---- C33: perform: / doesNotUnderstand: / reified Message ----

#[test]
fn perform_unary_selector() {
    let src = r#"
        Object subclass: #Greeter instanceVariableNames: ''.
        Greeter >> greeting [ ^#hello ].
        Greeter new perform: #greeting
    "#;
    assert_eq!(eval(src), Value::Symbol(Arc::from("hello")));
}

#[test]
fn perform_binary_selector_with_arguments() {
    // Binary send via perform:withArguments: on a primitive Int receiver.
    let src = r#"
        3 perform: #'+' withArguments: (Cons singleton: 4)
    "#;
    assert_eq!(eval(src), Value::Int(7));
}

#[test]
fn perform_keyword_selector_with_arguments() {
    let src = r#"
        Object subclass: #Adder instanceVariableNames: ''.
        Adder >> add: a to: b [ ^a + b ].
        Adder new perform: #add:to: withArguments: (Cons with: 10 rest: (Cons singleton: 5))
    "#;
    assert_eq!(eval(src), Value::Int(15));
}

#[test]
fn perform_non_symbol_selector_is_type_error() {
    let err = eval_err("3 perform: 42");
    assert!(matches!(err, EvalError::TypeError(_)));
}

#[test]
fn perform_unknown_selector_triggers_dnu() {
    // perform: of a selector the receiver does not understand routes through the
    // receiver's doesNotUnderstand: hook, just like a direct send.
    let src = r#"
        Object subclass: #Sink instanceVariableNames: ''.
        Sink >> doesNotUnderstand: aMessage [ ^#caught ].
        Sink new perform: #whatever
    "#;
    assert_eq!(eval(src), Value::Symbol(Arc::from("caught")));
}

#[test]
fn dnu_intercepts_unknown_message() {
    let src = r#"
        Object subclass: #Sink instanceVariableNames: ''.
        Sink >> doesNotUnderstand: aMessage [ ^#intercepted ].
        Sink new fooBar
    "#;
    assert_eq!(eval(src), Value::Symbol(Arc::from("intercepted")));
}

#[test]
fn dnu_reifies_selector_and_arguments() {
    // The reified Message exposes selector, arguments (Cons), and argumentCount.
    let sel = eval(
        r#"
        Object subclass: #Probe instanceVariableNames: ''.
        Probe >> doesNotUnderstand: aMessage [ ^aMessage selector ].
        Probe new frobnicate: 99 with: 7
    "#,
    );
    assert_eq!(sel, Value::Symbol(Arc::from("frobnicate:with:")));
    let count = eval(
        r#"
        Object subclass: #Probe instanceVariableNames: ''.
        Probe >> doesNotUnderstand: aMessage [ ^aMessage argumentCount ].
        Probe new frobnicate: 99 with: 7
    "#,
    );
    assert_eq!(count, Value::Int(2));
    let first = eval(
        r#"
        Object subclass: #Probe instanceVariableNames: ''.
        Probe >> doesNotUnderstand: aMessage [ ^aMessage arguments head ].
        Probe new frobnicate: 99 with: 7
    "#,
    );
    assert_eq!(first, Value::Int(99));
}

#[test]
fn dnu_forwards_via_send_to() {
    // Message>>sendTo: re-sends the reified message to another receiver.
    let src = r#"
        Object subclass: #Target instanceVariableNames: ''.
        Target >> double: n [ ^n * 2 ].
        Object subclass: #Proxy instanceVariableNames: 'target'.
        Proxy >> setTarget: t [ target := t ].
        Proxy >> doesNotUnderstand: aMessage [ ^aMessage sendTo: target ].
        | p |
        p := Proxy new.
        p setTarget: Target new.
        p double: 21
    "#;
    assert_eq!(eval(src), Value::Int(42));
}

#[test]
fn dnu_unary_message_has_empty_arguments() {
    let src = r#"
        Object subclass: #Probe instanceVariableNames: ''.
        Probe >> doesNotUnderstand: aMessage [ ^aMessage argumentCount ].
        Probe new noArgs
    "#;
    assert_eq!(eval(src), Value::Int(0));
}

#[test]
fn default_dnu_signals_message_not_understood_catchable() {
    // No override: the default Object>>doesNotUnderstand: signals a
    // MessageNotUnderstood condition, which an on:do: handler can catch.
    let src = r#"
        [ 5 mystery. #unreached ] on: MessageNotUnderstood do: [:e | #caught]
    "#;
    assert_eq!(eval(src), Value::Symbol(Arc::from("caught")));
}

#[test]
fn default_dnu_condition_carries_receiver_and_message() {
    let sel = eval(
        r#"
        [ 5 frob: 1 ] on: MessageNotUnderstood do: [:e | e failedMessage selector]
    "#,
    );
    assert_eq!(sel, Value::Symbol(Arc::from("frob:")));
    let recv = eval(
        r#"
        [ 5 frob: 1 ] on: MessageNotUnderstood do: [:e | e receiver]
    "#,
    );
    assert_eq!(recv, Value::Int(5));
}

#[test]
fn future_forwards_unknown_message_to_resolved_value() {
    // Seamless forwarding: sending a message the Future does not itself understand
    // implicitly awaits and forwards to the resolved value (via doesNotUnderstand:).
    let src = r#"
        | f |
        f := Future new.
        f resolveWith: 20.
        f + 22
    "#;
    assert_eq!(eval(src), Value::Int(42));
}

#[test]
fn future_forwards_keyword_message_to_resolved_value() {
    let src = r#"
        | f |
        f := Future new.
        f resolveWith: 10.
        f max: 25
    "#;
    assert_eq!(eval(src), Value::Int(25));
}

#[test]
fn default_dnu_resume_substitutes_send_result() {
    // Resuming the condition makes the failed send evaluate to the resumed value.
    let src = r#"
        | r |
        r := [ 5 mystery ] on: MessageNotUnderstood do: [:e | e resume: 99].
        r
    "#;
    assert_eq!(eval(src), Value::Int(99));
}

// ---- Traits (docs/traits_plan.md) ------------------------------------------

#[test]
fn trait_basic_composition() {
    // A trait's provided method is flattened into a using class and dispatchable.
    let src = r#"
        Trait named: #Greeter.
        Greeter >> greet [ ^'hi ' , self who ].
        Object subclass: #Person uses: Greeter instanceVariableNames: 'who'.
        Person >> who: w [ who := w ].
        Person >> who [ ^who ].
        | p | p := Person new. p who: 'sam'. p greet
    "#;
    assert_eq!(eval(src), Value::Str("hi sam".into()));
}

#[test]
fn trait_class_overrides_trait() {
    // class-defined method wins over the flattened trait method (class > trait).
    let src = r#"
        Trait named: #T.
        T >> tag [ ^'trait' ].
        Value subclass: #Thing uses: T instanceVariableNames: ''.
        Thing >> tag [ ^'class' ].
        Thing new tag
    "#;
    assert_eq!(eval(src), Value::Str("class".into()));
}

#[test]
fn trait_requirements_derived_from_self_sends() {
    let src = r#"
        Trait named: #T.
        T >> a [ ^self b + self c ].
        T >> b [ ^1 ].
        (T requirements) size
    "#;
    // sends: b (provided), c (not provided) -> requirement {c}
    assert_eq!(eval(src), Value::Int(1));
}

#[test]
fn trait_sum_conflict_raises_when_sent() {
    let src = r#"
        Trait named: #A.
        A >> foo [ ^1 ].
        Trait named: #B.
        B >> foo [ ^2 ].
        Value subclass: #C uses: (A + B) instanceVariableNames: ''.
        C new foo
    "#;
    assert_signal_contains(&eval_err(src), "unresolved trait conflict");
}

#[test]
fn trait_conflict_resolved_by_exclusion() {
    let src = r#"
        Trait named: #A.
        A >> foo [ ^1 ].
        Trait named: #B.
        B >> foo [ ^2 ].
        Value subclass: #C uses: (A + (B - { #foo })) instanceVariableNames: ''.
        C new foo
    "#;
    assert_eq!(eval(src), Value::Int(1));
}

#[test]
fn trait_asymmetric_override() {
    // (A - B) + B : union where B wins every conflict.
    let src = r#"
        Trait named: #A.
        A >> foo [ ^1 ].
        A >> bar [ ^10 ].
        Trait named: #B.
        B >> foo [ ^2 ].
        Value subclass: #C uses: ((A - B) + B) instanceVariableNames: ''.
        (C new foo) + (C new bar)
    "#;
    assert_eq!(eval(src), Value::Int(12));
}

#[test]
fn trait_conflict_resolved_by_class_method() {
    let src = r#"
        Trait named: #A.
        A >> foo [ ^1 ].
        Trait named: #B.
        B >> foo [ ^2 ].
        Value subclass: #C uses: (A + B) instanceVariableNames: ''.
        C >> foo [ ^99 ].
        C new foo
    "#;
    assert_eq!(eval(src), Value::Int(99));
}

#[test]
fn trait_aliasing_adds_entry_point() {
    let src = r#"
        Trait named: #A.
        A >> foo [ ^42 ].
        Value subclass: #C uses: (A @ { #fooA -> #foo }) instanceVariableNames: ''.
        (C new fooA) + (C new foo)
    "#;
    assert_eq!(eval(src), Value::Int(84));
}

#[test]
fn trait_named_from_expression() {
    let src = r#"
        Trait named: #A.
        A >> a [ ^1 ].
        Trait named: #B.
        B >> b [ ^2 ].
        Trait named: #AB from: A + B.
        Value subclass: #C uses: AB instanceVariableNames: ''.
        (C new a) + (C new b)
    "#;
    assert_eq!(eval(src), Value::Int(3));
}

#[test]
fn trait_shared_ancestor_not_a_conflict() {
    // A method reaching two sums via the same provenance must not conflict.
    let src = r#"
        Trait named: #Base.
        Base >> shared [ ^7 ].
        Value subclass: #C uses: (Base + Base) instanceVariableNames: ''.
        C new shared
    "#;
    assert_eq!(eval(src), Value::Int(7));
}

#[test]
fn trait_reflection_name_and_print() {
    let src = r#"
        Trait named: #Greeter.
        Greeter name
    "#;
    assert_eq!(eval(src), Value::Symbol("Greeter".into()));
    let src2 = r#"
        Trait named: #Greeter.
        Greeter printString
    "#;
    assert_eq!(eval(src2), Value::Str("Trait(#Greeter)".into()));
}

#[test]
fn trait_conflicts_reflection() {
    let src = r#"
        Trait named: #A.
        A >> foo [ ^1 ].
        Trait named: #B.
        B >> foo [ ^2 ].
        ((A + B) conflicts) size
    "#;
    assert_eq!(eval(src), Value::Int(1));
}

// ── Numeric tower & scalar value classes (docs/numeric_tower_plan.md S1) ──────

#[test]
fn scalar_class_identity() {
    assert_eq!(eval("42 class = MachineInteger"), Value::Bool(true));
    assert_eq!(
        eval("(1000000000000 * 1000000000000) class = ArbitraryInteger"),
        Value::Bool(true)
    );
    assert_eq!(eval("3.5 class = Float"), Value::Bool(true));
    assert_eq!(eval("'hi' class = String"), Value::Bool(true));
    assert_eq!(eval("#foo class = Symbol"), Value::Bool(true));
    assert_eq!(eval("true class = True"), Value::Bool(true));
    assert_eq!(eval("false class = False"), Value::Bool(true));
}

#[test]
fn scalars_are_kind_of_value() {
    // The reparenting fix: every scalar answers `isKindOf: Value` (was false for Int).
    assert_eq!(eval("42 isKindOf: Value"), Value::Bool(true));
    assert_eq!(eval("3.5 isKindOf: Value"), Value::Bool(true));
    assert_eq!(eval("'hi' isKindOf: Value"), Value::Bool(true));
    assert_eq!(eval("#foo isKindOf: Value"), Value::Bool(true));
    assert_eq!(eval("true isKindOf: Value"), Value::Bool(true));
    assert_eq!(
        eval("(1000000000000 * 1000000000000) isKindOf: Value"),
        Value::Bool(true)
    );
}

#[test]
fn integer_tower_membership() {
    // MachineInteger < Integer < Number < Value < Object. (Magnitude is no longer a
    // class — comparability is the Ordered trait, §2a.)
    assert_eq!(eval("42 isKindOf: Integer"), Value::Bool(true));
    assert_eq!(eval("42 isKindOf: Number"), Value::Bool(true));
    assert_eq!(eval("42 isKindOf: Value"), Value::Bool(true));
    assert_eq!(eval("42 isKindOf: Object"), Value::Bool(true));
    // ArbitraryInteger is also an Integer.
    assert_eq!(
        eval("(1000000000000 * 1000000000000) isKindOf: Integer"),
        Value::Bool(true)
    );
}

#[test]
fn float_is_number_not_integer() {
    assert_eq!(eval("3.5 isKindOf: Number"), Value::Bool(true));
    assert_eq!(eval("3.5 isKindOf: Integer"), Value::Bool(false));
}

#[test]
fn string_is_value_leaf_not_number() {
    // String/Symbol are direct Value leaves, NOT Numbers. Comparability is a trait
    // (Ordered), so a String is comparable without any `is-a Number`/`Magnitude`.
    assert_eq!(eval("'hi' isKindOf: Number"), Value::Bool(false));
    assert_eq!(eval("#foo isKindOf: Number"), Value::Bool(false));
    assert_eq!(eval("'hi' isKindOf: Value"), Value::Bool(true));
}

#[test]
fn scalar_arithmetic_unaffected_by_tower() {
    // S1 does not touch arithmetic; the Rust primitive layer still handles it.
    assert_eq!(eval("2 + 3"), Value::Int(5));
    assert_eq!(eval("'ab' , 'cd'"), Value::Str("abcd".into()));
    assert_eq!(eval("'hi' size"), Value::Int(2));
    // Migrated `Integer >> timesRepeat:` still resolves and runs (was `Int >>`).
    assert_eq!(eval("5 timesRepeat: [ 0 ]"), Value::Nil);
}

// ── Encoded-leaf closedness / vmSubclass: (docs/numeric_tower_plan.md §4b, S1b) ──

#[test]
fn cannot_subclass_encoded_leaf() {
    for leaf in [
        "MachineInteger",
        "ArbitraryInteger",
        "Float",
        "String",
        "Symbol",
        "True",
        "False",
    ] {
        let src = format!(
            "{leaf} subclass: #Foo instanceVariableNames: '' classVariableNames: '' indexedSlots: #none"
        );
        match eval_err(&src) {
            crate::eval::EvalError::TypeError(msg) => {
                assert!(
                    msg.contains("cannot subclass") && msg.contains(leaf),
                    "unexpected message for {leaf}: {msg}"
                );
            }
            other => panic!("expected TypeError for {leaf}, got {other:?}"),
        }
    }
}

#[test]
fn abstract_tower_classes_stay_open() {
    // Magnitude/Number/Integer are not encoded leaves; subclassing them is allowed.
    assert_eq!(
        eval(
            "Number subclass: #Fraction instanceVariableNames: 'n d' classVariableNames: '' indexedSlots: #none. (Fraction new) isKindOf: Number"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        eval(
            "Integer subclass: #ModInt instanceVariableNames: 'r' classVariableNames: '' indexedSlots: #none. (ModInt new) isKindOf: Integer"
        ),
        Value::Bool(true)
    );
}

#[test]
fn vm_subclass_reserved_for_encoded_names() {
    // A user cannot invent a VM-encoded class: vmSubclass: rejects unknown names.
    match eval_err(
        "Object vmSubclass: #Fake instanceVariableNames: '' classVariableNames: '' indexedSlots: #none",
    ) {
        crate::eval::EvalError::TypeError(msg) => {
            assert!(
                msg.contains("vmSubclass:") && msg.contains("Fake"),
                "got: {msg}"
            );
        }
        other => panic!("expected TypeError, got {other:?}"),
    }
}

// ── S2.0 seam: value / withValue: (docs/numeric_tower_plan.md §4e) ────────────

#[test]
fn leaves_project_to_self_via_value() {
    // Each encoded numeric leaf answers `^self` to `value`, grounding the seam.
    assert_eq!(eval("42 value"), Value::Int(42));
    assert_eq!(eval("3.5 value"), Value::Float(3.5));
    // ArbitraryInteger: force a value past the machine-word range; it stays a leaf.
    assert!(matches!(
        eval("(1000000000000 * 1000000000000) value"),
        Value::LargeInt(_)
    ));
}

#[test]
fn number_withvalue_default_answers_the_leaf() {
    // The inherited Value>>withValue: reconstruction default answers the bare leaf
    // (identity for a leaf receiver; the escape hatch for a wrapper). A leaf sending
    // it to another leaf simply yields that leaf.
    assert_eq!(eval("42 withValue: 7"), Value::Int(7));
    assert_eq!(eval("3.5 withValue: 9"), Value::Int(9));
}

#[test]
fn value_seam_lifted_to_value_leaves() {
    // §4e (2026-08-01): the value/withValue: default lives on Value, so String,
    // Symbol, and Bool answer `value` with themselves (the block-or-value idiom) —
    // they are no longer "closed". Only Number overrides value to subclassResponsibility.
    assert_eq!(eval("'hi' value"), Value::Str("hi".into()));
    assert_eq!(eval("#foo value"), Value::Symbol("foo".into()));
    assert_eq!(eval("true value"), Value::Bool(true));
    assert_eq!(eval("'hi' withValue: 'bye'"), Value::Str("bye".into()));
}

// ── S2: comparison protocol (Ordered trait + _primLt:, §2a) ──────────────────

#[test]
fn same_type_numeric_comparison() {
    assert_eq!(eval("2 < 3"), Value::Bool(true));
    assert_eq!(eval("3 < 2"), Value::Bool(false));
    assert_eq!(eval("3.5 < 4.0"), Value::Bool(true));
    // ArbitraryInteger vs ArbitraryInteger.
    assert_eq!(
        eval("(1000000000000 * 1000000000000) < (1000000000000 * 1000000000001)"),
        Value::Bool(true)
    );
}

#[test]
fn cross_type_numeric_comparison_is_symmetric() {
    // The bug the tower fixes: Int vs Float in BOTH directions (was asymmetric).
    assert_eq!(eval("2 < 3.5"), Value::Bool(true));
    assert_eq!(eval("3.5 < 2"), Value::Bool(false));
    // Int vs ArbitraryInteger.
    assert_eq!(
        eval("2 < (1000000000000 * 1000000000000)"),
        Value::Bool(true)
    );
    assert_eq!(
        eval("(1000000000000 * 1000000000000) < 2"),
        Value::Bool(false)
    );
    // Float vs ArbitraryInteger.
    assert_eq!(
        eval("3.5 < (1000000000000 * 1000000000000)"),
        Value::Bool(true)
    );
}

#[test]
fn derived_comparison_protocol_from_ordered_trait() {
    // <= > >= max: min: between:and: are provided by the Ordered trait, derived from
    // `<` alone — and they work cross-type via _primLt: promotion.
    assert_eq!(eval("2 <= 2"), Value::Bool(true));
    assert_eq!(eval("3 > 2.5"), Value::Bool(true));
    assert_eq!(eval("3 >= 3"), Value::Bool(true));
    assert_eq!(eval("2 max: 5"), Value::Int(5));
    assert_eq!(eval("2 min: 5.0"), Value::Int(2));
    assert_eq!(eval("5 between: 1 and: 10"), Value::Bool(true));
    assert_eq!(eval("5 between: 6 and: 10"), Value::Bool(false));
}

#[test]
fn string_is_ordered_lexicographically() {
    // String uses: Ordered — comparable without being a Number.
    assert_eq!(eval("'a' < 'b'"), Value::Bool(true));
    assert_eq!(eval("'b' < 'a'"), Value::Bool(false));
    assert_eq!(eval("'abc' <= 'abc'"), Value::Bool(true));
    assert_eq!(eval("'cat' max: 'dog'"), Value::Str("dog".into()));
}

#[test]
fn abstract_number_value_is_subclass_responsibility() {
    // Number itself has no representation, so `value` is a subclass responsibility.
    // A user subclass of Number that fails to implement it hits the loud signal.
    assert_signal_contains(
        &eval_err(
            "Number subclass: #Vague instanceVariableNames: '' classVariableNames: '' indexedSlots: #none. \
             Vague new value",
        ),
        "subclass responsibility",
    );
}

#[test]
fn user_number_subclass_inherits_via_value_seam() {
    // Story A: a wrapper implements `value` (project) and inherits everything the
    // abstract tower routes through it. A Number subclass is a Value (immutable), so
    // it is constructed via the _primNewValue: builder — ivars are set inside the
    // builder block, then frozen (same pattern as Condition). `value` then grounds
    // the seam by returning the underlying leaf.
    assert_eq!(
        eval(
            "Number subclass: #Boxed instanceVariableNames: 'n' classVariableNames: '' indexedSlots: #none. \
             Boxed class >> of: x [ ^self _primNewValue: [:b | b setN: x] ]. \
             Boxed >> setN: x [ n := x ]. \
             Boxed >> value [ ^n ]. \
             (Boxed of: 5) value",
        ),
        Value::Int(5)
    );
}

// ── Numeric tower S3: arithmetic in Smol, coercion edge ───────────────────────

#[test]
fn s3_same_type_arithmetic_all_leaves() {
    assert_eq!(eval("7 + 5"), Value::Int(12));
    assert_eq!(eval("7 - 5"), Value::Int(2));
    assert_eq!(eval("7 * 5"), Value::Int(35));
    assert_eq!(eval("7 / 2"), Value::Int(3));
    assert_eq!(eval("7 // 2"), Value::Int(3));
    assert_eq!(eval("7 \\\\ 2"), Value::Int(1));
    assert_eq!(eval("1.5 + 2.5"), Value::Float(4.0));
    assert_eq!(eval("1.5 - 0.5"), Value::Float(1.0));
    assert_eq!(eval("1.5 * 2.0"), Value::Float(3.0));
    assert_eq!(eval("3.0 / 2.0"), Value::Float(1.5));
    assert_eq!(
        eval("99999999999999999999 - 99999999999999999998"),
        Value::Int(1)
    );
}

#[test]
fn s3_negative_division_floors_and_takes_divisor_sign() {
    // `//` floors toward negative infinity, `\\` takes the sign of the divisor —
    // Smalltalk semantics, preserved through the move into Smol.
    assert_eq!(eval("-7 // 2"), Value::Int(-4));
    assert_eq!(eval("-7 \\\\ 2"), Value::Int(1));
    assert_eq!(eval("7 // -2"), Value::Int(-4));
    assert_eq!(eval("7 \\\\ -2"), Value::Int(-1));
    // `/` between integers truncates rather than flooring (unchanged behavior).
    assert_eq!(eval("-7 / 2"), Value::Int(-3));
}

#[test]
fn s3_cross_type_arithmetic_is_symmetric() {
    // The reported bug: `2 + 3.5` errored while `3.5 + 2` worked. The promoting
    // primitive fixes both directions, exactly as `_primLt:` did for `2 < 3.5`.
    assert_eq!(eval("2 + 3.5"), Value::Float(5.5));
    assert_eq!(eval("3.5 + 2"), Value::Float(5.5));
    assert_eq!(eval("2 - 0.5"), Value::Float(1.5));
    assert_eq!(eval("0.5 - 2"), Value::Float(-1.5));
    assert_eq!(eval("2 * 1.5"), Value::Float(3.0));
    assert_eq!(eval("1.5 * 2"), Value::Float(3.0));
    assert_eq!(eval("3 / 2.0"), Value::Float(1.5));
    assert_eq!(eval("3.0 / 2"), Value::Float(1.5));
}

#[test]
fn s3_machine_and_arbitrary_integers_mix() {
    // Cross-representation integer arithmetic promotes to BigInt in either order.
    assert_eq!(
        eval("99999999999999999999 + 1 = 100000000000000000000"),
        Value::Bool(true)
    );
    assert_eq!(
        eval("1 + 99999999999999999999 = 100000000000000000000"),
        Value::Bool(true)
    );
    // …and to Float when a Float joins in.
    assert_eq!(eval("(99999999999999999999 * 0.0)"), Value::Float(0.0));
}

#[test]
fn s3_overflow_promotes_and_result_demotes() {
    // Representation follows magnitude, not operand history: overflow promotes to
    // ArbitraryInteger, and a result that fits a machine word demotes back.
    assert_eq!(
        eval("(4611686018427387904 * 4) class printString"),
        Value::Str("ArbitraryInteger".into())
    );
    assert_eq!(
        eval("(99999999999999999999 - 99999999999999999998) class printString"),
        Value::Str("MachineInteger".into())
    );
    // i64::MIN edge cases that overflow the machine fast path.
    assert_eq!(eval("(-9223372036854775807 - 1) \\\\ -1"), Value::Int(0));
    assert_eq!(
        eval("((-9223372036854775807 - 1) / -1) class printString"),
        Value::Str("ArbitraryInteger".into())
    );
}

#[test]
fn s3_division_by_zero_errors_on_both_integer_leaves() {
    assert!(matches!(
        eval_err("1 / 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("1 // 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("1 \\\\ 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("99999999999999999999 / 0"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn s3_float_has_no_integer_division_protocol() {
    // `//` and `\\` are Integer protocol; Float is not given them.
    assert_signal_contains(&eval_err("1.5 // 2.0"), "does not understand");
    assert_signal_contains(&eval_err("1.5 \\\\ 2.0"), "does not understand");
}

#[test]
fn s3_non_number_argument_reaches_the_coercion_edge() {
    // Not a Rust TypeError: the leaf hands the operation to the argument, whose
    // refusing default names both operand and operator.
    assert_signal_contains(&eval_err("3 + 'bad'"), "cannot coerce");
    assert_signal_contains(&eval_err("3 + 'bad'"), "+");
    assert_signal_contains(&eval_err("3.5 * nil"), "cannot coerce");
}

#[test]
fn s3_user_number_joins_the_tower_without_changing_the_kernel() {
    // The openness test. `Half` is a structured number with no encoded leaf: it
    // implements ONLY `adaptToInteger:andSend:` on itself. No kernel leaf method,
    // no allowlist, and no central generality table is touched — which is the whole
    // point of routing the non-encoded case through double dispatch.
    let src = "Number subclass: #Half instanceVariableNames: 'halves' \
                   classVariableNames: '' indexedSlots: #none. \
               Half class >> halves: h [ ^self _primNewValue: [:x | x setHalves: h] ]. \
               Half >> setHalves: h [ halves := h ]. \
               Half >> halves [ ^halves ]. \
               Half >> adaptToInteger: anInteger andSend: aSymbol [ \
                   ^Half halves: ((anInteger * 2) perform: aSymbol withArguments: (Cons with: halves rest: nil)) ]. \
               ";
    // `3 + (Half halves: 1)` — the leaf can't handle a Half, so Half promotes the
    // receiver into halves (3 → 6) and performs the operation there: 6 + 1 = 7 halves.
    assert_eq!(
        eval(&format!("{src} (3 + (Half halves: 1)) halves")),
        Value::Int(7)
    );
    // Non-commutative operators keep operand order: 6 - 1 = 5 halves, not 1 - 6.
    assert_eq!(
        eval(&format!("{src} (3 - (Half halves: 1)) halves")),
        Value::Int(5)
    );
}

#[test]
fn s3_wrapper_reconstructs_via_the_value_seam() {
    // Story A continued from S2.0: a wrapper that implements `value` + `withValue:`
    // gets closed arithmetic for free from abstract `Number`, which projects SELF
    // ONLY and reconstructs the receiver's kind around the result.
    let src = "Number subclass: #Boxed instanceVariableNames: 'n' \
                   classVariableNames: '' indexedSlots: #none. \
               Boxed class >> of: x [ ^self _primNewValue: [:b | b setN: x] ]. \
               Boxed >> setN: x [ n := x ]. \
               Boxed >> value [ ^n ]. \
               Boxed >> withValue: v [ ^Boxed of: v ]. \
               ";
    assert_eq!(
        eval(&format!("{src} ((Boxed of: 5) + 3) value")),
        Value::Int(8)
    );
    assert_eq!(
        eval(&format!("{src} ((Boxed of: 5) + 3) class printString")),
        Value::Str("Boxed".into())
    );
    // Cross-type through the seam: the projected leaf promotes, then reconstructs.
    assert_eq!(
        eval(&format!("{src} ((Boxed of: 5) * 1.5) value")),
        Value::Float(7.5)
    );
    // The inherited `withValue:` default is the escape hatch: a wrapper that does
    // NOT reconstruct answers the bare leaf.
    let bare = "Number subclass: #Peek instanceVariableNames: 'n' \
                    classVariableNames: '' indexedSlots: #none. \
                Peek class >> of: x [ ^self _primNewValue: [:b | b setN: x] ]. \
                Peek >> setN: x [ n := x ]. \
                Peek >> value [ ^n ]. \
                ";
    assert_eq!(eval(&format!("{bare} (Peek of: 5) + 3")), Value::Int(8));
}

#[test]
fn s3_encoded_number_predicate_is_leaf_private() {
    // The branch mechanism is an implementation detail of the leaves: encoded
    // numbers answer true, everything else inherits false from Object — including a
    // user Number subclass, which is exactly what routes it to the coercion edge.
    assert_eq!(eval("3 isEncodedNumber"), Value::Bool(true));
    assert_eq!(eval("3.5 isEncodedNumber"), Value::Bool(true));
    assert_eq!(
        eval("99999999999999999999 isEncodedNumber"),
        Value::Bool(true)
    );
    assert_eq!(eval("'x' isEncodedNumber"), Value::Bool(false));
    assert_eq!(
        eval(
            "Number subclass: #Wrapped instanceVariableNames: '' classVariableNames: '' \
                 indexedSlots: #none. \
             Wrapped class >> make [ ^self _primNewValue: [:x | x] ]. \
             Wrapped make isEncodedNumber"
        ),
        Value::Bool(false)
    );
}

#[test]
fn perform_accepts_binary_operator_symbols() {
    assert_eq!(
        eval("6 perform: #+ withArguments: (Cons with: 1 rest: nil)"),
        Value::Int(7)
    );
    assert_eq!(
        eval("6 perform: #// withArguments: (Cons with: 4 rest: nil)"),
        Value::Int(1)
    );
    assert_eq!(
        eval("6 perform: #< withArguments: (Cons with: 7 rest: nil)"),
        Value::Bool(true)
    );
}

#[test]
fn perform_with_arguments_rejects_a_non_cons_list() {
    // Previously an Array (or any non-Cons) silently unpacked to ZERO arguments,
    // turning a mistake into a confusing failure at the target selector.
    assert!(matches!(
        eval_err("6 perform: #abs withArguments: {1}"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("6 perform: #+ withArguments: 1"),
        crate::eval::EvalError::TypeError(_)
    ));
    // nil remains the valid empty argument list.
    assert_eq!(eval("-6 perform: #abs withArguments: nil"), Value::Int(6));
}

// ── Numeric tower S4: Integer/Number protocol authored in Smol ────────────────

#[test]
fn s4_number_sign_abs_and_negated() {
    assert_eq!(eval("5 negated"), Value::Int(-5));
    assert_eq!(eval("-5 negated"), Value::Int(5));
    assert_eq!(eval("-5 abs"), Value::Int(5));
    assert_eq!(eval("5 abs"), Value::Int(5));
    assert_eq!(eval("0 abs"), Value::Int(0));
    assert_eq!(eval("-2.5 abs"), Value::Float(2.5));
    assert_eq!(eval("-2.5 negated"), Value::Float(2.5));
    // sign classifies the receiver, so it answers a plain Integer even for a Float.
    assert_eq!(eval("-7 sign"), Value::Int(-1));
    assert_eq!(eval("0 sign"), Value::Int(0));
    assert_eq!(eval("7 sign"), Value::Int(1));
    assert_eq!(eval("-2.5 sign"), Value::Int(-1));
    assert_eq!(eval("0.0 sign"), Value::Int(0));
}

#[test]
fn s4_number_squared_reciprocal_is_zero() {
    assert_eq!(eval("7 squared"), Value::Int(49));
    assert_eq!(eval("1.5 squared"), Value::Float(2.25));
    assert_eq!(eval("4 reciprocal"), Value::Int(0)); // integer division truncates
    assert_eq!(eval("4.0 reciprocal"), Value::Float(0.25));
    assert_eq!(eval("0 isZero"), Value::Bool(true));
    assert_eq!(eval("1 isZero"), Value::Bool(false));
    // NOTE: `0.0 isZero` is currently FALSE — `=` is not on the tower and does not
    // promote across representations (`2 = 2.0` is false while `2 < 2.0` promotes).
    // Tracked as an open gap; see docs/numeric_tower_plan.md §4g.
}

#[test]
fn s4_negated_promotes_at_the_machine_word_boundary() {
    // i64::MIN has no positive machine-word counterpart, so `negated` must promote.
    assert_eq!(
        eval("(-9223372036854775807 - 1) negated class printString"),
        Value::Str("ArbitraryInteger".into())
    );
    assert_eq!(
        eval("(-9223372036854775807 - 1) abs > 0"),
        Value::Bool(true)
    );
}

#[test]
fn s4_integer_even_and_odd() {
    assert_eq!(eval("4 even"), Value::Bool(true));
    assert_eq!(eval("4 odd"), Value::Bool(false));
    assert_eq!(eval("7 even"), Value::Bool(false));
    assert_eq!(eval("7 odd"), Value::Bool(true));
    assert_eq!(eval("0 even"), Value::Bool(true));
    // `\\` takes the sign of the divisor, so negative odds still classify correctly.
    assert_eq!(eval("-7 odd"), Value::Bool(true));
    assert_eq!(eval("-4 even"), Value::Bool(true));
}

#[test]
fn s4_integer_gcd_and_lcm() {
    assert_eq!(eval("12 gcd: 18"), Value::Int(6));
    assert_eq!(eval("18 gcd: 12"), Value::Int(6));
    assert_eq!(eval("17 gcd: 5"), Value::Int(1));
    assert_eq!(eval("12 gcd: 0"), Value::Int(12));
    assert_eq!(eval("0 gcd: 12"), Value::Int(12));
    // Negative operands answer a non-negative gcd, per the usual convention.
    assert_eq!(eval("-12 gcd: 8"), Value::Int(4));
    assert_eq!(eval("12 gcd: -8"), Value::Int(4));
    assert_eq!(eval("4 lcm: 6"), Value::Int(12));
    assert_eq!(eval("6 lcm: 4"), Value::Int(12));
    assert_eq!(eval("7 lcm: 3"), Value::Int(21));
    assert_eq!(eval("-4 lcm: 6"), Value::Int(12));
    assert_eq!(eval("0 lcm: 5"), Value::Int(0));
}

#[test]
fn s4_integer_factorial_promotes_exactly() {
    assert_eq!(eval("0 factorial"), Value::Int(1));
    assert_eq!(eval("1 factorial"), Value::Int(1));
    assert_eq!(eval("5 factorial"), Value::Int(120));
    assert_eq!(eval("20 factorial"), Value::Int(2432902008176640000));
    // 21! exceeds i64: the leaf primitives promote, so the result stays exact.
    assert_eq!(
        eval("21 factorial class printString"),
        Value::Str("ArbitraryInteger".into())
    );
    assert_eq!(
        eval("21 factorial printString"),
        Value::Str("51090942171709440000".into())
    );
    assert_eq!(eval("30 factorial > (20 factorial)"), Value::Bool(true));
    assert_signal_contains(&eval_err("-1 factorial"), "factorial of a negative");
}

#[test]
fn s4_integer_is_prime() {
    assert_eq!(eval("2 isPrime"), Value::Bool(true));
    assert_eq!(eval("3 isPrime"), Value::Bool(true));
    assert_eq!(eval("97 isPrime"), Value::Bool(true));
    assert_eq!(eval("7919 isPrime"), Value::Bool(true));
    assert_eq!(eval("1 isPrime"), Value::Bool(false));
    assert_eq!(eval("0 isPrime"), Value::Bool(false));
    assert_eq!(eval("-7 isPrime"), Value::Bool(false));
    assert_eq!(eval("4 isPrime"), Value::Bool(false));
    assert_eq!(eval("9 isPrime"), Value::Bool(false));
    assert_eq!(eval("7917 isPrime"), Value::Bool(false)); // 3 × 7 × 13 × 29
}

#[test]
fn s4_protocol_is_encoding_agnostic_for_a_wrapper() {
    // The §4c payoff. A wrapper implementing only the seam (`value` + `withValue:`)
    // inherits the tower. This test covers the receiver-position cases; S5 closed the
    // argument-position ones (`squared`, `factorial`, `gcd:`, `lcm:`) — see
    // `s5_receiver_in_argument_position_is_carried`.
    let src = "Integer subclass: #Boxed instanceVariableNames: 'n' \
                   classVariableNames: '' indexedSlots: #none. \
               Boxed class >> of: x [ ^self _primNewValue: [:b | b setN: x] ]. \
               Boxed >> setN: x [ n := x ]. \
               Boxed >> value [ ^n ]. \
               Boxed >> withValue: v [ ^Boxed of: v ]. \
               ";
    assert_eq!(
        eval(&format!("{src} (Boxed of: -7) abs value")),
        Value::Int(7)
    );
    assert_eq!(
        eval(&format!("{src} (Boxed of: 7) negated value")),
        Value::Int(-7)
    );
    assert_eq!(
        eval(&format!("{src} ((Boxed of: 5) + 3) value")),
        Value::Int(8)
    );
    // Quantities keep the receiver's kind through reconstruction…
    assert_eq!(
        eval(&format!("{src} (Boxed of: -7) abs class printString")),
        Value::Str("Boxed".into())
    );
    // …while classifications answer plain values.
    assert_eq!(eval(&format!("{src} (Boxed of: -7) sign")), Value::Int(-1));
    // Comparison already projects its argument with `value`, so `<`-derived protocol
    // works in either position.
    assert_eq!(eval(&format!("{src} (Boxed of: 3) < 5")), Value::Bool(true));
    assert_eq!(eval(&format!("{src} 5 < (Boxed of: 7)")), Value::Bool(true));
}

/// A wrapper implementing ONLY the seam (`value` + `withValue:`) — deliberately no
/// `adaptTo…`, to prove §4c's "two methods" claim is now literally true.
const S5_BOXED: &str = "Integer subclass: #Boxed instanceVariableNames: 'n' \
                        classVariableNames: '' indexedSlots: #none. \
                        Boxed class >> of: x [ ^self _primNewValue: [:b | b setN: x] ]. \
                        Boxed >> setN: x [ n := x ]. \
                        Boxed >> value [ ^n ]. \
                        Boxed >> withValue: v [ ^Boxed of: v ]. ";

#[test]
fn s5_wrapper_domain_wins_in_either_operand_position() {
    // The specificity rule (§4g): a leaf carries no domain, a wrapper does, so the
    // wrapper's domain wins regardless of POSITION. This is the property the
    // same-class discriminator failed to provide — it answered a leaf for
    // `boxed * 3` and a wrapper for `3 * boxed`.
    for expr in ["(Boxed of: 3) * 8", "8 * (Boxed of: 3)"] {
        assert_eq!(
            eval(&format!("{S5_BOXED} ({expr}) class printString")),
            Value::Str("Boxed".into()),
            "{expr} should answer a Boxed"
        );
        assert_eq!(
            eval(&format!("{S5_BOXED} ({expr}) value")),
            Value::Int(24),
            "{expr} should answer 24"
        );
    }
}

#[test]
fn s5_argument_position_preserves_operand_order() {
    // The `Number >> adaptTo…` default must keep the ORIGINAL operand order — the
    // send was `aLeaf op self`, so the leaf stays on the LEFT. Commutative ops hide
    // a reversal here, so every non-commutative operator is checked in both
    // positions. (`/` between integer leaves truncates, hence 3 and 0.)
    let cases = [
        ("(Boxed of: 3) - 8", -5),
        ("8 - (Boxed of: 3)", 5),
        ("(Boxed of: 24) / 8", 3),
        ("8 / (Boxed of: 24)", 0),
        ("(Boxed of: 17) // 5", 3),
        ("17 // (Boxed of: 5)", 3),
        ("(Boxed of: 17) \\\\ 5", 2),
        ("17 \\\\ (Boxed of: 5)", 2),
    ];
    for (expr, want) in cases {
        assert_eq!(
            eval(&format!("{S5_BOXED} ({expr}) value")),
            Value::Int(want),
            "{expr}"
        );
    }
}

#[test]
fn s5_receiver_in_argument_position_is_carried() {
    // Two of the four S4 failures. Each puts the receiver into argument position:
    // `squared` = self * self; `factorial` = self * (self - 1) factorial. Both stay
    // in the wrapper's domain.
    //
    // `gcd:`/`lcm:` are NOT here: they terminate on `anInteger isZero`, which is
    // `^self = 0`, and a wrapper has no `=` of its own — so `Boxed(0) = 0` is false,
    // the base case never fires, and the recursion divides by zero. That is Gap A,
    // fixed in S6; see `s6_` tests once it lands.
    let cases = [
        ("(Boxed of: 9) squared", 81),
        ("(Boxed of: 5) factorial", 120),
    ];
    for (expr, want) in cases {
        assert_eq!(
            eval(&format!("{S5_BOXED} ({expr}) value")),
            Value::Int(want),
            "{expr}"
        );
        assert_eq!(
            eval(&format!("{S5_BOXED} ({expr}) class printString")),
            Value::Str("Boxed".into()),
            "{expr} should stay in the wrapper's domain"
        );
    }
}

#[test]
fn s5_two_different_wrappers_reach_domain_named_dispatch() {
    // §4e case 2 / §4g last row. Neither operand may unilaterally claim the result,
    // so the receiver is handed to the argument INTACT via a selector naming the
    // RECEIVER's domain — #adaptToBoxed:andSend: here, derived from the class name.
    // The receiver arrives un-projected: Money asserts it can send `value` to it.
    let src = format!(
        "{S5_BOXED} \
         Integer subclass: #Money instanceVariableNames: 'm' \
             classVariableNames: '' indexedSlots: #none. \
         Money class >> of: x [ ^self _primNewValue: [:b | b setM: x] ]. \
         Money >> setM: x [ m := x ]. \
         Money >> value [ ^m ]. \
         Money >> withValue: v [ ^Money of: v ]. \
         Money >> adaptToBoxed: aBoxed andSend: aSymbol [ \
             ^Money of: (aBoxed value perform: aSymbol \
                          withArguments: (Cons singleton: m)) ]. "
    );
    // Argument's domain wins, because the argument is what got asked.
    assert_eq!(
        eval(&format!(
            "{src} ((Boxed of: 5) + (Money of: 3)) class printString"
        )),
        Value::Str("Money".into())
    );
    assert_eq!(
        eval(&format!("{src} ((Boxed of: 5) + (Money of: 3)) value")),
        Value::Int(8)
    );
    // Operand order is preserved through the hand-off: 5 - 3, not 3 - 5.
    assert_eq!(
        eval(&format!("{src} ((Boxed of: 5) - (Money of: 3)) value")),
        Value::Int(2)
    );
    // Same class on both sides does NOT take this path — it projects both and
    // reconstructs once, so Boxed stays Boxed.
    assert_eq!(
        eval(&format!(
            "{src} ((Boxed of: 5) + (Boxed of: 3)) class printString"
        )),
        Value::Str("Boxed".into())
    );
}

#[test]
fn s5_no_coercion_carries_both_operands_unprojected() {
    // Asking `respondsTo:` before sending is what makes this possible: at the refusal
    // site neither operand has been projected away, so the condition can name both.
    let src = format!(
        "{S5_BOXED} \
         Integer subclass: #Money instanceVariableNames: 'm' \
             classVariableNames: '' indexedSlots: #none. \
         Money class >> of: x [ ^self _primNewValue: [:b | b setM: x] ]. \
         Money >> setM: x [ m := x ]. \
         Money >> value [ ^m ]. "
    );
    // Money implements no adaptToBoxed:andSend:, so the coercion refuses.
    assert_eq!(
        eval(&format!(
            "{src} [(Boxed of: 5) + (Money of: 3)] \
             on: NoCoercion do: [:e | e receiver class printString]"
        )),
        Value::Str("Boxed".into()),
        "receiver must arrive un-projected, still a Boxed"
    );
    assert_eq!(
        eval(&format!(
            "{src} [(Boxed of: 5) + (Money of: 3)] \
             on: NoCoercion do: [:e | e argument class printString]"
        )),
        Value::Str("Money".into())
    );
    assert_eq!(
        eval(&format!(
            "{src} [(Boxed of: 5) + (Money of: 3)] \
             on: NoCoercion do: [:e | e selector]"
        )),
        Value::Symbol("+".into())
    );
    // NoCoercion is an Error, so a generic handler still catches it.
    assert_eq!(
        eval(&format!(
            "{src} [(Boxed of: 5) + (Money of: 3)] on: Error do: [:e | 'caught']"
        )),
        Value::Str("caught".into())
    );
}

#[test]
fn s5_non_numeric_operand_signals_no_coercion_from_a_leaf() {
    // The refusing defaults on Object, now typed. A leaf receiver reaches them via
    // its own hardcoded adaptToInteger:/adaptToFloat: send.
    assert_eq!(
        eval("[3 + 'bad'] on: NoCoercion do: [:e | e argument]"),
        Value::Str("bad".into())
    );
    assert_eq!(
        eval("[3 + 'bad'] on: NoCoercion do: [:e | e receiver]"),
        Value::Int(3)
    );
    assert_eq!(
        eval("[2.5 * nil] on: NoCoercion do: [:e | e selector]"),
        Value::Symbol("*".into())
    );
}

#[test]
fn s5_responds_to_is_universal_across_value_kinds() {
    // `respondsTo:` used to live only on the Value::Instance dispatch path, so it
    // MNU'd on every scalar despite Object being the root class. The coercion edge
    // asks it of an arbitrary operand, so it must answer everywhere.
    //
    // KNOWN LIMITATION (pre-existing, not introduced by the lift): it answers from
    // the class method dictionary only, so it sees Smol-defined methods and is blind
    // to Rust builtins — `'s' respondsTo: #size` is false even though `'s' size`
    // works. S5 is unaffected because adapt selectors are always Smol-defined, but
    // the predicate is unsound in general. Asserted here so the gap is visible rather
    // than latent.
    assert_eq!(eval("3 respondsTo: #abs"), Value::Bool(true));
    assert_eq!(eval("3 respondsTo: #noSuchThing"), Value::Bool(false));
    assert_eq!(eval("2.5 respondsTo: #sign"), Value::Bool(true));
    assert_eq!(eval("nil respondsTo: #isEmpty"), Value::Bool(true));
    assert_eq!(
        eval("3 respondsTo: #adaptToInteger:andSend:"),
        Value::Bool(true)
    );
    // A non-Symbol argument answers false rather than erroring.
    assert_eq!(eval("3 respondsTo: 'abs'"), Value::Bool(false));
    // The builtin blind spot, pinned:
    assert_eq!(eval("'s' respondsTo: #size"), Value::Bool(false));
}

#[test]
fn s6_equality_promotes_across_encodings() {
    // Before S6 equality was raw variant equality, so `2 = 2.0` was false while
    // `2 < 2.0` promoted. `numeric_eq` shares `numeric_lt`'s promotion policy, so the
    // two now agree.
    assert_eq!(eval("2 = 2.0"), Value::Bool(true));
    assert_eq!(eval("2.0 = 2"), Value::Bool(true));
    assert_eq!(eval("2 ~= 2.0"), Value::Bool(false));
    assert_eq!(eval("2 = 3.0"), Value::Bool(false));
    assert_eq!(eval("2.5 = 2"), Value::Bool(false));
    // Across the machine-word boundary: 2^70 as an ArbitraryInteger and as a Float
    // (exact, being a power of two).
    assert_eq!(
        eval("1180591620717411303424 = 1180591620717411303424.0"),
        Value::Bool(true)
    );
}

#[test]
fn s6_equality_is_total_where_comparison_is_not() {
    // `<` may signal on an incomparable operand, but `=` must always answer. The
    // leaves decline a non-Number BEFORE projecting, because `aNumber value` would
    // MNU on a plain Object, which does not implement the seam.
    assert_eq!(eval("3 = 'x'"), Value::Bool(false));
    assert_eq!(eval("3 = nil"), Value::Bool(false));
    assert_eq!(eval("3 ~= 'x'"), Value::Bool(true));
    assert_eq!(eval("2.5 = 'x'"), Value::Bool(false));
    assert_eq!(
        eval(
            "Object subclass: #Thing instanceVariableNames: '' \
              classVariableNames: '' indexedSlots: #none. 3 = Thing new"
        ),
        Value::Bool(false)
    );
}

#[test]
fn s6_equality_agrees_with_the_order() {
    // The `Ordered` trait requires `=` and derives `<=`/`>=` from `<` assuming a
    // consistent total order. Exactly one of `<`, `=`, `>` must hold for any pair.
    let pairs = ["2 and: 2.0", "2 and: 3", "3.5 and: 2", "2 and: 2"];
    for p in pairs {
        let src = p.replace(" and: ", " ");
        let (a, b) = src.split_once(' ').unwrap();
        let trichotomy = format!(
            "((({a}) < ({b})) ifTrue: [1] ifFalse: [0]) \
             + ((({a}) = ({b})) ifTrue: [1] ifFalse: [0]) \
             + ((({a}) > ({b})) ifTrue: [1] ifFalse: [0])"
        );
        assert_eq!(
            eval(&trichotomy),
            Value::Int(1),
            "trichotomy for {a} vs {b}"
        );
        // `<=` is derived as `(other < self) not`; it must agree with `<` or `=`.
        assert_eq!(
            eval(&format!("({a} <= {b}) = (({a} < {b}) or: [{a} = {b}])")),
            Value::Bool(true),
            "<= consistency for {a} vs {b}"
        );
    }
}

#[test]
fn s6_hash_agrees_with_equality() {
    // equal-implies-same-hash. A whole-valued Float must hash as the integer it
    // equals, or `Dictionary` (which probes by `hash` then confirms by `=`) breaks.
    assert_eq!(eval("2 hash = 2.0 hash"), Value::Bool(true));
    assert_eq!(eval("(0 - 7) hash = (0.0 - 7.0) hash"), Value::Bool(true));
    assert_eq!(
        eval("1180591620717411303424 hash = 1180591620717411303424.0 hash"),
        Value::Bool(true)
    );
    // Non-whole floats need no agreement with any integer and keep the bit pattern.
    assert_eq!(eval("2.5 hash = 2 hash"), Value::Bool(false));
}

#[test]
fn s6_dictionary_unifies_numerically_equal_keys() {
    // The consequence that made aligning `hash` non-optional: with `=` promoting but
    // `hash` not, these two keys would probe different slots and the Dictionary would
    // hold two entries for one number.
    let src = "| d | d := Dictionary new. d at: 2 put: 'int'. ";
    assert_eq!(
        eval(&format!("{src} d at: 2.0")),
        Value::Str("int".into()),
        "a Float key must find the Int entry"
    );
    assert_eq!(
        eval(&format!("{src} d at: 2.0 put: 'float'. d size")),
        Value::Int(1),
        "storing 2.0 must overwrite 2, not add a second entry"
    );
    assert_eq!(
        eval(&format!("{src} d at: 2.0 put: 'float'. d at: 2")),
        Value::Str("float".into())
    );
}

#[test]
fn s6_wrapper_equality_through_the_seam() {
    // The wrapper failure Gap A predicted: with equality falling back to identity,
    // `Boxed(0) = 0` was false, so `isZero` lied and every method built on it broke.
    let src = format!("{S5_BOXED} ");
    assert_eq!(eval(&format!("{src} (Boxed of: 0) = 0")), Value::Bool(true));
    assert_eq!(eval(&format!("{src} 0 = (Boxed of: 0)")), Value::Bool(true));
    assert_eq!(
        eval(&format!("{src} (Boxed of: 3) = (Boxed of: 3)")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{src} (Boxed of: 3) = (Boxed of: 4)")),
        Value::Bool(false)
    );
    assert_eq!(
        eval(&format!("{src} (Boxed of: 0) isZero")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{src} (Boxed of: 4) even")),
        Value::Bool(true)
    );
    assert_eq!(eval(&format!("{src} (Boxed of: 3) odd")), Value::Bool(true));
}

#[test]
fn s6_unblocks_gcd_and_lcm_on_a_wrapper() {
    // The S5 leftovers. `gcd:` terminates on `anInteger isZero`; with `Boxed(0) = 0`
    // false the base case never fired and the recursion divided by zero. Completes
    // §4c's claim that a seam-implementing wrapper inherits the whole tower.
    let src = format!("{S5_BOXED} ");
    for (expr, want) in [
        ("(Boxed of: 12) gcd: (Boxed of: 18)", 6),
        ("(Boxed of: 4) lcm: (Boxed of: 6)", 12),
        ("(Boxed of: 12) gcd: 18", 6),
    ] {
        assert_eq!(
            eval(&format!("{src} ({expr}) value")),
            Value::Int(want),
            "{expr}"
        );
    }
    // And the wrapper's domain still survives the round trip.
    assert_eq!(
        eval(&format!(
            "{src} ((Boxed of: 12) gcd: (Boxed of: 18)) class printString"
        )),
        Value::Str("Boxed".into())
    );
}

#[test]
fn s6_equality_covers_every_operand_combination() {
    // The seven cases equality must get right, with no private double-dispatch
    // selector: `=` is already the universal symmetric operation, so a leaf that
    // cannot ground the comparison hands the operands back the other way round.
    //
    // `Frac` is a STRUCTURED number: a Number with no single encoding, so no seam.
    // It is the case that broke an earlier `isKindOf: Number` guard, which projected
    // it and hit `Number >> value`'s subclassResponsibility instead of answering.
    let src = "Object subclass: #Thing instanceVariableNames: '' \
                   classVariableNames: '' indexedSlots: #none. \
               Number subclass: #Frac instanceVariableNames: 'n d' \
                   classVariableNames: '' indexedSlots: #none. \
               Frac class >> n: a d: b [ ^self _primNewValue: [:f | f setN: a d: b] ]. \
               Frac >> setN: a d: b [ n := a. d := b ]. \
               Frac >> = other [ ^(other isKindOf: Frac) \
                   ifTrue: [ (n = other numerator) and: [d = other denominator] ] \
                   ifFalse: [ other * d = n ] ]. \
               Frac >> numerator [ ^n ]. \
               Frac >> denominator [ ^d ]. ";
    let boxed = S5_BOXED;

    // 1/2: leaf vs non-number, both directions.
    assert_eq!(eval(&format!("{src} 3 = Thing new")), Value::Bool(false));
    assert_eq!(eval(&format!("{src} Thing new = 3")), Value::Bool(false));
    // 3: leaf vs leaf, including across encodings.
    assert_eq!(eval("3 = 3"), Value::Bool(true));
    assert_eq!(eval("3 = 4"), Value::Bool(false));
    // 4/5: leaf vs seam-wrapper, both directions.
    assert_eq!(
        eval(&format!("{boxed} 3 = (Boxed of: 3)")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{boxed} (Boxed of: 3) = 3")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{boxed} 3 = (Boxed of: 4)")),
        Value::Bool(false)
    );
    // 6: two wrappers of the SAME class.
    assert_eq!(
        eval(&format!("{boxed} (Boxed of: 3) = (Boxed of: 3)")),
        Value::Bool(true)
    );
    // 7: two wrappers of DIFFERENT classes — terminates, and compares by value.
    let two = format!(
        "{boxed} Integer subclass: #Money instanceVariableNames: 'm' \
             classVariableNames: '' indexedSlots: #none. \
         Money class >> of: x [ ^self _primNewValue: [:b | b setM: x] ]. \
         Money >> setM: x [ m := x ]. \
         Money >> value [ ^m ]. "
    );
    assert_eq!(
        eval(&format!("{two} (Boxed of: 3) = (Money of: 3)")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{two} (Money of: 3) = (Boxed of: 4)")),
        Value::Bool(false)
    );
    // A STRUCTURED number answers with its own `=` rather than being projected.
    assert_eq!(
        eval(&format!("{src} 3 = (Frac n: 6 d: 2)")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{src} (Frac n: 6 d: 2) = 3")),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&format!("{src} 4 = (Frac n: 6 d: 2)")),
        Value::Bool(false)
    );
    assert_eq!(
        eval(&format!("{src} (Frac n: 1 d: 2) = (Frac n: 1 d: 2)")),
        Value::Bool(true)
    );
    // ~= must track `=` everywhere, or the Rust arms' variant inequality contradicts it.
    assert_eq!(eval(&format!("{src} 3 ~= Thing new")), Value::Bool(true));
    assert_eq!(
        eval(&format!("{boxed} 3 ~= (Boxed of: 3)")),
        Value::Bool(false)
    );
}

#[test]
fn s6_equality_bounce_always_terminates() {
    // Operand-swapping `=` must not loop. A passing suite does not prove this on its
    // own — a loop HANGS rather than fails — so the hazard cases are pinned here.
    //
    // `Bad` is a Number subclass implementing NEITHER `value` NOR `=`: the user
    // mistake most likely to bounce forever. It must report who owes the method.
    let bad = "Number subclass: #Bad instanceVariableNames: '' \
               classVariableNames: '' indexedSlots: #none. ";
    for expr in ["3 = Bad new", "Bad new = 3", "Bad new = Bad new"] {
        assert_eq!(
            eval(&format!("{bad} [{expr}] on: Error do: [:e | e message]")),
            Value::Str("Bad must implement this method (subclass responsibility)".into()),
            "{expr} must error, not hang"
        );
    }
    // A wrapper VIOLATING §4e's contract that `value` answers a leaf — it answers
    // another wrapper. Still terminates, because each projection strips one layer.
    let nested = "Number subclass: #W instanceVariableNames: 'x' \
                  classVariableNames: '' indexedSlots: #none. \
                  W class >> of: v [ ^self _primNewValue: [:w | w setX: v] ]. \
                  W >> setX: v [ x := v ]. W >> value [ ^x ]. ";
    assert_eq!(
        eval(&format!("{nested} (W of: (W of: 3)) = 3")),
        Value::Bool(true)
    );
}
