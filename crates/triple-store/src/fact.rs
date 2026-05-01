//! Facts, pending facts, and layer membership.

use crate::id::{EntityId, TxId};
use crate::term::{Predicate, Term};
use crate::time::Interval;

/// Facts belong to a logical layer (PRD §2.3). External assertions carry the
/// asserter's identity as provenance; conflicts are preserved, not resolved.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    Internal,
    External { asserter: EntityId },
}

/// A committed fact: triple + bitemporal metadata + provenance (PRD §2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub subject: EntityId,
    pub predicate: Predicate,
    pub object: Term,
    pub valid: Interval,
    pub asserted: Interval,
    pub layer: Layer,
    pub tx_id: TxId,
}

/// A fact in flight before commit. The `asserted` interval is set by the
/// store at commit time, so it is absent here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingFact {
    pub subject: EntityId,
    pub predicate: Predicate,
    pub object: Term,
    pub valid: Interval,
    pub layer: Layer,
}
