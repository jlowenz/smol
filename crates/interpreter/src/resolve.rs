//! Variable resolution — env-frame redesign (`docs/perf_env_frame_redesign.md`
//! §5, §11).
//!
//! Classifies every variable reference in a method and its nested blocks into a
//! [`VarRef`], assigns each lexical scope a flat set of frame slots, and computes
//! each block's capture plan (with proper upvalue chaining — a name a nested block
//! reads is captured by every scope between it and the binder). This is the
//! analysis P2.1 uses to give method activations flat `Vec<Value>` frames and
//! blocks value snapshots.
//!
//! Status (P2.1a): [`resolve_and_rewrite`] fills `Expr::Var.res` for reads and is
//! wired into method installation, but the evaluator still resolves by name and
//! ignores `res` — so it is behavior-preserving. [`resolve_method`] is the
//! `ResolvedScope` analysis view (validation/tooling), which P2.1c will fold into
//! the AST once capture plans and slot writes land.

// `resolve_and_rewrite` is wired into method installation (P2.1a). The
// `ResolvedScope` analysis view (`resolve_method` + its walk) stays test/tooling-only
// until P2.1c folds capture plans onto the AST; allow its as-yet-unused items.
#![allow(dead_code)]

use std::sync::Arc;

use crate::ast::{Expr, Message, Stmt, VarRef};

/// Where a block capture slot is filled from at block-creation time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CaptureSource {
    /// Slot `i` in the immediately-enclosing scope's frame.
    ParentLocal(u16),
    /// Index `i` in the immediately-enclosing scope's own capture snapshot.
    ParentUpvalue(u16),
}

/// The resolved plan for one lexical scope: a method body or a block body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedScope {
    /// Frame slots in index order: parameters, then declared locals. `Local(i)`
    /// indexes here.
    pub slots: Vec<Arc<str>>,
    /// Capture snapshot layout (empty for a method scope — methods never capture).
    /// `Upvalue(i)` indexes here; `sources[i]` says where slot `i` is filled from.
    pub captures: Vec<Arc<str>>,
    /// Per-capture fill instructions, parallel to `captures`.
    pub sources: Vec<CaptureSource>,
    /// The `VarRef` for every variable reference appearing *directly* in this
    /// scope (reads and assignment targets, in source order) — references inside
    /// nested blocks live in that block's own `refs`. Seeds the P2.1 AST rewrite
    /// and makes resolution assertable in isolation.
    pub refs: Vec<VarRef>,
    /// Nested block scopes, in source order.
    pub children: Vec<ResolvedScope>,
}

/// Resolve a method scope and everything nested within it.
///
/// Requires: `params`/`locals` are the method's parameters and top-level temps;
///   `body` is its statement list, post-parse (so the strict block-assignment
///   rule already holds — no block assigns a non-local).
/// Ensures: the returned tree assigns every param/local a unique slot, classifies
///   every variable reference, and gives each nested block a capture plan whose
///   `sources` chain outward through intervening scopes.
pub(crate) fn resolve_method(
    params: &[Arc<str>],
    locals: &[Arc<str>],
    body: &[Stmt],
) -> ResolvedScope {
    let slots = scope_slots(params, locals, body);
    let mut stack: Vec<ScopeCtx> = Vec::new();
    resolve_scope(&mut stack, slots, body)
}

/// Rewrite a method body into a copy with every `Expr::Var.res` filled
/// (`Local` / `Upvalue` / `SelfRef` / `Dynamic`), reusing the same classification
/// as [`resolve_method`].
///
/// P2.1a scope: variable **reads** only. `Stmt::Assign`'s target keeps its bare
/// name — write-target resolution lands with slot writes in P2.1b. Because the
/// evaluator still resolves by name and ignores `res`, storing a rewritten body is
/// behavior-preserving.
///
/// Ensures: the returned statements are structurally identical to `body` except
///   that each `Expr::Var` carries its resolved `VarRef`.
pub(crate) fn resolve_and_rewrite(
    params: &[Arc<str>],
    locals: &[Arc<str>],
    body: &[Stmt],
) -> Vec<Stmt> {
    let slots = scope_slots(params, locals, body);
    let mut stack: Vec<ScopeCtx> = Vec::new();
    rewrite_scope(&mut stack, slots, body)
}

