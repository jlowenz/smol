//! Transaction context types for multi-entity atomic transactions (C31).
//!
//! A `TxContext` is the coordinator's view of an in-flight transaction at one
//! nesting level. The root level holds the shared store `TxId`; each nested
//! `atomic:` call creates a `Child` that links to its parent via `outer` and
//! owns its own op buffer. The whole tree commits under the root's `TxId`, so a
//! child commit merges (absorbs) its buffer into its parent and a child abort
//! simply drops its own buffer — independently of any sibling.
//!
//! The handle is opaque to Smol: it is carried as `Value::Handle(Arc<TxContext>)`
//! and downcast by the primitives that created it. There is deliberately no new
//! `Value` variant.
//!
//! See `docs/tx_design.md` and `docs/chunk_31_plan.md` (S1) for the full design.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use may::sync::Mutex;

use crate::entity::InstanceHandle;
use crate::value::Value;
use triple_store::id::TxId;
use triple_store::store::OwnedStoreTransaction;

/// Identifier for the shared store transaction (equal to `TxId.0`).
pub type SharedTxId = u64;

/// Process-unique identity for one transaction context (root or child). Used for
/// lock admission (ancestry checks) and as the node key in deadlock detection.
pub type TxCtxId = u64;

#[allow(dead_code)] // consumed by `_primBeginSharedTx` in S4
static NEXT_CTX_ID: AtomicU64 = AtomicU64::new(1);

/// Mint a fresh, process-unique `TxCtxId`.
#[allow(dead_code)] // consumed by `_primBeginSharedTx` in S4
pub(crate) fn next_ctx_id() -> TxCtxId {
    NEXT_CTX_ID.fetch_add(1, Ordering::Relaxed)
}

/// Coordinator state for one in-flight transaction level.
///
/// Carried as `Value::Handle(Arc<TxContext>)`. `Root` is the outermost
/// transaction; `Child` is a nested `atomic:` linked to its parent.
pub enum TxContext {
    Root(Arc<RootTxInner>),
    Child(Arc<ChildTxInner>),
}

/// State unique to the outermost transaction level.
pub struct RootTxInner {
    pub ctx_id: TxCtxId,
    /// The shared store `TxId` every participant and child writes under.
    pub tx_id: TxId,
    /// This level's op/value buffer. `None` after commit or rollback.
    pub tx: Mutex<Option<OwnedStoreTransaction>>,
    /// Participants enrolled at the root level (in enrollment order).
    pub enrolled: Mutex<Vec<InstanceHandle>>,
}

/// State for a nested transaction level.
pub struct ChildTxInner {
    pub ctx_id: TxCtxId,
    /// The enclosing context. Ancestry is a walk up this chain to the `Root`.
    pub outer: Arc<TxContext>,
    /// This child's own op/value buffer, opened under the root `TxId`. On commit
    /// it is absorbed into the parent's buffer; on abort it is dropped.
    pub tx: Mutex<Option<OwnedStoreTransaction>>,
    /// Participants enrolled at this child level (in enrollment order).
    pub participants: Mutex<Vec<InstanceHandle>>,
}

/// One frame of an entity's lock + shadow stack. Each frame is owned by the
/// context that pushed it and holds that level's speculative state for this
/// entity:
///   - `shadow` — in-memory named-ivar writes (mask the live slot; applied on
///     commit, discarded on abort);
///   - `tx` — durable store ops, buffered under the shared `TxId`. On commit it
///     merges into the frame below (or is handed to the coordinator's context
///     buffer at the last frame); on abort it is dropped (its ops never commit).
///
/// Keeping both in one frame makes the per-level push/merge/discard atomic — the
/// in-memory and durable speculative state can never desync.
pub struct TxFrame {
    pub ctx_id: TxCtxId,
    pub shadow: HashMap<Arc<str>, Value>,
    pub tx: OwnedStoreTransaction,
}

impl TxContext {
    /// This context's own identity.
    pub fn ctx_id(&self) -> TxCtxId {
        match self {
            TxContext::Root(r) => r.ctx_id,
            TxContext::Child(c) => c.ctx_id,
        }
    }

    /// The root level of this context's ancestry chain.
    pub fn root(&self) -> &RootTxInner {
        match self {
            TxContext::Root(r) => r,
            TxContext::Child(c) => c.outer.root(),
        }
    }

    /// The shared store `TxId` for the whole transaction tree.
    pub fn tx_id(&self) -> TxId {
        self.root().tx_id
    }

    /// This level's op/value buffer slot.
    pub fn buffer(&self) -> &Mutex<Option<OwnedStoreTransaction>> {
        match self {
            TxContext::Root(r) => &r.tx,
            TxContext::Child(c) => &c.tx,
        }
    }

    /// This level's participant list (`Root::enrolled` or `Child::participants`).
    pub fn participants(&self) -> &Mutex<Vec<InstanceHandle>> {
        match self {
            TxContext::Root(r) => &r.enrolled,
            TxContext::Child(c) => &c.participants,
        }
    }

    /// Whether `ctx_id` names this context or any of its ancestors.
    ///
    /// Ensures: `true` iff walking `self → outer → … → root` encounters `ctx_id`.
    ///   Used for lock admission: a tagged message from context `self` is
    ///   admitted by an entity whose top frame is `ctx_id` exactly when this
    ///   returns `true` (the holder is an ancestor-or-equal of the requester).
    pub fn has_ancestor_or_self(&self, ctx_id: TxCtxId) -> bool {
        let mut cur = self;
        loop {
            if cur.ctx_id() == ctx_id {
                return true;
            }
            match cur {
                TxContext::Root(_) => return false,
                TxContext::Child(c) => cur = &c.outer,
            }
        }
    }
}

impl std::fmt::Debug for TxContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxContext::Root(r) => write!(
                f,
                "TxContext::Root(ctx_id={}, tx_id={:?})",
                r.ctx_id, r.tx_id
            ),
            TxContext::Child(c) => write!(
                f,
                "TxContext::Child(ctx_id={}, outer={})",
                c.ctx_id,
                c.outer.ctx_id()
            ),
        }
    }
}
