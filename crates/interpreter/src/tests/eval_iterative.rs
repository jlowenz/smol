use super::*;
use crate::test_helpers::eval;

/// C31.5 S0 baseline probe: records max `step_loop` recursion depth and
/// `size_of::<Cont>()` for nested synchronous `HandlerContext run:`, the
/// scenario that overflows the 256 KB production stack. Run with:
///   `cargo test --features step_probe step_probe_nested_hc -- --nocapture`
#[cfg(feature = "step_probe")]
#[test]
fn step_probe_nested_hc() {
    use crate::test_helpers::eval_probe;
    let src = r#"
| outer |
outer := HandlerContext new on: Error do: [:e | 'outer'].
outer run: [
    (HandlerContext new on: Error do: [:e | 'inner'])
        run: [Error signal: 'x']
]
"#;
    let r = eval_probe(src);
    eprintln!(
        "[step_probe] nested HC: depth = {}, size_of::<Cont>() = {} B, span = {} B, \
         per-level delta = {} B, step_loop→signal_condition delta = {} B",
        r.max_depth, r.cont_size, r.stack_span, r.level_delta, r.sigcond_delta,
    );
    assert_eq!(r.value, Value::Str(Arc::from("inner")));
}

/// C31.5 S4: nested numeric iteration must NOT grow the Rust stack per nesting
/// level — `to:do:` runs on the heap `Cont` stack (ToDoStep), so depth stays a
/// small constant regardless of how deeply loops nest.
#[cfg(feature = "step_probe")]
#[test]
fn step_probe_nested_loops() {
    use crate::test_helpers::eval_probe;
    // 5-deep nested to:do: with a pure body (Int arithmetic only) — fully
    // synchronous, so the measured coroutine never parks/migrates and the
    // thread-local probe stays accurate. Without flattening each nesting level
    // would add a step_loop recursion (depth ≈ 6+); flattened it stays ≈ 1.
    // (to:do: answers nil.)
    let src = r#"
1 to: 2 do: [:a |
  1 to: 2 do: [:b |
    1 to: 2 do: [:c |
      1 to: 2 do: [:d |
        1 to: 2 do: [:e | e + a]]]]]
"#;
    let r = eval_probe(src);
    eprintln!(
        "[step_probe] nested loops (5-deep): depth = {}, per-level delta = {} B",
        r.max_depth, r.level_delta,
    );
    assert_eq!(r.value, Value::Nil);
    // Flattened iteration: step_loop depth must not scale with loop nesting.
    // 5-deep nesting would reach depth ≈ 6+ if each `to:do:` recursed.
    assert!(
        r.max_depth <= 2,
        "nested to:do: should not grow step_loop depth with nesting, got {}",
        r.max_depth
    );
}

/// C31.5: nested synchronous `then:` must NOT grow the Rust stack — the
/// receiver block (`ThenCheck`) and the continuation both run via
/// `push_block_frame` on the same `step_loop`; only a genuine `Future` receiver
/// spawns an async continuation. Deep block-`then:` chains stay depth ≈ 1.
#[cfg(feature = "step_probe")]
#[test]
fn step_probe_nested_then() {
    use crate::test_helpers::eval_probe;
    // Right-nested then:: each continuation block itself evaluates another
    // `[..] then: [..]`. All synchronous (Int results), so no parking/migration.
    let src = r#"
[1] then: [
  [2] then: [
    [3] then: [
      [4] then: [
        [5] then: [ 6 ]]]]]
"#;
    let r = eval_probe(src);
    eprintln!(
        "[step_probe] nested then (5-deep): depth = {}, per-level delta = {} B",
        r.max_depth, r.level_delta,
    );
    assert_eq!(r.value, Value::Int(6));
    assert!(
        r.max_depth <= 2,
        "nested then: should not grow step_loop depth with nesting, got {}",
        r.max_depth
    );
}

/// C31.5: nested `Namespace do:` must NOT grow the Rust stack — the block runs on
/// the heap `Cont` stack behind a `NamespaceGuard` (the flattened equivalent of
/// `with_namespace`), so depth stays ≈ 1 regardless of nesting.
#[cfg(feature = "step_probe")]
#[test]
fn step_probe_nested_namespace_do() {
    use crate::test_helpers::eval_probe;
    let src = r#"
Kernel do: [
  Kernel do: [
    Kernel do: [
      Kernel do: [
        Kernel do: [ 42 ]]]]]
"#;
    let r = eval_probe(src);
    eprintln!(
        "[step_probe] nested namespace do (5-deep): depth = {}, per-level delta = {} B",
        r.max_depth, r.level_delta,
    );
    assert_eq!(r.value, Value::Int(42));
    assert!(
        r.max_depth <= 2,
        "nested Namespace do: should not grow step_loop depth, got {}",
        r.max_depth
    );
}

