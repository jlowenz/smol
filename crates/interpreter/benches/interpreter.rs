use criterion::{Criterion, criterion_group, criterion_main};

use interpreter::env::Env;
use interpreter::eval::Evaluator;
use interpreter::parser::Parser;

fn bench_parse_and_eval(c: &mut Criterion) {
    let ev = Evaluator::new();

    c.bench_function("timesRepeat 1000", |b| {
        let src = "| n | n := 0. 1000 timesRepeat: [n := n + 1]. n";
        b.iter(|| {
            let prog = Parser::parse(src).unwrap();
            let mut env = Env::new();
            ev.eval_program(&prog.stmts, &mut env).unwrap()
        });
    });

    c.bench_function("fibonacci 20 iterative", |b| {
        let src = "| a b tmp |
a := 0. b := 1.
20 timesRepeat: [tmp := b. b := a + b. a := tmp].
a";
        b.iter(|| {
            let prog = Parser::parse(src).unwrap();
            let mut env = Env::new();
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

criterion_group!(benches, bench_parse_and_eval, bench_objects);
criterion_main!(benches);
