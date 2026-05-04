//! Core fact types.
//!
//! `Fact` is the domain assertion — what a caller claims about the world.
//! `StoredFact` is the committed record — `Fact` content plus store metadata
//! added at commit time (asserted interval, transaction id, index key).
//!
//! The `Layer` distinction (Internal vs External, PRD §2.3) is derived from
//! `subject == asserter`; see `is_internal` / `is_external`.

use crate::id::{EntityId, TxId};
use crate::predicates::OrderingKey;
use crate::term::{Predicate, Term};
use crate::time::{Interval, Timestamp};

/// A fact asserted about the world.
///
/// Carries the semantic content of an assertion: who is asserting what about
/// whom, and over what valid-time interval. The `asserted` interval is absent
/// here — it is set by the store at commit time.
///
/// `valid` is the interval during which the asserter claims this fact is true
/// in the modeled domain. `None` means "open from commit time" — the store
/// substitutes `Interval::new_open(commit_time)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub subject: EntityId,
    pub predicate: Predicate,
    pub object: Term,
    /// The entity making this assertion. Equal to `subject` for internal facts;
    /// a different entity for external assertions.
    pub asserter: EntityId,
    pub valid: Option<Interval>,
}

impl Fact {
    /// True iff this is an internal fact (`subject == asserter`).
    pub fn is_internal(&self) -> bool {
        self.subject == self.asserter
    }

    /// True iff this is an external assertion (`subject != asserter`).
    pub fn is_external(&self) -> bool {
        self.subject != self.asserter
    }

    /// Materialize this `Fact` as a `StoredFact` at commit time.
    ///
    /// `asserted` opens at `commit_time`. `valid` defaults to
    /// `Interval::new_open(commit_time)` when `None`.
    /// `index_key` is `Some(_)` for `Cardinality::Indexed`, `None` otherwise.
    ///
    /// Consumes `self`: the typical caller (the commit path) has no further
    /// use for the `Fact` after materializing it.
    pub fn into_stored(
        self,
        commit_time: Timestamp,
        tx_id: TxId,
        index_key: Option<OrderingKey>,
    ) -> StoredFact {
        StoredFact {
            subject: self.subject,
            predicate: self.predicate,
            object: self.object,
            asserter: self.asserter,
            valid: self
                .valid
                .unwrap_or_else(|| Interval::new_open(commit_time)),
            asserted: Interval::new_open(commit_time),
            index_key,
            tx_id,
        }
    }
}

/// A committed fact: domain content plus storage metadata (PRD §2.2).
///
/// `valid` and `asserted` are independent bitemporal intervals:
/// - `valid`: when the fact is true in the modeled domain (domain knowledge).
/// - `asserted`: when this record exists as live knowledge in the store.
///   Opened at commit time; closed when the fact is superseded or retracted.
///
/// A fact is true at time `t` only when `asserted.contains(t) AND valid.contains(t)`.
/// Asserted intervals for the same identity key never overlap — consecutive
/// intervals meet at their shared boundary (`close.end == next.start`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFact {
    pub subject: EntityId,
    pub predicate: Predicate,
    pub object: Term,
    pub asserter: EntityId,
    /// The valid-time interval claimed by the asserter. Set at assert time;
    /// never modified by the store after commit.
    pub valid: Interval,
    /// The assert-time interval: when this record is live in the store.
    /// Opened at commit; closed by supersession or retraction.
    pub asserted: Interval,
    /// Set iff this fact was committed under `Cardinality::Indexed`.
    pub index_key: Option<OrderingKey>,
    pub tx_id: TxId,
}

impl StoredFact {
    /// True iff this is an internal fact (`subject == asserter`).
    pub fn is_internal(&self) -> bool {
        self.subject == self.asserter
    }

    /// True iff this is an external assertion (`subject != asserter`).
    pub fn is_external(&self) -> bool {
        self.subject != self.asserter
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    fn stored_at(subject: EntityId, asserter: EntityId) -> StoredFact {
        StoredFact {
            subject,
            predicate: Predicate(0),
            object: Term::Int(0),
            asserter,
            valid: Interval::new_open(Timestamp::from_nanos(0)),
            asserted: Interval::new_open(Timestamp::from_nanos(0)),
            index_key: None,
            tx_id: TxId(0),
        }
    }

    #[test]
    fn stored_fact_is_internal_when_subject_equals_asserter() {
        let f = stored_at(EntityId(1), EntityId(1));
        assert!(f.is_internal());
        assert!(!f.is_external());
    }

    #[test]
    fn stored_fact_is_external_when_subject_differs_from_asserter() {
        let f = stored_at(EntityId(1), EntityId(2));
        assert!(!f.is_internal());
        assert!(f.is_external());
    }

    #[test]
    fn fact_is_internal_when_subject_equals_asserter() {
        let f = Fact {
            subject: EntityId(1),
            predicate: Predicate(0),
            object: Term::Int(0),
            asserter: EntityId(1),
            valid: None,
        };
        assert!(f.is_internal());
    }

    #[test]
    fn into_stored_resolves_none_valid_to_commit_time() {
        let f = Fact {
            subject: EntityId(1),
            predicate: Predicate(0),
            object: Term::Int(42),
            asserter: EntityId(1),
            valid: None,
        };
        let t = Timestamp::from_nanos(100);
        let sf = f.into_stored(t, TxId(1), None);
        assert_eq!(sf.valid.start, t);
        assert!(sf.valid.is_open());
        assert_eq!(sf.asserted.start, t);
        assert!(sf.asserted.is_open());
    }

    #[test]
    fn into_stored_preserves_explicit_valid_interval() {
        let valid = Interval::new_open(Timestamp::from_nanos(50));
        let f = Fact {
            subject: EntityId(1),
            predicate: Predicate(0),
            object: Term::Int(42),
            asserter: EntityId(1),
            valid: Some(valid),
        };
        let sf = f.into_stored(Timestamp::from_nanos(100), TxId(1), None);
        assert_eq!(sf.valid.start, Timestamp::from_nanos(50));
    }
}
