//! Query patterns and bitemporal point-in-time selection (PRD §2.6).

use crate::fact::Layer;
use crate::id::EntityId;
use crate::term::{Predicate, Term};
use crate::time::Timestamp;

/// Pattern element matcher. `Match::Is(_)` is exact-match in v0; combinators
/// (`And`, `Or`, `Not`, `Range`, `IsOneOf`) are deferred to v1+.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match<T> {
    Any,
    Is(T),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub subject: Match<EntityId>,
    pub predicate: Match<Predicate>,
    pub object: Match<Term>,
    /// `None` matches both layers.
    pub layer: Option<Layer>,
}

/// Bitemporal point-in-time selection (PRD §2.6).
/// `valid_time = None` means "now"; `tx_time = None` means "latest".
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct AsOf {
    pub valid_time: Option<Timestamp>,
    pub tx_time: Option<Timestamp>,
}
