use super::*;
use crate::test_helpers::{assert_signal_contains, eval, eval_diff, eval_err, eval_err_diff};

// ── Float ────────────────────────────────────────────────────────────────

#[test]
fn float_arithmetic() {
    assert_eq!(eval_diff("1.5 + 0.5"), Value::Float(2.0));
    assert_eq!(eval_diff("3.0 - 1.0"), Value::Float(2.0));
    assert_eq!(eval_diff("2.0 * 3.0"), Value::Float(6.0));
    assert_eq!(eval_diff("6.0 / 2.0"), Value::Float(3.0));
    // Int coercion for rhs
    assert_eq!(eval_diff("2.0 + 1"), Value::Float(3.0));
    assert_eq!(eval_diff("2.0 * 3"), Value::Float(6.0));
}

#[test]
fn float_comparisons() {
    assert_eq!(eval_diff("1.0 < 2.0"), Value::Bool(true));
    assert_eq!(eval_diff("2.0 > 1.0"), Value::Bool(true));
    assert_eq!(eval_diff("1.0 <= 1.0"), Value::Bool(true));
    assert_eq!(eval_diff("2.0 >= 2.0"), Value::Bool(true));
    assert_eq!(eval_diff("1.5 = 1.5"), Value::Bool(true));
    assert_eq!(eval_diff("1.5 ~= 2.5"), Value::Bool(true));
}

#[test]
fn float_unary() {
    assert_eq!(eval_diff("-3.7 negated"), Value::Float(3.7));
    assert_eq!(eval_diff("-3.7 abs"), Value::Float(3.7));
    assert_eq!(eval_diff("3.9 truncated"), Value::Int(3));
    assert_eq!(eval_diff("3.5 rounded"), Value::Int(4));
    assert_eq!(eval_diff("3.1 ceiling"), Value::Int(4));
    assert_eq!(eval_diff("3.9 floor"), Value::Int(3));
    assert_eq!(eval_diff("3.9 asInt"), Value::Int(3));
}

#[test]
fn float_print_string() {
    assert_eq!(eval_diff("3.14 printString"), Value::Str(Arc::from("3.14")));
}

// ── LargeInt ─────────────────────────────────────────────────────────────

#[test]
fn largeint_arithmetic() {
    let big = "99999999999999999999";
    assert!(matches!(
        eval_diff(&format!("{big} + 1")),
        Value::LargeInt(_)
    ));
    assert!(matches!(
        eval_diff(&format!("{big} - 1")),
        Value::LargeInt(_)
    ));
    assert!(matches!(
        eval_diff(&format!("{big} * 2")),
        Value::LargeInt(_)
    ));
    // division that normalises back to Int
    assert_eq!(eval_diff(&format!("{big} / {big}")), Value::Int(1));
}

#[test]
fn largeint_floor_ops() {
    let big = "99999999999999999999";
    assert!(matches!(
        eval_diff(&format!("{big} // 3")),
        Value::LargeInt(_)
    ));
    // modulo result may normalise to Int when the remainder fits in i64
    assert!(matches!(
        eval_diff(&format!("{big} \\\\ 7")),
        Value::Int(_) | Value::LargeInt(_)
    ));
}

#[test]
fn largeint_comparisons() {
    let big = "99999999999999999999";
    assert_eq!(
        eval_diff(&format!("{big} < ({big} + 1)")),
        Value::Bool(true)
    );
    assert_eq!(eval_diff(&format!("{big} > 1")), Value::Bool(true));
    assert_eq!(eval_diff(&format!("{big} <= {big}")), Value::Bool(true));
    assert_eq!(eval_diff(&format!("{big} >= {big}")), Value::Bool(true));
    assert_eq!(eval_diff(&format!("{big} = {big}")), Value::Bool(true));
    assert_eq!(eval_diff(&format!("{big} ~= 1")), Value::Bool(true));
}

#[test]
fn largeint_unary() {
    let big = "99999999999999999999";
    assert!(matches!(
        eval_diff(&format!("{big} negated")),
        Value::LargeInt(_)
    ));
    assert!(matches!(
        eval_diff(&format!("{big} abs")),
        Value::LargeInt(_)
    ));
    assert!(matches!(
        eval_diff(&format!("{big} printString")),
        Value::Str(_)
    ));
    assert!(matches!(
        eval_diff(&format!("{big} asFloat")),
        Value::Float(_)
    ));
}

