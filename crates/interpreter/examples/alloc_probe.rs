//! Per-send cost probe for the performance investigation
//! (`docs/next_steps_after_c34.md` §4).
//!
//! Two jobs:
//!   * report a MARGINAL ns/iteration (and ns/user-level-send) for scalar
//!     workloads via a two-size differential — running at size N and 2N and
//!     differencing cancels one-time setup and first-send lazy costs; and
//!   * serve as a dense hot loop for `samply`, which decomposes that time by
//!     function (allocation vs hashing vs mutex vs `Vec` clone).
//!
//! Only SCALAR workloads are used: integer arithmetic and tail-recursive
//! integer loops dispatch synchronously with no `may` coroutine, so the hot
//! path is the pure method-activation cost with no actor machinery mixed in.
//! Entity sends are a separate question.
//!
//! Deterministic allocation COUNTS (allocs/send) are a complementary
//! cross-check; a `GlobalAlloc` counter needs `unsafe`, which this workspace
//! forbids, so that measurement is handled separately (see the investigation
//! notes) and is not part of this file.
//!
//! Usage:
//!   cargo run --release --example alloc_probe -p interpreter          # timing table
//!   cargo run --release --example alloc_probe -p interpreter -- soak  # long hot loop
//!   samply record -- target/release/examples/alloc_probe soak         # flamegraph

use std::time::Instant;

use interpreter::ast::Program;
use interpreter::env::Env;
use interpreter::eval::Evaluator;
use interpreter::parser::Parser;

fn parse(src: &str) -> Program {
    Parser::parse(src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"))
}

/// Averaged wall-clock ns for one `eval_program` run of `prog`, over `reps`.
/// A fresh env + `populate_env` is included in every run (as in the benches),
/// but it is a per-run constant that cancels in the differential.
fn eval_ns(ev: &Evaluator, prog: &Program, reps: u32) -> f64 {
    for _ in 0..3 {
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        ev.eval_program(&prog.stmts, &mut env).unwrap();
    }
    let start = Instant::now();
    for _ in 0..reps {
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        ev.eval_program(&prog.stmts, &mut env).unwrap();
    }
    start.elapsed().as_nanos() as f64 / reps as f64
}

/// Marginal per-iteration / per-send timing from a two-size differential.
fn differential(
    ev: &Evaluator,
    label: &str,
    sends_per_iter: f64,
    n_lo: u64,
    n_hi: u64,
    make: impl Fn(u64) -> String,
) {
    let lo = parse(&make(n_lo));
    let hi = parse(&make(n_hi));
    let ns_lo = eval_ns(ev, &lo, 300);
    let ns_hi = eval_ns(ev, &hi, 300);
    let d_iters = (n_hi - n_lo) as f64;
    let ns_per_iter = (ns_hi - ns_lo) / d_iters;
    println!("── {label}");
    println!(
        "   ns/iteration: {ns_per_iter:7.1}   ns/send (÷{sends_per_iter}): {:6.1}",
        ns_per_iter / sends_per_iter
    );
}

/// Install the scalar helper methods used by the workloads.
fn setup(ev: &Evaluator) {
    let src = "\
Integer >> pvtSpin [ self = 0 ifTrue: [ ^0 ]. ^(self - 1) pvtSpin ]
Integer >> pvtFibA: a b: b [ self = 0 ifTrue: [ ^a ]. ^(self - 1) pvtFibA: b b: a + b ]
Integer >> fib [ ^self pvtFibA: 0 b: 1 ]";
    let prog = parse(src);
    let mut env = Env::new();
    ev.registry.populate_env(&mut env);
    ev.eval_program(&prog.stmts, &mut env).unwrap();
}

fn main() {
    let ev = Evaluator::new();
    setup(&ev);

    if std::env::args().any(|a| a == "soak") {
        // Dense hot loop for samply: pure MACHINE-INT scalar sends (pvtSpin
        // counts down, so no bignum promotion). ~800k iterations ≈ ~9s of the
        // pure method-activation hot path.
        let prog = parse("100000 pvtSpin");
        for _ in 0..8 {
            let mut env = Env::new();
            ev.registry.populate_env(&mut env);
            ev.eval_program(&prog.stmts, &mut env).unwrap();
        }
        return;
    }

    println!("\n=== per-send timing probe (scalar, coroutine-free) ===\n");
    // pvtSpin: `=`, `-`, `pvtSpin` per non-base iteration (ifTrue: arm inlined).
    differential(
        &ev,
        "pvtSpin (bare tail-recursive loop)",
        3.0,
        2_000,
        4_000,
        |n| format!("{n} pvtSpin"),
    );
    // timesRepeat: [1+1]: loop `-` + loop `timesRepeat:` send + body `+` ≈ 3.
    differential(&ev, "timesRepeat: [1 + 1]", 3.0, 2_000, 4_000, |n| {
        format!("{n} timesRepeat: [1 + 1]")
    });
    // fib: `=`, `-`, `+`, `pvtFibA:b:` per iteration.
    differential(&ev, "fib (iterative)", 4.0, 2_000, 4_000, |n| {
        format!("{n} fib")
    });
    println!();
}
