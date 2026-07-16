# Spurfire task runner. Portable across macOS and Linux.
# On Windows, run via `just` with PowerShell 7 (pwsh) as the shell:
#   set shell := ["pwsh", "-c"]   (or: just --shell pwsh --shell-arg -c <recipe>)

set dotenv-load := false

# Verify cargo/just are installed and fetch dependencies
setup:
    @command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found (install via https://rustup.rs)"; exit 1; }
    @command -v just >/dev/null 2>&1 || { echo "error: just not found (https://github.com/casey/just)"; exit 1; }
    cargo --version
    just --version
    cargo fetch

# Format all code
fmt:
    cargo fmt

# Lint with warnings denied
lint:
    cargo clippy --all-targets -- -D warnings

# Run the test suite
test:
    cargo test

# Full local gate: fmt check + lint + test
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Live Tailscale API smoke test (skips cleanly without .env)
e2e:
    @if [ ! -f .env ]; then \
        echo "SKIP: .env not found — copy .env.example and fill in Tailscale OAuth creds to run e2e"; \
        exit 0; \
    fi
    scripts/ts-api.sh token

# Remove build artifacts
clean:
    cargo clean
