//! Durable, file-backed `TripleStore<StoredFact>` implementation.
//!
//! All committed transactions are persisted to an append-only write-ahead log.
//! On `open`, the log is replayed in full to rebuild in-memory state. The WAL
//! record format is:
//!
//! ```text
//! [4-byte payload length, little-endian u32]
//! [bincode-encoded Vec<CommitChange<StoredFact>>]
//! ```
//!
//! Empty commits write no record. A truncated last record (crash mid-write)
//! is detected and silently skipped; earlier records are intact and replayed.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::change::{ChangeEvent, Subscriber, Subscription};
use crate::fact::StoredFact;
use crate::id::{EntityId, SubscriptionId, TxId};
use crate::in_memory_store::{InMemoryStorage, collapse_and_apply, dispatch_changes};
use crate::pattern::Pattern;
use crate::queryable::Queryable;
use crate::storage::{CommitChange, Storage};
use crate::term::Predicate;
use crate::time::{Clock, SystemClock, Timestamp};
use crate::transaction::{Applier, TentativeOp, TxError};
use crate::triple_store::TripleStore;

const DEFAULT_SHARD_COUNT: usize = 16;

// ── DurableInner ─────────────────────────────────────────────────────────────

pub(crate) struct DurableInner {
    storage: InMemoryStorage,
    clock: Arc<dyn Clock>,
    next_tx: AtomicU64,
    next_subscription: AtomicU64,
    subscribers: RwLock<Vec<Subscriber>>,
    log: Mutex<BufWriter<File>>,
}

impl DurableInner {
    fn create(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Ok(Self {
            storage: InMemoryStorage::new(DEFAULT_SHARD_COUNT),
            clock,
            next_tx: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
            subscribers: RwLock::new(Vec::new()),
            log: Mutex::new(BufWriter::new(file)),
        })
    }

    fn open(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self> {
        let storage = InMemoryStorage::new(DEFAULT_SHARD_COUNT);
        let max_tx = replay_log(path, &storage)?;
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Self {
            storage,
            clock,
            next_tx: AtomicU64::new(max_tx + 1),
            next_subscription: AtomicU64::new(1),
            subscribers: RwLock::new(Vec::new()),
            log: Mutex::new(BufWriter::new(file)),
        })
    }
}

impl Applier<StoredFact> for DurableInner {
    /// Apply `ops` atomically to in-memory storage, persist to the WAL, then
    /// dispatch change events.
    ///
    /// Requires: `tx_id` is unique within this store instance.
    /// Ensures: if this returns `Ok`, the changes are durable on stable storage
    ///   and visible to subsequent queries.
    ///
    /// Note: if the WAL write fails after in-memory storage has been updated,
    ///   the in-memory state diverges from the durable log for this session.
    ///   The caller receives `Err`; the in-memory store retains the changes.
    ///   Durability is restored on restart via log replay.
    fn apply(
        &self,
        tx_id: TxId,
        ops: Vec<TentativeOp>,
    ) -> Result<Vec<CommitChange<StoredFact>>, TxError> {
        let changes = collapse_and_apply(tx_id, ops, &*self.clock, &self.storage)?;

        if !changes.is_empty() {
            let encoded = bincode::serialize(&changes)
                .map_err(|_| TxError::Internal("serialization failed"))?;
            let len = u32::try_from(encoded.len())
                .map_err(|_| TxError::Internal("record too large"))?;
            let mut log = self.log.lock();
            log.write_all(&len.to_le_bytes())
                .map_err(|_| TxError::Internal("log write failed"))?;
            log.write_all(&encoded)
                .map_err(|_| TxError::Internal("log write failed"))?;
            log.flush()
                .map_err(|_| TxError::Internal("log write failed"))?;
            log.get_mut()
                .sync_data()
                .map_err(|_| TxError::Internal("fsync failed"))?;
        }

        dispatch_changes(tx_id, &changes, &self.subscribers);
        Ok(changes)
    }
}

// ── WAL replay ────────────────────────────────────────────────────────────────

fn replay_log(path: &Path, storage: &InMemoryStorage) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let mut max_tx = 0u64;
    loop {
        let mut len_buf = [0u8; 4];
        match file.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let payload_len = u32::from_le_bytes(len_buf) as usize;
        let mut payload = vec![0u8; payload_len];
        match file.read_exact(&mut payload) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // Truncated last record — crash mid-write; skip.
                break;
            }
            Err(e) => return Err(e),
        }
        let changes: Vec<CommitChange<StoredFact>> = bincode::deserialize(&payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        for change in &changes {
            max_tx = max_tx.max(replay_change(change, storage));
        }
    }
    Ok(max_tx)
}