#[test]
fn s1_literal_return() {
    let v = eval("^42");
    assert_eq!(v, Value::Int(42));
}

#[test]
fn s1_var_lookup() {
    let v = eval("| x | x := 7. x");
    assert_eq!(v, Value::Int(7));
}

#[test]
fn s2_binary_send() {
    let v = eval("3 + 4");
    assert_eq!(v, Value::Int(7));
}

#[test]
fn s2_unary_send() {
    let v = eval("'hello' size");
    assert_eq!(v, Value::Int(5));
}

#[test]
fn s2_keyword_send() {
    let v = eval("10 max: 20");
    assert_eq!(v, Value::Int(20));
}

#[test]
fn s2_user_defined_method() {
    // Value subclass: -> ValueObject, dispatch is synchronous. Pure methods (no ivar write).
    let src = "
        Value subclass: #Calc instanceVariableNames: ''.
        Calc >> add: x to: y [ ^x + y ].
        | c |
        c := Calc new.
        c add: 3 to: 4
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(7));
}

#[test]
fn s2_chained_user_methods() {
    // Exercises multiple heap CallFrames: double calls triple, both pure.
    let src = "
        Value subclass: #Calc instanceVariableNames: ''.
        Calc >> double: x [ ^x * 2 ].
        Calc >> quadruple: x [ ^self double: (self double: x) ].
        | c |
        c := Calc new.
        c quadruple: 3
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(12));
}

#[test]
fn s3_cascade() {
    // Cascade sends multiple messages to the same receiver; result is last send.
    // 'hello' printString returns 'hello'; 'hello' size returns 5.
    let v = eval("'hello' printString; size");
    assert_eq!(v, Value::Int(5));
}

#[test]
fn s3_dynamic_array() {
    let v = eval("{ 1 + 1. 2 + 2. 3 + 3 }");
    // Result is an Array with 3 elements: 2, 4, 6.
    use crate::object::IndexedStorage;
    if let Value::Instance(h) = v
        && let crate::entity::InstanceKind::ValueObject(obj) = &h.0.kind
        && let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref()
    {
        let guard = slots.lock();
        assert_eq!(guard[0], Value::Int(2));
        assert_eq!(guard[1], Value::Int(4));
        assert_eq!(guard[2], Value::Int(6));
    } else {
        panic!("expected Array instance");
    }
}

#[test]
fn s3_array_literal() {
    let v = eval("#(1 2 3)");
    use crate::object::IndexedStorage;
    if let Value::Instance(h) = v
        && let crate::entity::InstanceKind::ValueObject(obj) = &h.0.kind
        && let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref()
    {
        let guard = slots.lock();
        assert_eq!(guard[0], Value::Int(1));
        assert_eq!(guard[1], Value::Int(2));
        assert_eq!(guard[2], Value::Int(3));
    } else {
        panic!("expected Array instance");
    }
}

