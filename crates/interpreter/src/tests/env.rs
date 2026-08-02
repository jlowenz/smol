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

#[test]
fn global_scope_keeps_only_the_outermost_frame() {
    let mut env = Env::new();
    env.define(s("g"), int(1));
    env.push_frame();
    env.define(s("caller_local"), int(2));
    env.push_frame();
    env.define(s("inner_local"), int(3));

    let base = env.global_scope();
    assert_eq!(base.get("g"), Some(int(1)));
    assert_eq!(base.get("caller_local"), None);
    assert_eq!(base.get("inner_local"), None);
}

#[test]
fn global_scope_shares_the_global_frame() {
    let mut env = Env::new();
    env.define(s("g"), int(1));
    let mut base = env.global_scope();
    // The shared Arc means a definition through either handle is visible to both.
    base.define(s("later"), int(7));
    assert_eq!(env.get("later"), Some(int(7)));
    assert!(base.set("g", int(99)));
    assert_eq!(env.get("g"), Some(int(99)));
}
