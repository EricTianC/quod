#!/usr/bin/env bash
set -euo pipefail

TARGET="${RUST_TARGET:-x86_64-unknown-linux-musl}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist/${TARGET}"

echo "[quod] Building static binaries for target=${TARGET}"
cd "${ROOT_DIR}"

if command -v rustup >/dev/null 2>&1; then
  rustup target add "${TARGET}" >/dev/null 2>&1 || true
fi

cargo build --release --locked --target "${TARGET}" -p quod-hub -p quod-verifier -p quod-cli

mkdir -p "${DIST_DIR}"
cp "target/${TARGET}/release/quod-hub" "${DIST_DIR}/quod-hub"
cp "target/${TARGET}/release/quod-verifier" "${DIST_DIR}/quod-verifier"
cp "target/${TARGET}/release/quod" "${DIST_DIR}/quod"

echo "[quod] Built artifacts:"
ls -lh "${DIST_DIR}/quod-hub" "${DIST_DIR}/quod-verifier" "${DIST_DIR}/quod"
