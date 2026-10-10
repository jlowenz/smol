# Coverage Tooling Plan — Smol source coverage via AST source-tagging

Status: **in progress** (2026-10-10). S1 underway.
This is CLAUDE.md "Planned tooling" **item 1** (AST span tracking + `CoverageCollector`);
it unblocks **item 2** (SUnit-style Smol testing framework).

## 1. Problem & goal

`kernel.smol` is loaded as an embedded string (`eval.rs:238`, `include_str!`) and executed by
the tree-walker. `llvm-cov` (`just cov`) instruments **Rust** source only, so it is blind to
which *Smol* code actually ran. Today that gap is covered by a manual, method-by-method audit
("every method body exercised, both branches of every conditional"), which does not scale as the
kernel grows.

**Goal:** record which `.smol` source regions the evaluator executes during a run, and report
covered vs. uncovered lines/statements for any `.smol` source — kernel or user.

## 2. Locked design decision

See memory `design_ast_source_tagging`. In short:

- Tag nodes with source origin via a **wrapper**, not by mutating the sum type:
  ```rust
  pub struct Span { pub source: SourceId, pub start: u32, pub end: u32 }
  pub struct SourceTagged<T> { pub span: Span, pub node: T }
  pub type TaggedStmt = SourceTagged<Stmt>;
  ```
  `Span`, `SourceId`, and `SourceTagged<T>` live in a new **leaf module `span.rs`**, so the lexer
  can emit `SourceTagged<Token>` without depending on the AST. `TaggedStmt` is defined in `ast.rs`;
  `SpannedToken = SourceTagged<Token>` in `lexer.rs`. `SourceTagged` equality **ignores `span`**.
- **Granularity = statement-level.** The `Expr` enum (`ast.rs:79`) is left **completely untouched**
  — name, data, internals. Only statement *containers* hold tagged nodes. Branch coverage still
  falls out: a conditional is `cond ifTrue: [b1] ifFalse: [b2]`, block bodies are statement lists,
  and an untaken branch's statements never execute → never recorded → reported uncovered.
- **Metadata is extensible.** Starts with `span`; **trivia/comments** is the planned next tag
  (already lexed as `Token::Comment`, `lexer.rs:48`). `res: VarRef` stays *inside* `Var`/`Assign`
  — it is per-variant, not universal node metadata.
- **Rejected:** renaming `Expr`→`ExprKind` (it earned its name); field-injection macros
  (`diff-enum` is abandoned, `syn 0.15`; injection reshapes the tuple/unit variants and forfeits
  the free `PartialEq` derive).

## 3. Current-state facts (from recon)

- **Lexer** computes byte offsets (`Lexer { src, chars: CharIndices, pos }`, `lexer.rs:64`;
  `advance` updates `self.pos`, `lexer.rs:100`) but **discards** them — `tokenize` → `Vec<Token>`
  with no positions (`lexer.rs:82`); `Token` has no position field (`lexer.rs:6`).
- **Parser** holds `tokens: Vec<Token>` and indexes by **token position** `self.pos`
  (`parser.rs:32`, helpers `peek`/`advance` `parser.rs:63–86`). `parse_program` builds `Vec<Stmt>`
  directly (`parser.rs:114`). It already skips+collects comments (`skip_comments`, `parser.rs:68`).
- **Evaluator**: `dispatch_stmt(ev, stmt: Stmt, …)` is the single statement choke point
  (`eval_iterative.rs:1375`, takes `Stmt` by value). Body pre-scans for `LocalDecl` at
  `eval_iterative.rs:1494` and `:1651`; resolved bodies constructed at `:2390`/`:2415`. Entry
  `eval_program(stmts: &[Stmt], …)` (`eval.rs:634`). Bodies rewritten in `resolve.rs`.
- **No `SourceId` / source-map** infrastructure exists anywhere.

## 4. Steps

**Compile-order** (each increment builds green): `span` module → S2 (lexer spanned tokens) →
S1 container retype + S3 (statement wrapping) → S4 → S5 → S6. The container retype pulls S2 forward
because the parser needs real spans to build `TaggedStmt`.

