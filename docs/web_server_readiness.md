# Web-server readiness — interpreter gap analysis

**Status:** planning / gap analysis. No VM/compiler assumed — this scopes what the
*current tree-walking interpreter* needs to host a fast web server, and treats that
server as a forcing function for correctness, capability, and performance work.

## Spike log

**2026-07-11 — async TCP spike (commit 64953ce).** First non-blocking network I/O.
`OsInterface` gained `tcp_listen/accept/read/write/close` over `may::net`
(coroutine-parking, not OS-thread-blocking); prims `_primTcpListen:` (class-side)
and `_primTcpAccept:/Read:/Write:with:/Close:` (value-side, returning Futures
resolved from fresh I/O coroutines); kernel `Value` classes `TcpListener`/
`TcpSocket`. A Smol server (bind → accept → read → write → close) serves a real
`std::net` client end-to-end. **Confirmed first wall = string processing** (as
predicted below): no CRLF in a string literal, so the response is not yet real
HTTP; `read` resolves to a utf8-lossy `Str`. Still to do before load-testing:
concurrent accept-loop, HTTP parsing, and a byte-buffer/stream + CRLF-capable
strings. Load testing (where GC and value-object churn should dominate) is the next
increment.

**Why a web server as the driver.** It stresses exactly the three things the
interpreter has not been pushed on: sustained concurrency, real non-blocking I/O,
and allocation churn in a long-lived process. It is also the best possible
profiled workload to hand the eventual VM design, so work spiked now is reusable.

---

## Baseline — what exists today (grounded in the tree)

| Area                     | State                                                                                                                             | Reference                             |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------- |
| Concurrency              | `may` green threads, per-entity mailboxes, reduction-counter cooperative scheduling, Futures                                      | `entity.rs`, CLAUDE.md C29.5/C31.5    |
| Per-coroutine Rust stack | Bounded, independent of Smol nesting; debug 256 KB / release 64 KB per entity                                                     | CLAUDE.md C31.5                       |
| I/O surface              | **Timers only** (`schedule_timer`/`cancel_timer`/`timer_pending`). No sockets/files/network.                                      | `os_interface.rs`                     |
| Entry point              | stdin/stdout REPL                                                                                                                 | `main.rs`                             |
| Types                    | Int, LargeInt (BigInt), Float, Str, ByteArray, Array, OrderedCollection, Dictionary (open-addressing hash map), Association, Cons | `kernel.smol`, `value.rs`             |
| String algorithms        | Near-zero: `copyFrom:to:`, `includesSubString:`, `asString`                                                                       | `builtins_iterative.rs`               |
| GC                       | **None.** Entities and value objects live forever; `value_objects` registry grows per value created.                              | CLAUDE.md, `open_questions.md` Q3/Q11 |
| Condition system         | Complete (async errors, handler-class disposition, restarts)                                                                      | `docs/condition_system_guide.md`      |

---

## The two hard blockers

### B1. Networking (capability)

No non-blocking socket primitives. `may` ships its own I/O reactor (`may::net`), so
the path is: extend `OsInterface` with TCP `listen`/`accept`/`read`/`write` that
**yield to the scheduler and never block the OS worker thread**, surfaced to Smol as
`Socket`/`Stream` classes returning Futures. TLS (HTTPS) is a separate, larger lift
(crypto + handshake state machine) — defer to a second phase.

### B2. Garbage collection (correctness)

A long-running server leaks per request without reclamation — fatal, not cosmetic.
Open item #5 (Q11, tx-orphan facts) is *part* of this, but the general problem is
**entity + value-object lifetime**: the `value_objects` content-hash registry and
the store's fact set both grow unbounded. Unify entity-fact GC (Q11) and the
value-object glacier (Q3) as one store-layer reclamation pass. Store-layer, not a VM
prerequisite — but a hard prerequisite for *this*.

---

## By axis

### 1. Correctness — sound under sustained load

