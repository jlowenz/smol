//! Change-event subscriptions (PRD §3.4).
//!
//! Subscribers register a [`Pattern`] with the store and receive a
//! [`Subscription`] handle. After every successful commit, the store
//! dispatches one event per committed change that matches the subscription's
//! pattern.
//!
//! `ChangeEvent` is a deliberately lightweight projection of the internal
//! `CommitChange<StoredFact>`: it exposes only what subscribers commonly need
//! (identity + new/old object + tx_id). Storage internals (timestamps, index
//! key, asserter) stay inside the storage layer; subscribers that need them
//! can re-query using `tx_id` as the cut.
//!
//! The underlying channel implementation (`async-channel`) is intentionally
//! not part of the public surface — `Subscription` is the only handle
//! subscribers see, and `ChannelClosed` is the only error they observe. This
//! preserves the option of swapping the channel later (e.g. for the future
//! custom-VM scheduler) without breaking callers.

use async_channel::{Receiver, Sender, TryRecvError};

use crate::id::{SubscriptionId, TxId};
use crate::pattern::Pattern;
use crate::term::{Predicate, Term};

/// One change delivered to subscribers after a successful commit.
///
/// `subject`, `predicate`, and `tx_id` identify *which* fact changed and in
/// what transaction; `kind` describes *how* it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeEvent {
    pub subject: crate::id::EntityId,
    pub predicate: Predicate,
    pub kind: ChangeKind,
    pub tx_id: TxId,
}

/// The shape of a single change.
///
/// `Updated` reports both sides so subscribers can distinguish "the value
/// changed from X to Y" from "a value was set to Y" without a separate
/// follow-up query. The PRD names the fields `old_object`/`new_object`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// New fact: no prior asserted-open fact existed at this identity key.
    Asserted { new_object: Term },
    /// The asserted-open fact at this identity key was superseded.
    /// Both objects are reported. The prior fact's `asserted` interval was
    /// closed at `commit_time` and the new one opened at the same instant.
    Updated { old_object: Term, new_object: Term },
    /// The asserted-open fact at this identity key was explicitly retracted.
    /// `old_object` is the value of the closed fact; the `valid` interval
    /// is preserved (audit semantics, not deletion).
    Retracted { old_object: Term },
}

/// The store's view of a registered subscription. Held in the registry; not
/// exposed to callers.
pub(crate) struct Subscriber {
    pub(crate) id: SubscriptionId,
    pub(crate) pattern: Pattern,
    pub(crate) sender: Sender<ChangeEvent>,
}

/// The only error the subscription API surfaces.
///
/// Returned when the underlying channel is closed (the store dropped the
/// sender, e.g. on store shutdown). Calling `try_recv` after `ChannelClosed`
/// will continue to return `ChannelClosed`.
#[derive(Debug, PartialEq, Eq)]
pub struct ChannelClosed;

impl core::fmt::Display for ChannelClosed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("change-event channel closed")
    }
}

impl std::error::Error for ChannelClosed {}

/// A live subscription handle.
///
/// Returned by [`InMemoryTripleStore::subscribe`]. Use one of the recv
/// methods to consume events, and [`Subscription::id`] when calling
/// `unsubscribe`. Dropping the handle ends event delivery (the store prunes
/// the entry lazily on the next dispatch); calling `unsubscribe` is the
/// explicit, immediate alternative.
///
/// [`InMemoryTripleStore::subscribe`]: crate::in_memory_store::InMemoryTripleStore::subscribe
pub struct Subscription {
    id: SubscriptionId,
    rx: Receiver<ChangeEvent>,
}

impl Subscription {
    pub(crate) fn new(id: SubscriptionId, rx: Receiver<ChangeEvent>) -> Self {
        Self { id, rx }
    }

    /// The store-assigned identifier. Pass to `unsubscribe` for explicit
    /// cleanup.
    pub fn id(&self) -> SubscriptionId {
        self.id
    }

    /// Async receive: suspends the current task until an event arrives or
    /// the channel closes.
    ///
    /// Requires: an async runtime is driving the calling future.
    pub async fn recv(&self) -> Result<ChangeEvent, ChannelClosed> {
        self.rx.recv().await.map_err(|_| ChannelClosed)
    }

    /// Blocking receive: parks the current thread until an event arrives or
    /// the channel closes.
    ///
    /// Requires: not called from inside an async runtime task (would block
    /// the executor thread).
    pub fn recv_blocking(&self) -> Result<ChangeEvent, ChannelClosed> {
        self.rx.recv_blocking().map_err(|_| ChannelClosed)
    }

    /// Non-blocking receive.
    ///
    /// Returns `Ok(Some(event))` if an event was available, `Ok(None)` if
    /// the channel is currently empty but still open, and `Err(ChannelClosed)`
    /// if the store has closed the channel.
    pub fn try_recv(&self) -> Result<Option<ChangeEvent>, ChannelClosed> {
        match self.rx.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Closed) => Err(ChannelClosed),
        }
    }
}
