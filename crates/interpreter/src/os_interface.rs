use std::any::Any;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use may::net::{TcpListener, TcpStream};

/// Opaque socket handle type alias — a listener or stream, boxed as `Arc<dyn Any>`
/// and surfaced to Smol as `Value::Handle` (same convention as timers).
pub type OsHandle = Arc<dyn Any + Send + Sync>;

/// OS/world boundary for the interpreter.
///
/// Exposes the minimal surface of operating-system primitives that the evaluator
/// needs to interact with the world. Implementations are injected into `Evaluator`
/// so tests can supply fakes without spawning real OS threads.
///
/// All handles returned from `schedule_timer` are opaque `Arc<dyn Any + Send + Sync>`;
/// callers store them in `Value::Handle` and pass them back to `cancel_timer` /
/// `timer_pending`. The implementation downcasts to its concrete type internally.
pub trait OsInterface: Send + Sync {
    /// Schedule `callback` to be called after `ms` milliseconds.
    ///
    /// Requires: `callback` is `Send + 'static`.
    /// Ensures: after ≥ `ms` milliseconds (unless cancelled first), `callback`
    ///   is invoked exactly once on an unspecified thread. Returns an opaque handle
    ///   that identifies this timer for `cancel_timer` / `timer_pending`.
    fn schedule_timer(
        &self,
        ms: u64,
        callback: Box<dyn FnOnce() + Send + 'static>,
    ) -> Arc<dyn Any + Send + Sync>;

    /// Cancel a previously scheduled timer.
    ///
    /// Requires: `handle` was returned by `schedule_timer` on this implementation.
    /// Ensures: if the timer has not yet fired, the callback will not be called.
    ///   If it has already fired, this is a no-op.
    fn cancel_timer(&self, handle: &Arc<dyn Any + Send + Sync>);

    /// Returns true iff the timer has not fired and has not been cancelled.
    ///
    /// Requires: `handle` was returned by `schedule_timer` on this implementation.
    fn timer_pending(&self, handle: &Arc<dyn Any + Send + Sync>) -> bool;

    // ── TCP sockets (C-web spike) ──────────────────────────────────────────────
    //
    // These are synchronous, *coroutine-parking* operations: on the default
    // implementation they yield the calling `may` coroutine to the runtime's I/O
    // reactor rather than blocking an OS thread, so they must be invoked from
    // within a `may` coroutine. The primitive layer wraps `accept`/`read`/`write`
    // in a fresh coroutine + `Future` so the async-everywhere model is preserved.
    // Default bodies return `Unsupported` so alternative `OsInterface`s (e.g. test
    // fakes) need not provide networking.

    /// Bind a TCP listener on `127.0.0.1:port`.
    ///
    /// Ensures: on success returns an opaque listener handle for `tcp_accept`.
    fn tcp_listen(&self, _port: u16) -> io::Result<OsHandle> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "tcp_listen"))
    }

    /// Accept one inbound connection (parks the coroutine until one arrives).
    ///
    /// Requires: `listener` was returned by `tcp_listen` on this implementation.
    /// Ensures: on success returns an opaque stream handle for `tcp_read`/`tcp_write`.
    fn tcp_accept(&self, _listener: &OsHandle) -> io::Result<OsHandle> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "tcp_accept"))
    }

    /// Read up to `max` bytes from `stream` (parks until data or EOF).
    ///
    /// Requires: `stream` was returned by `tcp_accept` on this implementation.
    /// Ensures: an empty `Vec` means EOF (peer closed).
    fn tcp_read(&self, _stream: &OsHandle, _max: usize) -> io::Result<Vec<u8>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "tcp_read"))
    }

    /// Write `bytes` to `stream` (parks until written).
    ///
    /// Requires: `stream` was returned by `tcp_accept` on this implementation.
    /// Ensures: returns the number of bytes written.
    fn tcp_write(&self, _stream: &OsHandle, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "tcp_write"))
    }

    /// Shut down and close `stream`. Idempotent; errors are ignored.
    fn tcp_close(&self, _stream: &OsHandle) {}
}

// ── DefaultOsInterface ────────────────────────────────────────────────────────

/// Interior state for a scheduled OS-thread timer. Both flags use `Relaxed`
/// ordering — the mailbox `Condvar` provides the necessary happens-before edges
/// between the timer thread and the entity thread.
struct TimerInner {
    cancelled: AtomicBool,
    fired: AtomicBool,
}

/// Production implementation of `OsInterface` backed by `std::thread::spawn`.
///
/// Each active timer consumes one OS thread for the sleep duration.
pub struct DefaultOsInterface;

impl OsInterface for DefaultOsInterface {
    fn schedule_timer(
        &self,
        ms: u64,
        callback: Box<dyn FnOnce() + Send + 'static>,
    ) -> Arc<dyn Any + Send + Sync> {
        let timer = Arc::new(TimerInner {
            cancelled: AtomicBool::new(false),
            fired: AtomicBool::new(false),
        });
        let timer_clone = timer.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            if timer_clone.cancelled.load(Ordering::Relaxed) {
                return;
            }
            callback();
            timer_clone.fired.store(true, Ordering::Relaxed);
        });
        timer as Arc<dyn Any + Send + Sync>
    }

    fn cancel_timer(&self, handle: &Arc<dyn Any + Send + Sync>) {
        if let Ok(t) = Arc::clone(handle).downcast::<TimerInner>() {
            t.cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn timer_pending(&self, handle: &Arc<dyn Any + Send + Sync>) -> bool {
        Arc::clone(handle)
            .downcast::<TimerInner>()
            .map(|t| !t.cancelled.load(Ordering::Relaxed) && !t.fired.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    fn tcp_listen(&self, port: u16) -> io::Result<OsHandle> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        Ok(Arc::new(listener) as OsHandle)
    }

    fn tcp_accept(&self, listener: &OsHandle) -> io::Result<OsHandle> {
        let listener = Arc::clone(listener)
            .downcast::<TcpListener>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a TcpListener handle"))?;
        let (stream, _peer) = listener.accept()?;
        Ok(Arc::new(Mutex::new(stream)) as OsHandle)
    }

    fn tcp_read(&self, stream: &OsHandle, max: usize) -> io::Result<Vec<u8>> {
        let stream = downcast_stream(stream)?;
        let mut buf = vec![0u8; max];
        let n = stream
            .lock()
            .expect("stream mutex poisoned")
            .read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn tcp_write(&self, stream: &OsHandle, bytes: &[u8]) -> io::Result<usize> {
        let stream = downcast_stream(stream)?;
        stream.lock().expect("stream mutex poisoned").write(bytes)
    }

    fn tcp_close(&self, stream: &OsHandle) {
        if let Ok(stream) = downcast_stream(stream) {
            let _ = stream
                .lock()
                .expect("stream mutex poisoned")
                .shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Downcast an opaque handle to a shared, lockable `TcpStream`.
/// `may::net::TcpStream` reads/writes through `&mut self`, so the stream is held
/// behind a `Mutex`; each connection is served by a single coroutine, so the lock
/// is uncontended in practice.
fn downcast_stream(handle: &OsHandle) -> io::Result<Arc<Mutex<TcpStream>>> {
    Arc::clone(handle)
        .downcast::<Mutex<TcpStream>>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a TcpStream handle"))
}
