# Chunk 16: Interpreter Bootstrap — Value Model, Parser, and Tree-Walking Evaluator

## Status

Chunks 1–15 delivered a complete, durable triple store with WAL persistence,
predicate registry recovery, and project tooling (benchmarks + coverage). Chunk 16
begins **Phase 2** of the implementation plan: the bootstrap interpreter.

Per the Project Summary and build order, the goal of Phase 2 is a tree-walking
interpreter over a meaningful subset of the surface language. This chunk delivers
a new `interpreter` crate with:

1. A `Value` type covering all immediate/scalar kinds plus closures.
2. A lexical `Env` (environment frame stack) for variable bindings.
3. An AST definition for expressions and statements.
4. A hand-written lexer and recursive-descent parser.
5. A tree-walking evaluator with built-in method dispatch.
6. A minimal REPL binary (`smol`) that reads a line, evaluates, and prints.

**Explicitly deferred to later chunks:**

- User-defined class and method definitions (chunk 17+)
- Entity identity, actor model, and process spawning
- Triple store integration
- Garbage collection (bootstrap uses `Arc`; GC is a Phase 4 concern)
- Module and namespace declarations
- Error resumption / condition system
- Bytecode compilation

---

## Crate: `interpreter`

Add `crates/interpreter/` as a new workspace member. A binary target `smol`
(in `src/main.rs`) provides the REPL.

**`Cargo.toml` additions** (workspace root):

```toml
[workspace]
members = ["crates/triple-store", "crates/interpreter"]
```

**`crates/interpreter/Cargo.toml`**:

```toml
[package]
name = "interpreter"
# ... workspace defaults ...

[[bin]]
name = "smol"
path = "src/main.rs"

[dependencies]
thiserror = { workspace = true }
```

No `triple-store` dependency yet. The interpreter crate is self-contained in
this chunk.

---

## Item 1 — `Value` type (`src/value.rs`)

`Value` is the runtime representation of every object in the language. For the
bootstrap, this is a plain Rust enum. NaN-boxing and tagged-pointer schemes are
explicitly deferred — they are performance optimizations that would obscure the
bootstrap semantics and require `unsafe`.

```rust
pub enum Value {
    Int(i64),
    Float(f64),        // equality via bit-pattern (see below)
    Bool(bool),
    Nil,
    Symbol(Arc<str>), // identity by content; no global interner yet
    Str(Arc<str>),
    Block(Arc<Block>),
    // Object(Arc<Object>) — scaffolded but unused until chunk 17
}
```

`f64` does not implement `Eq` in Rust. `Value` derives `PartialEq` but must
implement `Eq` manually using bit-pattern comparison for `Float`. This matches
the `OrderingKey` pattern established in `predicates.rs` and is the correct
choice for a value that must be usable as a `HashMap` key in environments.

### `Block`

```rust
pub struct Block {
    pub params: Vec<Arc<str>>,
    pub body:   Vec<Stmt>,       // Stmt defined in ast.rs
    pub env:    Env,             // captured lexical env (clone at block creation)
}
```

Blocks capture the defining `Env` by clone. This is a bootstrap simplification:
in a production interpreter, closures would share a heap-allocated environment
cell to support mutation of captured variables. For this chunk, outer-variable
mutation from within a block is not supported; only reading captures is needed
to pass the test suite. Document this limitation explicitly.

`Value` must be `Clone`. `Block` is heap-allocated via `Arc` so cloning a
`Value::Block` is cheap.

---

## Item 2 — `Env` (`src/env.rs`)

Lexical environments as a mutable frame stack.

```rust
pub struct Env {
    frames: Vec<HashMap<Arc<str>, Value>>,
}

impl Env {
    pub fn new() -> Self;
    pub fn push_frame(&mut self);
    pub fn pop_frame(&mut self);

    /// Requires: at least one frame is live.
    /// Ensures: binds `name` in the innermost frame.
    pub fn define(&mut self, name: Arc<str>, value: Value);

    /// Walk from innermost to outermost; return the first binding found.
    pub fn get(&self, name: &str) -> Option<&Value>;

    /// Walk from innermost to outermost; update the first binding found.
    /// Returns false if no binding exists (caller should treat as error).
    pub fn set(&mut self, name: &str, value: Value) -> bool;
}
```

