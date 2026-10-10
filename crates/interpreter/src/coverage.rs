//! Opt-in execution-coverage collection for `.smol` source.
//!
//! A [`CoverageCollector`] accumulates the set of statement [`Span`]s the
//! evaluator actually executed during a run. It is shared across all evaluators
//! — the main thread and every entity coroutine — via the class registry, behind
//! an `Arc<Mutex<_>>`. The per-statement write takes the lock; that is fine
//! because coverage is opt-in (a test/profiling mode, not a production hot path),
//! and keeping one live collector lets the global view be read at any quiescent
//! point. See `docs/coverage_tooling_plan.md` for the alternatives considered.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use crate::ast::{Expr, Message, Program, Stmt, TaggedStmt};
use crate::span::Span;

/// Accumulates the spans of statements executed during a run.
#[derive(Debug, Default)]
pub struct CoverageCollector {
    touched: HashSet<Span>,
}

impl CoverageCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the statement at `span` executed.
    pub fn record(&mut self, span: Span) {
        self.touched.insert(span);
    }

    /// The set of distinct statement spans executed so far.
    pub fn touched(&self) -> &HashSet<Span> {
        &self.touched
    }

    /// Number of distinct statement spans executed.
    pub fn len(&self) -> usize {
        self.touched.len()
    }

    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }
}

// ── Report: map touched spans back to source lines ───────────────────────────

/// Every statement span in `prog`, in source order — the denominator for a
/// coverage report. Recurses into method bodies and nested block bodies.
pub fn collect_stmt_spans(prog: &Program) -> Vec<Span> {
    let mut out = Vec::new();
    collect_stmts(&prog.stmts, &mut out);
    out
}

fn collect_stmts(stmts: &[TaggedStmt], out: &mut Vec<Span>) {
    for s in stmts {
        out.push(s.span);
        match &s.node {
            Stmt::Expr(e) | Stmt::Return(e) => collect_expr(e, out),
            Stmt::Assign { value, .. } => collect_expr(value, out),
            Stmt::MethodDef(d) => collect_stmts(&d.body, out),
            Stmt::LocalDecl(_) => {}
        }
    }
}

fn collect_expr(e: &Expr, out: &mut Vec<Span>) {
    match e {
        Expr::Block { body, .. } => collect_stmts(body, out),
        Expr::Send { receiver, message } => {
            collect_expr(receiver, out);
            collect_message(message, out);
        }
        Expr::Cascade { receiver, messages } => {
            collect_expr(receiver, out);
            for m in messages {
                collect_message(m, out);
            }
        }
        Expr::DynamicArray(es) => {
            for x in es {
                collect_expr(x, out);
            }
        }
        Expr::Lit(_)
        | Expr::Var { .. }
        | Expr::Super
        | Expr::NamespaceRef { .. }
        | Expr::ArrayLiteral(_) => {}
    }
}

fn collect_message(m: &Message, out: &mut Vec<Span>) {
    match m {
        Message::Unary(_) => {}
        Message::Binary { arg, .. } => collect_expr(arg, out),
        Message::Keyword { parts } => {
            for (_, a) in parts {
                collect_expr(a, out);
            }
        }
    }
}

/// A statement-level coverage summary for one source.
#[derive(Debug, Clone)]
pub struct CoverageReport {
    /// Display name of the source (e.g. `"kernel.smol"`).
    pub source: Arc<str>,
    /// Total statements in the source.
    pub total: usize,
    /// Statements whose span was recorded as executed.
    pub covered: usize,
    /// 1-based line numbers holding at least one unexecuted statement, sorted
    /// and deduplicated.
    pub uncovered_lines: Vec<u32>,
}

impl CoverageReport {
    /// Percentage of statements covered (100.0 for an empty source).
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.covered as f64 * 100.0 / self.total as f64
        }
    }
}

impl std::fmt::Display for CoverageReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}/{} statements ({:.1}%)",
            self.source,
            self.covered,
            self.total,
            self.percent()
        )?;
        if !self.uncovered_lines.is_empty() {
            write!(f, "; uncovered lines: {:?}", self.uncovered_lines)?;
        }
        Ok(())
    }
}

