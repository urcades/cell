#!/usr/bin/env bash
set -euo pipefail

RUST_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="${RUST_ROOT}/fixtures/tui"
RUNNER="${RUST_ROOT}/scripts/tui_parity_runner.mjs"
RUST_BIN="${CELL_BIN:-${RUST_ROOT}/target/debug/cell}"
if [[ -n "${PI_TUI_RUNTIME:-}" ]]; then
  RUNTIME="${PI_TUI_RUNTIME}"
elif [[ -n "${PI_TS_REPO:-}" ]]; then
  RUNTIME="both"
else
  RUNTIME="rust"
fi
WIDTH="${PI_TUI_WIDTH:-80}"
HEIGHT="${PI_TUI_HEIGHT:-24}"

mkdir -p "${OUTPUT_DIR}"

for scenario in startup slash model resume scoped-models manual-bash sticky-footer bash diff; do
  output_path="${OUTPUT_DIR}/${scenario}.json"
  echo "Capturing ${scenario} -> ${output_path}"
  (
    cd "${RUST_ROOT}"
    node "${RUNNER}" \
      --runtime "${RUNTIME}" \
      --width "${WIDTH}" \
      --height "${HEIGHT}" \
      --rust-bin "${RUST_BIN}" \
      --scenario "${scenario}" > "${output_path}"
  )
done

echo "Captured TUI parity fixtures in ${OUTPUT_DIR}"
