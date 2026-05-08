//! Parity test suite: every behaviour verified for `InMemoryTripleStore` is
//! verified identically for `DurableFileTripleStore`.
//!
//! `run_suite` is parameterised over a store factory. Tests verify semantic
//! outcomes (which `CommitChange` variant, what the query returns) without
//! checking exact timestamps, so `SystemClock` is sufficient.

use triple_store::change::{ChangeEvent, ChangeKind};
use triple_store::fact::{Fact, StoredFact};
use triple_store::id::EntityId;
use triple_store::in_memory_store::InMemoryTripleStore;
use triple_store::pattern::{Match, MatchAsserter, Pattern};
use triple_store::predicates::OrderingKey;
use triple_store::storage::CommitChange;
use triple_store::term::{Predicate, Term};
use triple_store::transaction::TxError;
use triple_store::triple_store::TripleStore;
use triple_store::DurableFileTripleStore;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn mk_fact(subject: u64, asserter: u64, value: i64) -> Fact {
    Fact {
        subject: EntityId(subject),
        predicate: Predicate(0),
        object: Term::Int(value),
        asserter: EntityId(asserter),
        valid: None,
    }
}

fn key(v: f64) -> OrderingKey {
    OrderingKey::new(v).unwrap()
}

fn any_pattern() -> Pattern {
    Pattern {
        subject: Match::Any,
        predicate: Match::Any,
        object: Match::Any,
        asserter: MatchAsserter::Any,
    }
}

fn drain(sub: &triple_store::change::Subscription) -> Vec<ChangeEvent> {
    let mut out = Vec::new();
    while let Ok(Some(e)) = sub.try_recv() {
        out.push(e);
    }
    out
}

// ── Parity suite ─────────────────────────────────────────────────────────────

