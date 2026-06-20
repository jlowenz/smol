//! Test-support types for exercising error paths in the store and entity loop.
//!
//! [`AlwaysFailingStore`] is a `TripleStore<StoredFact>` whose transactions
//! always return `Err(TxError::Internal(...))` on commit.  Use it via
//! `Store::new(Arc::new(AlwaysFailingStore))` to drive commit-error branches
//! in the entity loop and store bridge without requiring disk-level failures.

use std::sync::Arc;

use crate::change::{ChangeEvent, EventFilter, Subscription};
use crate::fact::StoredFact;
use crate::id::{EntityId, SubscriptionId, TxId};
use crate::pattern::Pattern;
use crate::queryable::Queryable;
use crate::storage::CommitChange;
use crate::term::Predicate;
use crate::time::Timestamp;
use crate::transaction::{Applier, TentativeOp, Transaction, TxError};
use crate::triple_store::TripleStore;
use crate::value_store::StoredValue;

struct FailingApplier;

impl Applier<StoredFact> for FailingApplier {
    fn apply(
        &self,
        _tx_id: TxId,
        _ops: Vec<TentativeOp>,
        _values: Vec<(EntityId, StoredValue)>,
    ) -> Result<Vec<CommitChange<StoredFact>>, TxError> {
        Err(TxError::Internal("test: always-failing commit"))
    }
}

/// A `TripleStore<StoredFact>` whose transactions always fail on commit.
///
/// Every buffered operation is accepted silently; `commit()` returns
/// `Err(TxError::Internal("test: always-failing commit"))`.  Queries return
/// empty results; subscriptions are immediately closed.
pub struct AlwaysFailingStore;

impl Queryable<StoredFact> for AlwaysFailingStore {
    fn query_open(&self, _: EntityId, _: Predicate, _: Option<&Pattern>) -> Vec<StoredFact> {
        vec![]
    }

    fn query_at(
        &self,
        _: EntityId,
        _: Predicate,
        _: Timestamp,
        _: Timestamp,
        _: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        vec![]
    }

    fn history(&self, _: EntityId, _: Predicate, _: Option<&Pattern>) -> Vec<StoredFact> {
        vec![]
    }
}

impl TripleStore<StoredFact> for AlwaysFailingStore {
    fn new_transaction(&self) -> Transaction<StoredFact> {
        Transaction::new(Arc::new(FailingApplier), TxId(0))
    }

    fn alloc_tx_id(&self) -> TxId {
        TxId(0)
    }

    fn new_transaction_with_id(&self, tx_id: TxId) -> Transaction<StoredFact> {
        Transaction::new(Arc::new(FailingApplier), tx_id)
    }

    fn now(&self) -> Timestamp {
        Timestamp(0)
    }

    fn subscribe(&self, _: Pattern, _: Option<EventFilter>) -> Subscription {
        let (_, rx) = async_channel::unbounded::<ChangeEvent>();
        Subscription::new(SubscriptionId(0), rx)
    }

    fn unsubscribe(&self, _: SubscriptionId) {}

    fn lookup_value(&self, _: EntityId) -> Option<Arc<StoredValue>> {
        None
    }
}
