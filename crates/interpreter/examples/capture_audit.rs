//! Freeze-on-capture ergonomics audit.
//!
//! Question (env-frame redesign, snapshot-capture decision): if capturing a
//! variable in a block froze it (no reassignment after capture), how much real
//! code would break? This parses `kernel.smol` with the actual parser and, for
//! every scope (method body + every nested block), flags any local that is
//! **reassigned at or after** the point a nested block captures it.
//!
//! A "capture" here is a free reference to the scope's own local from lexically
//! inside a nested block (shadowing-aware). The freeze rule is violated when an
//! assignment to that local occurs at a top-level statement index >= the first
//! capturing statement's index. (`==` = capture-in-RHS-then-assign, same stmt.)
//!
//! Run: cargo run --release --example capture_audit -p interpreter

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use interpreter::ast::{Expr, Message, Stmt};
use interpreter::parser::Parser;

type Names = HashSet<Arc<str>>;

/// Collect, into `out`, every name in `declared` that is referenced from
/// lexically inside a block (shadow-aware). `in_block` is true once we've
/// crossed a block boundary relative to the scope being analyzed.
fn caps_expr(declared: &Names, e: &Expr, in_block: bool, shadow: &Names, out: &mut Names) {
    match e {
        Expr::Var { name: v, .. } => {
            if in_block && declared.contains(v) && !shadow.contains(v) {
                out.insert(v.clone());
            }
        }
        Expr::Send { receiver, message } => {
            caps_expr(declared, receiver, in_block, shadow, out);
            match message {
                Message::Unary(_) => {}
                Message::Binary { arg, .. } => caps_expr(declared, arg, in_block, shadow, out),
                Message::Keyword { parts } => {
                    for (_, a) in parts {
                        caps_expr(declared, a, in_block, shadow, out);
                    }
                }
            }
        }
        Expr::Block {
            params,
            locals,
            body,
            ..
        } => {
            let mut sh = shadow.clone();
            sh.extend(params.iter().cloned());
            sh.extend(locals.iter().cloned());
            for st in body {
                caps_stmt(declared, st, true, &sh, out);
            }
        }
        Expr::Cascade { receiver, messages } => {
            caps_expr(declared, receiver, in_block, shadow, out);
            for m in messages {
                match m {
                    Message::Unary(_) => {}
                    Message::Binary { arg, .. } => caps_expr(declared, arg, in_block, shadow, out),
                    Message::Keyword { parts } => {
                        for (_, a) in parts {
                            caps_expr(declared, a, in_block, shadow, out);
                        }
                    }
                }
            }
        }
        Expr::DynamicArray(es) => {
            for x in es {
                caps_expr(declared, x, in_block, shadow, out);
            }
        }
        Expr::Lit(_) | Expr::Super | Expr::NamespaceRef { .. } | Expr::ArrayLiteral(_) => {}
    }
}

fn caps_stmt(declared: &Names, s: &Stmt, in_block: bool, shadow: &Names, out: &mut Names) {
    match s {
        Stmt::Expr(e) | Stmt::Return(e) => caps_expr(declared, e, in_block, shadow, out),
        Stmt::Assign { value, .. } => caps_expr(declared, value, in_block, shadow, out),
        Stmt::LocalDecl(_) | Stmt::MethodDef(_) => {}
    }
}

/// A nested block, as (params, locals, body) borrowed from the AST.
type BlockRef<'a> = (&'a [Arc<str>], &'a [Arc<str>], &'a [Stmt]);

/// Every `Block` literal appearing anywhere in a statement (for recursing into
/// nested scopes), returned as (params∪locals, body).
fn collect_blocks<'a>(s: &'a Stmt, out: &mut Vec<BlockRef<'a>>) {
    fn ex<'a>(e: &'a Expr, out: &mut Vec<BlockRef<'a>>) {
        match e {
            Expr::Block {
                params,
                locals,
                body,
                ..
            } => {
                out.push((params, locals, body));
                for st in body {
                    collect_blocks(st, out);
                }
            }
            Expr::Send { receiver, message } => {
                ex(receiver, out);
                match message {
                    Message::Unary(_) => {}
                    Message::Binary { arg, .. } => ex(arg, out),
                    Message::Keyword { parts } => parts.iter().for_each(|(_, a)| ex(a, out)),
                }
            }
            Expr::Cascade { receiver, messages } => {
                ex(receiver, out);
                for m in messages {
                    match m {
                        Message::Unary(_) => {}
                        Message::Binary { arg, .. } => ex(arg, out),
                        Message::Keyword { parts } => parts.iter().for_each(|(_, a)| ex(a, out)),
                    }
                }
            }
            Expr::DynamicArray(es) => es.iter().for_each(|x| ex(x, out)),
            _ => {}
        }
    }
    match s {
        Stmt::Expr(e) | Stmt::Return(e) => ex(e, out),
        Stmt::Assign { value, .. } => ex(value, out),
        Stmt::LocalDecl(_) | Stmt::MethodDef(_) => {}
    }
}

struct Finding {
    scope: String,
    name: Arc<str>,
    first_capture: usize,
    assigns_after: Vec<usize>,
    same_stmt: bool,
}

