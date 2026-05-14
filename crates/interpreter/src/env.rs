use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::value::Value;

type Frame = RefCell<HashMap<Arc<str>, Value>>;

/// Lexical environment: a stack of variable-binding frames.
///
/// Frames are heap-allocated and reference-counted. Cloning an `Env` copies
/// the `Vec` of `Arc<Frame>` handles, so the clone shares the same live frames.
/// This gives correct closure semantics: a block that captures its defining
/// `Env` sees mutations made to outer variables after capture.
///
/// `RefCell` enforces Rust borrow rules at runtime; a panic here is an
/// evaluator bug (double borrow), not a user error.
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
            frames: vec![Arc::new(RefCell::new(HashMap::new()))],
        }
    }

    /// Pushes a new empty frame onto the stack.
    pub fn push_frame(&mut self) {
        self.frames.push(Arc::new(RefCell::new(HashMap::new())));
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
            .borrow_mut()
            .insert(name, value);
    }

    /// Walks from innermost to outermost frame; returns a clone of the first
    /// binding found, or `None`.
    ///
    /// Returns a cloned `Value` rather than a reference to avoid holding a
    /// `RefCell` borrow across further env operations (which would panic).
    pub fn get(&self, name: &str) -> Option<Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.borrow().get(name) {
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
            let mut map = frame.borrow_mut();
            if map.contains_key(name) {
                map.insert(Arc::from(name), value);
                return true;
            }
        }
        false
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
        // No 'x' in inner frame; set should find it in outer.
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
    fn clone_shares_frames() {
        let mut env = Env::new();
        env.define(s("x"), int(1));
        let mut snapshot = env.clone();
        // Mutating through the clone is visible in the original.
        assert!(snapshot.set("x", int(99)));
        assert_eq!(env.get("x"), Some(int(99)));
    }
}