### S1 — Span module + AST types + container retype
Add leaf module `span.rs` (`Span`, `SourceId`, `SourceTagged<T>`); define `TaggedStmt` in `ast.rs`.
Retype statement containers: `Program.stmts`, `Expr::Block.body` (`ast.rs:94`),
`MethodDef.body` (`ast.rs:147`) → `Vec<TaggedStmt>`. `Stmt` and `Expr` internals unchanged.
- **Contract — `SourceTagged<T>: PartialEq`:** `a == b` ⇔ `a.node == b.node` (span **excluded**),
  so existing structural AST tests need no byte offsets.
- Ripple (compiler-guided): `parser.rs` (construct `TaggedStmt`), `resolve.rs` (body rewrite over
  `Vec<TaggedStmt>`), `eval_iterative.rs` (`dispatch_stmt` caller loops, `LocalDecl` pre-scans at
  `:1494`/`:1651`, body construction `:2390`/`:2415`), `eval.rs:634` (`&[Stmt]` → `&[TaggedStmt]`).

### S2 — Lexer emits spanned tokens (`lexer.rs`)
Wrap each token with its source range: `tokenize` returns `Vec<SpannedToken>` where
`SpannedToken = SourceTagged<Token>` (alias in `lexer.rs`). The lexer already tracks byte offsets
(`self.pos`); capture `start` before and `end` after each `next_token`. The parser's token access
centralizes in `peek`/`advance` (`parser.rs:63,79`), which unwrap `.node`; the ~10 direct-index
lookahead sites gain `.node`. Token+span can't desync — structural alignment.
- **Ensures:** every `SpannedToken.span` is the source range of its `.node`.

### S3 — Parser threads statement spans (`parser.rs`)
At each statement site (`parse_program:123`, block body, method body), span the statement from its
first token to its last consumed token (`first.span.to(last.span)`, leading trivia excluded), then
wrap: `SourceTagged::new(span, stmt)`.
- **Ensures:** every `TaggedStmt.span` covers exactly its statement's tokens within one source.

### S4 — `CoverageCollector` + evaluator hook
Opt-in collector on `Evaluator` (e.g. `coverage: Option<RefCell<CoverageCollector>>`), **zero-cost
when `None`**. Record `tagged.span` as touched at statement dispatch — either take `TaggedStmt` in
`dispatch_stmt` and record at entry, or record in the body-iteration caller before dispatching `.node`.
- **Requires:** recording is behind the `Option` branch so a disabled collector adds no measurable
  cost to the ~1.2µs/send path.

### S5 — Report
Post-run: map touched spans → covered lines per source; emit uncovered statements as `file:line`
plus a coverage %. MVP target: `kernel.smol`. Introduces the source registry (`SourceId` → name + text).

### S6 — Tests + wiring
Run the kernel (or a fixture) with the collector on; assert known-covered and known-uncovered
regions. Expose via a `just cov-smol` target or an example binary.

## 5. Decisions (resolved)

1. **SourceId from day 1** — `Span` carries `source: SourceId`. (Avoids retrofitting the field
   across every construction later.)
2. **`SourceTagged` eq ignores span** — keeps AST structural tests span-free.
3. **Tokens are wrapped** (`Vec<SourceTagged<Token>>`), not a parallel `Vec<Span>` — type-enforced
   token/span alignment over a convention-based parallel array, reusing the `SourceTagged` wrapper.
   Cost ≈ 10 `.node` unwraps at the parser's direct-index sites.
4. **Report = kernel-only text MVP** (coverage % + uncovered `file:line`); richer views later.
5. **Sequencing:** coverage now, then resume env-frame d2–e. S1 shares statement-handling code in
   `eval_iterative.rs` with d4 — ordering overlap, not a semantic conflict.

## 6. Out of scope (future)

- **Expression-level tagging** (`Box<Expr>` → `Box<TaggedExpr>` through the tree) for sub-statement
  diagnostics and bytecode debug-info — deferred until needed.
- **Per-node trivia attachment** (reuse `SourceTagged` + a trivia field) — the `Program.comments`
  note's eventual home.
- **SUnit framework** (Planned tooling item 2) — builds on this.

## 7. Rust-lesson / Supernote-PDF hook

S2–S3 (lexer→parser position threading + a source map) is a clean, self-contained lesson on how
Rust parsers model source locations. Candidate for the first Supernote PDF: references on span/
source-map design (e.g. the `codespan` / `ariadne` diagnostic ecosystems and the rustc `SourceMap`).