/// Build a [`CoverageReport`] for one source from its full statement-span set
/// (`all_spans`, from [`collect_stmt_spans`]) and the executed-span set
/// (`touched`). Spans in `touched` from other sources are ignored.
///
/// Requires: every span in `all_spans` indexes into `text` (the same source).
pub fn report_source(
    name: &str,
    text: &str,
    all_spans: &[Span],
    touched: &HashSet<Span>,
) -> CoverageReport {
    let starts = line_starts(text);
    let mut covered = 0usize;
    let mut uncovered: BTreeSet<u32> = BTreeSet::new();
    for sp in all_spans {
        if touched.contains(sp) {
            covered += 1;
        } else {
            uncovered.insert(line_of(&starts, sp.start));
        }
    }
    CoverageReport {
        source: Arc::from(name),
        total: all_spans.len(),
        covered,
        uncovered_lines: uncovered.into_iter().collect(),
    }
}

/// Byte offsets at which each line starts; `line_starts(text)[0] == 0`.
fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push((i + 1) as u32);
        }
    }
    starts
}

/// 1-based line number containing byte `offset`.
fn line_of(starts: &[u32], offset: u32) -> u32 {
    starts.partition_point(|&s| s <= offset) as u32
}

#[cfg(test)]
mod tests {
    use super::{collect_stmt_spans, report_source};
    use crate::span::{SourceId, Span};
    use crate::test_helpers::eval_coverage;
    use std::collections::HashSet;

    #[test]
    fn records_executed_statement_spans() {
        // With coverage enabled, running a program records the spans of the
        // statements it executed — proves the `dispatch_stmt` hook fires.
        let touched = eval_coverage("| x | x := 3. x + 4");
        assert!(
            !touched.is_empty(),
            "coverage should record executed statement spans, got none"
        );
    }

    #[test]
    fn report_maps_spans_to_lines() {
        // Four one-char lines: "a\nb\nc\nd\n".
        let text = "a\nb\nc\nd\n";
        let s1 = Span::new(SourceId::KERNEL, 0, 1); // line 1
        let s2 = Span::new(SourceId::KERNEL, 2, 3); // line 2
        let s3 = Span::new(SourceId::KERNEL, 4, 5); // line 3
        let all = vec![s1, s2, s3];
        let touched: HashSet<Span> = [s1, s3].into_iter().collect();
        let r = report_source("k", text, &all, &touched);
        assert_eq!(r.total, 3);
        assert_eq!(r.covered, 2);
        assert_eq!(r.uncovered_lines, vec![2]); // only s2 (line 2) is uncovered
    }

    #[test]
    fn kernel_coverage_is_partial() {
        // Exercise some kernel methods, then report against the whole kernel.
        let touched = eval_coverage("1 to: 5 do: [:i | i + 1 ]");
        let kernel = include_str!("kernel.smol");
        let prog =
            crate::parser::Parser::parse_source(SourceId::KERNEL, kernel).expect("kernel parse");
        let all = collect_stmt_spans(&prog);
        let report = report_source("kernel.smol", kernel, &all, &touched);
        assert!(
            report.total > 100,
            "kernel has many statements, got {}",
            report.total
        );
        assert!(
            report.covered > 0,
            "running kernel code should cover some statements"
        );
        assert!(
            report.covered < report.total,
            "a tiny program should not cover the whole kernel"
        );
    }

    #[test]
    fn branch_coverage_tracks_untaken_arm() {
        // Only the `ifTrue:` arm runs; the `ifFalse:` arm's statement must stay
        // uncovered — the original manual-audit requirement, now automatic.
        let src = "true\n  ifTrue: [ 100 ]\n  ifFalse: [ 200 ]";
        let touched = eval_coverage(src);
        let prog = crate::parser::Parser::parse(src).expect("parse");
        let all = collect_stmt_spans(&prog);
        let report = report_source("branch", src, &all, &touched);
        // Line 2 = taken arm `[ 100 ]`; line 3 = untaken arm `[ 200 ]`.
        assert!(
            !report.uncovered_lines.contains(&2),
            "taken arm (line 2) should be covered, uncovered={:?}",
            report.uncovered_lines
        );
        assert!(
            report.uncovered_lines.contains(&3),
            "untaken arm (line 3) should be uncovered, uncovered={:?}",
            report.uncovered_lines
        );
    }
}
