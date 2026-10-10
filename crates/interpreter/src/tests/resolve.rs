use super::*;
use crate::ast::{Expr, Message, Stmt, TaggedStmt, VarRef};
use crate::parser::Parser;
use std::sync::Arc;

/// Parse `X >> sel [...]` and rewrite its (first) method with `res` filled.
fn rewrite_src(src: &str) -> Vec<TaggedStmt> {
    let prog = Parser::parse(src).expect("parse");
    let def = prog
        .stmts
        .iter()
        .find_map(|s| match &s.node {
            Stmt::MethodDef(d) => Some(d),
            _ => None,
        })
        .expect("a method definition");
    resolve_and_rewrite(&def.pattern.params(), &def.locals, &def.body)
}

/// The `VarRef` of an `Expr::Var`, else panic.
fn as_var(e: &Expr) -> &VarRef {
    match e {
        Expr::Var { res, .. } => res,
        other => panic!("expected Var, got {other:?}"),
    }
}

/// Parse `X >> sel [...]` and resolve the (first) method definition in it.
fn resolve_src(src: &str) -> ResolvedScope {
    let prog = Parser::parse(src).expect("parse");
    let def = prog
        .stmts
        .iter()
        .find_map(|s| match &s.node {
            Stmt::MethodDef(d) => Some(d),
            _ => None,
        })
        .expect("a method definition");
    resolve_method(&def.pattern.params(), &def.locals, &def.body)
}

fn names(ss: &[Arc<str>]) -> Vec<&str> {
    ss.iter().map(|s| s.as_ref()).collect()
}

#[test]
fn locals_and_params_get_slots() {
    // `a`,`b` are slots 0,1; `p` is a param at slot 0 in `foo:`.
    let r = resolve_src("X >> foo [ | a b | a := 1. ^a + b ]");
    assert_eq!(names(&r.slots), ["a", "b"]);
    assert!(r.captures.is_empty());
    // target a, read a, read b
    assert_eq!(
        r.refs,
        vec![VarRef::Local(0), VarRef::Local(0), VarRef::Local(1)]
    );
    assert!(r.children.is_empty());
}

#[test]
fn param_is_slot_zero() {
    let r = resolve_src("X >> foo: p [ ^p ]");
    assert_eq!(names(&r.slots), ["p"]);
    assert_eq!(r.refs, vec![VarRef::Local(0)]);
}

#[test]
fn self_resolves_to_selfref() {
    let r = resolve_src("X >> foo [ ^self ]");
    assert_eq!(r.refs, vec![VarRef::SelfRef]);
}

#[test]
fn undeclared_name_is_dynamic() {
    // `ivar` is neither param, local, nor self → resolved at runtime (ivar/
    // classvar/global). The resolver does not need to distinguish which.
    let r = resolve_src("X >> foo [ ^ivar ]");
    assert_eq!(r.refs, vec![VarRef::Dynamic(Arc::from("ivar"))]);
}

#[test]
fn block_captures_a_method_local() {
    // `[ m ]` reads the method local `m` → Upvalue(0) filled from parent slot 0.
    let r = resolve_src("X >> foo [ | m | m := 1. ^[ m ] ]");
    assert_eq!(names(&r.slots), ["m"]);
    // method-level refs: the assignment target `m`.
    assert_eq!(r.refs, vec![VarRef::Local(0)]);
    assert_eq!(r.children.len(), 1);
    let blk = &r.children[0];
    assert!(blk.slots.is_empty());
    assert_eq!(names(&blk.captures), ["m"]);
    assert_eq!(blk.sources, vec![CaptureSource::ParentLocal(0)]);
    assert_eq!(blk.refs, vec![VarRef::Upvalue(0)]);
}

#[test]
fn block_param_and_capture_coexist() {
    // `[:x | x + m]` — x is the block's slot 0, m is captured as Upvalue(0).
    let r = resolve_src("X >> foo [ | m | m := 1. ^[:x | x + m] ]");
    let blk = &r.children[0];
    assert_eq!(names(&blk.slots), ["x"]);
    assert_eq!(names(&blk.captures), ["m"]);
    assert_eq!(blk.sources, vec![CaptureSource::ParentLocal(0)]);
    // read x (Local 0), read m (Upvalue 0)
    assert_eq!(blk.refs, vec![VarRef::Local(0), VarRef::Upvalue(0)]);
}