fn rewrite_scope(stack: &mut Vec<ScopeCtx>, slots: Vec<Arc<str>>, body: &[Stmt]) -> Vec<Stmt> {
    stack.push(ScopeCtx {
        slots,
        captures: Vec::new(),
        sources: Vec::new(),
    });
    let out = body.iter().map(|s| rewrite_stmt(stack, s)).collect();
    stack.pop();
    out
}

fn rewrite_stmt(stack: &mut Vec<ScopeCtx>, s: &Stmt) -> Stmt {
    match s {
        Stmt::Expr(e) => Stmt::Expr(rewrite_expr(stack, e)),
        Stmt::Return(e) => Stmt::Return(rewrite_expr(stack, e)),
        Stmt::Assign { name, value } => Stmt::Assign {
            name: name.clone(),
            value: rewrite_expr(stack, value),
        },
        Stmt::LocalDecl(n) => Stmt::LocalDecl(n.clone()),
        // A nested method def is resolved when it is itself installed.
        Stmt::MethodDef(d) => Stmt::MethodDef(d.clone()),
    }
}

fn rewrite_expr(stack: &mut Vec<ScopeCtx>, e: &Expr) -> Expr {
    match e {
        Expr::Var { name, .. } => {
            let depth = stack.len() - 1;
            let res = resolve_name(stack, depth, name);
            Expr::Var {
                name: name.clone(),
                res,
            }
        }
        Expr::Block {
            params,
            locals,
            body,
        } => {
            let slots = scope_slots(params, locals, body);
            let new_body = rewrite_scope(stack, slots, body);
            Expr::Block {
                params: params.clone(),
                locals: locals.clone(),
                body: new_body,
            }
        }
        Expr::Send { receiver, message } => Expr::Send {
            receiver: Box::new(rewrite_expr(stack, receiver)),
            message: rewrite_message(stack, message),
        },
        Expr::Cascade { receiver, messages } => Expr::Cascade {
            receiver: Box::new(rewrite_expr(stack, receiver)),
            messages: messages.iter().map(|m| rewrite_message(stack, m)).collect(),
        },
        Expr::DynamicArray(es) => {
            Expr::DynamicArray(es.iter().map(|x| rewrite_expr(stack, x)).collect())
        }
        Expr::Lit(_) | Expr::Super | Expr::NamespaceRef { .. } | Expr::ArrayLiteral(_) => e.clone(),
    }
}

fn rewrite_message(stack: &mut Vec<ScopeCtx>, msg: &Message) -> Message {
    match msg {
        Message::Unary(s) => Message::Unary(s.clone()),
        Message::Binary { op, arg } => Message::Binary {
            op: op.clone(),
            arg: Box::new(rewrite_expr(stack, arg)),
        },
        Message::Keyword { parts } => Message::Keyword {
            parts: parts
                .iter()
                .map(|(k, e)| (k.clone(), rewrite_expr(stack, e)))
                .collect(),
        },
    }
}

/// A scope's mutable resolution state while its subtree is being walked.
struct ScopeCtx {
    slots: Vec<Arc<str>>,
    captures: Vec<Arc<str>>,
    sources: Vec<CaptureSource>,
}

/// Slots of a scope: params, then declared locals, then mid-body `| … |` locals,
/// in that order. All are live for the whole scope (`push_*_frame` pre-binds them
/// to `Nil`), so ordering only fixes slot indices.
fn scope_slots(params: &[Arc<str>], locals: &[Arc<str>], body: &[Stmt]) -> Vec<Arc<str>> {
    let mut slots: Vec<Arc<str>> = params.to_vec();
    slots.extend(locals.iter().cloned());
    for s in body {
        if let Stmt::LocalDecl(names) = s {
            slots.extend(names.iter().cloned());
        }
    }
    slots
}

fn resolve_scope(stack: &mut Vec<ScopeCtx>, slots: Vec<Arc<str>>, body: &[Stmt]) -> ResolvedScope {
    stack.push(ScopeCtx {
        slots,
        captures: Vec::new(),
        sources: Vec::new(),
    });
    let mut refs = Vec::new();
    let mut children = Vec::new();
    for s in body {
        walk_stmt(stack, s, &mut refs, &mut children);
    }
    let ctx = stack.pop().expect("scope stack underflow");
    ResolvedScope {
        slots: ctx.slots,
        captures: ctx.captures,
        sources: ctx.sources,
        refs,
        children,
    }
}

