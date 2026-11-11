//! Lists the parser's diagnostics for `kernel.smol`, one per line, followed by a
//! count per kind. Measures the Q24 migration (`docs/q24_v0_plan.md` steps 2–3).
//!
//! Run: cargo run --example kernel_warnings -p interpreter
//!      (or: just kernel-warnings)

use std::collections::BTreeMap;

use interpreter::parser::Parser;
use interpreter::span::SourceId;

const KERNEL: &str = include_str!("../src/kernel.smol");

fn main() {
    let (_, diagnostics) = Parser::parse_with_diagnostics(SourceId::KERNEL, KERNEL);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for d in &diagnostics {
        println!("{}", d.render("crates/interpreter/src/kernel.smol", KERNEL));
        let kind = format!("{:?}", d.node);
        let name = kind.split('(').next().unwrap_or(&kind).to_string();
        *counts.entry(name).or_default() += 1;
    }
    println!();
    for (kind, n) in &counts {
        println!("{n:4} {kind}");
    }
    println!("{:4} total", diagnostics.len());
}
