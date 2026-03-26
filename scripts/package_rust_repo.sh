#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(awk -F'\"' '/^version = \"/ { print $2; exit }' "$ROOT_DIR/Cargo.toml")"
TARGET_TRIPLE="$(rustc -vV | awk '/^host: / { print $2 }')"
OUTPUT_PATH="$ROOT_DIR/dist/releases/${VERSION}/cell-${VERSION}-${TARGET_TRIPLE}.tar.gz"
BINARY_NAME="cell"

usage() {
  cat <<'EOF'
Usage: package_rust_repo.sh [--output <path>]

Build the release cell binary and create a current-platform archive without depending on PI_TS_REPO.
By default the archive is written to dist/releases/<version>/cell-<version>-<target>.tar.gz.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      shift
      if [[ $# -eq 0 ]]; then
        echo "Missing value for --output" >&2
        exit 1
      fi
      OUTPUT_PATH="$1"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

CHECKSUM_PATH="${OUTPUT_PATH}.sha256"

mkdir -p "$(dirname "$OUTPUT_PATH")"

cd "$ROOT_DIR"

cargo build --release -p cell-cli

STAGING_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGING_DIR"' EXIT
ARCHIVE_ROOT="cell-${VERSION}-${TARGET_TRIPLE}"
mkdir -p "$STAGING_DIR/$ARCHIVE_ROOT"

cp "$ROOT_DIR/target/release/$BINARY_NAME" "$STAGING_DIR/$ARCHIVE_ROOT/"
cp "$ROOT_DIR/README.md" "$STAGING_DIR/$ARCHIVE_ROOT/"
cp "$ROOT_DIR/CHANGELOG.md" "$STAGING_DIR/$ARCHIVE_ROOT/"
cp "$ROOT_DIR/LICENSE" "$STAGING_DIR/$ARCHIVE_ROOT/"
cp -R "$ROOT_DIR/docs" "$STAGING_DIR/$ARCHIVE_ROOT/"

export COPYFILE_DISABLE=1
export COPY_EXTENDED_ATTRIBUTES_DISABLE=1
tar -C "$STAGING_DIR" -czf "$OUTPUT_PATH" "$ARCHIVE_ROOT"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$OUTPUT_PATH" > "$CHECKSUM_PATH"
else
  shasum -a 256 "$OUTPUT_PATH" > "$CHECKSUM_PATH"
fi

echo "$OUTPUT_PATH"
