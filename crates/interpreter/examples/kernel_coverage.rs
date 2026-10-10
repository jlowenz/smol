//! Statement-level execution-coverage report for `kernel.smol`.
//!
//! Enables coverage *before* kernel load (so method-definition statements count),
//! runs a battery of exercise snippets on one coverage-enabled evaluator
//! (tolerating failures — an unknown selector simply contributes no coverage),
//! then prints which kernel statements executed.
//!
//! A single battery will NOT reach 100%: it reveals which kernel methods and
//! branches this exercise never runs. Cross-whole-test-suite coverage would need
//! a process-global sink (see `docs/coverage_tooling_plan.md`).
//!
//! Run: cargo run --release --example kernel_coverage -p interpreter
//!      (or: just cov-smol)

use std::sync::mpsc;

use interpreter::coverage::{collect_stmt_spans, report_source};
use interpreter::entity::init_coroutine_stack;
use interpreter::env::Env;
use interpreter::eval::{EvalError, Evaluator};
use interpreter::parser::Parser;
use interpreter::span::SourceId;
use interpreter::value::Value;

const KERNEL: &str = include_str!("../src/kernel.smol");

/// Exercise snippets. Each runs on the same coverage-enabled evaluator; a parse
/// or eval failure is tolerated (counted as "skipped").
const BATTERY: &[&str] = &[
    "1 + 2. 7 - 3. 2 * 5. 17 // 5. 17 \\\\ 5. 2 negated. 5 abs. -5 abs.",
    "3 < 4. 4 > 3. 3 <= 3. 3 >= 3. 3 = 3. 3 ~= 4. 3 max: 9. 9 min: 3.",
    "5 factorial. 3 even. 4 even. 3 odd. 7 between: 1 and: 10.",
    "true ifTrue: [1] ifFalse: [2]. false ifTrue: [1] ifFalse: [2].",
    "true & false. true | false. true and: [false]. false or: [true]. true not.",
    "(3 > 2) ifTrue: [10]. (3 < 2) ifFalse: [20].",
    "[:x | x + 1] value: 41. [:a :b | a + b] value: 2 value: 3. [42] value.",
    "1 to: 5 do: [:i | i + 1]. 3 timesRepeat: [1].",
    "#(1 2 3) size. #(1 2 3) isEmpty. #(1 2 3) first. #(1 2 3) do: [:x | x].",
    "#(1 2 3) collect: [:x | x * 2]. #(1 2 3) select: [:x | x > 1].",
    "#(1 2 3) inject: 0 into: [:a :b | a + b].",
    "| c | c := OrderedCollection new. c add: 1. c add: 2. c size.",
    "'hello' size. 'hello' , ' world'. 'hello' isEmpty.",
];

/// Wait out any `Value::Future` so entity-handler work (e.g. collections)
/// completes and records its coverage before we read the sink.
fn resolve(v: Value) -> Result<Value, EvalError> {
    match v {
        Value::Future(f) => resolve(f.wait()?),
        other => Ok(other),
    }
}

fn main() {
    init_coroutine_stack();
    let (tx, rx) = mpsc::channel();
    // 4 MB stack: kernel load + a broad battery exceed the per-entity stack.
    let _ = may::go_with!(0x80000, move || {
        let ev = Evaluator::new_without_kernel();
        let cov = ev.registry.enable_coverage();

        // Load the kernel with coverage already on.
        let kprog = Parser::parse_source(SourceId::KERNEL, KERNEL).expect("kernel parse");
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        ev.eval_program(&kprog.stmts, &mut env)
            .expect("kernel load");

        let (mut ok, mut skipped) = (0usize, 0usize);
        for snippet in BATTERY {
            let outcome = Parser::parse(snippet)
                .map_err(|_| ())
                .and_then(|p| ev.eval_program(&p.stmts, &mut env).map_err(|_| ()))
                .and_then(|v| resolve(v).map_err(|_| ()));
            if outcome.is_ok() {
                ok += 1;
            } else {
                skipped += 1;
            }
        }

        let all = collect_stmt_spans(&kprog);
        let touched = cov.lock().touched().clone();
        let report = report_source("kernel.smol", KERNEL, &all, &touched);
        ev.shutdown();
        tx.send((report, ok, skipped)).unwrap();
    });

    let (report, ok, skipped) = rx.recv().expect("coverage coroutine panicked");
    println!("exercise snippets: {ok} ran, {skipped} skipped (parse/eval error)");
    println!("{report}");
    let u = &report.uncovered_lines;
    let head = &u[..u.len().min(30)];
    println!(
        "uncovered kernel lines: {} total; first {}: {head:?}",
        u.len(),
        head.len()
    );
}
