default:
    @just --list

# Run the test suite (uses cargo-nextest if installed).
test:
    cargo nextest run --workspace

rel-test:
    cargo nextest run --workspace --release

# Fallback test runner using built-in cargo test.
test-cargo:
    cargo test --workspace

# Watch the workspace and re-run tests on changes.
watch:
    cargo watch -x "nextest run --workspace"

# Format all crates.
fmt:
    cargo fmt --all

# Check formatting without modifying files.
fmt-check:
    cargo fmt --all -- --check

# Lint with clippy; warnings become errors.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# License/security audit via cargo-deny.
deny:
    cargo deny check

# Build the entire workspace.
build:
    cargo build --workspace

# CI-equivalent local check.
check: fmt-check lint test deny

# ── Benchmarks ────────────────────────────────────────────────────────────────

# Run the full Criterion benchmark suite (HTML report written to target/criterion/).
bench:
    cargo bench -p triple-store

# Run a single benchmark group, e.g.: just bench-group in_memory/assert
bench-group group:
    cargo bench -p triple-store -- {{group}}

# ── Coverage ──────────────────────────────────────────────────────────────────

# Paths to LLVM tools in the rustup-managed toolchain. Uses `rustup run` so
# the sysroot is always the rustup toolchain (not a Homebrew or system rustc),
# which is the only one that bundles llvm-tools-preview.
_sysroot := `rustup run 1.94.1 rustc --print sysroot`
_host    := `rustc -vV | sed -n 's/^host: //p'`
_llvm    := _sysroot + "/lib/rustlib/" + _host + "/bin"

# Print a coverage summary table to stdout.
cov:
    LLVM_COV={{_llvm}}/llvm-cov LLVM_PROFDATA={{_llvm}}/llvm-profdata \
        cargo llvm-cov --workspace

# Generate an HTML coverage report (open target/llvm-cov/html/index.html).
cov-html:
    LLVM_COV={{_llvm}}/llvm-cov LLVM_PROFDATA={{_llvm}}/llvm-profdata \
        cargo llvm-cov --workspace --html
    @echo "Coverage report: target/llvm-cov/html/index.html"

# Generate an LCOV file for CI / badge tools (target: lcov.info).
cov-lcov:
    LLVM_COV={{_llvm}}/llvm-cov LLVM_PROFDATA={{_llvm}}/llvm-profdata \
        cargo llvm-cov --workspace --lcov --output-path lcov.info

# Statement-level execution-coverage report for kernel.smol itself (Smol-level,
# via the interpreter's own CoverageCollector — distinct from the llvm-cov
# targets above, which measure Rust coverage).
cov-smol:
    cargo run --release --example kernel_coverage -p interpreter
