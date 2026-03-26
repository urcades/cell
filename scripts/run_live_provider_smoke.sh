#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo test -p cell-cli --test live_provider_smoke -- --ignored --nocapture "$@"
