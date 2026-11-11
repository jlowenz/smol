use std::io::{self, BufRead, Write};

use interpreter::eval::Evaluator;
use interpreter::parser::{Diagnostic, Parser, Severity, TaggedDiagnostic};
use interpreter::span::SourceId;
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
            let (chunk, diagnostics) = Parser::parse_with_diagnostics(SourceId::ANON, &src);
            let first_error = diagnostics
                .iter()
                .find(|d| d.node.severity() == Severity::Error);
            if let Some(TaggedDiagnostic {
                node: Diagnostic::UnexpectedEof(_),
                ..
            }) = first_error
            {
                print!(".. ");
                stdout.flush().ok();
                src.push('\n');
                if read_line(&stdin, &mut src).is_err() {
                    break;
                }
                continue;
            }
            for d in diagnostics
                .iter()
                .filter(|d| d.node.severity() == Severity::Warning)
            {
                eprintln!("{}", d.render("<input>", &src));
            }
            match (chunk, first_error) {
                (Some(prog), _) => match ev.eval_chunk(&prog, &mut workspace) {
                    Ok(val) => println!("{val}"),
                    Err(interpreter::eval::EvalError::Return(val, _)) => println!("{val}"),
                    Err(e) => println!("error: {e}"),
                },
                (None, Some(e)) => println!("parse error: {}", e.render("<input>", &src)),
                (None, None) => unreachable!("a failed parse reports at least one error"),
            }
            break;
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