/// Apply one `CommitChange` to `storage` during WAL replay, using the stored
/// timestamps directly (not the clock). Returns the tx_id of the change.
fn replay_change(change: &CommitChange<StoredFact>, storage: &InMemoryStorage) -> u64 {
    match change {
        CommitChange::Asserted { fact } => {
            storage.write(fact.subject, fact.predicate, fact.clone());
            fact.tx_id.0
        }
        CommitChange::Updated { closed, opened } => {
            // `supersede` closes the matching open fact at `opened.asserted.start`
            // (which equals `closed.asserted.end`), then appends `opened`.
            let asserter = closed.asserter;
            let index_key = closed.index_key;
            match storage.supersede(
                closed.subject,
                closed.predicate,
                |f| f.asserter == asserter && f.index_key == index_key,
                opened.clone(),
            ) {
                Ok(_) => {}
                Err(_) => {
                    // Prior open fact not present (log inconsistency); write
                    // the opened fact directly so replay is at least complete.
                    storage.write(opened.subject, opened.predicate, opened.clone());
                }
            }
            opened.tx_id.0
        }
        CommitChange::Retracted { closed } => {
            let close_time = closed
                .asserted
                .end
                .expect("Retracted.closed must have asserted.end set");
            let asserter = closed.asserter;
            let object = closed.object.clone();
            let index_key = closed.index_key;
            let _ = storage.close(
                closed.subject,
                closed.predicate,
                |f| {
                    f.asserter == asserter
                        && f.object == object
                        && f.index_key == index_key
                },
                close_time,
            );
            closed.tx_id.0
        }
    }
}

// ── DurableFileTripleStore ────────────────────────────────────────────────────

/// A `TripleStore<StoredFact>` backed by an append-only write-ahead log.
///
/// All committed facts survive process restart. On `open`, the log is replayed
/// in full to rebuild in-memory state before new transactions are accepted.
///
/// Requires: at most one `DurableFileTripleStore` may open a given path at a
///   time. Concurrent access from multiple instances corrupts the log.
pub struct DurableFileTripleStore {
    inner: Arc<DurableInner>,
}

impl DurableFileTripleStore {
    /// Create a new store at `path` using `SystemClock`, failing if the file
    /// already exists.
    ///
    /// Ensures: the log file is created empty; the store is ready for use.
    pub fn create(path: &Path) -> io::Result<Self> {
        Self::create_with_clock(path, Arc::new(SystemClock))
    }

    /// Create a new store at `path` with an injected clock, failing if the
    /// file already exists.
    ///
    /// Ensures: the log file is created empty; the store is ready for use.
    pub fn create_with_clock(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(DurableInner::create(path, clock)?),
        })
    }

    /// Open an existing store at `path` using `SystemClock`, replaying the
    /// log to restore state.
    ///
    /// Ensures: all facts committed in prior sessions are queryable.
    ///   `TxId`s issued after `open` are strictly greater than all replayed ones.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_clock(path, Arc::new(SystemClock))
    }

    /// Open an existing store at `path` with an injected clock.
    ///
    /// Ensures: all facts committed in prior sessions are queryable.
    pub fn open_with_clock(path: &Path, clock: Arc<dyn Clock>) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(DurableInner::open(path, clock)?),
        })
    }

    /// Open the store at `path` if it exists; otherwise create a new one.
    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        if path.exists() {
            Self::open(path)
        } else {
            Self::create(path)
        }
    }
}

impl Queryable<StoredFact> for DurableFileTripleStore {
    fn query_open(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.inner.storage.query_open(subject, predicate, pattern)
    }

    fn query_at(
        &self,
        subject: EntityId,
        predicate: Predicate,
        tx_time: Timestamp,
        valid_time: Timestamp,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.inner
            .storage
            .query_at(subject, predicate, tx_time, valid_time, pattern)
    }

    fn history(
        &self,
        subject: EntityId,
        predicate: Predicate,
        pattern: Option<&Pattern>,
    ) -> Vec<StoredFact> {
        self.inner.storage.history(subject, predicate, pattern)
    }
}

impl TripleStore<StoredFact> for DurableFileTripleStore {
    fn new_transaction(&self) -> crate::transaction::Transaction<StoredFact> {
        let tx_id = TxId(self.inner.next_tx.fetch_add(1, Ordering::Relaxed));
        crate::transaction::Transaction::new(
            Arc::clone(&self.inner) as Arc<dyn Applier<StoredFact>>,
            tx_id,
        )
    }

    fn now(&self) -> Timestamp {
        self.inner.clock.now()
    }

    fn subscribe(
        &self,
        pattern: Pattern,
        filter: Option<Box<dyn Fn(&ChangeEvent) -> bool + Send + Sync>>,
    ) -> Subscription {
        let id = SubscriptionId(self.inner.next_subscription.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = async_channel::unbounded();
        self.inner.subscribers.write().push(Subscriber {
            id,
            pattern,
            sender,
            filter,
        });
        Subscription::new(id, receiver)
    }

    fn unsubscribe(&self, id: SubscriptionId) {
        self.inner.subscribers.write().retain(|s| s.id != id);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::*;
    use crate::fact::Fact;
    use crate::storage::CommitChange;
    use crate::term::Term;
    use crate::time::Timestamp;
    use crate::triple_store::TripleStore;

    // ── Test clock ──────────────────────────────────────────────────────────

    struct TickClock {
        nanos: AtomicU64,
    }

    impl TickClock {
        fn new(start: u64) -> Arc<Self> {
            Arc::new(Self {
                nanos: AtomicU64::new(start),
            })
        }

        fn set(&self, t: u64) {
            self.nanos.store(t, Ordering::Relaxed);
        }
    }

    impl Clock for TickClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_nanos(self.nanos.load(Ordering::Relaxed))
        }
    }

