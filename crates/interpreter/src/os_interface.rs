use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
}
