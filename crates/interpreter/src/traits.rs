//! Runtime representation of **traits** — stateless, flattened units of behavioral
//! reuse (see `docs/traits_plan.md`).
//!
//! A trait provides *methods only* (no instance variables); it reaches host state
//! through **required methods**, which are *derived* from the selectors its own
//! method bodies send to `self`. A [`TraitData`] is the native payload held behind a
//! `Value::Handle` inside a kernel `Trait` value object, so traits are ordinary
//! first-class Smol objects whose `+` / `-` / `@` algebra is plain message sends.
//!
//! Traits are **flattened at composition**: when a class `uses:` a trait expression,
//! the provided methods are copied into the class's method dict with per-method
//! provenance recorded. Dispatch is therefore unchanged — no runtime trait search.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::ast::{Expr, Message, Stmt};
use crate::class::Method;

/// One method entry provided by a trait, carrying provenance and conflict state.
#[derive(Clone)]
pub struct TraitMethod {
    /// The compiled method body. For a conflict marker this is one of the
    /// colliding bodies, retained only so provenance/selector stay well-formed;
    /// it is never dispatched (composition fails while the marker is unresolved).
    pub method: Arc<Method>,
    /// Name of the atomic trait this method originally came from. Two entries for
    /// the same selector with *equal* provenance are the shared-ancestor case and
    /// do **not** conflict; differing provenance ⇒ conflict.
    pub provenance: Arc<str>,
    /// `true` iff this entry is a conflict marker produced by `+` over differing
    /// provenance. An unresolved marker at composition time is an error.
    pub is_conflict: bool,
}

/// The native payload of a `Trait` value object: a name plus a provided-method dict.
///
/// Stateless by construction — there is no instance-variable storage here. All
/// mutation is confined to building a `named:` trait up with `>>`; the algebra
/// operators (`+`/`-`/`@`) never mutate their operands, they build fresh
/// `TraitData`.
pub struct TraitData {
    /// Display name. Mutable only so `Trait named: #X` can be renamed by
    /// `named:from:`; ordinarily fixed at creation.
    pub name: RwLock<Arc<str>>,
    /// Provided methods keyed by selector.
    pub methods: RwLock<HashMap<Arc<str>, TraitMethod>>,
}

impl TraitData {
    /// A fresh empty trait named `name`. Methods are added later via `>>`.
    pub fn new_empty(name: Arc<str>) -> Arc<TraitData> {
        Arc::new(TraitData {
            name: RwLock::new(name),
            methods: RwLock::new(HashMap::new()),
        })
    }

    /// Insert (or replace) a `named:`-trait's own method. Provenance is the trait's
    /// own name — this is an *atomic* provider.
    ///
    /// Requires: called only while defining a `named:` trait (not on a composed one).
    pub fn insert_own_method(self: &Arc<Self>, selector: Arc<str>, method: Arc<Method>) {
        let provenance = self.name.read().clone();
        self.methods.write().insert(
            selector,
            TraitMethod {
                method,
                provenance,
                is_conflict: false,
            },
        );
    }

    /// A snapshot clone of the provided-method map (for building composed traits).
    fn snapshot(&self) -> HashMap<Arc<str>, TraitMethod> {
        self.methods.read().clone()
    }

    /// `T1 + T2` — union. A selector provided by both with **differing provenance**
    /// becomes a conflict marker; equal provenance (shared ancestor) keeps one copy.
    pub fn sum(a: &Arc<TraitData>, b: &Arc<TraitData>) -> Arc<TraitData> {
        let mut merged = a.snapshot();
        for (sel, entry) in b.snapshot() {
            match merged.get(sel.as_ref()) {
                None => {
                    merged.insert(sel, entry);
                }
                Some(existing) => {
                    // Already a conflict, or provenance differs ⇒ conflict marker.
                    let conflict = existing.is_conflict || existing.provenance != entry.provenance;
                    if conflict {
                        let mut marker = existing.clone();
                        marker.is_conflict = true;
                        merged.insert(sel, marker);
                    }
                    // equal provenance ⇒ shared ancestor, keep existing.
                }
            }
        }
        let name = Arc::from(format!("({} + {})", a.name.read(), b.name.read()));
        Arc::new(TraitData {
            name: RwLock::new(name),
            methods: RwLock::new(merged),
        })
    }

    /// `T - {selectors}` — remove an explicit set of selectors.
    pub fn exclude_selectors(t: &Arc<TraitData>, selectors: &HashSet<Arc<str>>) -> Arc<TraitData> {
        let mut methods = t.snapshot();
        methods.retain(|sel, _| !selectors.contains(sel));
        let name = Arc::from(format!("({} - {{...}})", t.name.read()));
        Arc::new(TraitData {
            name: RwLock::new(name),
            methods: RwLock::new(methods),
        })
    }