    fn mk_fact(subject: u64, asserter: u64, value: i64) -> Fact {
        Fact {
            subject: EntityId(subject),
            predicate: Predicate(0),
            object: Term::Int(value),
            asserter: EntityId(asserter),
            valid: None,
        }
    }

    // ── Durability-specific tests ────────────────────────────────────────────

    #[test]
    fn persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.wal");

        {
            let s = DurableFileTripleStore::create(&path).unwrap();
            let mut tx = s.new_transaction();
            tx.assert_single(mk_fact(1, 1, 42));
            tx.commit().unwrap();
        }

        let s2 = DurableFileTripleStore::open(&path).unwrap();
        let facts = s2.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].object, Term::Int(42));
    }

    #[test]
    fn tx_id_continuity_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.wal");

        let first_tx_id = {
            let s = DurableFileTripleStore::create(&path).unwrap();
            let mut tx = s.new_transaction();
            tx.assert_single(mk_fact(1, 1, 1));
            let changes = tx.commit().unwrap();
            match &changes[0] {
                CommitChange::Asserted { fact } => fact.tx_id,
                _ => panic!(),
            }
        };

        let s2 = DurableFileTripleStore::open(&path).unwrap();
        let mut tx2 = s2.new_transaction();
        tx2.assert_single(mk_fact(2, 2, 2));
        let changes2 = tx2.commit().unwrap();
        let second_tx_id = match &changes2[0] {
            CommitChange::Asserted { fact } => fact.tx_id,
            _ => panic!(),
        };

        assert!(
            second_tx_id > first_tx_id,
            "tx_id after reopen must exceed last replayed tx_id"
        );
    }

    #[test]
    fn truncated_last_record_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.wal");

        {
            let s = DurableFileTripleStore::create(&path).unwrap();
            let mut tx = s.new_transaction();
            tx.assert_single(mk_fact(1, 1, 42));
            tx.commit().unwrap();
        }

        // Append a valid length prefix but a truncated payload (simulated crash).
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&100u32.to_le_bytes()).unwrap(); // claims 100 bytes of payload
            f.write_all(&[0u8; 10]).unwrap(); // only 10 bytes written
        }

        let s2 = DurableFileTripleStore::open(&path).unwrap();
        let facts = s2.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(facts.len(), 1, "valid record must survive; truncated must be skipped");
        assert_eq!(facts[0].object, Term::Int(42));
    }

    #[test]
    fn empty_file_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.wal");
        let s = DurableFileTripleStore::create(&path).unwrap();
        drop(s);
        let s2 = DurableFileTripleStore::open(&path).unwrap();
        assert!(s2.query_open(EntityId(1), Predicate(0), None).is_empty());
    }

    #[test]
    fn updated_fact_replays_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.wal");

        {
            let clock = TickClock::new(100);
            let s = DurableFileTripleStore::create_with_clock(&path, clock.clone()).unwrap();
            let mut tx1 = s.new_transaction();
            tx1.assert_single(mk_fact(1, 1, 10));
            tx1.commit().unwrap();

            clock.set(200);
            let mut tx2 = s.new_transaction();
            tx2.assert_single(mk_fact(1, 1, 20));
            tx2.commit().unwrap();
        }

        let s2 = DurableFileTripleStore::open(&path).unwrap();
        let facts = s2.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].object, Term::Int(20), "replayed state must reflect last asserted value");

        let history = s2.history(EntityId(1), Predicate(0), None);
        assert_eq!(history.len(), 2, "full history must be preserved through replay");
    }

    #[test]
    fn retracted_fact_replays_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.wal");

        {
            let clock = TickClock::new(100);
            let s = DurableFileTripleStore::create_with_clock(&path, clock.clone()).unwrap();
            let mut tx1 = s.new_transaction();
            tx1.assert_single(mk_fact(1, 1, 42));
            tx1.commit().unwrap();

            clock.set(200);
            let mut tx2 = s.new_transaction();
            tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
            tx2.commit().unwrap();
        }

        let s2 = DurableFileTripleStore::open(&path).unwrap();
        assert!(
            s2.query_open(EntityId(1), Predicate(0), None).is_empty(),
            "retracted fact must not appear as open after replay"
        );
        assert_eq!(
            s2.history(EntityId(1), Predicate(0), None).len(),
            1,
            "retracted fact must still appear in history"
        );
    }
}
