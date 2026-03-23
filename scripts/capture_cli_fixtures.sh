#!/usr/bin/env bash
set -euo pipefail

RUST_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURES_DIR="$RUST_ROOT/fixtures/cli"
RUST_BIN="${PI_RUST_BIN:-$RUST_ROOT/target/debug/pi-rust}"
UPSTREAM_HELP="$FIXTURES_DIR/upstream-help.txt"
UPSTREAM_VERSION="$FIXTURES_DIR/upstream-version.txt"
INSTALL_HELP="$FIXTURES_DIR/install-help.txt"
INSTALL_INVALID_OPTION_STDERR="$FIXTURES_DIR/install-invalid-option-stderr.txt"
EXTENSION_UNSUPPORTED_STDERR="$FIXTURES_DIR/extension-unsupported-stderr.txt"

mkdir -p "$FIXTURES_DIR"

upstream_ok=true
TS_REPO_DIR="${PI_TS_REPO:-}"
TSX=()

if [[ -n "$TS_REPO_DIR" ]]; then
  TS_REPO_DIR="$(cd "$TS_REPO_DIR" && pwd)"
  if [[ -f "$TS_REPO_DIR/node_modules/tsx/dist/cli.mjs" ]]; then
    TSX=(node "$TS_REPO_DIR/node_modules/tsx/dist/cli.mjs")
  elif [[ -x "$TS_REPO_DIR/node_modules/.bin/tsx" ]]; then
    TSX=("$TS_REPO_DIR/node_modules/.bin/tsx")
  else
    echo "Warning: unable to locate tsx in $TS_REPO_DIR; skipping upstream TS fixture capture." >&2
  fi
else
  echo "Warning: PI_TS_REPO is unset; skipping upstream TS fixture capture." >&2
fi

capture_optional_stdout() {
  local output_file="$1"
  shift
  local tmp_file
  tmp_file="$(mktemp)"
  if "$@" > "$tmp_file"; then
    mv "$tmp_file" "$output_file"
    return 0
  fi
  rm -f "$tmp_file"
  return 1
}

if [[ ${#TSX[@]} -gt 0 ]]; then
  if ! capture_optional_stdout "$UPSTREAM_HELP" "${TSX[@]}" "$TS_REPO_DIR/packages/coding-agent/src/cli.ts" --help; then
    upstream_ok=false
  fi
  if ! capture_optional_stdout "$UPSTREAM_VERSION" "${TSX[@]}" "$TS_REPO_DIR/packages/coding-agent/src/cli.ts" --version; then
    upstream_ok=false
  fi
  if [[ "$upstream_ok" != "true" ]]; then
    echo "Warning: unable to capture upstream TS fixtures; continuing with deterministic local fixtures." >&2
  fi
fi

capture_expected_failure_stderr() {
  local output_file="$1"
  local expected_substring="$2"
  shift 2
  local tmp_file
  tmp_file="$(mktemp)"

  set +e
  "$@" > /dev/null 2> "$tmp_file"
  local status=$?
  set -e

  if [[ $status -eq 0 ]]; then
    echo "Expected command to fail while capturing $output_file"
    rm -f "$tmp_file"
    return 1
  fi

  if ! grep -Fq "$expected_substring" "$tmp_file"; then
    echo "Captured stderr did not contain expected marker: $expected_substring" >&2
    rm -f "$tmp_file"
    return 1
  fi

  mv "$tmp_file" "$output_file"
}

(
  cd "$RUST_ROOT"
  cargo build -q -p pi-rust-cli
  capture_optional_stdout "$INSTALL_HELP" \
    "$RUST_BIN" install --help
  capture_expected_failure_stderr "$INSTALL_INVALID_OPTION_STDERR" \
    "Unknown option --bogus for \"install\"." \
    "$RUST_BIN" install --bogus
  capture_expected_failure_stderr "$EXTENSION_UNSUPPORTED_STDERR" \
    "does not execute JS/TS extensions" \
    "$RUST_BIN" --extension ./example.ts
)

echo "Captured upstream fixtures:"
if [[ ${#TSX[@]} -gt 0 && "$upstream_ok" == "true" ]]; then
  echo "  $UPSTREAM_HELP"
  echo "  $UPSTREAM_VERSION"
else
  echo "  (skipped because PI_TS_REPO was not provided or tsx was unavailable)"
fi
echo "Captured deterministic local fixtures:"
echo "  $INSTALL_HELP"
echo "  $INSTALL_INVALID_OPTION_STDERR"
echo "  $EXTENSION_UNSUPPORTED_STDERR"
