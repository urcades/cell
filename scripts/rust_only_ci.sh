#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

production_warning_gate() {
  cat >&2 <<'EOF'
Production warning: this is the internal Rust milestone flow.
It runs broad verification and packaging, but it does not publish a public release.
EOF
}

production_warning_gate
cargo test --workspace
env -u PI_TS_REPO cargo test -p pi-rust-cli --test tui_parity
