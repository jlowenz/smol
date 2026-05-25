use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::value::Value;

type Frame = Mutex<HashMap<Arc<str>, Value>>;

/// Lexical environment: a stack of variable-binding frames.
///
/// Frames are heap-allocated and reference-counted. Cloning an `Env` copies
/// the `Vec` of `Arc<Frame>` handles, so the clone shares the same live frames.
/// This gives correct closure semantics: a block that captures its defining
/// `Env` sees mutations made to outer variables after capture.
///
/// Frames use `std::sync::Mutex` so that `Arc<Frame>` is `Send + Sync`,
/// allowing captured environments to cross entity thread boundaries safely.
#[derive(Debug, Clone)]
pub struct Env {
    frames: Vec<Arc<Frame>>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    /// Creates a new environment with one root frame.
    pub fn new() -> Self {
        Self {
            frames: vec![Arc::new(Mutex::new(HashMap::new()))],
        }
    }

    /// Pushes a new empty frame onto the stack.
    pub fn push_frame(&mut self) {
        self.frames.push(Arc::new(Mutex::new(HashMap::new())));
    }

    /// Requires: at least two frames are live (the root frame is never popped).
    /// Ensures: the innermost frame is removed.
    pub fn pop_frame(&mut self) {
        debug_assert!(self.frames.len() > 1, "attempted to pop the root frame");
        self.frames.pop();
    }

    /// Requires: at least one frame is live.
    /// Ensures: binds `name` in the innermost frame, shadowing any outer binding.
    pub fn define(&mut self, name: Arc<str>, value: Value) {
        self.frames
            .last()
            .expect("no frames")
            .lock()
            .unwrap()
            .insert(name, value);
    }

    /// Walks from innermost to outermost frame; returns a clone of the first
    /// binding found, or `None`.
    ///
    /// Returns a cloned `Value` rather than a reference to avoid holding a
    /// `Mutex` lock across further env operations.
    pub fn get(&self, name: &str) -> Option<Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.lock().unwrap().get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Walks from innermost to outermost frame; updates the first binding found.
    ///
    /// Ensures: returns `true` if a binding was found and updated, `false` if
    /// no binding for `name` exists in any frame.
    pub fn set(&mut self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter().rev() {
            let mut map = frame.lock().unwrap();
            if map.contains_key(name) {
                map.insert(Arc::from(name), value);
                return true;
            }
        }
        false
    }

    /// Look up `name` in the innermost frame only; does not walk outer frames.
    pub fn lookup_top_frame(&self, name: &str) -> Option<Value> {
        self.frames.last()?.lock().unwrap().get(name).cloned()
    }

    /// Update a binding in the innermost frame only.
    ///
    /// Ensures: returns `true` if a binding for `name` was found and updated.
    pub fn set_top_frame(&mut self, name: &str, value: Value) -> bool {
        if let Some(frame) = self.frames.last() {
            let mut map = frame.lock().unwrap();
            if map.contains_key(name) {
                map.insert(Arc::from(name), value);
                return true;
            }
        }
        false
    }

