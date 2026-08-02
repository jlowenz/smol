use criterion::{Criterion, criterion_group, criterion_main};

use interpreter::env::Env;
use interpreter::eval::Evaluator;
use interpreter::parser::Parser;

fn bench_parse_and_eval(c: &mut Criterion) {
    let ev = Evaluator::new();

    // Loop overhead + arithmetic, no cross-iteration state. `timesRepeat:` is a
    // pure-Smol tail-recursive kernel method, so this measures method dispatch,
    // TCO and the leaf `_primAdd:` grounding.
    c.bench_function("timesRepeat 1000", |b| {
        let src = "1000 timesRepeat: [1 + 1]";
        b.iter(|| {
            let prog = Parser::parse(src).unwrap();
            let mut env = Env::new();
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    // Iterative fibonacci carrying its state in accumulator PARAMETERS rather
    // than a mutated temp: a method temp cannot be assigned from inside a block
    // (Smol_Patterns pattern 1), and TCO makes the tail call O(1) in stack, so
    // this is the iterative form in every respect that matters.
    c.bench_function("fibonacci 20 iterative", |b| {
        let setup_src = "
Integer >> pvtFibA: a b: b [ self = 0 ifTrue: [ ^a ]. ^(self - 1) pvtFibA: b b: a + b ]
Integer >> fib [ ^self pvtFibA: 0 b: 1 ]
";
        let setup_prog = Parser::parse(setup_src).unwrap();
        let mut setup_env = Env::new();
        ev.registry.populate_env(&mut setup_env);
        ev.eval_program(&setup_prog.stmts, &mut setup_env).unwrap();

        let src = "20 fib";
        b.iter(|| {
            let prog = Parser::parse(src).unwrap();
            let mut env = Env::new();
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });
}

/// Loop constructs, parsed once so the numbers are evaluation only.
///
/// These exist to give chunk 34 S1b a before/after number: `to:do:` / `to:by:do:`
/// are currently an evaluator special-case (`Cont::ToDoStep`) and become ordinary
/// Smol methods on `Number`. `array do:` bottoms out in `to:do:`, so it measures
/// the same change one protocol layer up.
fn bench_loops(c: &mut Criterion) {
    let ev = Evaluator::new();

    c.bench_function("to:do: 100000", |b| {
        let prog = Parser::parse("0 to: 100000 do: [:i | i]").unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    c.bench_function("to:by:do: 100000", |b| {
        let prog = Parser::parse("0 to: 200000 by: 2 do: [:i | i]").unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    // Nested iteration is the case `Cont::ToDoStep` was built for: the Rust stack
    // must not grow per nesting level.
    c.bench_function("nested to:do: 300x300", |b| {
        let prog = Parser::parse("0 to: 300 do: [:i | 0 to: 300 do: [:j | j]]").unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    c.bench_function("array do: 10000", |b| {
        let prog = Parser::parse("| a | a := Array new: 10000. a do: [:x | x]").unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.registry.populate_env(&mut env);
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    // Pattern 3 (Smol_Patterns): cross-iteration mutation goes through a
    // ValueHolder actor, so this measures the sanctioned mutate-in-a-loop idiom
    // INCLUDING its mailbox cost — the thing `n := n + 1` used to stand in for
    // before the strict block-assignment rule made that form illegal.
    //
    // `h get` answers a Future; the trailing `value` send forces it (seamless
    // Future forwarding, C33), so the accumulation is guaranteed to have
    // completed inside the timed region rather than merely been enqueued.
    c.bench_function("valueholder accumulate 1000", |b| {
        let src = "| h | h := ValueHolder initialValue: 0.
1000 timesRepeat: [h update: [:a | a + 1]].
h get value";
        let prog = Parser::parse(src).unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.registry.populate_env(&mut env);
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });
}

fn bench_objects(c: &mut Criterion) {
    // One-time class setup: Counter with initialize/increment/count.
    let ev = Evaluator::new();
    let setup_src = "
Object subclass: #Counter instanceVariableNames: 'count'.
Counter >> initialize [ count := 0 ]
Counter >> increment [ count := count + 1 ]
Counter >> count [ ^count ]
";
    let setup_prog = Parser::parse(setup_src).unwrap();
    let mut setup_env = Env::new();
    ev.registry.populate_env(&mut setup_env);
    ev.eval_program(&setup_prog.stmts, &mut setup_env).unwrap();

    // Object creation: 1000 allocations + initialize per iteration.
    c.bench_function("object new 1000", |b| {
        let src = "1000 timesRepeat: [Counter new]";
        let prog = Parser::parse(src).unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.registry.populate_env(&mut env);
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    // Method send: 1000 unary sends to the same object.
    c.bench_function("method send increment 1000", |b| {
        let src = "| c | c := Counter new. 1000 timesRepeat: [c increment]. c count";
        let prog = Parser::parse(src).unwrap();
        b.iter(|| {
            let mut env = Env::new();
            ev.registry.populate_env(&mut env);
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });
}

criterion_group!(benches, bench_parse_and_eval, bench_loops, bench_objects);
criterion_main!(benches);
