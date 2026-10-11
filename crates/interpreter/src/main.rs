use std::io::{self, BufRead, Write};

use interpreter::eval::Evaluator;
use interpreter::parser::{ParseError, Parser};
use interpreter::workspace::Workspace;

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let ev = Evaluator::new();
    let mut workspace = Workspace::new(&ev);

    println!("smol 0.1.0");
    loop {
        print!("> ");
        stdout.flush().ok();

        let mut src = String::new();
        if read_line(&stdin, &mut src).is_err() {
            break;
        }
        if src.trim().is_empty() {
            continue;
        }

        // Accumulate lines until we have a complete parse or a real error.
        loop {
            match Parser::parse(&src) {
                Ok(prog) => {
                    match ev.eval_chunk(&prog, &mut workspace) {
                        Ok(val) => println!("{val}"),
                        Err(interpreter::eval::EvalError::Return(val, _)) => println!("{val}"),
                        Err(e) => println!("error: {e}"),
                    }
                    break;
                }
                Err(ParseError::UnexpectedEof(_)) => {
                    print!(".. ");
                    stdout.flush().ok();
                    src.push('\n');
                    if read_line(&stdin, &mut src).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    println!("parse error: {e}");
                    break;
                }
            }
        }
    }
}

fn read_line(stdin: &io::Stdin, buf: &mut String) -> Result<(), ()> {
    match stdin.lock().read_line(buf) {
        Ok(0) => Err(()),
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("read error: {e}");
            Err(())
        }
    }
}