    /// Look up `name` in all frames except the innermost.
    ///
    /// Used when the innermost (method-local) frame has already been searched
    /// and lookup should continue into the outer (global) scope.
    pub fn lookup_outer_frames(&self, name: &str) -> Option<Value> {
        let outer = self.frames.len().saturating_sub(1);
        for frame in self.frames[..outer].iter().rev() {
            if let Some(v) = frame.lock().unwrap().get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Define a binding in the outermost (root) frame.
    ///
    /// Ensures: the name is bound globally regardless of the current frame depth.
    pub fn define_global(&mut self, name: Arc<str>, value: Value) {
        self.frames
            .first()
            .expect("no frames")
            .lock()
            .unwrap()
            .insert(name, value);
    }

    /// Update the first existing binding found (innermost to outermost), or define
    /// in the innermost frame if no binding exists. Takes ownership of both
    /// arguments to avoid extra clones at call sites.
    ///
    /// Ensures: `name` is bound to `value` in the shallowest frame where it
    ///   already existed, or in the innermost frame if it did not exist.
    pub fn update_or_define(&mut self, name: Arc<str>, value: Value) {
        for frame in self.frames.iter().rev() {
            let mut map = frame.lock().unwrap();
            if map.contains_key(name.as_ref()) {
                map.insert(name, value);
                return;
            }
        }
        self.frames
            .last()
            .expect("no frames")
            .lock()
            .unwrap()
            .insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Arc<str> {
        Arc::from(v)
    }
    fn int(n: i64) -> Value {
        Value::Int(n)
    }

    #[test]
    fn define_and_get_single_frame() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        assert_eq!(env.get("x"), Some(int(1)));
        assert_eq!(env.get("y"), None);
    }

    #[test]
    fn inner_frame_shadows_outer() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        env.push_frame();
        env.define(s("x"), int(2));
        assert_eq!(env.get("x"), Some(int(2)));
        env.pop_frame();
        assert_eq!(env.get("x"), Some(int(1)));
    }

    #[test]
    fn set_updates_outer_frame() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        env.push_frame();
        assert!(env.set("x", int(42)));
        env.pop_frame();
        assert_eq!(env.get("x"), Some(int(42)));
    }

    #[test]
    fn set_returns_false_for_undefined() {
        let mut env = Env::new();
        assert!(!env.set("x", int(1)));
    }

    #[test]
    fn lookup_top_frame_ignores_outer() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        env.push_frame();
        assert_eq!(env.lookup_top_frame("x"), None);
        env.define(s("x"), int(2));
        assert_eq!(env.lookup_top_frame("x"), Some(int(2)));
        env.pop_frame();
    }

    #[test]
    fn lookup_outer_frames_skips_top() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        env.push_frame();
        env.define(s("x"), int(99));
        assert_eq!(env.lookup_outer_frames("x"), Some(int(1)));
        assert_eq!(env.lookup_outer_frames("y"), None);
        env.pop_frame();
    }

    #[test]
    fn set_top_frame_does_not_update_outer() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        env.push_frame();
        assert!(!env.set_top_frame("x", int(99)));
        env.pop_frame();
        assert_eq!(env.get("x"), Some(int(1)));
    }

    #[test]
    fn set_top_frame_updates_top_binding() {
        let mut env = Env::new();
        env.push_frame();
        env.define(s("x"), int(1));
        assert!(env.set_top_frame("x", int(42)));
        assert_eq!(env.lookup_top_frame("x"), Some(int(42)));
        env.pop_frame();
    }

    #[test]
    fn update_or_define_updates_existing() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        env.push_frame();
        env.update_or_define(s("x"), int(99));
        env.pop_frame();
        assert_eq!(env.get("x"), Some(int(99)));
    }

    #[test]
    fn update_or_define_creates_in_top_when_absent() {
        let mut env = Env::new();
        env.push_frame();
        env.update_or_define(s("fresh"), int(7));
        assert_eq!(env.lookup_top_frame("fresh"), Some(int(7)));
        env.pop_frame();
        assert_eq!(env.get("fresh"), None);
    }

    #[test]
    fn define_global_visible_after_push() {
        let mut env = Env::new();
        env.push_frame();
        env.push_frame();
        env.define_global(s("g"), int(42));
        assert_eq!(env.get("g"), Some(int(42)));
        assert_eq!(env.lookup_top_frame("g"), None);
        env.pop_frame();
        env.pop_frame();
        assert_eq!(env.get("g"), Some(int(42)));
    }

    #[test]
    fn clone_shares_frames() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        let mut snapshot = env.clone();
        assert!(snapshot.set("x", int(99)));
        assert_eq!(env.get("x"), Some(int(99)));
    }
}
