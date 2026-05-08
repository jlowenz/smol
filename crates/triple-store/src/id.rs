//! Stable, opaque identifiers for entities, transactions, and subscriptions.
//!
//! Production ID generation is the runtime's responsibility (PRD §8.6); the
//! store does not allocate `EntityId`s itself. Tests use a simple monotonic
//! generator in test harness code.

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId(pub u64);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TxId(pub u64);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubscriptionId(pub u64);