/// Analyze one scope's own locals for freeze-on-capture violations, then recurse
/// into every nested block as its own scope.
fn analyze(
    mut declared: Names,
    body: &[Stmt],
    label: &str,
    findings: &mut Vec<Finding>,
    stats: &mut Stats,
) {
    for s in body {
        if let Stmt::LocalDecl(ns) = s {
            declared.extend(ns.iter().cloned());
        }
    }
    stats.scopes += 1;

    let mut assign_idx: HashMap<Arc<str>, Vec<usize>> = HashMap::new();
    let mut capture_idx: HashMap<Arc<str>, Vec<usize>> = HashMap::new();
    for (i, s) in body.iter().enumerate() {
        let mut caps = Names::new();
        caps_stmt(&declared, s, false, &Names::new(), &mut caps);
        for c in caps {
            capture_idx.entry(c).or_default().push(i);
        }
        if let Stmt::Assign { name, .. } = s
            && declared.contains(name)
        {
            assign_idx.entry(name.clone()).or_default().push(i);
        }
    }

    // Straight-line reassignment: a local assigned more than once. (Method temps
    // can only be assigned in straight-line code — never in a block/branch/loop —
    // so >1 assignment is a genuine mutable-local reassignment, the thing a
    // `let`-only language would forbid and `let mut` would require.)
    for (name, assigns) in &assign_idx {
        if assigns.len() > 1 {
            stats.reassigned.push(format!(
                "{label}: `{name}` assigned {}× @{assigns:?}",
                assigns.len()
            ));
        }
    }

    for (name, caps) in &capture_idx {
        stats.captured_pairs += 1;
        let Some(assigns) = assign_idx.get(name) else {
            continue;
        };
        stats.captured_and_assigned += 1;
        let first_cap = *caps.iter().min().unwrap();
        let after: Vec<usize> = assigns.iter().copied().filter(|&a| a > first_cap).collect();
        let same = assigns.contains(&first_cap);
        if !after.is_empty() || same {
            findings.push(Finding {
                scope: label.to_string(),
                name: name.clone(),
                first_capture: first_cap,
                assigns_after: after,
                same_stmt: same,
            });
        } else {
            stats.safe.push(format!(
                "{label}: `{name}` assigned@{assigns:?}, first captured@{first_cap}"
            ));
        }
    }

    // Recurse into nested blocks as their own scopes.
    let mut blocks = Vec::new();
    for s in body {
        collect_blocks(s, &mut blocks);
    }
    for (params, locals, b) in blocks {
        let mut d = Names::new();
        d.extend(params.iter().cloned());
        d.extend(locals.iter().cloned());
        analyze(d, b, &format!("{label}/[]"), findings, stats);
    }
}

#[derive(Default)]
struct Stats {
    methods: usize,
    scopes: usize,
    captured_pairs: usize,
    captured_and_assigned: usize,
    reassigned: Vec<String>,
    safe: Vec<String>,
}

fn main() {
    let src = include_str!("../src/kernel.smol");
    let prog = Parser::parse(src).expect("kernel.smol parse");
    let mut findings = Vec::new();
    let mut stats = Stats::default();

    for s in &prog.stmts {
        if let Stmt::MethodDef(m) = s {
            stats.methods += 1;
            let mut declared: Names = m.pattern.params().into_iter().collect();
            declared.extend(m.locals.iter().cloned());
            let label = format!("{} >> {}", m.class_name, m.pattern.selector());
            analyze(declared, &m.body, &label, &mut findings, &mut stats);
        }
    }

    println!("\n=== freeze-on-capture audit: kernel.smol ===");
    println!("methods: {}", stats.methods);
    println!("scopes analyzed (method + nested blocks): {}", stats.scopes);
    println!("(name, scope) capture pairs: {}", stats.captured_pairs);
    println!(
        "  of which the captured name is ALSO assigned in-scope: {}",
        stats.captured_and_assigned
    );
    println!(
        "VIOLATIONS (assigned at/after first capture): {}\n",
        findings.len()
    );
    for f in &findings {
        let kind = if f.same_stmt && f.assigns_after.is_empty() {
            "same-stmt (capture-in-RHS)".to_string()
        } else {
            format!(
                "assign(s) at stmt {:?} after capture@{}",
                f.assigns_after, f.first_capture
            )
        };
        println!("  [{}]  `{}`  — {}", f.scope, f.name, kind);
    }
    if findings.is_empty() {
        println!("  (none)");
    }
    println!("\n=== SAFE captured-and-assigned cases (assigned strictly before capture) ===");
    for s in &stats.safe {
        println!("  {s}");
    }

    println!(
        "\n=== STRAIGHT-LINE REASSIGNMENTS (locals assigned >1×) — the `let mut` demand ===\n\
         count: {}   (0 ⇒ let-only is viable; >0 ⇒ these need `let mut` or shadowing)",
        stats.reassigned.len()
    );
    for s in &stats.reassigned {
        println!("  {s}");
    }
    if stats.reassigned.is_empty() {
        println!("  (none)");
    }
}
