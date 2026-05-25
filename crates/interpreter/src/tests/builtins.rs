use super::*;
use crate::test_helpers::{eval, eval_err};

// ── Float ────────────────────────────────────────────────────────────────

#[test]
fn float_arithmetic() {
    assert_eq!(eval("1.5 + 0.5"), Value::Float(2.0));
    assert_eq!(eval("3.0 - 1.0"), Value::Float(2.0));
    assert_eq!(eval("2.0 * 3.0"), Value::Float(6.0));
    assert_eq!(eval("6.0 / 2.0"), Value::Float(3.0));
    // Int coercion for rhs
    assert_eq!(eval("2.0 + 1"), Value::Float(3.0));
    assert_eq!(eval("2.0 * 3"), Value::Float(6.0));
}

#[test]
fn float_comparisons() {
    assert_eq!(eval("1.0 < 2.0"), Value::Bool(true));
    assert_eq!(eval("2.0 > 1.0"), Value::Bool(true));
    assert_eq!(eval("1.0 <= 1.0"), Value::Bool(true));
    assert_eq!(eval("2.0 >= 2.0"), Value::Bool(true));
    assert_eq!(eval("1.5 = 1.5"), Value::Bool(true));
    assert_eq!(eval("1.5 ~= 2.5"), Value::Bool(true));
}

#[test]
fn float_unary() {
    assert_eq!(eval("-3.7 negated"), Value::Float(3.7));
    assert_eq!(eval("-3.7 abs"), Value::Float(3.7));
    assert_eq!(eval("3.9 truncated"), Value::Int(3));
    assert_eq!(eval("3.5 rounded"), Value::Int(4));
    assert_eq!(eval("3.1 ceiling"), Value::Int(4));
    assert_eq!(eval("3.9 floor"), Value::Int(3));
    assert_eq!(eval("3.9 asInt"), Value::Int(3));
}

#[test]
fn float_print_string() {
    assert_eq!(eval("3.14 printString"), Value::Str(Arc::from("3.14")));
}

// ── LargeInt ─────────────────────────────────────────────────────────────

#[test]
fn largeint_arithmetic() {
    let big = "99999999999999999999";
    assert!(matches!(eval(&format!("{big} + 1")), Value::LargeInt(_)));
    assert!(matches!(eval(&format!("{big} - 1")), Value::LargeInt(_)));
    assert!(matches!(eval(&format!("{big} * 2")), Value::LargeInt(_)));
    // division that normalises back to Int
    assert_eq!(eval(&format!("{big} / {big}")), Value::Int(1));
}

#[test]
fn largeint_floor_ops() {
    let big = "99999999999999999999";
    assert!(matches!(eval(&format!("{big} // 3")), Value::LargeInt(_)));
    // modulo result may normalise to Int when the remainder fits in i64
    assert!(matches!(
        eval(&format!("{big} \\\\ 7")),
        Value::Int(_) | Value::LargeInt(_)
    ));
}

#[test]
fn largeint_comparisons() {
    let big = "99999999999999999999";
    assert_eq!(eval(&format!("{big} < ({big} + 1)")), Value::Bool(true));
    assert_eq!(eval(&format!("{big} > 1")), Value::Bool(true));
    assert_eq!(eval(&format!("{big} <= {big}")), Value::Bool(true));
    assert_eq!(eval(&format!("{big} >= {big}")), Value::Bool(true));
    assert_eq!(eval(&format!("{big} = {big}")), Value::Bool(true));
    assert_eq!(eval(&format!("{big} ~= 1")), Value::Bool(true));
}

#[test]
fn largeint_unary() {
    let big = "99999999999999999999";
    assert!(matches!(
        eval(&format!("{big} negated")),
        Value::LargeInt(_)
    ));
    assert!(matches!(eval(&format!("{big} abs")), Value::LargeInt(_)));
    assert!(matches!(eval(&format!("{big} printString")), Value::Str(_)));
    assert!(matches!(eval(&format!("{big} asFloat")), Value::Float(_)));
}

