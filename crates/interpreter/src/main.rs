use std::io::{self, BufRead, Write};

use interpreter::env::Env;
use interpreter::eval::Evaluator;
use interpreter::parser::Parser;

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let ev = Evaluator;
    let mut env = Env::new();

    println!("smol 0.1.0");
    loop {
        print!("> ");
        stdout.flush().ok();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => { eprintln!("read error: {e}"); break; }
        }

        let src = line.trim();
        if src.is_empty() { continue; }

        let prog = match Parser::parse(src) {
            Ok(p) => p,
            Err(e) => { println!("parse error: {e}"); continue; }
        };

        match ev.eval_program(&prog.stmts, &mut env) {
            Ok(val) => println!("{val}"),
            Err(interpreter::eval::EvalError::Return(val)) => println!("{val}"),
            Err(e) => println!("error: {e}"),
        }
    }
}