`Env` is `Clone` so closures can snapshot it at definition time.

---

## Item 3 — AST (`src/ast.rs`)

Minimal expression/statement node types. All nodes are owned values (no
lifetime parameters); `Box<Expr>` for recursive nodes.

```rust
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    Str(Arc<str>),
    Symbol(Arc<str>),
}

pub enum Message {
    Unary(Arc<str>),
    Binary { op: Arc<str>, arg: Box<Expr> },
    Keyword { parts: Vec<(Arc<str>, Expr)> },
}

pub enum Expr {
    Lit(Literal),
    Var(Arc<str>),
    Send { receiver: Box<Expr>, message: Message },
    Block { params: Vec<Arc<str>>, body: Vec<Stmt> },
    Cascade { receiver: Box<Expr>, messages: Vec<Message> },
}

pub enum Stmt {
    Expr(Expr),
    Assign { name: Arc<str>, value: Expr },
    Return(Expr),        // `^`
    LocalDecl(Vec<Arc<str>>),   // `| x y z |`
}

pub struct Program(pub Vec<Stmt>);
```

`Cascade` (e.g. `stream nextPut: 1; nextPut: 2; yourself`) is included at the
AST level because it requires no additional evaluator machinery — it is simply
syntactic sugar for repeated sends on the same receiver. Desugar at parse time
into a `Cascade` node that the evaluator handles in one loop.

`LocalDecl` represents the Smalltalk `| x y z |` temporary-variable
declaration at the start of a method body or block. The evaluator binds all
declared names to `Value::Nil` in the current frame before evaluating any
statements.

---

## Item 4 — Lexer (`src/lexer.rs`)

A hand-written tokenizer. Produces a `Vec<Token>` (not a streaming iterator) so
the parser can look ahead without combinator machinery.

### Token set

```rust
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Str(Arc<str>),    // single-quoted
    Symbol(Arc<str>), // #word or #'arbitrary string'
    True, False, Nil,
    // Identifiers
    Ident(Arc<str>),  // starts with lowercase or uppercase
    Keyword(Arc<str>), // identifier followed immediately by ':'
    // Operators / punctuation
    BinOp(Arc<str>),  // +  -  *  /  <  >  <=  >=  =  ~=  ,  @  \\  //
    Assign,           // :=
    Caret,            // ^
    Dot,              // .
    Semi,             // ; (cascade)
    Pipe,             // |
    LParen, RParen,
    LBracket, RBracket,
    // Sentinel
    Eof,
}
```

Comments (`"..."`) are skipped during tokenization. Character literals
(`$A`) are deferred.

### Error handling

`LexError` via `thiserror`, variants: `UnexpectedChar`, `UnterminatedString`,
`InvalidNumber`.

---

## Item 5 — Parser (`src/parser.rs`)

Recursive-descent parser following Smalltalk's three-level message precedence:

```
program      ::= local_decl? stmt* EOF
local_decl   ::= '|' IDENT* '|'
stmt         ::= '^' expr '.'?
              | IDENT ':=' expr '.'?
              | expr '.'?
expr         ::= keyword_expr
keyword_expr ::= binary_expr (KEYWORD binary_expr)+ cascade?
              | binary_expr cascade?
cascade      ::= (';' message)+
message      ::= KEYWORD binary_expr   (keyword part)
              | BINOP unary_expr       (binary part)
              | IDENT                  (unary part)
binary_expr  ::= unary_expr (BINOP unary_expr)*
unary_expr   ::= primary IDENT*
primary      ::= literal
              | IDENT
              | '(' expr ')'
              | '[' local_decl? (':' IDENT)* ('|' stmt*)? ']'
literal      ::= INT | FLOAT | STR | SYMBOL | TRUE | FALSE | NIL
```

`ParseError` via `thiserror`, variants: `UnexpectedToken`, `UnexpectedEof`,
`InvalidAssignTarget`.

**Key parsing notes:**

- `KEYWORD` tokens (`at:`, `put:`, `ifTrue:ifFalse:`) are collected greedily
  until the sequence ends. `obj at: 1 put: 2` is one three-part keyword send
  with selector `at:put:`, not two separate sends.