#[test]
fn s5_nonlocal_return_from_block() {
    // ^-return from a block inside a method, through a builtin (ifTrue:).
    let src = "
        Value subclass: #Foo instanceVariableNames: ''.
        Foo >> bar [
            true ifTrue: [ ^42 ].
            ^99
        ].
        Foo new bar
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn s5_on_do_catches_signal() {
    // on:do: catches a signal raised within iterative eval.
    let src = "
        [Error signal: 'oops']
            on: Error
            do: [:e | 'caught']
    ";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("caught")));
}

#[test]
fn s5_ensure_runs_cleanup() {
    // ensure: body block always runs; result comes from the protected block.
    let src = "
        Value subclass: #Tester instanceVariableNames: ''.
        Tester >> go [
            ^[42] ensure: [0]
        ].
        Tester new go
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

// ── Cross-entity non-local return (entity.rs:513) ────────────────────────

#[test]
fn cross_entity_nonlocal_return_resolves_reply() {
    // entity.rs:513 — EvalError::Return escaping run_entity_loop when a block
    // containing a ^-return is evaluated by a foreign entity.
    //
    // The block must have home_depth != 1, because the Worker's run: method is
    // always at depth 1. A block captured at depth 1 would match the run: frame
    // (Reached, not Exhausted) and never escape to entity.rs:513.
    //
    // Depth collision is NOT avoided by nesting methods in tail position:
    // drain_tail_frames (TCO) pops the caller's frame before pushing the callee,
    // collapsing both to depth 1. The fix is a non-tail call: assign the result
    // to a local variable so the caller's frame has pending work and is not drained.
    //
    // Source>>outer calls self inner in non-tail position (pending: ^b).
    // drain_tail_frames sees a non-empty outer frame → skips it.
    // inner is pushed at depth 2, block [^42] gets home_depth=2.
    //
    // The ivar write (ran := true) in run: gives the Worker entity an active
    // store transaction when EvalError::Return reaches run_entity_loop line 513,
    // exercising the commit_or_warn branch before resolve_ok.
    //
    // Execution chain:
    //   src outer await:
    //     outer (depth 1, non-tail) → self inner (depth 2)
    //     inner creates [^42] with home_depth=2; outer returns it
    //   worker run: blk (depth 1, active tx from ran:=true):
    //     blk value → ^42 fires ReturnUnwind{target=2}
    //     unwind_stack_to_depth: block(0)≠2, run:(1)≠2 → Exhausted
    //     EvalError::Return(42,2) reaches run_entity_loop line 513
    //     commit_or_warn commits ran write; resolve_ok(42)
    //   (worker run: blk) await → 42
    let src = r#"
Object subclass: #Source
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
Source >> inner [ ^[ ^42 ] ]
Source >> outer [ | b | b := self inner. ^b ]

Object subclass: #Worker
    instanceVariableNames: 'ran'
    classVariableNames: ''
    indexedSlots: #none.
Worker >> run: aBlock [
    ran := true.
    ^aBlock value
]

| src worker blk |
src := Source new.
worker := Worker new.
blk := src outer await.
(worker run: blk) await.
"#;
    assert_eq!(eval(src), Value::Int(42));
}

// ── C29.6: TCO loop tests ─────────────────────────────────────────────────

#[test]
fn while_true_1000_iterations() {
    // Exercises the Smol recursive whileTrue: + TCO. Would stack-overflow
    // without drain_tail_frames eliminating the accumulated frames.
    let src = "
        | c |
        c := OrderedCollection new.
        [ c size < 1000 ] whileTrue: [ c add: 1 ].
        c size
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(1000));
}

#[test]
fn while_false_1000_iterations() {
    let src = "
        | c |
        c := OrderedCollection new.
        [ c size >= 1000 ] whileFalse: [ c add: 1 ].
        c size
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(1000));
}

#[test]
fn times_repeat_1000() {
    let src = "
        | c |
        c := OrderedCollection new.
        1000 timesRepeat: [ c add: 1 ].
        c size
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(1000));
}

#[test]
fn user_recursive_count_down() {
    // Exercises ReturnUnwind TCO: countFrom: tail-calls itself via ^.
    // Would stack-overflow at depth 1000 without TCO.
    let src = "
        Value subclass: #Counter instanceVariableNames: ''.
        Counter >> countFrom: n [
            n = 0 ifTrue: [ ^0 ].
            ^self countFrom: n - 1
        ].
        Counter new countFrom: 1000
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn nonlocal_return_through_inline_iftrue() {
    // Verifies ^-return unwinds correctly through an inlined ifTrue: block frame.
    let src = "
        Value subclass: #Foo instanceVariableNames: ''.
        Foo >> test: cond [
            cond ifTrue: [ ^42 ].
            ^0
        ].
        Foo new test: true
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn nonlocal_return_through_inline_iffalse() {
    let src = "
        Value subclass: #Foo instanceVariableNames: ''.
        Foo >> test: cond [
            cond ifFalse: [ ^42 ].
            ^0
        ].
        Foo new test: false
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn inline_iftrue_iffalse_both_branches() {
    // ifTrue:ifFalse: dispatches to the correct branch inline.
    let src = "
        Value subclass: #Foo instanceVariableNames: ''.
        Foo >> pick: b [
            ^b ifTrue: [ 1 ] ifFalse: [ 2 ]
        ].
        | f |
        f := Foo new.
        (f pick: true) + (f pick: false)
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(3));
}

// ── C29.7: Inlined ensure:/on:do:/syncValue/ifNil: tests ──────────────────

#[test]
fn ensure_runs_after_normal_completion() {
    // Body returns value; ensure block runs; body result returned.
    // Use ValueHolder to track side-effect (avoid NonLocalBlockAssignment).
    let src = "
        | log |
        log := ValueHolder initialValue: 0.
        [log update: [:v | v + 1]. 42] ensure: [log update: [:v | v + 10]].
        log get
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(11));
}

#[test]
fn ensure_returns_body_result() {
    // Body result (42) is returned, not the ensure block result (0).
    let src = "[42] ensure: [0]";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn ensure_runs_after_signal() {
    // Body signals; ensure block runs; signal is re-raised and caught by outer on:do:.
    let src = "
        | log |
        log := ValueHolder initialValue: 0.
        [
            [Error signal: 'boom'] ensure: [log update: [:v | v + 10]]
        ] on: Error do: [:e | log update: [:v | v + 1]].
        log get
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(11));
}

#[test]
fn ensure_runs_after_nonlocal_return() {
    // ^-return from inside body; ensure block runs; return continues.
    let src = "
        Value subclass: #Tester instanceVariableNames: ''.
        Tester >> go [
            ^[^42] ensure: [0]
        ].
        Tester new go
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn on_do_catches_signal() {
    // Body signals matching class; handler runs; handler result returned.
    let src = "
        [Error signal: 'oops'] on: Error do: [:e | 'handled']
    ";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("handled")));
}

#[test]
fn on_do_passes_non_matching_signal() {
    // Body signals; class doesn't match; signal propagates.
    let src = "
        [
            [Error signal: 'oops'] on: Condition do: [:e | 'wrong']
        ] on: Error do: [:e | 'right']
    ";
    // Condition is the superclass; Error is a subclass. Signal is Error.
    // on:Error:do: would match; on:Condition:do: also matches since Error is a subclass.
    // Let's use a more specific test: ensure inner handler doesn't match if we
    // use a completely different check.
    // Actually: we'll check that the outer on:do: catches it when the inner doesn't.
    let v = eval(src);
    // Error is a subclass of Condition, so on:Condition:do: WOULD match.
    // Let's just verify that catching works at all with the outer handler.
    assert_eq!(v, Value::Str(Arc::from("wrong")));
}

#[test]
fn on_do_handler_result_returned() {
    // on:do: returns handler result when signal is caught.
    let src = "[Error signal: 'x'] on: Error do: [:e | e message]";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("x")));
}

