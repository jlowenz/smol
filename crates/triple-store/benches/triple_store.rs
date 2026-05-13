use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

use triple_store::DurableFileTripleStore;
use triple_store::exec_context::ExecContext;
use triple_store::fact::Fact;
use triple_store::id::EntityId;
use triple_store::in_memory_store::InMemoryTripleStore;
use triple_store::predicate_registry::Visibility;
use triple_store::predicates::Cardinality;
use triple_store::store::Store;
use triple_store::term::{Predicate, Term};
use triple_store::time::{Clock, Timestamp};
use triple_store::queryable::Queryable;
use triple_store::triple_store::TripleStore;

// Auto-incrementing clock: each `now()` returns a strictly increasing
// nanosecond so commit timestamps are unique without requiring manual
// clock management. Eliminates system-call noise from `SystemClock`.
struct TickClock(AtomicU64);

fn tick_clock() -> Arc<dyn Clock> {
    Arc::new(TickClock(AtomicU64::new(1)))
}

impl Clock for TickClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_nanos(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

fn mk_fact(subject: u64, value: i64) -> Fact {
    Fact {
        subject: EntityId(subject),
        predicate: Predicate(0),
        object: Term::Int(value),
        asserter: EntityId(subject),
        valid: None,
    }
}

// ── in_memory/assert ──────────────────────────────────────────────────────────

fn bench_in_memory_assert(c: &mut Criterion) {
    let mut group = c.benchmark_group("in_memory/assert");

    for &n in &[1usize, 100, 1_000] {
        // N distinct-subject facts committed in one transaction: pure insert throughput.
        group.bench_with_input(BenchmarkId::new("assert_single", n), &n, |b, &n| {
            b.iter_batched(
                || InMemoryTripleStore::with_clock(tick_clock()),
                |store| {
                    let mut tx = store.new_transaction();
                    for i in 0..n as u64 {
                        tx.assert_single(mk_fact(i, i as i64));
                    }
                    tx.commit().unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });

        // N separate single-fact transactions to the same key: supersede throughput.
        // The first commit is Asserted; each subsequent commit is Updated (supersede path).
        group.bench_with_input(
            BenchmarkId::new("assert_single_supersede", n),
            &n,
            |b, &n| {
                b.iter_batched(
                    || InMemoryTripleStore::with_clock(tick_clock()),
                    |store| {
                        for v in 0..n as i64 {
                            let mut tx = store.new_transaction();
                            tx.assert_single(mk_fact(0, v));
                            tx.commit().unwrap();
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

// ── in_memory/query ───────────────────────────────────────────────────────────

fn bench_in_memory_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("in_memory/query");

    // Shared store: 10_000 distinct-subject facts, one open fact per subject.
    let store = {
        let s = InMemoryTripleStore::with_clock(tick_clock());
        let mut tx = s.new_transaction();
        for i in 0..10_000u64 {
            tx.assert_single(mk_fact(i, i as i64));
        }
        tx.commit().unwrap();
        s
    };

    // Subject 500 has one open fact.
    group.bench_function("query_open_hit", |b| {
        b.iter(|| black_box(store.query_open(EntityId(500), Predicate(0), None)));
    });

    // Subject 20_000 has no facts.
    group.bench_function("query_open_miss", |b| {
        b.iter(|| black_box(store.query_open(EntityId(20_000), Predicate(0), None)));
    });

    // Call history once per subject across all 10_000 subjects.
    group.bench_function("history_10k", |b| {
        b.iter(|| {
            for i in 0..10_000u64 {
                black_box(store.history(EntityId(i), Predicate(0), None));
            }
        });
    });

    group.finish();
}

// ── durable/assert ────────────────────────────────────────────────────────────

fn bench_durable_assert(c: &mut Criterion) {
    let mut group = c.benchmark_group("durable/assert");

    // Single-fact transaction: encode + write + fsync latency.
    group.bench_function("durable_assert_single", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                let path = dir.path().join("wal.log");
                let store =
                    DurableFileTripleStore::create_with_clock(&path, tick_clock()).unwrap();
                (dir, store)
            },
            |(dir, store)| {
                let mut tx = store.new_transaction();
                tx.assert_single(mk_fact(1, 42));
                tx.commit().unwrap();
                drop(store);
                drop(dir);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // 100-fact transaction committed as one WAL record.
    group.bench_function("durable_assert_batch_100", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                let path = dir.path().join("wal.log");
                let store =
                    DurableFileTripleStore::create_with_clock(&path, tick_clock()).unwrap();
                (dir, store)
            },
            |(dir, store)| {
                let mut tx = store.new_transaction();
                for i in 0..100u64 {
                    tx.assert_single(mk_fact(i, i as i64));
                }
                tx.commit().unwrap();
                drop(store);
                drop(dir);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── durable/replay ────────────────────────────────────────────────────────────

fn bench_durable_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("durable/replay");

    for &n in &[100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("replay", n), &n, |b, &n| {
            // Build a template WAL with N committed facts once, outside the
            // timed region. Each benchmark iteration copies the template so
            // `open` always replays a fresh file without paying fsync cost
            // in setup.
            let template_dir = TempDir::new().unwrap();
            let template_path = template_dir.path().join("template.log");
            {
                let store =
                    DurableFileTripleStore::create_with_clock(&template_path, tick_clock())
                        .unwrap();
                for i in 0..n as u64 {
                    let mut tx = store.new_transaction();
                    tx.assert_single(mk_fact(i, i as i64));
                    tx.commit().unwrap();
                }
            }

            b.iter_batched(
                || {
                    let dir = TempDir::new().unwrap();
                    let path = dir.path().join("wal.log");
                    std::fs::copy(&template_path, &path).unwrap();
                    (dir, path)
                },
                |(dir, path)| {
                    // Timed region: full log replay.
                    let _store =
                        DurableFileTripleStore::open_with_clock(&path, tick_clock()).unwrap();
                    drop(_store);
                    drop(path);
                    drop(dir);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ── store/overhead ────────────────────────────────────────────────────────────

fn bench_store_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("store/overhead");

    // 10 distinct predicate declarations: ArcSwap publish path × 10.
    group.bench_function("store_declare_10", |b| {
        b.iter_batched(
            || Store::new(Arc::new(InMemoryTripleStore::with_clock(tick_clock()))),
            |store| {
                for i in 0..10u32 {
                    store
                        .declare(&format!("pred.{i}"), Visibility::Public, Cardinality::Single)
                        .unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // One assert_single through the Store facade: access check + transaction round-trip.
    group.bench_function("store_assert_single", |b| {
        let store = Store::new(Arc::new(InMemoryTripleStore::with_clock(tick_clock())));
        let pred = store
            .declare("bench.pred", Visibility::Public, Cardinality::Single)
            .unwrap();
        let ctx = ExecContext::System;
        let f = Fact {
            subject: EntityId(1),
            predicate: pred,
            object: Term::Int(42),
            asserter: EntityId(1),
            valid: None,
        };
        // Steady state: each iteration supersedes the previous fact (Updated path).
        b.iter(|| store.assert_single(&ctx, f.clone()).unwrap());
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_in_memory_assert,
    bench_in_memory_query,
    bench_durable_assert,
    bench_durable_replay,
    bench_store_overhead,
);
criterion_main!(benches);
