//! Facts and pending facts.
//!
//! The `Layer` distinction (Internal vs External) from PRD §2.3 is not stored
//! as a separate field; it is derived from `subject == asserter`. See
//! `Fact::is_internal` / `is_external`.

use crate::id::{EntityId, TxId};
use crate::predicates::OrderingKey;
use crate::term::{Predicate, Term};
use crate::time::{Interval, Timestamp};

/// A committed fact: triple + asserter + bitemporal metadata + optional
/// ordering key (PRD §2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub subject: EntityId,
    pub predicate: Predicate,
    pub object: Term,
    /// The entity that asserted this fact. Equal to `subject` for internal
    /// facts; some other entity for external assertions.
    pub asserter: EntityId,
    pub valid: Interval,
    pub asserted: Interval,
    /// Set iff this fact was asserted under `Cardinality::Indexed`.
    pub index_key: Option<OrderingKey>,
    pub tx_id: TxId,
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
}

/// A fact in flight before commit. The `asserted` interval is set by the
/// store at commit time, so it is absent here. The valid interval may be
/// `None`; in that case the store substitutes `Interval::new_open(commit_time)`.
///
/// `PendingFact` is cardinality-uniform: the `OrderingKey` for the Indexed
/// cardinality is supplied as a separate argument to `FactStore::assert_indexed`,
/// not carried here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingFact {
    pub subject: EntityId,
    pub predicate: Predicate,
    pub object: Term,
    pub asserter: EntityId,
    pub valid: Option<Interval>,
}

impl PendingFact {
    /// Materialize this `PendingFact` as a committed `Fact` at `commit_time`.
    ///
    /// `index_key` is `Some(_)` for `Cardinality::Indexed`, `None` otherwise.
    /// `asserted` is set to `Interval::new_open(commit_time)`. `valid` is
    /// taken as-is when `Some`, otherwise defaulted to
    /// `Interval::new_open(commit_time)`.
    ///
    /// Consumes `self` because the typical caller (the commit path) has no
    /// further use for the `PendingFact` after materializing.
    pub fn into_fact(
        self,
        commit_time: Timestamp,
        tx_id: TxId,
        index_key: Option<OrderingKey>,
    ) -> Fact {
        Fact {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Predicate;
    use crate::time::{Interval, Timestamp};

    fn fact_at(subject: EntityId, asserter: EntityId) -> Fact {
        Fact {
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
    fn is_internal_when_subject_equals_asserter() {
        let f = fact_at(EntityId(1), EntityId(1));
        assert!(f.is_internal());
        assert!(!f.is_external());
    }

    #[test]
    fn is_external_when_subject_differs_from_asserter() {
        let f = fact_at(EntityId(1), EntityId(2));
        assert!(!f.is_internal());
        assert!(f.is_external());
    }
}