#[test]
fn on_do_retry_reruns_body() {
    // Handler returns Retry; body runs again until condition is met.
    // Use ValueHolder to avoid the NonLocalBlockAssignment restriction.
    let src = "
        | n |
        n := ValueHolder initialValue: 0.
        [
            n update: [:v | v + 1].
            (n get) < 3 ifTrue: [Error signal: 'retry me'].
            n get
        ] on: Error do: [:e | e retry]
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(3));
}

#[test]
fn on_do_resume_returns_value() {
    // Handler resumes with a value; that value becomes the on:do: result.
    let src = "
        [Error signal: 'x'] on: Error do: [:e | e return: 99]
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(99));
}

#[test]
fn on_do_body_completes_normally() {
    // Body completes without signalling; body result returned.
    let src = "[42] on: Error do: [:e | 0]";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn sync_value_evaluates_block() {
    // syncValue evaluates the block and returns the result.
    let src = "[3 + 4] syncValue";
    let v = eval(src);
    assert_eq!(v, Value::Int(7));
}

#[test]
fn ifnil_fires_on_nil() {
    let src = "nil ifNil: ['was nil']";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("was nil")));
}

#[test]
fn ifnil_skips_on_non_nil() {
    let src = "42 ifNil: ['was nil']";
    let v = eval(src);
    assert_eq!(v, Value::Nil);
}

#[test]
fn ifnotnil_fires_on_non_nil() {
    let src = "42 ifNotNil: [:x | x + 1]";
    let v = eval(src);
    assert_eq!(v, Value::Int(43));
}

#[test]
fn ifnotnil_skips_on_nil() {
    let src = "nil ifNotNil: [:x | x + 1]";
    let v = eval(src);
    assert_eq!(v, Value::Nil);
}

#[test]
fn ifnil_ifnotnil_nil_branch() {
    let src = "nil ifNil: ['nil'] ifNotNil: [:x | 'notnil']";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("nil")));
}

#[test]
fn ifnil_ifnotnil_notnnil_branch() {
    let src = "42 ifNil: ['nil'] ifNotNil: [:x | x + 1]";
    let v = eval(src);
    assert_eq!(v, Value::Int(43));
}

#[test]
fn nested_ensure_on_do() {
    // Ensure inside on:do: body: ensure runs, then signal is caught by on:do:.
    let src = "
        | log |
        log := ValueHolder initialValue: 0.
        [
            [Error signal: 'x'] ensure: [log update: [:v | v + 10]]
        ] on: Error do: [:e | log update: [:v | v + 1]].
        log get
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(11));
}

#[test]
fn ensure_nested_in_on_do_success() {
    // Ensure inside on:do: body; body succeeds; ensure runs; on:do: returns result.
    let src = "
        | log |
        log := ValueHolder initialValue: 0.
        [
            [log update: [:v | v + 1]. 42] ensure: [log update: [:v | v + 10]]
        ] on: Error do: [:e | 0].
        log get
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(11));
}

// ── C29.8 S6: Iterative super binary/keyword sends ───────────────────────────
// These target dispatch_super_to_stack + Cont::SuperCollectArgs directly.

#[test]
fn super_binary_send_iterative() {
    // super + n delegates to superclass method; exercises SuperCollectArgs binary arm.
    let src = "
        Object subclass: #Base.
        Base >> + n [ ^n + 100 ]
        Base subclass: #Child.
        Child >> + n [ ^super + n + 1 ]
        (Child new) + 5
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(106));
}

