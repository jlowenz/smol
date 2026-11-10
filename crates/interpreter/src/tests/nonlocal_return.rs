//! Non-local return (`^` inside a block) across entity boundaries — issue I5.
//!
//! `^expr` is meant to return from the method that *lexically* encloses the
//! block, at any nesting depth (`docs/Smol_Patterns.md` Pattern 4). That holds
//! only when the block runs on its home method's own coroutine. When another
//! entity (or a later turn) runs the block, the current mechanism targets the
//! home by a *stack-depth integer* (`Block::home_depth`), not by the identity of
//! the home activation, so the return lands on the wrong frame or escapes as the
//! foreign receiver's reply. See `docs/issues.md` I5 and the runnable companion
//! `docs/experiments/i5_nonlocal_return.smol` (same E-labels as these tests).
//!
//! Test groups:
//!
//! - `green_*` — correct today; regression guards. Should stay passing.
//! - `red_*` — would FAIL on the current tree; `#[ignore]`d pending Q24. Each asserts
//!   the value that delivering the `^` back to its home across entities would
//!   produce. They are correct targets only if cross-entity `^` is supported. If
//!   cross-entity `^` is rejected instead, e1/e5 become programs that should be
//!   rejected (compile-time or run-time), and e11 is a condition-idiom question
//!   (see its note). If `^` in blocks is dropped altogether, all of these change.
//!   Re-label once decided (I5, Q24).
//! - ignored — semantics not yet decided (dead/stale home, fire-and-forget
//!   state). Marked `#[ignore]`; the body characterizes today's (wrong) answer so
//!   the gap stays visible and un-ignoring one documents reality until we agree
//!   on the real semantics.

use crate::test_helpers::eval;
use crate::value::Value;

/// Helper classes and the methods under test, prepended to every program.
/// Each test runs on a fresh `Evaluator::new()`, so these definitions are
/// re-declared per test in an isolated namespace (no cross-test collision).
const PRELUDE: &str = r#"
"Slow answers a value from its OWN entity, so (s seven) is a genuine Future."
Object subclass: #Slow instanceVariableNames: ''.
Slow >> seven [ ^7 ]

"Failer signals an Error on its own entity."
Object subclass: #Failer instanceVariableNames: ''.
Failer >> fail [ ^self error: 'failed' ]

"Counter records how many times a block body ran."
Object subclass: #Counter instanceVariableNames: 'n'.
Counter >> initialize [ n := 0 ]
Counter >> tick [ n := n + 1 ]
Counter >> n [ ^n ]

"Worker runs a block two method-calls deep; the +100/+1000 reveal which frames
 actually completed (used only by the dead-home experiments)."
Object subclass: #Worker instanceVariableNames: ''.
Worker >> run: aBlock [ | r | r := self helper: aBlock. ^r + 1000 ]
Worker >> helper: aBlock [ | r | r := aBlock value. ^r + 100 ]

Object subclass: #Home instanceVariableNames: ''.

"Block runs on Home's OWN coroutine via a self-send."
Home >> each: aBlock [ aBlock value: 1. aBlock value: -1. aBlock value: 7 ]
Home >> firstNegViaSelf [ self each: [:x | x < 0 ifTrue: [^x]]. ^nil ]

"Does NOT wait for do: to finish (fire-and-forget send)."
Home >> firstNeg: coll [ coll do: [:x | x < 0 ifTrue: [^x]]. ^nil ]

"Waits for do: to finish before reaching ^nil."
Home >> firstNegAwaited: coll [ (coll do: [:x | x < 0 ifTrue: [^x]]) await. ^nil ]

"Waits, and counts how many elements the block saw before ^ fired."
Home >> firstNegCounting: coll counter: c [
    (coll do: [:x | c tick. x < 0 ifTrue: [^x]]) await. ^nil ]