    /// `T1 - T2` — remove T2's provided selectors from T1.
    pub fn exclude_trait(a: &Arc<TraitData>, b: &Arc<TraitData>) -> Arc<TraitData> {
        let provided: HashSet<Arc<str>> = b.methods.read().keys().cloned().collect();
        let result = TraitData::exclude_selectors(a, &provided);
        *result.name.write() = Arc::from(format!("({} - {})", a.name.read(), b.name.read()));
        result
    }

    /// `T @ {newSel -> oldSel. ...}` — add shallow alias entry points. Each new
    /// selector gets `oldSel`'s body (unchanged); internal `self oldSel` sends in T
    /// are **not** redirected.
    ///
    /// Requires: every `oldSel` is provided by `T` (else that alias is skipped).
    pub fn alias(t: &Arc<TraitData>, aliases: &[(Arc<str>, Arc<str>)]) -> Arc<TraitData> {
        let mut methods = t.snapshot();
        for (new_sel, old_sel) in aliases {
            if let Some(entry) = t.methods.read().get(old_sel.as_ref()) {
                methods.insert(new_sel.clone(), entry.clone());
            }
        }
        let name = Arc::from(format!("({} @ {{...}})", t.name.read()));
        Arc::new(TraitData {
            name: RwLock::new(name),
            methods: RwLock::new(methods),
        })
    }

    /// Selectors carrying an unresolved conflict marker.
    pub fn conflict_selectors(&self) -> Vec<Arc<str>> {
        self.methods
            .read()
            .iter()
            .filter(|(_, e)| e.is_conflict)
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// **Derived requirements** — every selector this trait's method bodies send to
    /// `self` that the trait does not itself provide (§3 of the plan). Computed from
    /// the ASTs so it is directly displayable.
    ///
    /// Blind spots (documented): sends via `perform:` with a computed selector, and
    /// sends inside blocks are still collected structurally, but a dynamically
    /// computed selector cannot be seen — hence the optional explicit `requires:`
    /// augmentation deferred to later.
    pub fn requirements(&self) -> HashSet<Arc<str>> {
        let methods = self.methods.read();
        let provided: HashSet<Arc<str>> = methods.keys().cloned().collect();
        let mut reqs: HashSet<Arc<str>> = HashSet::new();
        for entry in methods.values() {
            for stmt in &entry.method.body {
                collect_self_sends_stmt(stmt, &mut reqs);
            }
        }
        reqs.retain(|s| !provided.contains(s));
        reqs
    }
}

/// Is `expr` the pseudo-variable `self`?
fn is_self(expr: &Expr) -> bool {
    matches!(expr, Expr::Var { name, .. } if name.as_ref() == "self")
}

fn collect_self_sends_stmt(stmt: &Stmt, out: &mut HashSet<Arc<str>>) {
    match stmt {
        Stmt::Expr(e) | Stmt::Return(e) | Stmt::Assign { value: e, .. } => {
            collect_self_sends_expr(e, out)
        }
        Stmt::LocalDecl(_) | Stmt::MethodDef(_) => {}
    }
}

fn collect_self_sends_expr(expr: &Expr, out: &mut HashSet<Arc<str>>) {
    match expr {
        Expr::Send { receiver, message } => {
            if is_self(receiver) {
                out.insert(message.selector());
            }
            collect_self_sends_expr(receiver, out);
            collect_self_sends_message(message, out);
        }
        Expr::Cascade { receiver, messages } => {
            let self_recv = is_self(receiver);
            collect_self_sends_expr(receiver, out);
            for m in messages {
                if self_recv {
                    out.insert(m.selector());
                }
                collect_self_sends_message(m, out);
            }
        }
        Expr::Block { body, .. } => {
            for s in body {
                collect_self_sends_stmt(s, out);
            }
        }
        Expr::DynamicArray(elems) => {
            for e in elems {
                collect_self_sends_expr(e, out);
            }
        }
        Expr::Lit(_)
        | Expr::Var { .. }
        | Expr::Super
        | Expr::NamespaceRef { .. }
        | Expr::ArrayLiteral(_) => {}
    }
}

fn collect_self_sends_message(message: &Message, out: &mut HashSet<Arc<str>>) {
    match message {
        Message::Unary(_) => {}
        Message::Binary { arg, .. } => collect_self_sends_expr(arg, out),
        Message::Keyword { parts } => {
            for (_, e) in parts {
                collect_self_sends_expr(e, out);
            }
        }
    }
}