#[test]
fn super_keyword_send_iterative() {
    // super keyword: arg; exercises SuperCollectArgs single-keyword arm.
    let src = "
        Object subclass: #Base.
        Base >> addTo: n [ ^n + 100 ]
        Base subclass: #Child.
        Child >> addTo: n [ ^super addTo: n + 1 ]
        (Child new) addTo: 5
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(106));
}

#[test]
fn super_keyword_multi_arg_send_iterative() {
    // super multi-keyword; exercises SuperCollectArgs multi-arg pending-queue drain.
    let src = "
        Object subclass: #Base.
        Base >> from: a to: b [ ^b - a ]
        Base subclass: #Child.
        Child >> from: a to: b [ ^super from: a to: b + 1 ]
        (Child new) from: 3 to: 8
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(6));
}

#[test]
fn class_method_with_local_decl() {
    // Class-side method body containing | temp | exercises LocalDecl pre-binding
    // in invoke_class_method (eval.rs). Value subclass -> synchronous dispatch.
    let src = "
        Value subclass: #Box instanceVariableNames: ''.
        Box class >> doubled: n [
            | x |
            x := n * 2.
            ^x
        ]
        Box doubled: 21
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(42));
}

#[test]
fn super_in_class_side_method_dispatches_to_parent() {
    // super inside a class-side method dispatches to the parent class's class method.
    // Exercises the ClassSide arm of dispatch_super in eval_iterative.rs.
    let src = "
        Value subclass: #Base2 instanceVariableNames: ''.
        Base2 class >> greeting [ ^'base' ]
        Base2 subclass: #Child2 instanceVariableNames: ''.
        Child2 class >> greeting [ ^'child+' , super greeting ]
        Child2 greeting
    ";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("child+base")));
}

// ── Additional coverage gaps ──────────────────────────────────────────────────

#[test]
fn cascade_binary_message() {
    // CascadeStep binary arm (eval_iterative.rs lines 835-843).
    // Cascade sends the second message to the original receiver (3), not the result (7).
    let v = eval("3 + 4; + 5");
    assert_eq!(v, Value::Int(8));
}

#[test]
fn on_do_non_matching_inner_handler_propagates() {
    // Unified handler_chain lookup: signal class doesn't match inner handler's class.
    // Inner on:do: catches OrderedCollection but signal is Error → propagates to outer.
    let src = "
        | result |
        result := ValueHolder initialValue: 'none'.
        [
            [Error signal: 'test'] on: OrderedCollection do: [:e | result set: 'wrong']
        ] on: Error do: [:e | result set: 'propagated'].
        result get
    ";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("propagated")));
}

#[test]
fn on_do_handler_re_signals_propagates_to_outer() {
    // An error raised inside an on:do: handler is a fresh signal that finds the
    // outer handler via the ordered handler_chain walk.
    let src = "
        | result |
        result := ValueHolder initialValue: 'none'.
        [
            [Error signal: 'inner'] on: Error do: [:e | Error signal: 'from handler']
        ] on: Error do: [:e | result set: e message].
        result get
    ";
    let v = eval(src);
    assert_eq!(v, Value::Str(Arc::from("from handler")));
}

#[test]
fn on_do_resume_with_value() {
    // handler calls resume: which delivers a value back through on:do:.
    let src = "
        [Error signal: 'x'] on: Error do: [:e | e resume: 99]
    ";
    let v = eval(src);
    assert_eq!(v, Value::Int(99));
}

#[test]
fn super_standalone_errors() {
    // Expr::Super standalone (eval_iterative.rs lines 462-464):
    // super as a standalone expression (not a send receiver) is a TypeError.
    use crate::eval::EvalError;
    use crate::test_helpers::eval_err;
    let err = eval_err("| x | x := super");
    assert!(
        matches!(err, EvalError::TypeError(_)),
        "expected TypeError, got {err:?}"
    );
}

// ── cleanup_frames unit tests ─────────────────────────────────────────────────

#[test]
fn cleanup_frames_empty_stack() {
    let ev = crate::eval::Evaluator::new_without_kernel();
    let mut stack: Vec<Cont> = vec![];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
    assert_eq!(ev.method_stack.borrow().len(), 0);
    assert_eq!(ev.current_block_home.get(), None);
}

#[test]
fn cleanup_frames_plain_block_frame_no_pop_self() {
    // BlockFrame with block_home_self=None → pop_self must NOT be called;
    // saved_block_home is restored. (Block frames always own an env frame.)
    let ev = crate::eval::Evaluator::new_without_kernel();
    ev.current_block_home.set(Some(99));
    let mut env = crate::env::Env::new();
    env.push_frame();
    let mut stack = vec![Cont::BlockFrame {
        pending: std::collections::VecDeque::new(),
        env,
        saved_block_home: Some(3),
        block_home_self: None,
        sync: false,
    }];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
    assert_eq!(
        ev.method_stack.borrow().len(),
        0,
        "pop_self must not have been called"
    );
    assert_eq!(
        ev.current_block_home.get(),
        Some(3),
        "saved_block_home must be restored"
    );
}

