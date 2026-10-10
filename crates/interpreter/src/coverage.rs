//! Opt-in execution-coverage collection for `.smol` source.
//!
//! A [`CoverageCollector`] accumulates the set of statement [`Span`]s the
//! evaluator actually executed during a run. It is shared across all evaluators
//! — the main thread and every entity coroutine — via the class registry, behind
//! an `Arc<Mutex<_>>`. The per-statement write takes the lock; that is fine
//! because coverage is opt-in (a test/profiling mode, not a production hot path),
//! and keeping one live collector lets the global view be read at any quiescent
//! point. See `docs/coverage_tooling_plan.md` for the alternatives considered.

use std::collections::HashSet;

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

#[cfg(test)]
mod tests {
    use crate::test_helpers::eval_coverage;

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
}
