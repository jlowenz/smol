default:
    @just --list

# Run the test suite (uses cargo-nextest if installed).
test:
    cargo nextest run --workspace

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