- **GC** (B2) — highest-priority correctness item.
- **Per-entity footprint at scale** — 64 KB stack (release) + mailbox per entity;
  10k connections ≈ 640 MB of stacks alone. *This is a live concern even in release
  mode* — the smaller release stack helps but does not resolve density. Validate the
  connection-per-entity model, or introduce an entity pool / stackless handler.
  (Note: `--release` is now testable via `just rel-test`; a release-only test gap was
  found and fixed — `future_double_resolve_panics_in_debug` is debug-assertion-only,
  now `#[cfg(debug_assertions)]`. Release test coverage should be exercised
  regularly, not just debug.)
- **Self-send vs async-send semantics** (open issue #2) — must be pinned down before
  building request handlers on it; concurrent correctness depends on it.
- **Async-failure propagation under real I/O** — condition system routes async
  rejections; stress against connection resets, partial reads, timeouts.
- **Scheduler fairness/liveness** — confirm no starvation with thousands of entities;
  deadlock detection is currently tx-scoped only.

### 2. Capability — features to add

- **Non-blocking networking** (B1) + Smol socket/stream classes.
- **Streams** — `ReadStream`/`WriteStream` over bytes; essential for HTTP parsing and
  response assembly.
- **String/bytes processing** — UTF-8 decode, slicing, find/split, percent/URL decode,
  header parsing, possibly minimal regex.
- **Transients** — mutable-during-construction buffers/collections that freeze into
  value objects. Without them, immutability makes parsing and response assembly
  allocate quadratically. Directly coupled to performance (see below).
- **Richer collections** — Set, Interval, growable byte buffer.
- **Dates/wall-clock** — `Duration` exists, but no `Date`/clock (needed for headers,
  logging, timeouts).
- **Traits — potentially a blocker, not merely nice-to-have.** A long-standing design
  intent is *shallower class hierarchies via traits* (composable behavior instead of
  deep inheritance). Introducing traits after the core/library classes are written
  forces a rewrite of those hierarchies; introducing them before lets the server's
  library classes (handlers, middleware, streams, collections) be composed correctly
  from the start. Because it affects the *shape* of core classes, it should be
  decided **before** the stdlib build-out, even if the trait mechanism itself lands
  incrementally. Treat as a sequencing dependency, not an optional extra.
- **Namespaced stdlib** — organize HTTP, encoding, collections into namespaces.
- **FFI vs targeted primitives** — for a first server, targeted Rust primitives
  (sockets, later crypto/TLS) are far more tractable than a general FFI. Revisit FFI
  once the primitive surface is understood.

### 3. Performance — throughput/latency

- **Tree-walker cost** — every request walks the AST. The real fix is the bytecode VM
  (deferred). Interim wins: inline/polymorphic method-lookup caches; eliminate
  per-dispatch argument cloning (already flagged in the perf-review item); prune
  redundant `force_value`.
- **Value-object churn** — immutability + content-hash registration on every value
  creation is both a hot-path cost and a leak (ties to B2). Transients + a rope/arena
  for buffers are the leverage.
- **Per-entity footprint** — coroutine stack + mailbox per connection; measure, then
  pool or shrink.
- **Collection constant factors** — Dictionary/OC access routes through Smol dispatch;
  hot paths (routing, header maps) may need primitive-backed variants.

---

## Recommended sequencing — spike first, let reality prioritize

Do **not** plan the whole stack top-down. Build a deliberately minimal echo/HTTP
server as a spike to empirically surface the real walls:

1. Extend `OsInterface` with `may::net` TCP (accept/read/write → Futures).
2. Add a `Socket`/`ByteStream` class + growable byte buffer + minimal string ops.
3. Accept-loop entity + connection-per-entity handler.
4. Load-test it.

Expected order of walls the spike will expose: **GC and value-object churn first,
then string/stream ergonomics, then method-dispatch cost.** The spike also gives the
eventual VM a concrete, profiled workload.

**Two decisions that gate the build-out (should precede the stdlib work):**

- **Traits** — because it changes the shape of core/library classes (above).
- **Transients** — because it changes how every collection/string is built.

Everything else is incremental and can follow the spike.
