//! Source provenance for syntax nodes.
//!
//! A [`Span`] is a byte range within a [`SourceId`]-identified source unit, and
//! [`SourceTagged`] pairs any value with the span it originated from. These types
//! sit below both the lexer and the AST so each can produce source-tagged values
//! (`SourceTagged<Token>`, `SourceTagged<Stmt>`) without depending on the other.

/// Identifies one source unit within a run — e.g. the embedded `kernel.smol`, or a
/// user-supplied file. Interned by the source registry (added with the coverage
/// report); the inner value is an opaque index, not a stable on-disk identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub u32);

impl SourceId {
    /// Placeholder id used while parsing, before the source registry (coverage
    /// report, S5) assigns a distinct id per source. Ordinary `parse`/`tokenize`
    /// stamp spans with this.
    pub const ANON: SourceId = SourceId(0);
}

/// A half-open byte range `[start, end)` within the text of `source`.
///
/// Offsets are **bytes**, not char indices, so they index directly into the source
/// `&str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub source: SourceId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// Requires: `start <= end`; both are byte offsets into `source`'s text.
    pub fn new(source: SourceId, start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { source, start, end }
    }

    /// The smallest span covering both `self` and `other` — used to span a
    /// statement from its first token to its last.
    ///
    /// Requires: `self.source == other.source`.
    /// Ensures: `result.start == min(starts)` and `result.end == max(ends)`.
    pub fn to(self, other: Span) -> Span {
        debug_assert_eq!(
            self.source, other.source,
            "cannot join spans from different sources"
        );
        Span {
            source: self.source,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// A value paired with the source [`Span`] it originated from.
///
/// Equality **ignores `span`**: two `SourceTagged` are equal iff their `node`s are
/// equal. This keeps structural comparisons (AST tests, token-stream checks)
/// independent of source position. `Hash` is intentionally not implemented; if
/// these ever key a map, add it ignoring `span`, to stay consistent with `eq`.
#[derive(Debug, Clone)]
pub struct SourceTagged<T> {
    pub span: Span,
    pub node: T,
}

impl<T> SourceTagged<T> {
    pub fn new(span: Span, node: T) -> Self {
        Self { span, node }
    }
}

impl<T: PartialEq> PartialEq for SourceTagged<T> {
    /// Ensures: ignores `span` — `a == b` ⇔ `a.node == b.node`.
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<T: Eq> Eq for SourceTagged<T> {}