fn walk_stmt(
    stack: &mut Vec<ScopeCtx>,
    s: &Stmt,
    refs: &mut Vec<VarRef>,
    children: &mut Vec<ResolvedScope>,
) {
    match s {
        Stmt::Expr(e) | Stmt::Return(e) => walk_expr(stack, e, refs, children),
        Stmt::Assign { name, value } => {
            // The assignment target is itself a reference (an lvalue). The strict
            // block rule guarantees it is a scope-local or a dynamic name, never a
            // capture, so resolving it never adds an upvalue.
            let depth = stack.len() - 1;
            let r = resolve_name(stack, depth, name);
            refs.push(r);
            walk_expr(stack, value, refs, children);
        }
        Stmt::LocalDecl(_) | Stmt::MethodDef(_) => {}
    }
}

fn walk_expr(
    stack: &mut Vec<ScopeCtx>,
    e: &Expr,
    refs: &mut Vec<VarRef>,
    children: &mut Vec<ResolvedScope>,
) {
    match e {
        Expr::Var { name, .. } => {
            let depth = stack.len() - 1;
            let r = resolve_name(stack, depth, name);
            refs.push(r);
        }
        Expr::Block {
            params,
            locals,
            body,
        } => {
            let slots = scope_slots(params, locals, body);
            let child = resolve_scope(stack, slots, body);
            children.push(child);
        }
        Expr::Send { receiver, message } => {
            walk_expr(stack, receiver, refs, children);
            walk_message(stack, message, refs, children);
        }
        Expr::Cascade { receiver, messages } => {
            walk_expr(stack, receiver, refs, children);
            for m in messages {
                walk_message(stack, m, refs, children);
            }
        }
        Expr::DynamicArray(es) => {
            for x in es {
                walk_expr(stack, x, refs, children);
            }
        }
        Expr::Lit(_) | Expr::Super | Expr::NamespaceRef { .. } | Expr::ArrayLiteral(_) => {}
    }
}

fn walk_message(
    stack: &mut Vec<ScopeCtx>,
    msg: &Message,
    refs: &mut Vec<VarRef>,
    children: &mut Vec<ResolvedScope>,
) {
    match msg {
        Message::Unary(_) => {}
        Message::Binary { arg, .. } => walk_expr(stack, arg, refs, children),
        Message::Keyword { parts } => {
            for (_, e) in parts {
                walk_expr(stack, e, refs, children);
            }
        }
    }
}

/// Classify `name` as seen from scope `depth`, adding upvalues along the way.
///
/// Ensures: a name bound in an enclosing scope resolves to `Upvalue`, and every
///   scope between the reference and the binder gains a capture entry whose
///   `source` points one level out (`ParentLocal` at the binder, `ParentUpvalue`
///   through intermediates). A name bound nowhere lexically is `Dynamic`.
fn resolve_name(stack: &mut [ScopeCtx], depth: usize, name: &Arc<str>) -> VarRef {
    if name.as_ref() == "self" {
        return VarRef::SelfRef;
    }
    if let Some(i) = position(&stack[depth].slots, name) {
        return VarRef::Local(i);
    }
    if let Some(i) = position(&stack[depth].captures, name) {
        return VarRef::Upvalue(i);
    }
    if depth == 0 {
        // Method scope: no enclosing lexical scope to capture from.
        return VarRef::Dynamic(name.clone());
    }
    match resolve_name(stack, depth - 1, name) {
        VarRef::Local(pi) => add_capture(stack, depth, name, CaptureSource::ParentLocal(pi)),
        VarRef::Upvalue(pu) => add_capture(stack, depth, name, CaptureSource::ParentUpvalue(pu)),
        // `self` handled above; only reachable if a parent classified it so.
        VarRef::SelfRef => VarRef::SelfRef,
        // Not lexically bound anywhere → dynamic at every level.
        VarRef::Dynamic(n) => VarRef::Dynamic(n),
        // `resolve_name` only ever yields the four classifications above.
        VarRef::Unresolved => unreachable!("resolve_name never yields Unresolved"),
    }
}

fn add_capture(
    stack: &mut [ScopeCtx],
    depth: usize,
    name: &Arc<str>,
    source: CaptureSource,
) -> VarRef {
    let idx = stack[depth].captures.len() as u16;
    stack[depth].captures.push(name.clone());
    stack[depth].sources.push(source);
    VarRef::Upvalue(idx)
}

fn position(names: &[Arc<str>], name: &Arc<str>) -> Option<u16> {
    names
        .iter()
        .position(|s| s.as_ref() == name.as_ref())
        .map(|i| i as u16)
}

#[cfg(test)]
#[path = "tests/resolve.rs"]
mod tests;
