//! Storage trait — the committed-fact persistence abstraction.
//!
//! `Storage<TFact>` is the lowest layer: it holds committed facts and provides
//! atomic bucket-level operations. It knows nothing about transactions,
//! cardinality, or tentative state — those concerns belong to `Transaction`.
//!
//! The three write operations correspond to the three commit outcomes:
//! - **`write`**: new fact, no prior open fact at its identity key (`Asserted`)
//! - **`supersede`**: prior open fact closed, replacement appended (`Updated`)
//! - **`close`**: prior open fact closed, no replacement (`Retracted`)

use crate::id::EntityId;
use crate::term::Predicate;
use crate::time::Timestamp;

/// One change produced when a transaction commits.
///
/// Reported to subscribers via the change-event channel (PRD §3.4).
/// Generic over `TFact` so the storage layer does not hard-code the fact type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitChange<TFact> {
    /// A new fact was asserted with no prior asserted-open fact at its key.
    Asserted { fact: TFact },
    /// A prior asserted-open fact was superseded: its `asserted` interval was
    /// closed at `commit_time` and a new fact was opened.
    Updated { closed: TFact, opened: TFact },
    /// A prior asserted-open fact was explicitly retracted: its `asserted`
    /// interval was closed at `commit_time`. The `valid` interval is preserved.
    Retracted { closed: TFact },
}

/// Errors from storage-layer operations.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// No asserted-open fact matched the identity key in `supersede` or `close`.
    FactNotFound,
    /// Backend-internal failure the caller cannot resolve.
    Internal(&'static str),
}

/// The committed-fact persistence layer.
///
/// Implementations are responsible for holding committed facts and providing
/// atomic read-write access to `(subject, predicate)` buckets. The `find`
/// closures in `supersede` and `close` express the cardinality-specific
/// identity key; the implementation adds the `asserted.is_open()` constraint.
///
/// All write methods acquire and release a shard lock internally; callers
/// never hold storage locks directly.
pub trait Storage<TFact>: Send + Sync {
    /// Append `fact` to the `(subject, predicate)` bucket.
    ///
    /// Ensures: `fact` is visible to subsequent `with_facts` calls on the same key.
    fn write(&self, subject: EntityId, predicate: Predicate, fact: TFact);

    /// Close the asserted interval of the first asserted-open fact matching
    /// `find`, then append `replacement`. Returns the now-closed prior fact.
    ///
    /// The prior fact's `asserted.end` is set to `replacement.asserted.start`
    /// so the two intervals meet with no gap.
    ///
    /// Requires: an asserted-open fact matching `find` exists in the bucket.
    /// Ensures: prior `asserted.end` is set; `replacement` is appended.
    fn supersede<F>(
        &self,
        subject: EntityId,
        predicate: Predicate,
        find: F,
        replacement: TFact,
    ) -> Result<TFact, Error>
    where
        F: Fn(&TFact) -> bool;

    /// Close the asserted interval of the first asserted-open fact matching
    /// `find` at timestamp `at`. Returns the now-closed fact.
    ///
    /// Requires: an asserted-open fact matching `find` exists in the bucket.
    /// Ensures: `fact.asserted.end == Some(at)`; `valid` interval is untouched.
    fn close<F>(
        &self,
        subject: EntityId,
        predicate: Predicate,
        find: F,
        at: Timestamp,
    ) -> Result<TFact, Error>
    where
        F: Fn(&TFact) -> bool;

    /// Read-only access to all facts (open and closed) in the
    /// `(subject, predicate)` bucket.
    ///
    /// Callers filter by `asserted.is_open()` for current state or by both
    /// `asserted` and `valid` intervals for bitemporal point-in-time queries.
    fn with_facts<F, R>(&self, subject: EntityId, predicate: Predicate, f: F) -> R
    where
        F: FnOnce(&[TFact]) -> R;
}