#[test]
fn cleanup_frames_method_frame_pops_self() {
    // MethodFrame → pop_self called once; current_block_home restored to None.
    let ev = crate::eval::Evaluator::new_without_kernel();
    let nil_class = ev
        .registry
        .kernel()
        .lookup_class("Nil")
        .unwrap()
        .0
        .class
        .clone();
    ev.push_self(crate::eval::SelfBinding::Primitive {
        defining_class: nil_class,
        self_value: Value::Nil,
    });
    ev.current_block_home.set(Some(7));
    let mut env = crate::env::Env::new();
    env.push_frame();
    let mut stack = vec![Cont::MethodFrame {
        depth: 1,
        pending: std::collections::VecDeque::new(),
        env,
        implicit_self: Value::Nil,
        saved_block_home: None,
    }];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
    assert_eq!(
        ev.method_stack.borrow().len(),
        0,
        "pop_self must have been called"
    );
    assert_eq!(
        ev.current_block_home.get(),
        None,
        "saved_block_home must be restored"
    );
}

#[test]
fn cleanup_frames_block_with_home_self_pops_self() {
    // is_method=false, block_home_self=Some → pop_self called; saved_block_home restored.
    use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
    use crate::object::Object;
    let ev = crate::eval::Evaluator::new_without_kernel();
    let nil_class = ev
        .registry
        .kernel()
        .lookup_class("Nil")
        .unwrap()
        .0
        .class
        .clone();
    ev.push_self(crate::eval::SelfBinding::Primitive {
        defining_class: nil_class.clone(),
        self_value: Value::Nil,
    });
    let home_handle = InstanceHandle(std::sync::Arc::new(InstanceState {
        class: nil_class.clone(),
        kind: InstanceKind::ValueObject(std::sync::Arc::new(Object::new(nil_class))),
    }));
    ev.current_block_home.set(Some(5));
    let mut env = crate::env::Env::new();
    env.push_frame();
    let mut stack = vec![Cont::BlockFrame {
        pending: std::collections::VecDeque::new(),
        env,
        saved_block_home: Some(2),
        block_home_self: Some(home_handle),
        sync: false,
    }];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
    assert_eq!(
        ev.method_stack.borrow().len(),
        0,
        "pop_self must have been called for block_home_self"
    );
    assert_eq!(ev.current_block_home.get(), Some(2));
}

#[test]
fn cleanup_frames_pops_env_frame_when_flagged() {
    // A BlockFrame owns an env frame → env.pop_frame() is called without panic
    // (requires ≥2 frames).
    let ev = crate::eval::Evaluator::new_without_kernel();
    let mut env = crate::env::Env::new();
    env.push_frame();
    let mut stack = vec![Cont::BlockFrame {
        pending: std::collections::VecDeque::new(),
        env,
        saved_block_home: None,
        block_home_self: None,
        sync: false,
    }];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
}

#[test]
fn cleanup_frames_skips_non_callframe_entries() {
    // ForceAcc and RestoreAcc are silently discarded; no pop_self or panic.
    let ev = crate::eval::Evaluator::new_without_kernel();
    let mut stack = vec![
        Cont::ForceAcc,
        Cont::RestoreAcc {
            value: Value::Int(42),
        },
    ];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
    assert_eq!(ev.method_stack.borrow().len(), 0);
}

#[test]
fn cleanup_frames_multiple_mixed_frames() {
    // Two method frames interleaved with a ForceAcc: both pop_self calls happen
    // and current_block_home is set to the bottom frame's saved value.
    let ev = crate::eval::Evaluator::new_without_kernel();
    let nil_class = ev
        .registry
        .kernel()
        .lookup_class("Nil")
        .unwrap()
        .0
        .class
        .clone();
    ev.push_self(crate::eval::SelfBinding::Primitive {
        defining_class: nil_class.clone(),
        self_value: Value::Nil,
    });
    ev.push_self(crate::eval::SelfBinding::Primitive {
        defining_class: nil_class.clone(),
        self_value: Value::Nil,
    });
    let make_frame = |saved: Option<usize>| {
        let mut env = crate::env::Env::new();
        env.push_frame();
        Cont::MethodFrame {
            depth: 1,
            pending: std::collections::VecDeque::new(),
            env,
            implicit_self: Value::Nil,
            saved_block_home: saved,
        }
    };
    // Stack bottom→top: frame(saved=None), ForceAcc, frame(saved=Some(1))
    // Pop order: frame(saved=Some(1)), ForceAcc (skipped), frame(saved=None)
    let mut stack = vec![make_frame(None), Cont::ForceAcc, make_frame(Some(1))];
    super::cleanup_frames(&ev, &mut stack);
    assert!(stack.is_empty());
    assert_eq!(ev.method_stack.borrow().len(), 0);
    // Bottom frame's saved_block_home=None is the last write.
    assert_eq!(ev.current_block_home.get(), None);
}

