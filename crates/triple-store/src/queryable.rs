//! Queryable trait — clock-free, read-only access to committed facts.
//!
//! `Queryable<TFact>` is implementable by both storage backends and the full
//! `TripleStore`. All methods take explicit `Timestamp` arguments so the trait
//! carries no clock dependency; callers that want `AsOf::None` resolution use
//! `TripleStore::query_as_of` instead.

use crate::fact::BitemporalFact;
use crate::id::EntityId;
use crate::pattern::Pattern;
use crate::term::Predicate;
use crate::time::Timestamp;

/// Clock-free, read-only query interface over committed facts.
///
/// All methods accept fully-resolved `Timestamp` arguments. The `TripleStore`
/// supertrait adds `query_as_of`, which resolves `AsOf::None` against the
/// store's clock before delegating here.
///
/// The optional `pattern` parameter filters on object and asserter fields;
/// `None` imposes no additional constraint beyond the `(subject, predicate)` key.
pub trait Queryable<TFact: BitemporalFact>: Send + Sync {
    /// All currently asserted-open facts at `(subject, predicate)`.
    ///
    /// Ensures: every returned fact `f` satisfies `f.asserted().is_open()`.
    fn query_open(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact>;

    /// Facts visible at the bitemporal point `(tx_time, valid_time)`.
    ///
    /// Ensures: every returned fact `f` satisfies
    ///   `f.asserted().contains(tx_time) && f.valid().contains(valid_time)`.
    fn query_at(
        &self,
        subject: EntityId,
        predicate: Predicate,
        tx_time: Timestamp,
        valid_time: Timestamp,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact>;

    /// Every fact ever committed at `(subject, predicate)`, open and closed.
    fn history(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<TFact>;
}