"then: block runs after the Future settles; method does NOT wait for it."
Home >> thenNotAwaited: s [ (s seven) then: [:v | ^v + 1] orElse: #reject. ^99 ]

"then: block, and the method waits for the chain."
Home >> thenAwaited: s [ ((s seven) then: [:v | ^v + 1] orElse: #reject) await. ^99 ]

"then: chain is the method's own answer."
Home >> thenAsAnswer: s [ ^(s seven) then: [:v | ^v + 1] orElse: #reject ]

"Handler for an error signaled on THIS same entity."
Home >> handlerLocal [ [self error: 'failed'. ^1] on: Error do: [:e | ^2]. ^3 ]

"Handler for an error signaled on ANOTHER entity (handler runs on that entity)."
Home >> handlerRemote: f [ [(f fail) await. ^1] on: Error do: [:e | ^2]. ^3 ]

"Answers a block whose home method (makeBlock) has already returned."
Home >> makeBlock [ ^[^42] ]
Home >> blockWithDeadHome [ | b | b := self makeBlock. ^b ]

"Runs that dead-home block on Home itself, two calls deep."
Home >> runDeadHere [ | b r | b := self makeBlock. r := self helper: b. ^r + 1000 ]
Home >> helper: aBlock [ | r | r := aBlock value. ^r + 100 ]
"#;

/// Run `PRELUDE` followed by `driver` as one chunk; the chunk's value (futures
/// resolved by the test helper) is returned.
fn run(driver: &str) -> Value {
    eval(&format!("{PRELUDE}\n{driver}"))
}

// ════════════════════════════════════════════════════════════════════════
//  GREEN — correct today. Regression guards; must keep passing.
// ════════════════════════════════════════════════════════════════════════

/// E0: block runs on Home's own coroutine via a self-send. `^x` returns -1 from
/// `firstNegViaSelf`. This is the only situation `^` handles correctly, and the
/// baseline all the red cases are measured against.
#[test]
fn green_e0_self_send_returns_neg_one() {
    assert_eq!(run("(Home new firstNegViaSelf) await"), Value::Int(-1));
}

/// E3: receiver is a `Cons`, which is a *Value*, so `do:` runs on the caller's
/// coroutine — same stack as the home method — and `^x` returns -1.
#[test]
fn green_e3_value_receiver_returns_neg_one() {
    let driver = "| c | c := Cons with: 1 rest: (Cons singleton: -1). \
                  (Home new firstNeg: c) await";
    assert_eq!(run(driver), Value::Int(-1));
}

/// E9: `^` does stop the `do:` loop — three elements, negative at index 1, so
/// the block body runs exactly twice. (Only the *return target* is broken; the
/// early-exit of the iteration itself works.)
#[test]
fn green_e9_caret_stops_the_loop_after_two_ticks() {
    let driver = "| a c | a := Array new: 3. \
                  a at: 0 put: 1. a at: 1 put: -1. a at: 2 put: 7. \
                  c := Counter new. \
                  (Home new firstNegCounting: a counter: c) await. (c n) await";
    assert_eq!(run(driver), Value::Int(2));
}

/// E10: a handler for an error signaled on *this* entity. The handler block runs
/// on Home's own coroutine, so its `^2` returns 2 from `handlerLocal`.
#[test]
fn green_e10_handler_local_signal_returns_two() {
    assert_eq!(run("(Home new handlerLocal) await"), Value::Int(2));
}

/// E5t: `then:` chain is the method's own answer (`^(s seven) then: [...]`).
/// This *looks* like a working non-local return but is not: `thenAsAnswer`'s
/// activation has already ended, and the then-block's `^v + 1` merely settles
/// the continuation's reply with 8. It passes today; it is kept green as a
/// regression guard AND as the concrete case behind tension 1 in I5 (does `^`
/// mean "return from the activation" or "settle this method's reply"?).
#[test]
fn green_e5t_then_as_method_answer_returns_eight() {
    assert_eq!(
        run("(Home new thenAsAnswer: Slow new) await"),
        Value::Int(8)
    );
}

// ════════════════════════════════════════════════════════════════════════
//  RED — answers assuming ^ is delivered across entities; ignored pending Q24.
// ════════════════════════════════════════════════════════════════════════

/// E1: receiver is an `Array` (an entity), and the method *waits* for `do:`.
/// The home method is parked on the block's evaluator, so `^x` has a live home
/// to return to: the correct answer is -1.
/// TODAY: nil — `^x` only ends the Array's `do:` turn; `firstNegAwaited` resumes
/// and runs `^nil`.
#[test]
#[ignore = "I5/Q24: asserts delivering ^ across entities; relabel when Q24 decides"]
fn red_e1_awaited_entity_receiver_returns_neg_one() {
    let driver = "| a | a := Array new: 2. a at: 0 put: 1. a at: 1 put: -1. \
                  (Home new firstNegAwaited: a) await";
    assert_eq!(run(driver), Value::Int(-1));
}

/// E5: `then:` block, awaited. The method waits for the chain, so `^v + 1`
/// should return 8 from `thenAwaited`.
/// TODAY: 99 — the `^` settles the continuation's reply; the method runs on
/// to `^99`.
#[test]
#[ignore = "I5/Q24: asserts delivering ^ across entities; relabel when Q24 decides"]
fn red_e5_then_awaited_returns_eight() {
    assert_eq!(run("(Home new thenAwaited: Slow new) await"), Value::Int(8));
}

/// E11: handler for an error signaled on another entity. NOTE: this is NOT the
/// same bug as e1/e5, and its "correct" answer is genuinely open. The handler
/// `[:e | ^2]` is authored in `handlerRemote` but runs at the signal point, on
/// the *signaling* entity's coroutine (`condition_system_guide.md` §1); the `^2`
/// is therefore a cross-coroutine `^` — the very thing I5 is about. The condition
/// system's own control model does not include `^`-to-a-remote-home: it supplies
/// `return:` / `resume:` / `retry` (local to the signaling coroutine) and the
/// declared-class disposition precisely so a handler need not reach back. So the
/// condition-idiomatic form is `[:e | e return: 2]` or relying on `Error`'s abort,
/// not `^2`. The assertion below (2) presumes `^`-to-home works; whether that
/// should be supported at all is the open question. TODAY: 1.
#[test]
#[ignore = "I5/Q24: asserts delivering ^ across entities; relabel when Q24 decides"]
fn red_e11_handler_remote_signal_returns_two() {
    assert_eq!(
        run("(Home new handlerRemote: Failer new) await"),
        Value::Int(2)
    );
}

/// E7: the concrete kernel fallout, and the one red case fixable without the
/// evaluator redesign. `Dictionary >> =` used to do
/// `(other at: assoc key ifAbsent: [^false]) = assoc value ifFalse: [^false]`;
/// the `^false` inside the `ifAbsent:` block ran on `other`'s coroutine, so
/// `at:ifAbsent:` merely answered false, and two dictionaries with different keys
/// and value `false` compared EQUAL. Fixed 2026-11-06 by rewriting the kernel
/// method to await `other includesKey:` / `other at:` on this coroutine, keeping
/// the `^false`s on the home frame — it does NOT fix the underlying `^`-across-
/// entities bug (see the still-red e1/e5/e11). Correct: `d1 = d2` is false.
#[test]
fn green_e7_dictionary_unequal_keys_false_values_are_not_equal() {
    let driver = "| d1 d2 | d1 := Dictionary new. d2 := Dictionary new. \
                  (d1 at: #a put: false) await. (d2 at: #b put: false) await. \
                  (d1 = d2) await";
    assert_eq!(run(driver), Value::Bool(false));
}

/// E7c: control for E7 — identical shape, value `1` instead of `false`. Here the
/// spurious `false` from the escaped `^false` does NOT equal `1`, so the bug is
/// masked and the answer is already correct. Kept to show E7 is specifically the
/// `false`-value interaction, and as a guard that a fix keeps this correct.
#[test]
fn green_e7c_dictionary_unequal_keys_nonfalse_values_are_not_equal() {
    let driver = "| d1 d2 | d1 := Dictionary new. d2 := Dictionary new. \
                  (d1 at: #a put: 1) await. (d2 at: #b put: 1) await. \
                  (d1 = d2) await";
    assert_eq!(run(driver), Value::Bool(false));
}

// ════════════════════════════════════════════════════════════════════════
//  IGNORED — semantics undecided (I5 "missing / stale target" question).
//  Bodies characterize TODAY's (wrong) answer so the gap stays tracked.
//  Flip each to the agreed semantics once decided, then remove #[ignore].
// ════════════════════════════════════════════════════════════════════════

/// E2: receiver is an `Array` (entity) and the method does NOT wait. This is the
/// case you flagged as arguably broken-by-design: the method fires `do:` and
/// returns nil before the block can return anything. Whether `^x` should (a)
/// return -1, (b) be a no-op, or (c) be rejected statically is undecided.
/// TODAY: nil.
#[test]
#[ignore = "I5: fire-and-forget send with ^ in block; return semantics undecided"]
fn ignored_e2_unawaited_entity_receiver() {
    let driver = "| a | a := Array new: 2. a at: 0 put: 1. a at: 1 put: -1. \
                  (Home new firstNeg: a) await";
    assert_eq!(run(driver), Value::Nil); // characterizes current (contested) result
}

/// E4: `then:` block, NOT awaited. Same undecided fire-and-forget shape as E2,
/// but via a genuine Future continuation.  TODAY: 99.
#[test]
#[ignore = "I5: fire-and-forget then: with ^ in block; return semantics undecided"]
fn ignored_e4_then_not_awaited() {
    assert_eq!(
        run("(Home new thenNotAwaited: Slow new) await"),
        Value::Int(99)
    );
}

/// E8: the home method (`makeBlock`) has already returned, and a `Worker` runs
/// the orphaned `[^42]` two calls deep. In Smalltalk this raises `cannotReturn:`.
/// Here `^42` targets stack depth 2, which happens to be `Worker>>helper:`, so it
/// returns 42 from helper:, then `run:` adds 1000.
/// TODAY: 1042. Correct answer (error? no-op?) is the open "missing target"
/// question.
#[test]
#[ignore = "I5: dead-home / stale-frame target; cannotReturn-style semantics undecided"]
fn ignored_e8_dead_home_run_by_worker() {
    let driver = "| b | b := (Home new blockWithDeadHome) await. \
                  (Worker new run: b) await";
    assert_eq!(run(driver), Value::Int(1042)); // characterizes current depth-collision bug
}

/// E12: same dead-home block, run on Home itself two calls deep — shows the
/// frame mix-up needs no second entity, just a stale `home_depth`.
/// TODAY: 1042.
#[test]
#[ignore = "I5: dead-home / stale-frame target; cannotReturn-style semantics undecided"]
fn ignored_e12_dead_home_same_entity() {
    assert_eq!(run("(Home new runDeadHere) await"), Value::Int(1042));
}