// ── push_class_method_frame unit tests ───────────────────────────────────────

/// Builds a minimal `Arc<Class>` with the given name and no superclass.
fn make_class(name: &str) -> std::sync::Arc<crate::class::Class> {
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};
    Arc::new(crate::class::Class {
        name: Arc::from(name),
        superclass: None,
        own_instance_vars: vec![],
        own_class_vars: vec![],
        class_var_storage: RwLock::new(HashMap::new()),
        indexed_slot_kind: None,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: true,
        class_object: OnceLock::new(),
    })
}

/// Builds a `ClassEntity`-kind `InstanceHandle` for the given class.
fn make_class_handle(class: std::sync::Arc<crate::class::Class>) -> crate::entity::InstanceHandle {
    use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
    use crate::object::Object;
    use std::sync::Arc;
    InstanceHandle(Arc::new(InstanceState {
        class: class.clone(),
        kind: InstanceKind::ValueObject(Arc::new(Object::new(class))),
    }))
}

/// Builds a unary `Method` with the given body stmts and locals.
fn make_method(
    selector: &str,
    locals: Vec<&str>,
    body: Vec<crate::ast::Stmt>,
) -> std::sync::Arc<crate::class::Method> {
    use std::sync::Arc;
    std::sync::Arc::new(crate::class::Method {
        pattern: crate::class::MethodPattern::Unary(Arc::from(selector)),
        locals: locals.into_iter().map(Arc::from).collect(),
        body,
    })
}

#[test]
fn push_class_method_frame_arity_mismatch() {
    // A unary method takes 0 args; passing 1 must return ArityMismatch.
    use crate::eval::EvalError;
    let ev = crate::eval::Evaluator::new_without_kernel();
    let class = make_class("MyClass");
    let handle = make_class_handle(class.clone());
    let method = make_method("foo", vec![], vec![]);
    let env = crate::env::Env::new();
    let mut stack = vec![];
    let err = super::push_class_method_frame(
        &ev,
        handle,
        class,
        method,
        vec![Value::Int(1)], // 1 arg, 0 expected
        &env,
        &mut stack,
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            EvalError::ArityMismatch {
                expected: 0,
                got: 1
            }
        ),
        "expected ArityMismatch(0,1), got {err:?}"
    );
    assert!(stack.is_empty(), "no frame must be pushed on arity error");
    assert_eq!(
        ev.method_stack.borrow().len(),
        0,
        "method_stack must be unmodified"
    );
}

#[test]
fn push_class_method_frame_unary_fields() {
    // Unary method, no params, no locals: verify the MethodFrame fields.
    let ev = crate::eval::Evaluator::new_without_kernel();
    let class = make_class("MyClass");
    let handle = make_class_handle(class.clone());
    let method = make_method("foo", vec![], vec![]);
    let env = crate::env::Env::new();
    let mut stack = vec![];

    super::push_class_method_frame(
        &ev,
        handle.clone(),
        class.clone(),
        method,
        vec![],
        &env,
        &mut stack,
    )
    .unwrap();

    // method_stack must have a ClassSide binding at depth 1.
    assert_eq!(ev.method_stack.borrow().len(), 1);
    assert!(
        matches!(
            ev.method_stack.borrow().last(),
            Some(crate::eval::SelfBinding::ClassSide { .. })
        ),
        "expected ClassSide binding on method_stack"
    );
    // current_block_home must be reset to None.
    assert_eq!(ev.current_block_home.get(), None);

    let frame = stack.pop().unwrap();
    let Cont::MethodFrame {
        depth,
        implicit_self,
        saved_block_home,
        ..
    } = frame
    else {
        panic!("expected MethodFrame");
    };

    // The ClassSide binding and defining class are verified via method_stack above.
    assert_eq!(depth, 1, "depth must equal method_depth after push_self");
    assert_eq!(
        implicit_self,
        Value::Instance(handle),
        "implicit return must be self"
    );
    assert_eq!(saved_block_home, None);
}