#[test]
fn largeint_division_by_zero() {
    let big = "99999999999999999999";
    assert!(matches!(
        eval_err_diff(&format!("{big} / 0")),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff(&format!("{big} // 0")),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff(&format!("{big} \\\\ 0")),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Int extensions ───────────────────────────────────────────────────────

#[test]
fn int_negated_and_abs() {
    assert_eq!(eval_diff("5 negated"), Value::Int(-5));
    assert_eq!(eval_diff("-5 abs"), Value::Int(5));
    // abs of i64::MIN promotes to BigInt (no positive i64 representation)
    assert!(matches!(
        eval_diff("-9223372036854775808 abs"),
        Value::LargeInt(_)
    ));
}

#[test]
fn int_max_min() {
    assert_eq!(eval_diff("3 max: 5"), Value::Int(5));
    assert_eq!(eval_diff("3 min: 5"), Value::Int(3));
}

#[test]
fn int_as_float() {
    assert_eq!(eval_diff("3 asFloat"), Value::Float(3.0));
}

#[test]
fn int_floor_division_semantics() {
    assert_eq!(eval_diff("7 // 2"), Value::Int(3));
    // Negative numerator floors toward negative infinity
    assert_eq!(eval_diff("-7 // 2"), Value::Int(-4));
    assert_eq!(eval_diff("-7 \\\\ 2"), Value::Int(1));
}

#[test]
fn int_to_by_do() {
    // ascending: 1+4+7+10 = 22
    let src = "| oc | oc := OrderedCollection new. 1 to: 10 by: 3 do: [:i | oc add: i]. oc inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src), Value::Int(22));
    // descending: 5+4+3+2+1 = 15
    let src2 = "| oc | oc := OrderedCollection new. 5 to: 1 by: -1 do: [:i | oc add: i]. oc inject: 0 into: [:acc :e | acc + e]";
    assert_eq!(eval_diff(src2), Value::Int(15));
}

#[test]
fn int_to_by_do_zero_step_errors() {
    assert!(matches!(
        eval_err_diff("1 to: 5 by: 0 do: [:i | i]"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn int_division_by_zero_errors() {
    assert!(matches!(
        eval_err_diff("5 / 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff("5 // 0"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff("5 \\\\ 0"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Bool ─────────────────────────────────────────────────────────────────

#[test]
fn bool_if_false_branch() {
    assert_eq!(
        eval_diff("false ifFalse: ['no']"),
        Value::Str(Arc::from("no"))
    );
    assert_eq!(eval_diff("true ifFalse: ['no']"), Value::Nil);
    assert_eq!(
        eval_diff("true ifTrue: ['yes']"),
        Value::Str(Arc::from("yes"))
    );
    assert_eq!(eval_diff("false ifTrue: ['yes']"), Value::Nil);
}

#[test]
fn bool_if_false_if_true() {
    assert_eq!(
        eval_diff("false ifFalse: ['no'] ifTrue: ['yes']"),
        Value::Str(Arc::from("no"))
    );
    assert_eq!(
        eval_diff("true ifFalse: ['no'] ifTrue: ['yes']"),
        Value::Str(Arc::from("yes"))
    );
}

#[test]
fn bool_ampersand() {
    // `|` is Token::Pipe (reserved for local-var syntax), so only `&` can be tested here
    assert_eq!(eval_diff("true & true"), Value::Bool(true));
    assert_eq!(eval_diff("true & false"), Value::Bool(false));
}

#[test]
fn bool_and_colon_short_circuits() {
    assert_eq!(eval_diff("false and: [true]"), Value::Bool(false));
    assert_eq!(eval_diff("true and: [true]"), Value::Bool(true));
}

#[test]
fn bool_or_colon_short_circuits() {
    assert_eq!(eval_diff("true or: [false]"), Value::Bool(true));
    assert_eq!(eval_diff("false or: [false]"), Value::Bool(false));
}

#[test]
fn bool_not() {
    assert_eq!(eval_diff("true not"), Value::Bool(false));
    assert_eq!(eval_diff("false not"), Value::Bool(true));
}

#[test]
fn bool_eq() {
    assert_eq!(eval_diff("true = true"), Value::Bool(true));
    assert_eq!(eval_diff("true ~= false"), Value::Bool(true));
}

#[test]
fn bool_print_string() {
    assert_eq!(eval_diff("true printString"), Value::Str(Arc::from("true")));
    assert_eq!(
        eval_diff("false printString"),
        Value::Str(Arc::from("false"))
    );
}

#[test]
fn bool_is_kind_of() {
    assert_eq!(eval("true isKindOf: True"), Value::Bool(true));
    assert_eq!(eval("true isKindOf: Boolean"), Value::Bool(true));
    assert_eq!(eval("true isKindOf: Value"), Value::Bool(true));
    assert_eq!(eval("true isKindOf: Object"), Value::Bool(true));
    assert_eq!(eval("true isKindOf: False"), Value::Bool(false));
    assert_eq!(eval("false isKindOf: False"), Value::Bool(true));
    assert_eq!(eval("false isKindOf: Boolean"), Value::Bool(true));
    assert_eq!(eval("false isKindOf: True"), Value::Bool(false));
}

// ── Nil ──────────────────────────────────────────────────────────────────

#[test]
fn nil_if_nil() {
    assert_eq!(
        eval_diff("nil ifNil: ['was nil']"),
        Value::Str(Arc::from("was nil"))
    );
    assert_eq!(eval_diff("nil ifNotNil: ['not nil']"), Value::Nil);
}

#[test]
fn nil_if_nil_if_not_nil() {
    assert_eq!(
        eval_diff("nil ifNil: ['nil'] ifNotNil: ['nope']"),
        Value::Str(Arc::from("nil"))
    );
    assert_eq!(
        eval_diff("nil ifNotNil: ['nope'] ifNil: ['nil']"),
        Value::Str(Arc::from("nil"))
    );
}

#[test]
fn nil_eq() {
    assert_eq!(eval_diff("nil = nil"), Value::Bool(true));
    assert_eq!(eval_diff("nil = 1"), Value::Bool(false));
    assert_eq!(eval_diff("nil ~= 1"), Value::Bool(true));
}

#[test]
fn nil_print_string() {
    assert_eq!(eval_diff("nil printString"), Value::Str(Arc::from("nil")));
}

// ── String ───────────────────────────────────────────────────────────────

#[test]
fn string_size() {
    assert_eq!(eval_diff("'hello' size"), Value::Int(5));
    assert_eq!(eval_diff("'' size"), Value::Int(0));
}

#[test]
fn string_at() {
    assert_eq!(eval_diff("'abc' at: 1"), Value::Str(Arc::from("a")));
    assert_eq!(eval_diff("'abc' at: 3"), Value::Str(Arc::from("c")));
}

#[test]
fn string_eq() {
    assert_eq!(eval_diff("'hi' = 'hi'"), Value::Bool(true));
    assert_eq!(eval_diff("'hi' ~= 'bye'"), Value::Bool(true));
}

#[test]
fn string_print_string() {
    assert_eq!(
        eval_diff("'abc' printString"),
        Value::Str(Arc::from("'abc'"))
    );
}

#[test]
fn string_as_symbol() {
    assert_eq!(eval_diff("'foo' asSymbol"), Value::Symbol(Arc::from("foo")));
}

#[test]
fn string_reversed() {
    assert_eq!(eval_diff("'abc' reversed"), Value::Str(Arc::from("cba")));
}

#[test]
fn string_copy_from_to() {
    assert_eq!(
        eval_diff("'hello' copyFrom: 2 to: 4"),
        Value::Str(Arc::from("ell"))
    );
}

#[test]
fn string_includes_sub_string() {
    assert_eq!(
        eval_diff("'hello' includesSubString: 'ell'"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_diff("'hello' includesSubString: 'xyz'"),
        Value::Bool(false)
    );
}

// ── Symbol ───────────────────────────────────────────────────────────────

#[test]
fn symbol_eq() {
    assert_eq!(eval_diff("#foo = #foo"), Value::Bool(true));
    assert_eq!(eval_diff("#foo ~= #bar"), Value::Bool(true));
}

#[test]
fn symbol_print_string() {
    assert_eq!(
        eval_diff("#hello printString"),
        Value::Str(Arc::from("#hello"))
    );
}

#[test]
fn symbol_as_string_and_symbol() {
    assert_eq!(eval_diff("#foo asString"), Value::Str(Arc::from("foo")));
    assert_eq!(eval_diff("#foo asSymbol"), Value::Symbol(Arc::from("foo")));
}

// ── Block ─────────────────────────────────────────────────────────────────

#[test]
fn block_value_no_arg() {
    assert_eq!(eval_diff("[42] value"), Value::Int(42));
}

#[test]
fn block_value_two_args() {
    assert_eq!(
        eval_diff("[:a :b | a + b] value: 3 value: 4"),
        Value::Int(7)
    );
}

#[test]
fn block_value_three_args() {
    assert_eq!(
        eval_diff("[:a :b :c | a + b + c] value: 1 value: 2 value: 3"),
        Value::Int(6)
    );
}

#[test]
fn block_while_false() {
    let src = "| oc | oc := OrderedCollection new. [oc size >= 3] whileFalse: [oc add: 1]. oc size";
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn block_while_true_no_body() {
    let src = "| oc | oc := OrderedCollection new. [oc add: 1. oc size < 3] whileTrue. oc size";
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn block_while_false_no_body() {
    let src = "| oc | oc := OrderedCollection new. [oc add: 1. oc size >= 3] whileFalse. oc size";
    assert_eq!(eval_diff(src), Value::Int(3));
}

#[test]
fn block_on_do_catches_error() {
    // on:do: catches EvalError::Signal whose condition class matches the catch class.
    assert_eq!(
        eval_diff("[Error signal: 'oops'] on: Error do: [:e | 'caught']"),
        Value::Str(Arc::from("caught"))
    );
}

#[test]
fn block_on_do_passes_through_on_success() {
    assert_eq!(eval_diff("[42] on: Object do: [:e | 0]"), Value::Int(42));
}

// ── Loop primitives (whileTrue:/whileFalse:/timesRepeat: via kernel.smol) ───
// Blocks cannot assign to outer locals, so state is tracked via OC message sends.

#[test]
fn while_true_counts_to_100() {
    let src = "| oc | oc := OrderedCollection new.
               [oc size < 100] whileTrue: [oc add: 1].
               oc size";
    assert_eq!(eval_diff(src), Value::Int(100));
}

#[test]
fn while_false_counts_to_5() {
    let src = "| oc | oc := OrderedCollection new.
               [oc size >= 5] whileFalse: [oc add: 1].
               oc size";
    assert_eq!(eval_diff(src), Value::Int(5));
}

#[test]
fn while_true_body_never_runs_when_condition_starts_false() {
    let src = "| oc | oc := OrderedCollection new.
               [false] whileTrue: [oc add: 1].
               oc size";
    assert_eq!(eval_diff(src), Value::Int(0));
}

#[test]
fn while_true_propagates_signal_from_body() {
    assert_signal_contains(
        &eval_err_diff("[true] whileTrue: [Error signal: 'stop']"),
        "stop",
    );
}

#[test]
fn times_repeat_five_times() {
    let src = "| oc | oc := OrderedCollection new.
               5 timesRepeat: [oc add: 1].
               oc size";
    assert_eq!(eval_diff(src), Value::Int(5));
}

#[test]
fn times_repeat_zero_never_runs() {
    let src = "| oc | oc := OrderedCollection new.
               0 timesRepeat: [oc add: 99].
               oc size";
    assert_eq!(eval_diff(src), Value::Int(0));
}

#[test]
fn times_repeat_negative_never_runs() {
    let src = "| oc | oc := OrderedCollection new.
               -1 timesRepeat: [oc add: 99].
               oc size";
    assert_eq!(eval_diff(src), Value::Int(0));
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
    assert_eq!(eval_diff(src), Value::Bool(true));
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
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn object_class_message() {
    let src = r#"
Object subclass: #Cat instanceVariableNames: ''.
Cat new class name
"#;
    assert_eq!(eval_diff(src), Value::Str(Arc::from("Cat")));
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
    assert_eq!(eval_diff(src), Value::Bool(true));
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
    assert_eq!(eval_diff(src), Value::Bool(true));
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
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn object_is_nil_and_not_nil() {
    let src = r#"
Object subclass: #Thing instanceVariableNames: ''.
| t |
t := Thing new.
t isNil
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
    let src2 = r#"
Object subclass: #Thing2 instanceVariableNames: ''.
| t |
t := Thing2 new.
t notNil
"#;
    assert_eq!(eval_diff(src2), Value::Bool(true));
}

// ── Universal yourself / isNil / notNil ──────────────────────────────────

#[test]
fn universal_is_nil_not_nil() {
    assert_eq!(eval_diff("1.5 isNil"), Value::Bool(false));
    assert_eq!(eval_diff("'hi' isNil"), Value::Bool(false));
    assert_eq!(eval_diff("#sym isNil"), Value::Bool(false));
    assert_eq!(eval_diff("true isNil"), Value::Bool(false));
    assert_eq!(eval_diff("1 notNil"), Value::Bool(true));
    assert_eq!(eval_diff("nil notNil"), Value::Bool(false));
}

#[test]
#[allow(clippy::approx_constant)]
fn universal_yourself_on_various() {
    assert_eq!(eval_diff("3.14 yourself"), Value::Float(3.14));
    assert_eq!(eval_diff("'hi' yourself"), Value::Str(Arc::from("hi")));
    assert_eq!(eval_diff("nil yourself"), Value::Nil);
    assert_eq!(eval_diff("true yourself"), Value::Bool(true));
}

// ── Namespace ─────────────────────────────────────────────────────────────

#[test]
fn namespace_name_message() {
    assert_eq!(eval_diff("Kernel name"), Value::Symbol(Arc::from("Kernel")));
}

#[test]
fn namespace_print_string_message() {
    assert_eq!(
        eval_diff("Kernel printString"),
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
    assert_eq!(eval_diff(src), Value::Str(Arc::from("Foo")));
    let src2 = r#"
Object subclass: #Bar instanceVariableNames: ''.
Bar printString
"#;
    assert_eq!(eval_diff(src2), Value::Str(Arc::from("Bar")));
}

#[test]
fn class_new_with_indexed() {
    let src = r#"
Object subclass: #ByteBuf instanceVariableNames: '' indexedSlots: #byte.
ByteBuf new: 8
"#;
    let v = eval_diff(src);
    use crate::entity::InstanceKind;
    assert!(matches!(v, Value::Instance(ref h) if matches!(h.0.kind, InstanceKind::Entity(_))));
}

// ── Error cases ───────────────────────────────────────────────────────────

#[test]
fn mnu_for_unknown_float_selector() {
    assert!(matches!(
        eval_err_diff("1.5 unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

// C33: Bool/Nil route through user-method lookup, so an unknown selector now
// reaches the default `Object>>doesNotUnderstand:`, which signals a
// MessageNotUnderstood condition (iterative path only; the recursive kernel has
// no DNU). Str/Symbol/Float/LargeInt still surface the raw error on both paths.
#[test]
fn mnu_for_unknown_bool_selector() {
    assert_signal_contains(&eval_err("true unknownMsg"), "does not understand");
}

#[test]
fn mnu_for_unknown_nil_selector() {
    assert_signal_contains(&eval_err("nil unknownMsg"), "does not understand");
}

#[test]
fn mnu_for_unknown_string_selector() {
    assert!(matches!(
        eval_err_diff("'hi' unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_symbol_selector() {
    assert!(matches!(
        eval_err_diff("#foo unknownMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn mnu_for_unknown_largeint_selector() {
    assert!(matches!(
        eval_err_diff("99999999999999999999 unknownMsg"),
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
    assert_eq!(eval_diff(src), Value::Nil);
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
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn prim_size_returns_length() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
(Arr new: 5) _primSize
"#;
    assert_eq!(eval_diff(src), Value::Int(5));
}

#[test]
fn prim_at_byte_returns_int() {
    let src = r#"
Value subclass: #BA instanceVariableNames: '' indexedSlots: #byte.
| b |
b := BA new: 4.
b _primAt: 0
"#;
    assert_eq!(eval_diff(src), Value::Int(0));
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
    assert_eq!(eval_diff(src), Value::Int(200));
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
    assert_eq!(eval_diff(src), Value::Str(Arc::from("hello")));
}

#[test]
fn prim_at_bounds_error() {
    let src = r#"
Value subclass: #Arr instanceVariableNames: '' indexedSlots: #pointer.
(Arr new: 3) _primAt: 3
"#;
    assert!(matches!(
        eval_err_diff(src),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn error_colon_raises_signal() {
    let src = r#"
Object subclass: #Thing instanceVariableNames: ''.
Thing new error: 'boom'
"#;
    assert_signal_contains(&eval_err_diff(src), "boom");
}

// ── String edge cases ─────────────────────────────────────────────────────

#[test]
fn string_at_out_of_bounds() {
    assert!(matches!(
        eval_err_diff("'abc' at: 10"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Block whileTrue:/whileFalse: non-Bool errors ──────────────────────────
// After C29.6, the loop primitives are implemented as pure Smol recursive methods.
// A non-Bool condition causes `self value ifFalse:` (or ifTrue:) to be sent to the
// non-Bool result, which produces MessageNotUnderstood rather than TypeError.
// This is the correct Smalltalk semantics: `1 ifFalse: [...]` is MNU.

// C33: `1 ifFalse:`/`1 ifTrue:` — Int routes through user-method lookup, so the
// non-Bool condition now signals MessageNotUnderstood via the default DNU
// (iterative path). See note above.
#[test]
fn block_while_true_colon_non_bool_condition_errors() {
    assert_signal_contains(&eval_err("[1] whileTrue: []"), "does not understand");
}

#[test]
fn block_while_false_colon_non_bool_condition_errors() {
    assert_signal_contains(&eval_err("[1] whileFalse: []"), "does not understand");
}

#[test]
fn block_while_true_no_arg_non_bool_errors() {
    assert_signal_contains(&eval_err("[1] whileTrue"), "does not understand");
}

#[test]
fn block_while_false_no_arg_non_bool_errors() {
    assert_signal_contains(&eval_err("[1] whileFalse"), "does not understand");
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
        eval_err_diff(src),
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
    assert_eq!(eval_diff(src), Value::Float(3.0));
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
        eval_err_diff(src),
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
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Int op LargeInt promotion ─────────────────────────────────────────────

#[test]
fn int_arithmetic_with_largeint_arg() {
    let big = "99999999999999999999";
    // Int + LargeInt
    let v = eval_diff(&format!("1 + {big}"));
    assert!(matches!(v, Value::LargeInt(_)));
    // Int - LargeInt
    let v = eval_diff(&format!("1 - {big}"));
    assert!(matches!(v, Value::LargeInt(_)));
    // Int * LargeInt
    let v = eval_diff(&format!("1 * {big}"));
    assert!(matches!(v, Value::LargeInt(_)));
    // Int / LargeInt (small / huge → 0 via BigInt)
    let v = eval_diff(&format!("10 / {big}"));
    assert_eq!(v, Value::Int(0));
    // Int // LargeInt
    let v = eval_diff(&format!("10 // {big}"));
    assert_eq!(v, Value::Int(0));
    // Int \\ LargeInt
    let v = eval_diff(&format!("10 \\\\ {big}"));
    assert_eq!(v, Value::Int(10));
}

#[test]
fn int_mul_overflow_promotes_to_bigint() {
    // i64::MAX * 2 overflows i64; should yield a LargeInt.
    let v = eval_diff("9223372036854775807 * 2");
    assert!(matches!(v, Value::LargeInt(_)));
}

#[test]
fn int_negated_min_promotes_to_bigint() {
    // i64::MIN has no positive i64 representation.
    let v = eval_diff("-9223372036854775808 negated");
    assert!(matches!(v, Value::LargeInt(_)));
}

// ── Type errors in helper functions ───────────────────────────────────────

#[test]
fn as_int_type_error() {
    // as_int is called when an Int is expected but another type is given.
    assert!(matches!(
        eval_err_diff("1 < 'hello'"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff("1 > true"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff("1 max: 'x'"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn as_block_type_error() {
    // After C29.6, loop primitives are Smol recursive methods. The body argument
    // is not type-checked eagerly; errors surface as MNU when `aBlock value` is sent
    // to a non-Block. The condition must evaluate to false so that `aBlock value` is
    // actually reached; otherwise the method exits before calling the body.
    // 'notABlock' is a Str, which does not route through user-method lookup, so
    // it still surfaces the raw MNU error on both paths.
    assert!(matches!(
        eval_err_diff("[true] whileTrue: 'notABlock'"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
    // Use [false] so the condition is false on the first check, meaning aBlock value
    // (Int 42) is reached. Int routes through DNU (C33), so this signals.
    assert_signal_contains(&eval_err("[false] whileFalse: 42"), "does not understand");
}

#[test]
fn bigint_from_value_type_error() {
    // bigint_from_value is called for LargeInt arithmetic; wrong type should TypeError.
    assert!(matches!(
        eval_err_diff("99999999999999999999 + 'bad'"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Int arithmetic type errors ────────────────────────────────────────────

#[test]
fn int_arithmetic_type_errors() {
    for op in ["+", "-", "*", "/", "//", "\\\\"] {
        let src = format!("1 {op} 'bad'");
        assert!(
            matches!(eval_err_diff(&src), crate::eval::EvalError::TypeError(_)),
            "op {op}: expected TypeError"
        );
    }
}

// ── Float type error ──────────────────────────────────────────────────────

#[test]
fn float_arithmetic_type_error() {
    assert!(matches!(
        eval_err_diff("1.5 + 'bad'"),
        crate::eval::EvalError::TypeError(_)
    ));
    assert!(matches!(
        eval_err_diff("1.5 < nil"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Bool type errors ──────────────────────────────────────────────────────

#[test]
fn bool_pipe_operator() {
    assert_eq!(eval_diff("true | false"), Value::Bool(true));
    assert_eq!(eval_diff("false | false"), Value::Bool(false));
    assert_eq!(eval_diff("false | true"), Value::Bool(true));
}

#[test]
fn bool_pipe_passthrough() {
    // | short-circuits on True (returns true, ignores arg) and passes through on False.
    // Dual of &: True & arg → arg (passthrough), False | arg → arg (passthrough).
    assert_eq!(eval_diff("true | 1"), Value::Bool(true));
    assert_eq!(eval_diff("false | 1"), Value::Int(1));
}

#[test]
fn bool_ampersand_passthrough() {
    // & passes the argument through for True and ignores it for False.
    assert_eq!(eval_diff("true & 1"), Value::Int(1));
    assert_eq!(eval_diff("true & nil"), Value::Nil);
    assert_eq!(eval_diff("false & 1"), Value::Bool(false));
}

#[test]
fn bool_and_colon_returns_block_result() {
    // and:/or: return whatever the block returns; DNU happens downstream if needed.
    assert_eq!(eval_diff("true and: [1]"), Value::Int(1));
    assert_eq!(eval_diff("true and: ['x']"), Value::Str(Arc::from("x")));
}

#[test]
fn bool_or_colon_returns_block_result() {
    assert_eq!(eval_diff("false or: [1]"), Value::Int(1));
    assert_eq!(eval_diff("false or: ['x']"), Value::Str(Arc::from("x")));
}

// ── String type errors ────────────────────────────────────────────────────

#[test]
fn string_concat_type_error() {
    assert!(matches!(
        eval_err_diff("'hello' , 42"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn string_includes_type_error() {
    assert!(matches!(
        eval_err_diff("'hello' includesSubString: 1"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Block on:do: pass-through for non-local return ────────────────────────

#[test]
fn block_on_do_passes_through_non_local_return() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo >> test [
    [^42] on: Error do: [:e | 'caught'].
]
Foo new test
"#;
    assert_eq!(eval_diff(src), Value::Int(42));
}

#[test]
fn block_mnu() {
    // Block routes through user-method lookup, so an unknown selector signals via
    // the default DNU (C33, iterative path).
    assert_signal_contains(&eval_err("[] unknownBlockMsg"), "does not understand");
}

// ── Object primitive paths ────────────────────────────────────────────────

#[test]
fn object_is_kind_of_returns_false_for_unrelated_class() {
    let src = r#"
Object subclass: #Dog instanceVariableNames: ''.
Object subclass: #Cat instanceVariableNames: ''.
Dog new isKindOf: Cat
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn object_is_kind_of_non_class_arg_returns_false() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new isKindOf: 'notAClass'
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn object_responds_to_non_symbol_arg_returns_false() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new respondsTo: 42
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn object_mnu() {
    // An entity that does not understand a message routes through the default
    // Object>>doesNotUnderstand:, which signals MessageNotUnderstood (C33).
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new completelyUnknownMessage
"#;
    assert_signal_contains(&eval_err(src), "does not understand");
}

#[test]
fn object_error_colon_non_string_arg() {
    let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new error: 42
"#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::Signal(_)
    ));
}

#[test]
fn class_new_indexed_via_plain_new() {
    // `new` (no size) on an indexed class creates a zero-size indexed object.
    let src = r#"
Value subclass: #FA instanceVariableNames: '' classVariableNames: '' indexedSlots: #float64.
FA new _primSize
"#;
    assert_eq!(eval_diff(src), Value::Int(0));
}

#[test]
fn class_new_without_indexed_slots_errors() {
    let src = r#"
Value subclass: #Plain instanceVariableNames: ''.
Plain new: 3
"#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn class_not_equal() {
    assert_eq!(eval_diff("Array ~= ByteArray"), Value::Bool(true));
    assert_eq!(eval_diff("Array ~= Array"), Value::Bool(false));
}

#[test]
fn class_mnu() {
    assert!(matches!(
        eval_err_diff("Array unknownClassMsg"),
        crate::eval::EvalError::MessageNotUnderstood { .. }
    ));
}

#[test]
fn namespace_do_non_block_arg_errors() {
    assert!(matches!(
        eval_err_diff("Kernel do: 1"),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn namespace_mnu() {
    assert!(matches!(
        eval_err_diff("Kernel unknownNsMsg"),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
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
        eval_err_diff(src),
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
    assert_eq!(eval_diff(src), Value::Int(4));
}

#[test]
fn prim_as_string_no_indexed_storage_errors() {
    let src = r#"
Value subclass: #Plain instanceVariableNames: ''.
Plain new _primAsString
"#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── handle_subclass error paths ───────────────────────────────────────────

#[test]
fn subclass_bad_class_name_type_errors() {
    let src = r#"Object subclass: 'notASymbol' instanceVariableNames: ''."#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_bad_ivar_names_type_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: 42."#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_bad_class_var_names_type_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: '' classVariableNames: 42."#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_bad_indexed_slots_type_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: '' classVariableNames: '' indexedSlots: 'notASymbol'."#;
    assert!(matches!(
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

#[test]
fn subclass_unknown_indexed_kind_errors() {
    let src = r#"Object subclass: #Foo instanceVariableNames: '' classVariableNames: '' indexedSlots: #badKind."#;
    assert!(matches!(
        eval_err_diff(src),
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
        eval_err_diff(src),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Universal ifNil: / ifNotNil: family ───────────────────────────────────

#[test]
fn if_nil_on_nil_runs_block() {
    assert_eq!(eval_diff("nil ifNil: [42]"), Value::Int(42));
}

#[test]
fn if_nil_on_non_nil_returns_nil() {
    assert_eq!(eval_diff("7 ifNil: [42]"), Value::Nil);
    assert_eq!(eval_diff("'hi' ifNil: [42]"), Value::Nil);
    assert_eq!(eval_diff("true ifNil: [42]"), Value::Nil);
    assert_eq!(eval_diff("3.14 ifNil: [42]"), Value::Nil);
    assert_eq!(eval_diff("#sym ifNil: [42]"), Value::Nil);
}

#[test]
fn if_not_nil_on_nil_returns_nil() {
    assert_eq!(eval_diff("nil ifNotNil: [:v | v]"), Value::Nil);
}

#[test]
fn if_not_nil_on_non_nil_passes_receiver_to_block() {
    assert_eq!(eval_diff("7 ifNotNil: [:v | v + 1]"), Value::Int(8));
    assert_eq!(
        eval_diff("'hi' ifNotNil: [:v | v]"),
        Value::Str(Arc::from("hi"))
    );
    assert_eq!(eval_diff("true ifNotNil: [:v | v]"), Value::Bool(true));
}

#[test]
fn if_nil_if_not_nil_nil_receiver() {
    assert_eq!(
        eval_diff("nil ifNil: [1] ifNotNil: [:v | v + 10]"),
        Value::Int(1)
    );
}

#[test]
fn if_nil_if_not_nil_non_nil_receiver() {
    assert_eq!(
        eval_diff("5 ifNil: [1] ifNotNil: [:v | v + 10]"),
        Value::Int(15)
    );
}

#[test]
fn if_not_nil_if_nil_nil_receiver() {
    assert_eq!(
        eval_diff("nil ifNotNil: [:v | v + 10] ifNil: [99]"),
        Value::Int(99)
    );
}

#[test]
fn if_not_nil_if_nil_non_nil_receiver() {
    assert_eq!(
        eval_diff("5 ifNotNil: [:v | v + 10] ifNil: [99]"),
        Value::Int(15)
    );
}

// ── Universal then: ───────────────────────────────────────────────────────

#[test]
fn then_on_non_future_runs_block_synchronously() {
    // then: blocks are 1-arg: they receive the resolved receiver value (iterative
    // path only — the recursive evaluator retains the legacy 0-arg then:).
    assert_eq!(eval("42 then: [:v | 99]"), Value::Int(99));
    assert_eq!(eval("'hi' then: [:v | 1]"), Value::Int(1));
    assert_eq!(eval("nil then: [:v | true]"), Value::Bool(true));
}

#[test]
fn then_passes_resolved_value_to_block() {
    // The resolved receiver value is delivered as the block argument.
    assert_eq!(eval("42 then: [:v | v + 1]"), Value::Int(43));
    assert_eq!(eval("[10 * 4] then: [:v | v + 2]"), Value::Int(42));
    // Future receiver: the resolved value flows into the block.
    assert_eq!(
        eval("| f | f := Future new. f resolveWith: 40. (f then: [:v | v + 2]) await"),
        Value::Int(42)
    );
}

#[test]
fn then_on_block_evaluates_receiver_then_continuation() {
    // Receiver block runs first, then continuation runs synchronously (result is not a Future).
    assert_eq!(eval("[1 + 1] then: [:v | 42]"), Value::Int(42));
    // Continuation can read values established before the receiver block ran.
    assert_eq!(
        eval(
            "| holder | holder := ValueHolder initialValue: 0. [holder set: 7] then: [:v | holder get]"
        ),
        Value::Int(7)
    );
}

#[test]
fn then_on_block_with_future_result_chains_async() {
    // Receiver block returns a Future (cross-entity send); continuation chains after it resolves.
    let src = r#"
| holder |
holder := ValueHolder initialValue: 0.
[holder set: 99] then: [:v | holder get]
"#;
    // holder set: returns a Future (cross-entity send to holder entity);
    // but since [holder set: 99] evaluates to the Future returned by set:,
    // the continuation [:v | holder get] chains after it. The resolved result is 99.
    assert_eq!(eval(src), Value::Int(99));
}

#[test]
fn then_on_block_loop_then_read() {
    // The canonical [loop] then: [read] pattern: loop queues updates, then read follows.
    let src = r#"
| holder |
holder := ValueHolder initialValue: 0.
[3 timesRepeat: [holder update: [:n | n + 1]]] then: [:v | holder get]
"#;
    assert_eq!(eval(src), Value::Int(3));
}

// ── then: on Future receiver ──────────────────────────────────────────────────

#[test]
fn then_on_future_receiver_chains_after_resolution() {
    // Cross-entity send returns a Future; then: on that Future chains the block
    // after the send completes. This exercises the Value::Future then: arm in
    // dispatch() — made reachable by passing then: through dispatch_to_stack
    // before force_value so the raw Future is seen.
    let src = r#"
| vh |
vh := ValueHolder initialValue: 0.
(vh set: 42) then: [:v | vh get]
"#;
    assert_eq!(eval(src), Value::Int(42));
}

// ── as_block type error ───────────────────────────────────────────────────────

#[test]
fn as_block_type_error_non_block_arg() {
    // Passing a non-Block where a Block is required hits the as_block Err path.
    // then: dispatches through dispatch() directly (before force_value), so the
    // _ arm calls as_block on the argument.
    assert!(matches!(
        eval_err("42 then: 99"),
        crate::eval::EvalError::TypeError(_)
    ));
}

// ── Nil user-defined method (UserMethodFound::Primitive frame path) ──────────

#[test]
fn nil_user_method_via_cons_inject_into() {
    // Cons inject:into: bottoms out at nil. The iterative evaluator routes Nil
    // user-defined methods through lookup_user_method -> push_method_call_frame
    // with UserMethodFound::Primitive, exercising the Primitive variant of
    // push_method_call_frame.
    let src = r#"
Value subclass: #Cons instanceVariableNames: 'head tail'.
Cons class >> head: h tail: t [
    ^self _primNewValue: [:c | c pvtHead: h tail: t]
]
Cons >> pvtHead: h tail: t [ head := h. tail := t ]
Cons >> inject: seed into: aBlock [
    ^tail inject: (aBlock value: seed value: head) into: aBlock
]
Nil >> inject: seed into: aBlock [ ^seed ]

| list |
list := Cons head: 3 tail: (Cons head: 2 tail: (Cons head: 1 tail: nil)).
list inject: 0 into: [:acc :e | acc + e]
"#;
    assert_eq!(eval(src), Value::Int(6));
}

// ── nil isEmpty ───────────────────────────────────────────────────────────────

#[test]
fn nil_is_empty() {
    assert_eq!(eval_diff("nil isEmpty"), Value::Bool(true));
}

// ── Int arithmetic with LargeInt args ────────────────────────────────────────

#[test]
fn int_div_largeint_arg() {
    // 100 / (large int) exercises the LargeInt branch of dispatch_int "/".
    assert_eq!(
        eval_diff("100 / (99999999999999999999 + 1 - 99999999999999999999)"),
        Value::Int(100)
    );
}

#[test]
fn int_floor_div_largeint_arg() {
    assert_eq!(
        eval_diff("100 // (99999999999999999999 + 1 - 99999999999999999999)"),
        Value::Int(100)
    );
}

#[test]
fn int_mod_largeint_arg() {
    assert_eq!(
        eval_diff("7 \\\\ (99999999999999999999 + 1 - 99999999999999999999)"),
        Value::Int(0)
    );
}

// ── Int <= and >= ─────────────────────────────────────────────────────────────

#[test]
fn int_le_and_ge() {
    assert_eq!(eval_diff("3 <= 3"), Value::Bool(true));
    assert_eq!(eval_diff("4 <= 3"), Value::Bool(false));
    assert_eq!(eval_diff("3 >= 3"), Value::Bool(true));
    assert_eq!(eval_diff("2 >= 3"), Value::Bool(false));
}

// ── Int / LargeInt asString ───────────────────────────────────────────────────

#[test]
fn int_as_string() {
    assert_eq!(eval_diff("42 asString"), Value::Str(Arc::from("42")));
}

#[test]
fn largeint_as_string() {
    assert_eq!(
        eval_diff("99999999999999999999 asString"),
        Value::Str(Arc::from("99999999999999999999"))
    );
}

// ── Universal error: on primitive types ──────────────────────────────────────

#[test]
fn error_colon_on_int_signals() {
    assert_signal_contains(&eval_err_diff("5 error: 'boom'"), "boom");
}

#[test]
fn error_colon_on_string_signals() {
    assert_signal_contains(&eval_err_diff("'hi' error: 'bad'"), "bad");
}

#[test]
fn error_colon_on_bool_signals() {
    assert_signal_contains(&eval_err_diff("true error: 'oops'"), "oops");
}

// ── _primNew / _primNew: ──────────────────────────────────────────────────────

#[test]
fn prim_new_value_class_returns_value_object() {
    let src = r#"
Value subclass: #MyVal instanceVariableNames: ''.
MyVal _primNew
"#;
    use crate::entity::InstanceKind;
    assert!(
        matches!(eval_diff(src), Value::Instance(ref h) if matches!(h.0.kind, InstanceKind::ValueObject(_)))
    );
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
    assert_eq!(eval_diff(src), Value::Nil);
}

#[test]
fn prim_new_colon_value_class_correct_size() {
    let src = r#"
Value subclass: #MyBuf instanceVariableNames: '' indexedSlots: #byte.
| v |
v := MyBuf _primNew: 7.
v _primSize
"#;
    assert_eq!(eval_diff(src), Value::Int(7));
}

#[test]
fn prim_new_actor_class_returns_entity_handle() {
    let src = r#"
Object subclass: #MyActor instanceVariableNames: ''.
MyActor _primNew
"#;
    let v = eval_diff(src);
    match &v {
        Value::Instance(handle) => {
            use crate::entity::InstanceKind;
            match &handle.0.kind {
                InstanceKind::Entity(data) => {
                    let mb = data.mailbox.lock().unwrap();
                    assert!(mb.is_empty(), "mailbox should be empty after _primNew");
                }
                other => panic!("expected InstanceKind::Entity, got {other:?}"),
            }
        }
        other => panic!("expected Value::Instance, got {other:?}"),
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
    assert_eq!(eval_diff(src), Value::Nil);
}

#[test]
fn prim_new_colon_requires_indexed_slots() {
    let src = r#"
Object subclass: #Plain instanceVariableNames: ''.
Plain _primNew: 4
"#;
    assert!(matches!(
        eval_err_diff(src),
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
        eval_err_diff(src),
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
    assert_eq!(eval_diff(src), Value::Nil);
}

// ── force_value audit: comparisons and signals with future args ───────────────

// Helper: an actor whose only method returns a known value — the reply is a
// future from the caller's perspective, exercising the force_value paths.

#[test]
fn class_neq_forces_future_arg() {
    // Class ~= should resolve a future arg before comparing.
    // Uses two distinct classes; myClass returns ClassA as a future from the actor.
    let src = r#"
Object subclass: #ClassA instanceVariableNames: ''.
Object subclass: #ClassB instanceVariableNames: ''.
ClassA >> myClass [ ^ClassA ]
| a |
a := ClassA new.
ClassB ~= a myClass
"#;
    assert_eq!(eval_diff(src), Value::Bool(true));
}

#[test]
fn entity_eq_forces_future_arg() {
    // Entity = should resolve a future arg (another entity reply) before comparing.
    let src = r#"
Object subclass: #Box instanceVariableNames: ''.
Box >> self [ ^self ]
| a b |
a := Box new.
b := Box new.
a = (b self)
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

#[test]
fn entity_neq_forces_future_arg() {
    let src = r#"
Object subclass: #Box2 instanceVariableNames: ''.
Box2 >> self [ ^self ]
| a |
a := Box2 new.
a ~= (a self)
"#;
    assert_eq!(eval_diff(src), Value::Bool(false));
}

// ── dispatch_primitive_fallback coverage ─────────────────────────────────────

#[test]
fn error_non_string_arg_on_primitive() {
    // Primitive receivers (Int, Float, etc.) use dispatch_primitive_fallback for error:.
    // Passing a non-String arg hits the `other =>` branch (formats the value).
    let err = eval_err_diff("42 error: 42");
    assert!(matches!(err, crate::eval::EvalError::Signal(_)));
}

// ── Int arithmetic overflow and exact division ───────────────────────────────

#[test]
fn int_subtraction_overflow_promotes_to_largeint() {
    // i64::MAX - (-1) overflows i64::MAX → normalize_bigint path.
    let result = eval_diff("9223372036854775807 - 1 negated");
    assert!(matches!(result, Value::LargeInt(_)));
}

#[test]
fn int_exact_division() {
    assert_eq!(eval_diff("10 / 2"), Value::Int(5));
}

// ── BigInt floor-division negative-remainder path ────────────────────────────

#[test]
fn bigint_floor_div_negative_sign_via_int() {
    // Int // LargeInt where signs differ → bigint_floor_div adjustment branch.
    // 5 // (-big) = floor(5 / -big) = -1
    assert_eq!(
        eval_diff("5 // 99999999999999999999 negated"),
        Value::Int(-1)
    );
}

#[test]
fn bigint_floor_rem_negative_sign_via_int() {
    // Int \\ LargeInt where signs differ → bigint_floor_rem adjustment branch.
    // 5 \\ (-big): r = 5 + (-big) = big negative
    let result = eval_diff("5 \\\\ 99999999999999999999 negated");
    assert!(matches!(result, Value::LargeInt(_)));
}

#[test]
fn bigint_floor_div_negative_via_largeint() {
    // LargeInt // Int where signs differ → bigint_floor_div adjustment branch.
    // (-big) // 7 = floor(-big / 7) which needs the q-1 adjustment.
    let result = eval_diff("99999999999999999999 negated // 7");
    assert!(matches!(result, Value::LargeInt(_)));
}

#[test]
fn bigint_floor_rem_negative_via_largeint() {
    // LargeInt \\ Int where signs differ → bigint_floor_rem adjustment branch.
    // (-big) \\ 7: mathematically should be 6 (positive, in range 0..7).
    assert_eq!(
        eval_diff("99999999999999999999 negated \\\\ 7"),
        Value::Int(6)
    );
}

// ── hash for non-Instance types ───────────────────────────────────────────────

#[test]
fn hash_false_is_zero() {
    assert_eq!(eval_diff("false hash"), Value::Int(0));
}

#[test]
fn hash_true_is_one() {
    assert_eq!(eval_diff("true hash"), Value::Int(1));
}

#[test]
fn hash_largeint_returns_int() {
    assert!(matches!(
        eval_diff("99999999999999999999 hash"),
        Value::Int(_)
    ));
}

#[test]
fn hash_block_returns_int() {
    assert_eq!(eval_diff("[1 + 1] hash isNil"), Value::Bool(false));
}

#[test]
fn hash_namespace_returns_int() {
    assert!(matches!(eval_diff("Kernel hash"), Value::Int(_)));
}

// ── _primSize on object without indexed storage ───────────────────────────────

#[test]
fn prim_size_no_indexed_storage() {
    // Value subclass has no indexed slots — _primSize should signal a TypeError.
    let err = eval_err_diff("Value subclass: #NoIdxV. NoIdxV new _primSize");
    assert!(matches!(err, crate::eval::EvalError::TypeError(_)));
}

// ── hash for additional primitive types ──────────────────────────────────────

#[test]
fn hash_float_returns_int() {
    assert!(matches!(eval_diff("3.14 hash"), Value::Int(_)));
}

#[test]
fn hash_nil_returns_zero() {
    assert_eq!(eval_diff("nil hash"), Value::Int(0));
}

#[test]
fn hash_negative_largeint_returns_int() {
    // Negative LargeInt triggers the wrapping_neg() branch in hash_value.
    assert!(matches!(
        eval_diff("99999999999999999999 negated hash"),
        Value::Int(_)
    ));
}

// ── subclass: with unknown keyword silently ignored ───────────────────────────

#[test]
fn subclass_unknown_keyword_silently_ignored() {
    // Unknown keyword pair in subclass: declaration is ignored; class is still defined.
    assert_eq!(
        eval_diff("Object subclass: #FooUnknown unknownKeyword: 42. FooUnknown printString"),
        Value::Str(Arc::from("FooUnknown"))
    );
}