/// Run the full parity suite against a store produced by `make`.
///
/// `make` is called once per logical test case and must return a fresh,
/// empty store each time.
fn run_suite<F>(make: F)
where
    F: Fn() -> Box<dyn TripleStore<StoredFact>>,
{
    // ── Transaction lifecycle ────────────────────────────────────────────

    {
        let s = make();
        let tx = s.new_transaction();
        assert!(tx.commit().unwrap().is_empty(), "empty tx yields no changes");
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.rollback();
        // Rollback: a fresh assert on the same key should yield Asserted (not Updated).
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        let changes = tx2.commit().unwrap();
        assert!(
            matches!(&changes[0], CommitChange::Asserted { .. }),
            "after rollback the slot is empty"
        );
    }

    {
        let s = make();
        let mut tx_a = s.new_transaction();
        let mut tx_b = s.new_transaction();
        tx_a.assert_single(mk_fact(1, 1, 10));
        tx_b.assert_single(mk_fact(2, 2, 20));
        let ca = tx_a.commit().unwrap();
        let cb = tx_b.commit().unwrap();
        assert_eq!(ca.len(), 1);
        assert_eq!(cb.len(), 1);
    }

    // ── Single cardinality ───────────────────────────────────────────────

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_single(mk_fact(1, 1, 42));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], CommitChange::Asserted { fact } if fact.object == Term::Int(42))
        );
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();

        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 43));
        let changes = tx2.commit().unwrap();
        assert_eq!(changes.len(), 1);
        let CommitChange::Updated { closed, opened } = &changes[0] else {
            panic!("expected Updated, got {:?}", changes[0]);
        };
        assert_eq!(closed.object, Term::Int(42));
        assert_eq!(opened.object, Term::Int(43));
        assert!(closed.asserted.end.is_some(), "prior fact must be closed");
        assert!(opened.asserted.is_open(), "new fact must be open");
    }

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_single(mk_fact(1, 1, 42));
        tx.assert_single(mk_fact(1, 1, 43));
        tx.assert_single(mk_fact(1, 1, 44));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], CommitChange::Asserted { fact } if fact.object == Term::Int(44))
        );
    }

    // ── Set cardinality ──────────────────────────────────────────────────

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 42));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], CommitChange::Asserted { .. }));
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_set(mk_fact(1, 1, 42));
        assert_eq!(tx1.commit().unwrap().len(), 1);
        let mut tx2 = s.new_transaction();
        tx2.assert_set(mk_fact(1, 1, 42));
        assert!(
            tx2.commit().unwrap().is_empty(),
            "re-assert of open Set fact must be no-op"
        );
    }

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|c| matches!(c, CommitChange::Asserted { .. })));
    }

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 42));
        tx.assert_set(mk_fact(1, 1, 42));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
    }

    // ── Indexed cardinality ──────────────────────────────────────────────

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_indexed(mk_fact(1, 1, 42), key(0.0));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], CommitChange::Asserted { fact } if fact.index_key == Some(key(0.0)))
        );
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_indexed(mk_fact(1, 1, 10), key(1.0));
        tx1.commit().unwrap();
        let mut tx2 = s.new_transaction();
        tx2.assert_indexed(mk_fact(1, 1, 20), key(1.0));
        let changes = tx2.commit().unwrap();
        assert!(matches!(&changes[0], CommitChange::Updated { .. }));
    }

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_indexed(mk_fact(1, 1, 10), key(0.0));
        tx.assert_indexed(mk_fact(1, 1, 20), key(1.0));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 2);
    }

    // ── Retract ──────────────────────────────────────────────────────────

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();
        let mut tx2 = s.new_transaction();
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        let changes = tx2.commit().unwrap();
        assert!(matches!(&changes[0], CommitChange::Retracted { closed } if closed.object == Term::Int(42)));
    }

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.retract_single(EntityId(1), Predicate(0), EntityId(1));
        assert_eq!(tx.commit().unwrap_err(), TxError::OpenFactNotFound);
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_set(mk_fact(1, 1, 10));
        tx1.assert_set(mk_fact(1, 1, 20));
        tx1.commit().unwrap();
        let mut tx2 = s.new_transaction();
        tx2.retract_set(EntityId(1), Predicate(0), EntityId(1), Term::Int(10));
        let changes = tx2.commit().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], CommitChange::Retracted { .. }));
        // object=20 still open
        let open = s.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].object, Term::Int(20));
    }

    // ── Query: query_open ────────────────────────────────────────────────

    {
        let s = make();
        assert!(s.query_open(EntityId(1), Predicate(0), None).is_empty());
    }

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        tx.assert_set(mk_fact(1, 1, 30));
        tx.commit().unwrap();
        assert_eq!(s.query_open(EntityId(1), Predicate(0), None).len(), 3);
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        tx2.commit().unwrap();
        let results = s.query_open(EntityId(1), Predicate(0), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(99));
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();
        let mut tx2 = s.new_transaction();
        tx2.retract_single(EntityId(1), Predicate(0), EntityId(1));
        tx2.commit().unwrap();
        assert!(s.query_open(EntityId(1), Predicate(0), None).is_empty());
    }

    // ── Query: history ───────────────────────────────────────────────────

    {
        let s = make();
        assert!(s.history(EntityId(1), Predicate(0), None).is_empty());
    }

    {
        let s = make();
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 42));
        tx1.commit().unwrap();
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(1, 1, 99));
        tx2.commit().unwrap();
        let all = s.history(EntityId(1), Predicate(0), None);
        assert_eq!(all.len(), 2);
        let objects: Vec<&Term> = all.iter().map(|f| &f.object).collect();
        assert!(objects.contains(&&Term::Int(42)));
        assert!(objects.contains(&&Term::Int(99)));
    }

    // ── Pattern filtering ────────────────────────────────────────────────

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        tx.commit().unwrap();
        let pat = Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Is(Term::Int(10)),
            asserter: MatchAsserter::Any,
        };
        let results = s.query_open(EntityId(1), Predicate(0), Some(&pat));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, Term::Int(10));
    }

    // ── Subscriptions ────────────────────────────────────────────────────

    {
        let s = make();
        let sub = s.subscribe(any_pattern(), None);
        let mut tx = s.new_transaction();
        tx.assert_single(mk_fact(1, 1, 42));
        tx.commit().unwrap();
        let events = drain(&sub);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0].kind, ChangeKind::Asserted { new_object } if *new_object == Term::Int(42)));
    }

    {
        let s = make();
        let sub = s.subscribe(any_pattern(), None);
        let tx = s.new_transaction();
        tx.commit().unwrap();
        assert!(drain(&sub).is_empty(), "empty commit must not dispatch");
    }

    {
        let s = make();
        let sub = s.subscribe(any_pattern(), None);
        let mut tx1 = s.new_transaction();
        tx1.assert_single(mk_fact(1, 1, 1));
        tx1.commit().unwrap();
        s.unsubscribe(sub.id());
        let mut tx2 = s.new_transaction();
        tx2.assert_single(mk_fact(2, 2, 2));
        tx2.commit().unwrap();
        // After unsubscribe the channel is closed once buffered events are drained.
        let _ = sub.try_recv(); // drain the one buffered event
        assert_eq!(sub.try_recv(), Err(triple_store::change::ChannelClosed));
    }

    // ── Multi-op transaction ─────────────────────────────────────────────

    {
        let s = make();
        let mut tx = s.new_transaction();
        tx.assert_set(mk_fact(1, 1, 10));
        tx.assert_set(mk_fact(1, 1, 20));
        tx.assert_set(mk_fact(2, 2, 30));
        let changes = tx.commit().unwrap();
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().all(|c| matches!(c, CommitChange::Asserted { .. })));
    }
}

// ── Test entry points ─────────────────────────────────────────────────────────

#[test]
fn in_memory_suite() {
    run_suite(|| Box::new(InMemoryTripleStore::new()));
}

#[test]
fn durable_suite() {
    run_suite(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.wal");
        // `dir` is dropped here but the file handle remains valid on Unix;
        // the directory is cleaned up once the file handle is also dropped.
        let store = DurableFileTripleStore::create(&path).unwrap();
        Box::new(store)
    });
}