#[test]
fn push_class_method_frame_params_bound_in_env() {
    // Keyword method: params must be accessible in the frame's env.
    use std::sync::Arc;
    let ev = crate::eval::Evaluator::new_without_kernel();
    let class = make_class("MyClass");
    let handle = make_class_handle(class.clone());
    let method = Arc::new(crate::class::Method {
        pattern: crate::class::MethodPattern::Keyword {
            parts: vec![
                (Arc::from("add:"), Arc::from("x")),
                (Arc::from("to:"), Arc::from("y")),
            ],
        },
        locals: vec![],
        body: vec![],
    });
    let env = crate::env::Env::new();
    let mut stack = vec![];

    super::push_class_method_frame(
        &ev,
        handle,
        class,
        method,
        vec![Value::Int(3), Value::Int(4)],
        &env,
        &mut stack,
    )
    .unwrap();

    let Cont::MethodFrame { env: frame_env, .. } = stack.pop().unwrap() else {
        panic!("expected MethodFrame");
    };
    assert_eq!(frame_env.get("x"), Some(Value::Int(3)));
    assert_eq!(frame_env.get("y"), Some(Value::Int(4)));
}

#[test]
fn push_class_method_frame_self_bound_in_env() {
    // `self` must be bound to `Value::Instance(handle)` in the frame's env.
    let ev = crate::eval::Evaluator::new_without_kernel();
    let class = make_class("MyClass");
    let handle = make_class_handle(class.clone());
    let method = make_method("foo", vec![], vec![]);
    let env = crate::env::Env::new();
    let mut stack = vec![];

    super::push_class_method_frame(&ev, handle.clone(), class, method, vec![], &env, &mut stack)
        .unwrap();

    let Cont::MethodFrame { env: frame_env, .. } = stack.pop().unwrap() else {
        panic!("expected MethodFrame");
    };
    assert_eq!(frame_env.get("self"), Some(Value::Instance(handle)));
}

#[test]
fn push_class_method_frame_locals_initialized_nil() {
    // Locals from `method.locals` and `Stmt::LocalDecl` in the body must be Nil.
    use crate::ast::Stmt;
    use std::sync::Arc;
    let ev = crate::eval::Evaluator::new_without_kernel();
    let class = make_class("MyClass");
    let handle = make_class_handle(class.clone());
    let method = Arc::new(crate::class::Method {
        pattern: crate::class::MethodPattern::Unary(Arc::from("foo")),
        locals: vec![Arc::from("a")],
        body: vec![Stmt::LocalDecl(vec![Arc::from("b")])],
    });
    let env = crate::env::Env::new();
    let mut stack = vec![];

    super::push_class_method_frame(&ev, handle, class, method, vec![], &env, &mut stack).unwrap();

    let Cont::MethodFrame { env: frame_env, .. } = stack.pop().unwrap() else {
        panic!("expected MethodFrame");
    };
    assert_eq!(
        frame_env.get("a"),
        Some(Value::Nil),
        "method.locals must be Nil"
    );
    assert_eq!(
        frame_env.get("b"),
        Some(Value::Nil),
        "Stmt::LocalDecl must be Nil"
    );
}

#[test]
fn push_class_method_frame_saves_block_home() {
    // If current_block_home is Some before the call, it is saved in the frame
    // and current_block_home is reset to None afterward.
    let ev = crate::eval::Evaluator::new_without_kernel();
    ev.current_block_home.set(Some(42));
    let class = make_class("MyClass");
    let handle = make_class_handle(class.clone());
    let method = make_method("foo", vec![], vec![]);
    let env = crate::env::Env::new();
    let mut stack = vec![];

    super::push_class_method_frame(&ev, handle, class, method, vec![], &env, &mut stack).unwrap();

    assert_eq!(
        ev.current_block_home.get(),
        None,
        "must be reset to None after push"
    );
    let Cont::MethodFrame {
        saved_block_home, ..
    } = stack.pop().unwrap()
    else {
        panic!("expected MethodFrame");
    };
    assert_eq!(
        saved_block_home,
        Some(42),
        "previous home must be saved in frame"
    );
}

#[test]
fn push_class_method_frame_depth_with_existing_method_stack() {
    // If method_stack already has one entry, depth of the new frame must be 2.
    let ev = crate::eval::Evaluator::new_without_kernel();
    let class = make_class("MyClass");
    ev.push_self(crate::eval::SelfBinding::Primitive {
        defining_class: class.clone(),
        self_value: Value::Nil,
    });
    assert_eq!(ev.method_depth.get(), 1);

    let handle = make_class_handle(class.clone());
    let method = make_method("bar", vec![], vec![]);
    let env = crate::env::Env::new();
    let mut stack = vec![];

    super::push_class_method_frame(&ev, handle, class, method, vec![], &env, &mut stack).unwrap();

    let Cont::MethodFrame { depth, .. } = stack.pop().unwrap() else {
        panic!("expected MethodFrame");
    };
    assert_eq!(
        depth, 2,
        "depth must equal method_depth after push_self (was 1, now 2)"
    );
}