#[test]
fn nested_block_chains_upvalue_through_intermediate() {
    // `[ [ m ] ]` — the inner block reads `m` (a method local). The OUTER block
    // must also capture `m` (from the parent frame), and the inner captures it
    // from the outer's snapshot. This is the upvalue-chaining property.
    let r = resolve_src("X >> foo [ | m | m := 1. ^[ [ m ] ] ]");
    let outer = &r.children[0];
    assert_eq!(names(&outer.captures), ["m"]);
    assert_eq!(outer.sources, vec![CaptureSource::ParentLocal(0)]);
    assert!(outer.refs.is_empty()); // outer block only contains the inner block
    let inner = &outer.children[0];
    assert_eq!(names(&inner.captures), ["m"]);
    assert_eq!(inner.sources, vec![CaptureSource::ParentUpvalue(0)]);
    assert_eq!(inner.refs, vec![VarRef::Upvalue(0)]);
}

#[test]
fn same_capture_referenced_twice_is_one_slot() {
    let r = resolve_src("X >> foo [ | m | m := 1. ^[ m + m ] ]");
    let blk = &r.children[0];
    assert_eq!(names(&blk.captures), ["m"]); // one capture slot
    assert_eq!(blk.refs, vec![VarRef::Upvalue(0), VarRef::Upvalue(0)]);
}

// ── resolve_and_rewrite: res is filled on Expr::Var (P2.1a, reads only) ───────

#[test]
fn rewrite_fills_local_param_and_dynamic() {
    // params=[p] → slot 0; locals=[m] → slot 1. `ivar` is Dynamic.
    let body = rewrite_src("X >> foo: p [ | m | m := p. ^m + ivar ]");
    // stmt 0: `m := p` — the VALUE `p` resolves to Local(0). (Target stays a bare
    // name in P2.1a.)
    let Stmt::Assign { value, .. } = &body[0].node else {
        panic!("expected assign, got {:?}", body[0]);
    };
    assert_eq!(as_var(value), &VarRef::Local(0));
    // stmt 1: `^m + ivar` — m → Local(1), ivar → Dynamic.
    let Stmt::Return(Expr::Send { receiver, message }) = &body[1].node else {
        panic!("expected return-send, got {:?}", body[1]);
    };
    assert_eq!(as_var(receiver), &VarRef::Local(1));
    let Message::Binary { arg, .. } = message else {
        panic!("expected binary");
    };
    assert_eq!(as_var(arg), &VarRef::Dynamic(Arc::from("ivar")));
}

#[test]
fn rewrite_fills_selfref() {
    let body = rewrite_src("X >> foo [ ^self ]");
    let Stmt::Return(e) = &body[0].node else {
        panic!()
    };
    assert_eq!(as_var(e), &VarRef::SelfRef);
}

#[test]
fn rewrite_fills_upvalue_in_block() {
    // The block reads the method local `m` → Upvalue(0).
    let body = rewrite_src("X >> foo [ | m | m := 1. ^[ m ] ]");
    let Stmt::Return(Expr::Block { body: blk, .. }) = &body[1].node else {
        panic!("expected return-block, got {:?}", body[1]);
    };
    let Stmt::Expr(e) = &blk[0].node else {
        panic!()
    };
    assert_eq!(as_var(e), &VarRef::Upvalue(0));
}

#[test]
fn resolves_the_entire_kernel_without_panic() {
    // Every real kernel method must resolve cleanly (no panic, sane slot counts).
    // Broader coverage than the hand-written cases above.
    let src = include_str!("../kernel.smol");
    let prog = Parser::parse(src).expect("kernel parse");
    let mut methods = 0usize;
    for s in &prog.stmts {
        if let Stmt::MethodDef(d) = &s.node {
            let r = resolve_method(&d.pattern.params(), &d.locals, &d.body);
            assert!(r.slots.len() <= u16::MAX as usize);
            methods += 1;
        }
    }
    assert!(methods > 300, "expected the whole kernel, got {methods}");
}

#[test]
fn shadowing_block_local_wins_over_outer() {
    // The block declares its own `m`, so `m` inside is the block's Local, not a
    // capture of the method's `m`.
    let r = resolve_src("X >> foo [ | m | m := 1. ^[ | m | m := 2. m ] ]");
    let blk = &r.children[0];
    assert_eq!(names(&blk.slots), ["m"]);
    assert!(blk.captures.is_empty());
    // target m (Local 0), read m (Local 0)
    assert_eq!(blk.refs, vec![VarRef::Local(0), VarRef::Local(0)]);
}