- Assignment (`IDENT ':=' expr`) is detected by one-token lookahead: if the
  current token is an `Ident` and the next is `Assign`, parse as assignment.
  Otherwise parse as expression-statement.
- Block parameters: `[:x :y | ...]`. The `|` that separates params from body
  is the same token as the local-decl `| x y |` pipe; disambiguate by position
  (after block params vs at the start of any sequence).

---

## Item 6 — Evaluator (`src/eval.rs`) and Built-ins (`src/builtins.rs`)

### `EvalError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("message not understood: {receiver_kind} does not understand #{selector}")]
    MessageNotUnderstood { receiver_kind: &'static str, selector: Arc<str> },
    #[error("undefined variable: {0}")]
    UndefinedVariable(Arc<str>),
    #[error("wrong number of arguments: expected {expected}, got {got}")]
    ArityMismatch { expected: usize, got: usize },
    #[error("type error: {0}")]
    TypeError(String),
    // Non-local return: propagated up the call stack until the enclosing
    // method frame catches it. Not an actual error condition.
    #[error("^-return")]
    Return(Value),
}
```

`EvalError::Return` is the standard tree-walker trick for implementing `^`
(non-local return from a block to its enclosing method). The method-level
`eval_program` catches `Return` and unwraps it. Un-caught `Return` at the
top level of the REPL is treated as a normal result.

### Evaluator entry point

```rust
pub struct Evaluator;