#[test]
fn largeint_division_by_zero() {
    let big = "99999999999999999999";
    assert!(matches!(
        eval_err(&format!("{big} / 0")),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err(&format!("{big} // 0")),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err(&format!("{big} \\\\ 0")),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Int extensions ───────────────────────────────────────────────────────

#[test]
fn int_negated_and_abs() {
    assert_eq!(eval("5 negated"), Value::Int(-5));
    assert_eq!(eval("-5 abs"), Value::Int(5));
    // abs of i64::MIN promotes to BigInt (no positive i64 representation)
    assert!(matches!(
        eval("-9223372036854775808 abs"),
        Value::LargeInt(_)
    ));
}

#[test]
fn int_max_min() {
    assert_eq!(eval("3 max: 5"), Value::Int(5));
    assert_eq!(eval("3 min: 5"), Value::Int(3));
}

#[test]
fn int_as_float() {
    assert_eq!(eval("3 asFloat"), Value::Float(3.0));
}

#[test]
fn int_floor_division_semantics() {
    assert_eq!(eval("7 // 2"), Value::Int(3));
    // Negative numerator floors toward negative infinity
    assert_eq!(eval("-7 // 2"), Value::Int(-4));
    assert_eq!(eval("-7 \\\\ 2"), Value::Int(1));
}

#[test]
fn int_to_by_do() {
    assert_eq!(
        eval("| s | s := 0. 1 to: 10 by: 3 do: [:i | s := s + i]. s"),
        Value::Int(22) // 1+4+7+10
    );
    // Descending
    assert_eq!(
        eval("| s | s := 0. 5 to: 1 by: -1 do: [:i | s := s + i]. s"),
        Value::Int(15)
    );
}

#[test]
fn int_to_by_do_zero_step_errors() {
    assert!(matches!(
        eval_err("1 to: 5 by: 0 do: [:i | i]"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn int_division_by_zero_errors() {
    assert!(matches!(
        eval_err("5 / 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("5 // 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("5 \\\\ 0"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Bool ─────────────────────────────────────────────────────────────────

#[test]
fn bool_if_false_branch() {
    assert_eq!(eval("false ifFalse: ['no']"), Value::Str(Arc::from("no")));
    assert_eq!(eval("true ifFalse: ['no']"), Value::Nil);
    assert_eq!(eval("true ifTrue: ['yes']"), Value::Str(Arc::from("yes")));
    assert_eq!(eval("false ifTrue: ['yes']"), Value::Nil);
}

#[test]
fn bool_if_false_if_true() {
    assert_eq!(
        eval("false ifFalse: ['no'] ifTrue: ['yes']"),
        Value::Str(Arc::from("no"))
    );
    assert_eq!(
        eval("true ifFalse: ['no'] ifTrue: ['yes']"),
        Value::Str(Arc::from("yes"))
    );
}

#[test]
fn bool_ampersand() {
    // `|` is Token::Pipe (reserved for local-var syntax), so only `&` can be tested here
    assert_eq!(eval("true & true"), Value::Bool(true));
    assert_eq!(eval("true & false"), Value::Bool(false));
}

#[test]
fn bool_and_colon_short_circuits() {
    assert_eq!(eval("false and: [true]"), Value::Bool(false));
    assert_eq!(eval("true and: [true]"), Value::Bool(true));
}

#[test]
fn bool_or_colon_short_circuits() {
    assert_eq!(eval("true or: [false]"), Value::Bool(true));
    assert_eq!(eval("false or: [false]"), Value::Bool(false));
}

#[test]
fn bool_not() {
    assert_eq!(eval("true not"), Value::Bool(false));
    assert_eq!(eval("false not"), Value::Bool(true));
}

#[test]
fn bool_eq() {
    assert_eq!(eval("true = true"), Value::Bool(true));
    assert_eq!(eval("true ~= false"), Value::Bool(true));
}

#[test]
fn bool_print_string() {
    assert_eq!(eval("true printString"), Value::Str(Arc::from("true")));
    assert_eq!(eval("false printString"), Value::Str(Arc::from("false")));
}

// ── Nil ──────────────────────────────────────────────────────────────────

#[test]
fn nil_if_nil() {
    assert_eq!(
        eval("nil ifNil: ['was nil']"),
        Value::Str(Arc::from("was nil"))
    );
    assert_eq!(eval("nil ifNotNil: ['not nil']"), Value::Nil);
}

#[test]
fn nil_if_nil_if_not_nil() {
    assert_eq!(
        eval("nil ifNil: ['nil'] ifNotNil: ['nope']"),
        Value::Str(Arc::from("nil"))
    );
    assert_eq!(
        eval("nil ifNotNil: ['nope'] ifNil: ['nil']"),
        Value::Str(Arc::from("nil"))
    );
}

#[test]
fn nil_eq() {
    assert_eq!(eval("nil = nil"), Value::Bool(true));
    assert_eq!(eval("nil = 1"), Value::Bool(false));
    assert_eq!(eval("nil ~= 1"), Value::Bool(true));
}

#[test]
fn nil_print_string() {
    assert_eq!(eval("nil printString"), Value::Str(Arc::from("nil")));
}

// ── String ───────────────────────────────────────────────────────────────

#[test]
fn string_size() {
    assert_eq!(eval("'hello' size"), Value::Int(5));
    assert_eq!(eval("'' size"), Value::Int(0));
}

#[test]
fn string_at() {
    assert_eq!(eval("'abc' at: 1"), Value::Str(Arc::from("a")));
    assert_eq!(eval("'abc' at: 3"), Value::Str(Arc::from("c")));
}

#[test]
fn string_eq() {
    assert_eq!(eval("'hi' = 'hi'"), Value::Bool(true));
    assert_eq!(eval("'hi' ~= 'bye'"), Value::Bool(true));
}

#[test]
fn string_print_string() {
    assert_eq!(eval("'abc' printString"), Value::Str(Arc::from("'abc'")));
}

#[test]
fn string_as_symbol() {
    assert_eq!(eval("'foo' asSymbol"), Value::Symbol(Arc::from("foo")));
}

#[test]
fn string_reversed() {
    assert_eq!(eval("'abc' reversed"), Value::Str(Arc::from("cba")));
}

#[test]
fn string_copy_from_to() {
    assert_eq!(
        eval("'hello' copyFrom: 2 to: 4"),
        Value::Str(Arc::from("ell"))
    );
}

#[test]
fn string_includes_sub_string() {
    assert_eq!(eval("'hello' includesSubString: 'ell'"), Value::Bool(true));
    assert_eq!(eval("'hello' includesSubString: 'xyz'"), Value::Bool(false));
}

// ── Symbol ───────────────────────────────────────────────────────────────

#[test]
fn symbol_eq() {
    assert_eq!(eval("#foo = #foo"), Value::Bool(true));
    assert_eq!(eval("#foo ~= #bar"), Value::Bool(true));
}

#[test]
fn symbol_print_string() {
    assert_eq!(eval("#hello printString"), Value::Str(Arc::from("#hello")));
}

#[test]
fn symbol_as_string_and_symbol() {
    assert_eq!(eval("#foo asString"), Value::Str(Arc::from("foo")));
    assert_eq!(eval("#foo asSymbol"), Value::Symbol(Arc::from("foo")));
}

// ── Block ─────────────────────────────────────────────────────────────────

#[test]
fn block_value_no_arg() {
    assert_eq!(eval("[42] value"), Value::Int(42));
}

#[test]
fn block_value_two_args() {
    assert_eq!(eval("[:a :b | a + b] value: 3 value: 4"), Value::Int(7));
}

#[test]
fn block_value_three_args() {
    assert_eq!(
        eval("[:a :b :c | a + b + c] value: 1 value: 2 value: 3"),
        Value::Int(6)
    );
}

#[test]
fn block_while_false() {
    assert_eq!(
        eval("| n | n := 0. [n >= 3] whileFalse: [n := n + 1]. n"),
        Value::Int(3)
    );
}

#[test]
fn block_while_true_no_body() {
    assert_eq!(
        eval("| n | n := 0. [n := n + 1. n < 3] whileTrue. n"),
        Value::Int(3)
    );
}

#[test]
fn block_while_false_no_body() {
    assert_eq!(
        eval("| n | n := 0. [n := n + 1. n >= 3] whileFalse. n"),
        Value::Int(3)
    );
}

#[test]
fn block_on_do_catches_error() {
    // `Error` class is not defined in the bootstrap; use `Object` (always in scope).
    // The condition class is ignored by `on:do:` — any value works.
    assert_eq!(
        eval("[1 unknownMsg] on: Object do: [:e | 'caught']"),
        Value::Str(Arc::from("caught"))
    );
}

#[test]
fn block_on_do_passes_through_on_success() {
    assert_eq!(eval("[42] on: Object do: [:e | 0]"), Value::Int(42));
}

// ── Object primitives ─────────────────────────────────────────────────────

#[test]
fn object_eq_identity() {
    let src = r#"
Object subclass: #Marker instanceVariableNames: ''.
| a |
a := Marker new.
a = a
"#;
    assert_eq!(eval(src), Value::Bool(true));
}

#[test]
fn object_not_eq_different_instances() {
    let src = r#"
Object subclass: #Dot instanceVariableNames: ''.
| a b |
a := Dot new.
b := Dot new.
a ~= b
"#;
    assert_eq!(eval(src), Value::Bool(true));
}

#[test]
fn object_class_message() {
    let src = r#"
Object subclass: #Cat instanceVariableNames: ''.
Cat new class name
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("Cat")));
}

#[test]
fn object_is_kind_of() {
    let src = r#"
Object subclass: #Vehicle instanceVariableNames: ''.
Vehicle subclass: #Car instanceVariableNames: ''.
| c |
c := Car new.
c isKindOf: Car
"#;
    assert_eq!(eval(src), Value::Bool(true));
}

#[test]
fn object_is_kind_of_superclass() {
    let src = r#"
Object subclass: #Shape instanceVariableNames: ''.
Shape subclass: #Circle instanceVariableNames: ''.
| c |
c := Circle new.
c isKindOf: Shape
"#;
    assert_eq!(eval(src), Value::Bool(true));
}

#[test]
fn object_responds_to() {
    let src = r#"
Object subclass: #Robot instanceVariableNames: ''.
Robot >> beep [ ^'beep' ]
| r |
r := Robot new.
r respondsTo: #beep
"#;
    assert_eq!(eval(src), Value::Bool(true));
}

#[test]
fn object_is_nil_and_not_nil() {
    let src = r#"
Object subclass: #Thing instanceVariableNames: ''.
| t |
t := Thing new.
t isNil
"#;
    assert_eq!(eval(src), Value::Bool(false));
    let src2 = r#"
Object subclass: #Thing2 instanceVariableNames: ''.
| t |
t := Thing2 new.
t notNil
"#;
    assert_eq!(eval(src2), Value::Bool(true));
}

// ── Universal yourself / isNil / notNil ──────────────────────────────────

#[test]
fn universal_is_nil_not_nil() {
    assert_eq!(eval("1.5 isNil"), Value::Bool(false));
    assert_eq!(eval("'hi' isNil"), Value::Bool(false));
    assert_eq!(eval("#sym isNil"), Value::Bool(false));
    assert_eq!(eval("true isNil"), Value::Bool(false));
    assert_eq!(eval("1 notNil"), Value::Bool(true));
    assert_eq!(eval("nil notNil"), Value::Bool(false));
}

#[test]
#[allow(clippy::approx_constant)]
fn universal_yourself_on_various() {
    assert_eq!(eval("3.14 yourself"), Value::Float(3.14));
    assert_eq!(eval("'hi' yourself"), Value::Str(Arc::from("hi")));
    assert_eq!(eval("nil yourself"), Value::Nil);
    assert_eq!(eval("true yourself"), Value::Bool(true));
}

// ── Namespace ─────────────────────────────────────────────────────────────

#[test]
fn namespace_name_message() {
    assert_eq!(eval("Kernel name"), Value::Symbol(Arc::from("Kernel")));
}

#[test]
fn namespace_print_string_message() {
    assert_eq!(
        eval("Kernel printString"),
        Value::Str(Arc::from("Namespace(Kernel)"))
    );
}

// ── Class-side ───────────────────────────────────────────────────────────

#[test]
fn class_name_and_print_string() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo name
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("Foo")));
    let src2 = r#"
Object subclass: #Bar instanceVariableNames: ''.
Bar printString
"#;
    assert_eq!(eval(src2), Value::Str(Arc::from("Bar")));
}

#[test]
fn class_new_with_indexed() {
    let src = r#"
Object subclass: #ByteBuf instanceVariableNames: '' indexedSlots: #byte.
ByteBuf new: 8
"#;
    let v = eval(src);
    assert!(matches!(v, Value::Object(_)));
}

// ── Error cases ───────────────────────────────────────────────────────────

#[test]
fn mnu_for_unknown_float_selector() {
    assert!(matches!(
        eval_err("1.5 unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_bool_selector() {
    assert!(matches!(
        eval_err("true unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_nil_selector() {
    assert!(matches!(
        eval_err("nil unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_string_selector() {
    assert!(matches!(
        eval_err("'hi' unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_symbol_selector() {
    assert!(matches!(
        eval_err("#foo unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_largeint_selector() {
    assert!(matches!(
        eval_err("99999999999999999999 unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

// ── _primAt: / _primAt:put: / _primSize / _primAsString / error: ──────────

#[test]
fn prim_at_pointer_reads() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
| a |
a := Arr new: 3.
a _primAt: 0
"#;
    assert_eq!(eval(src), Value::Nil);
}

#[test]
fn prim_at_put_and_at_roundtrip() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
| a |
a := Arr new: 3.
a _primAt: 1 put: 42.
a _primAt: 1
"#;
    assert_eq!(eval(src), Value::Int(42));
}

#[test]
fn prim_size_returns_length() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
(Arr new: 5) _primSize
"#;
    assert_eq!(eval(src), Value::Int(5));
}

#[test]
fn prim_at_byte_returns_int() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' indexedSlots: #byte.
| b |
b := BA new: 4.
b _primAt: 0
"#;
    assert_eq!(eval(src), Value::Int(0));
}

#[test]
fn prim_at_put_byte_stores_and_reads() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' indexedSlots: #byte.
| b |
b := BA new: 4.
b _primAt: 2 put: 200.
b _primAt: 2
"#;
    assert_eq!(eval(src), Value::Int(200));
}

#[test]
fn prim_as_string_returns_utf8() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' indexedSlots: #byte.
| b |
b := BA new: 5.
b _primAt: 0 put: 104.
b _primAt: 1 put: 101.
b _primAt: 2 put: 108.
b _primAt: 3 put: 108.
b _primAt: 4 put: 111.
b _primAsString
"#;
    assert_eq!(eval(src), Value::Str(Arc::from("hello")));
}

#[test]
fn prim_at_bounds_error() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
(Arr new: 3) _primAt: 3
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_negative_index_error() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
(Arr new: 3) _primAt: -1
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_put_byte_out_of_range_error() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' indexedSlots: #byte.
(BA new: 2) _primAt: 0 put: 256
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_no_indexed_storage_error() {
    let src = r#"
Value subclass: #Plain instanceVariableNames: ''.
Plain new _primAt: 0
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn error_colon_raises_signal() {
    let src = r#"
Object subclass: #Thing instanceVariableNames: ''.
Thing new error: 'boom'
"#;
    match eval_err(src) {
        crate::eval::EvalError::Signal(msg) => assert!(msg.contains("boom")),
        e => panic!("expected Signal, got {e:?}"),
    }
}

// ── String edge cases ─────────────────────────────────────────────────────

#[test]
fn string_at_out_of_bounds() {
    assert!(matches!(
        eval_err("'abc' at: 10"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Block whileTrue:/whileFalse: non-Bool errors ──────────────────────────

#[test]
fn block_while_true_colon_non_bool_condition_errors() {
    assert!(matches!(
        eval_err("[1] whileTrue: []"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn block_while_false_colon_non_bool_condition_errors() {
    assert!(matches!(
        eval_err("[1] whileFalse: []"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn block_while_true_no_arg_non_bool_errors() {
    assert!(matches!(
        eval_err("[1] whileTrue"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn block_while_false_no_arg_non_bool_errors() {
    assert!(matches!(
        eval_err("[1] whileFalse"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── _primAt:put: edge cases ───────────────────────────────────────────────

#[test]
fn prim_at_put_byte_storage_non_int_arg_errors() {
    let src = r#"
| ba |
ba := ByteArray new: 2.
ba _primAt: 0 put: 'x'
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_put_float64_storage_and_int_coerce() {
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
| a |
a := FA new: 2.
a _primAt: 0 put: 3.
a _primAt: 1 put: 1.5.
a _primAt: 0
"#;
    assert_eq!(eval(src), Value::Float(3.0));
}

#[test]
fn prim_at_put_float64_wrong_type_errors() {
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
| a |
a := FA new: 2.
a _primAt: 0 put: 'bad'
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_as_string_on_non_byte_storage_errors() {
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
| a |
a := FA new: 2.
a _primAsString
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Int op LargeInt promotion ─────────────────────────────────────────────

#[test]
fn int_arithmetic_with_largeint_arg() {
    let big = "99999999999999999999";
    // Int + LargeInt
    let v = eval(&format!("1 + {big}"));
    assert!(matches!(v, Value::LargeInt(_)));
    // Int - LargeInt
    let v = eval(&format!("1 - {big}"));
    assert!(matches!(v, Value::LargeInt(_)));
    // Int * LargeInt
    let v = eval(&format!("1 * {big}"));
    assert!(matches!(v, Value::LargeInt(_)));
    // Int / LargeInt (small / huge → 0 via BigInt)
    let v = eval(&format!("10 / {big}"));
    assert_eq!(v, Value::Int(0));
    // Int // LargeInt
    let v = eval(&format!("10 // {big}"));
    assert_eq!(v, Value::Int(0));
    // Int \\ LargeInt
    let v = eval(&format!("10 \\\\ {big}"));
    assert_eq!(v, Value::Int(10));
}

#[test]
fn int_mul_overflow_promotes_to_bigint() {
    // i64::MAX * 2 overflows i64; should yield a LargeInt.
    let v = eval("9223372036854775807 * 2");
    assert!(matches!(v, Value::LargeInt(_)));
}

#[test]
fn int_negated_min_promotes_to_bigint() {
    // i64::MIN has no positive i64 representation.
    let v = eval("-9223372036854775808 negated");
    assert!(matches!(v, Value::LargeInt(_)));
}

// ── Type errors in helper functions ───────────────────────────────────────

#[test]
fn as_int_type_error() {
    // as_int is called when an Int is expected but another type is given.
    assert!(matches!(
        eval_err("1 < 'hello'"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("1 > true"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("1 max: 'x'"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn as_block_type_error() {
    // as_block is called when a Block is expected but another type is given.
    assert!(matches!(
        eval_err("[true] whileTrue: 'notABlock'"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("[true] whileFalse: 42"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn bigint_from_value_type_error() {
    // bigint_from_value is called for LargeInt arithmetic; wrong type should TypeError.
    assert!(matches!(
        eval_err("99999999999999999999 + 'bad'"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Int arithmetic type errors ────────────────────────────────────────────

#[test]
fn int_arithmetic_type_errors() {
    for op in ["+", "-", "*", "/", "//", "\\\\"] {
        let src = format!("1 {op} 'bad'");
        assert!(
            matches!(eval_err(&src), crate::eval::EvalError::TypeError(_)),
            "op {op}: expected TypeError"
        );
    }
}

// ── Float type error ──────────────────────────────────────────────────────

#[test]
fn float_arithmetic_type_error() {
    assert!(matches!(
        eval_err("1.5 + 'bad'"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("1.5 < nil"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Bool type errors ──────────────────────────────────────────────────────

#[test]
fn bool_pipe_operator() {
    assert_eq!(eval("true | false"), Value::Bool(true));
    assert_eq!(eval("false | false"), Value::Bool(false));
    assert_eq!(eval("false | true"), Value::Bool(true));
}

#[test]
fn bool_pipe_type_error() {
    assert!(matches!(
        eval_err("true | 1"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn bool_ampersand_type_error() {
    assert!(matches!(
        eval_err("true & 1"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err("true & nil"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn bool_and_colon_non_bool_block_result_errors() {
    assert!(matches!(
        eval_err("true and: [1]"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn bool_or_colon_non_bool_block_result_errors() {
    assert!(matches!(
        eval_err("false or: [1]"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── String type errors ────────────────────────────────────────────────────

#[test]
fn string_concat_type_error() {
    assert!(matches!(
        eval_err("'hello' , 42"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn string_includes_type_error() {
    assert!(matches!(
        eval_err("'hello' includesSubString: 1"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Block on:do: pass-through for non-local return ────────────────────────

#[test]
fn block_on_do_passes_through_non_local_return() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo >> test [
    [^42] on: nil do: [:e | 'caught'].
]
Foo new test
"#;
    assert_eq!(eval(src), Value::Int(42));
}

#[test]
fn block_mnu() {
    assert!(matches!(
        eval_err("[] unknownBlockMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

// ── Object primitive paths ────────────────────────────────────────────────

#[test]
fn object_is_kind_of_returns_false_for_unrelated_class() {
    let src = r#"
Object subclass: #Dog instanceVariableNames: ''.
Object subclass: #Cat instanceVariableNames: ''.
Dog new isKindOf: Cat
"#;
    assert_eq!(eval(src), Value::Bool(false));
}

#[test]
fn object_is_kind_of_non_class_arg_returns_false() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new isKindOf: 'notAClass'
"#;
    assert_eq!(eval(src), Value::Bool(false));
}

#[test]
fn object_responds_to_non_symbol_arg_returns_false() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new respondsTo: 42
"#;
    assert_eq!(eval(src), Value::Bool(false));
}

#[test]
fn object_mnu() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new completelyUnknownMessage
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn object_error_colon_non_string_arg() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new error: 42
"#;
    assert!(matches!(eval_err(src), crate::eval::EvalError::Signal(_)));
}

#[test]
fn class_new_indexed_via_plain_new() {
    // `new` (no size) on an indexed class creates a zero-size indexed object.
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
FA new _primSize
"#;
    assert_eq!(eval(src), Value::Int(0));
}

#[test]
fn class_new_without_indexed_slots_errors() {
    let src = r#"
Value subclass: #Plain instanceVariableNames: ''.
Plain new: 3
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn class_not_equal() {
    assert_eq!(eval("Array ~= ByteArray"), Value::Bool(true));
    assert_eq!(eval("Array ~= Array"), Value::Bool(false));
}

#[test]
fn class_mnu() {
    assert!(matches!(
        eval_err("Array unknownClassMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn namespace_do_non_block_arg_errors() {
    assert!(matches!(
        eval_err("Kernel do: 1"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn namespace_mnu() {
    assert!(matches!(
        eval_err("Kernel unknownNsMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

// ── _primAt: bounds for Byte and Float64 storage ──────────────────────────

#[test]
fn prim_at_byte_storage_out_of_bounds() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' classVariableNames: '' indexedSlots: #byte.
| a |
a := BA new: 3.
a _primAt: 5
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_float64_storage_out_of_bounds() {
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
| a |
a := FA new: 3.
a _primAt: 5
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── _primAt:put: no storage, bounds errors ────────────────────────────────

#[test]
fn prim_at_put_no_indexed_storage_error() {
    let src = r#"
Value subclass: #Plain instanceVariableNames: ''.
Plain new _primAt: 0 put: 1
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_put_pointer_bounds_error() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
| a |
a := Arr new: 2.
a _primAt: 5 put: 42
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_put_byte_bounds_error() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' classVariableNames: '' indexedSlots: #byte.
| a |
a := BA new: 2.
a _primAt: 5 put: 100
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn prim_at_put_float64_bounds_error() {
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
| a |
a := FA new: 2.
a _primAt: 5 put: 1.0
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── _primSize and _primAsString edge cases ────────────────────────────────

#[test]
fn prim_size_float64_storage() {
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
| a |
a := FA new: 4.
a _primSize
"#;
    assert_eq!(eval(src), Value::Int(4));
}

#[test]
fn prim_as_string_no_indexed_storage_errors() {
    let src = r#"
Value subclass: #Plain instanceVariableNames: ''.
Plain new _primAsString
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── handle_subclass error paths ───────────────────────────────────────────

#[test]
fn subclass_bad_class_name_type_errors() {
    let src = r#"Object subclass: 'notASymbol' instanceVariableNames: ''."#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_bad_ivar_names_type_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: 42."#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_bad_class_var_names_type_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: '' classVariableNames: 42."#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_bad_indexed_slots_type_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: '' classVariableNames: '' indexedSlots: 'notASymbol'."#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_unknown_indexed_kind_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: '' classVariableNames: '' indexedSlots: #badKind."#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_ivar_overlap_with_superclass_errors() {
    let src = r#"
Object subclass: #A instanceVariableNames: 'x'.
A subclass: #B instanceVariableNames: 'x'.
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Universal ifNil: / ifNotNil: family ───────────────────────────────────

#[test]
fn if_nil_on_nil_runs_block() {
    assert_eq!(eval("nil ifNil: [42]"), Value::Int(42));
}

#[test]
fn if_nil_on_non_nil_returns_nil() {
    assert_eq!(eval("7 ifNil: [42]"), Value::Nil);
    assert_eq!(eval("'hi' ifNil: [42]"), Value::Nil);
    assert_eq!(eval("true ifNil: [42]"), Value::Nil);
    assert_eq!(eval("3.14 ifNil: [42]"), Value::Nil);
    assert_eq!(eval("#sym ifNil: [42]"), Value::Nil);
}

#[test]
fn if_not_nil_on_nil_returns_nil() {
    assert_eq!(eval("nil ifNotNil: [:v | v]"), Value::Nil);
}

#[test]
fn if_not_nil_on_non_nil_passes_receiver_to_block() {
    assert_eq!(eval("7 ifNotNil: [:v | v + 1]"), Value::Int(8));
    assert_eq!(eval("'hi' ifNotNil: [:v | v]"), Value::Str(Arc::from("hi")));
    assert_eq!(eval("true ifNotNil: [:v | v]"), Value::Bool(true));
}

#[test]
fn if_nil_if_not_nil_nil_receiver() {
    assert_eq!(
        eval("nil ifNil: [1] ifNotNil: [:v | v + 10]"),
        Value::Int(1)
    );
}

#[test]
fn if_nil_if_not_nil_non_nil_receiver() {
    assert_eq!(eval("5 ifNil: [1] ifNotNil: [:v | v + 10]"), Value::Int(15));
}

#[test]
fn if_not_nil_if_nil_nil_receiver() {
    assert_eq!(
        eval("nil ifNotNil: [:v | v + 10] ifNil: [99]"),
        Value::Int(99)
    );
}

#[test]
fn if_not_nil_if_nil_non_nil_receiver() {
    assert_eq!(
        eval("5 ifNotNil: [:v | v + 10] ifNil: [99]"),
        Value::Int(15)
    );
}

// ── Universal then: ───────────────────────────────────────────────────────

#[test]
fn then_on_non_future_runs_block_synchronously() {
    assert_eq!(eval("42 then: [99]"), Value::Int(99));
    assert_eq!(eval("'hi' then: [1]"), Value::Int(1));
    assert_eq!(eval("nil then: [true]"), Value::Bool(true));
}

// ── Universal error: / _primSignal: on primitive types ────────────────────

#[test]
fn error_colon_on_int_signals() {
    match eval_err("5 error: 'boom'") {
        crate::eval::EvalError::Signal(msg) => assert!(msg.contains("boom")),
        e => panic!("expected Signal, got {e:?}"),
    }
}

#[test]
fn error_colon_on_string_signals() {
    match eval_err("'hi' error: 'bad'") {
        crate::eval::EvalError::Signal(msg) => assert!(msg.contains("bad")),
        e => panic!("expected Signal, got {e:?}"),
    }
}

#[test]
fn error_colon_on_bool_signals() {
    match eval_err("true error: 'oops'") {
        crate::eval::EvalError::Signal(msg) => assert!(msg.contains("oops")),
        e => panic!("expected Signal, got {e:?}"),
    }
}

#[test]
fn prim_signal_on_nil_signals() {
    match eval_err("nil _primSignal: 'raw'") {
        crate::eval::EvalError::Signal(msg) => assert!(msg.contains("raw")),
        e => panic!("expected Signal, got {e:?}"),
    }
}

// ── _primNew / _primNew: ──────────────────────────────────────────────────────

#[test]
fn prim_new_value_class_returns_value_object() {
    let src = r#"
Value subclass: #MyVal instanceVariableNames: ''.
MyVal _primNew
"#;
    assert!(matches!(eval(src), Value::ValueObject(_)));
}

#[test]
fn prim_new_value_class_no_initialize() {
    let src = r#"
Value subclass: #MyVal instanceVariableNames: 'flag'.
MyVal >> initialize [ flag := true ]
MyVal >> flag [ ^flag ]
| v |
v := MyVal _primNew.
v flag
"#;
    // initialize was not called, so flag is still nil
    assert_eq!(eval(src), Value::Nil);
}

#[test]
fn prim_new_colon_value_class_correct_size() {
    let src = r#"
Value subclass: #MyBuf instanceVariableNames: '' indexedSlots: #byte.
| v |
v := MyBuf _primNew: 7.
v _primSize
"#;
    assert_eq!(eval(src), Value::Int(7));
}

#[test]
fn prim_new_actor_class_returns_entity_handle_running() {
    let src = r#"
Object subclass: #MyActor instanceVariableNames: ''.
MyActor _primNew
"#;
    let v = eval(src);
    match &v {
        Value::Object(handle) => {
            let mb = handle.0.mailbox.lock().unwrap();
            assert!(mb.is_empty(), "mailbox should be empty after _primNew");
            drop(mb);
            let status = handle.0.status.lock().unwrap();
            assert!(
                matches!(*status, crate::entity::EntityStatus::Running),
                "status should be Running immediately after _primNew"
            );
        }
        other => panic!("expected Value::Object, got {other:?}"),
    }
}

#[test]
fn prim_new_actor_class_no_initialize() {
    let src = r#"
Object subclass: #MyActor instanceVariableNames: 'flag'.
MyActor >> initialize [ flag := true ]
MyActor >> flag [ ^flag ]
| a |
a := MyActor _primNew.
a flag
"#;
    // initialize was not called, so flag is still nil
    assert_eq!(eval(src), Value::Nil);
}

#[test]
fn prim_new_colon_requires_indexed_slots() {
    let src = r#"
Object subclass: #Plain instanceVariableNames: ''.
Plain _primNew: 4
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── _prim* self-check ─────────────────────────────────────────────────────────

#[test]
fn prim_self_check_rejects_non_self_receiver() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Object subclass: #Bar instanceVariableNames: ''.
Bar >> bad: other [
    other _primNew
]
"#;
    assert!(matches!(
        eval_err(src),
        crate::eval::EvalError::TypeError(msg) if msg.contains("_prim*")
    ));
}

#[test]
fn prim_self_check_allows_self_receiver() {
    // A class-side _primNew sent to self should install without error.
    let src = r#"
Object subclass: #Baz instanceVariableNames: ''.
Baz class >> create [
    ^self _primNew
]
"#;
    // If this eval returns without error the method installed successfully.
    assert_eq!(eval(src), Value::Nil);
}
