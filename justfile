# Spurfire task runner. Uses a POSIX shell on macOS/Linux and Git Bash on Windows.
# Game helper scripts explicitly require Bash.

set dotenv-load := false

# Verify cargo/just are installed and fetch dependencies
setup:
    @command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found (install via https://rustup.rs)"; exit 1; }
    @command -v just >/dev/null 2>&1 || { echo "error: just not found (https://github.com/casey/just)"; exit 1; }
    cargo --version
    just --version
    cargo fetch --locked

# Format all code
fmt:
    cargo fmt

# Lint with warnings denied
lint:
    cargo clippy --locked --all-targets -- -D warnings

# Run the test suite
test:
    cargo test --locked

# Build and install the Godot GDExtension (profile: debug or release)
game-build profile="debug":
    scripts/build-gdext.sh "{{profile}}"

# Build, import, and run the Godot M0 smoke test headlessly
game-test: game-build
    scripts/test-godot.sh

# Build and open the Godot editor
game-editor: game-build
    @godot_bin="${GODOT_BIN:-}"; if [ -z "$godot_bin" ]; then godot_bin="$(command -v godot4 || command -v godot || true)"; fi; [ -n "$godot_bin" ] || { echo "error: Godot 4 not found; set GODOT_BIN" >&2; exit 1; }; "$godot_bin" --editor --path game

# Build and run the Godot project
game-run: game-build
    @godot_bin="${GODOT_BIN:-}"; if [ -z "$godot_bin" ]; then godot_bin="$(command -v godot4 || command -v godot || true)"; fi; [ -n "$godot_bin" ] || { echo "error: Godot 4 not found; set GODOT_BIN" >&2; exit 1; }; "$godot_bin" --path game

# Run the HTTP service in zero-mutation mode on loopback
serve-dry:
    cargo run --locked -p spurfire-server -- --dry-run --bind 127.0.0.1:8080

# Full local gate: fmt check + lint + test
check:
    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked

# Live Tailscale API smoke test (skips cleanly without .env)
e2e:
    @if [ ! -f .env ]; then \
        echo "SKIP: .env not found — copy .env.example and fill in Tailscale OAuth creds to run e2e"; \
        exit 0; \
    fi
    scripts/ts-api.sh token

# Provision a disposable child tailnet and exchange real Spurfire UDP frames through RustScale
p2p-live:
    scripts/live-rustscale-p2p.sh

# Open three real Godot clients with route/RTT telemetry on a disposable child tailnet
p2p-demo:
    scripts/run-p2p-demo.sh

# Remove build artifacts
clean:
    cargo clean