impl Evaluator {
    /// Evaluate a parsed program in the given environment.
    ///
    /// Requires: `env` is initialized (at least one frame live).
    /// Ensures: returns the value of the last statement, or `Value::Nil` if
    ///   `stmts` is empty. A `^`-return propagates as `EvalError::Return`;
    ///   callers that represent a method frame should catch and unwrap it.
    pub fn eval_program(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError>;
    pub fn eval_expr(&self, expr: &Expr, env: &mut Env) -> Result<Value, EvalError>;
}
```

Evaluation of a `Stmt::Assign` writes to the environment via `env.set`; if the
variable is not yet bound, falls back to `env.define` in the current frame
(Smalltalk `| x |` declarations pre-bind, but assignment to undeclared names is
also allowed at the top level of the REPL).

### Built-in method dispatch (`src/builtins.rs`)

`Value::perform(selector, args, eval, env)` dispatches built-in messages.
Each `Value` variant handles its own selectors:

| Receiver | Selectors |
|---|---|
| `Int` | `+` `-` `*` `/` `//` `\\` `<` `>` `<=` `>=` `=` `~=` `negated` `abs` `max:` `min:` `printString` `asFloat` `timesRepeat:` `to:do:` `to:by:do:` |
| `Float` | `+` `-` `*` `/` `<` `>` `<=` `>=` `=` `~=` `negated` `abs` `truncated` `rounded` `ceiling` `floor` `printString` `asInt` |
| `Bool` | `ifTrue:` `ifFalse:` `ifTrue:ifFalse:` `ifFalse:ifTrue:` `&` `\|` `not` `=` `printString` `and:` `or:` |
| `Nil` | `isNil` `notNil` `ifNil:` `ifNotNil:` `ifNil:ifNotNil:` `printString` `=` |
| `Str` | `size` `at:` `,` `=` `~=` `printString` `asSymbol` `reversed` `copyFrom:to:` `includesSubString:` |
| `Symbol` | `=` `~=` `printString` `asString` `asSymbol` |
| `Block` | `value` `value:` `value:value:` `value:value:value:` `whileTrue:` `whileFalse:` `whileTrue` `whileFalse` `on:do:` |

`Block` dispatch for `value`, `value:value:`, etc. creates a new frame, binds
parameters, evaluates the body via `Evaluator::eval_program`, and catches
`EvalError::Return`.

`timesRepeat:`, `to:do:`, `whileTrue:`, etc. evaluate the block argument
repeatedly. These are the only looping constructs in chunk 16; there is no
syntactic `while` or `for`.

`on:do:` is a basic exception handler: the first argument is a symbol (the
condition class), the second is a one-argument handler block. For chunk 16,
only `#Error` is recognized as a catch-all; re-raising is not supported.

---

## Item 7 — REPL (`src/main.rs`)

A minimal read-eval-print loop. No external readline library yet; use
`std::io::stdin().lines()`.

```
smol 0.1.0
> 3 + 4
7
> 'hello' , ' world'
'hello world'
> [:x | x * x] value: 7
49
> ^D
```

Each line is lexed, parsed as a `Program`, evaluated in a persistent top-level
`Env`, and the result is printed via the `printString` built-in. Errors print
the error message and continue (do not exit). Multi-line input (e.g. block
literals spanning a line) is not supported in chunk 16 — each line is an
independent program.

The `printString` contract: every `Value` variant must produce a deterministic
string representation. `Block` prints as `'a Block'`. `Nil` prints as `'nil'`.

---

## Implementation Order

1. Create `crates/interpreter/` with skeleton `lib.rs` and `Cargo.toml`. Add to
   workspace. Verify `cargo build -p interpreter` compiles empty crate.
2. `ast.rs`: define all AST node types. No logic.
3. `value.rs`: `Value`, `Block`. Derive/implement `Clone`, `PartialEq`, `Eq`.
   Test `Eq` on Float bit-pattern.
4. `env.rs`: `Env`. Unit tests for push/pop/define/get/set frame scoping.
5. `lexer.rs`: `Token`, `LexError`, `Lexer::tokenize`. Test all token types
   including edge cases (negative literals, symbol quoting, adjacent keywords).
6. `parser.rs`: `ParseError`, `Parser::parse`. Test AST shapes for a range of
   inputs: literals, message sends at all three levels, blocks, cascades,
   assignments, local decls, `^`.
7. `builtins.rs` + `eval.rs`: evaluator and built-in dispatch. Start with `Int`
   arithmetic, then `Bool` conditionals (needed for `whileTrue:`), then `Block`
   value-sends (needed for iteration), then remaining kinds. Test incrementally.
8. `main.rs`: REPL. Manual smoke test in the REPL; no automated test for
   interactive I/O.
9. Add `interpreter` benchmark group to `crates/interpreter/benches/interpreter.rs`:
   measure parse + eval throughput for a representative program (e.g. a 1000-
   iteration `timesRepeat:` loop, a Fibonacci recursion).

---

## Open Questions

### Cascade evaluation and return value

In standard Smalltalk, a cascade (`a msg1; msg2; yourself`) sends `msg1` and
`msg2` to the same receiver; `yourself` returns the receiver. The cascade's
value is the value of the last message. Should chunk 16 implement `yourself`
as a built-in on all values (returns `self`), or omit cascades from the REPL
until they are needed? Recommend including `yourself` since it costs nothing to
implement and makes the cascade feature usable.

### `printString` vs `print:`

In Smalltalk, `printString` returns a string; side-effecting printing goes
through a stream (`Transcript print: x`). The REPL currently calls
`printString` to obtain the display string, then prints it in Rust. A
`Transcript` entity (store-backed, subscribable) is a chunk 17+ concern.
For now, `printString` is the correct hook.

### Symbol interning

`Value::Symbol(Arc<str>)` uses content equality (string comparison). A global
symbol table (interner) would give pointer equality, which matters for
performance in method dispatch. Deferred to the point when method lookup
benchmarks show measurable overhead. For chunk 16, content equality is correct
and measurable.

### Float precision and `printString`

`Float printString` must produce a round-trippable representation (use
`ryu` or Rust's `{:?}` format which produces round-trippable output). Do not
use `{}` (may lose precision). Decide and record the choice before implementing.

### Error class hierarchy

`on:do:` accepts a symbol as the "exception class." Real Smalltalk has a class
hierarchy (`Error`, `ZeroDivide`, etc.). For chunk 16, only `#Error` is
recognized as a universal catch. Document this as a known gap; a real condition
system is a post-process-model concern.

### `super` and `thisContext`

Neither `super` (superclass send) nor `thisContext` (reification of the call
stack) can be supported without a class/method model. Do not add AST nodes or
evaluator cases for these in chunk 16. Document as deferred.
