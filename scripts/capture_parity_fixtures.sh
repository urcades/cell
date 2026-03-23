#!/usr/bin/env bash
set -euo pipefail

RUST_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE_DIR="${RUST_ROOT}/fixtures/parity"
RUNNER="${RUST_ROOT}/scripts/ts_parity_runner.mts"

TS_REPO_DIR="${PI_TS_REPO:-}"
if [[ -z "${TS_REPO_DIR}" ]]; then
  echo "PI_TS_REPO is required for TS parity captures. Set it to the TypeScript repo root." >&2
  exit 1
fi
TS_REPO_DIR="$(cd "${TS_REPO_DIR}" && pwd)"
TSX_CLI="${TS_REPO_DIR}/node_modules/tsx/dist/cli.mjs"

if [[ ! -f "${TSX_CLI}" ]]; then
  echo "Missing ${TSX_CLI}. Run npm install in the TypeScript repo first." >&2
  exit 1
fi

mkdir -p "${FIXTURE_DIR}"

for scenario in print-text print-json rpc session-artifact resource-precedence package-commands rpc-images rpc-bash export-cli; do
  output_path="${FIXTURE_DIR}/${scenario}.json"
  echo "Capturing ${scenario} -> ${output_path}"
  (
    cd "${TS_REPO_DIR}"
    PI_TS_REPO="${TS_REPO_DIR}" node "${TSX_CLI}" "${RUNNER}" "${scenario}" > "${output_path}"
  )
done

echo "Captured parity fixtures in ${FIXTURE_DIR}"
