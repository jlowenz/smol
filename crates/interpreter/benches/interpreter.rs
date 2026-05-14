use criterion::{criterion_group, criterion_main, Criterion};

use interpreter::env::Env;
use interpreter::eval::Evaluator;
use interpreter::parser::Parser;

fn bench_parse_and_eval(c: &mut Criterion) {
    let ev = Evaluator;

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

criterion_group!(benches, bench_parse_and_eval);
criterion_main!(benches);
