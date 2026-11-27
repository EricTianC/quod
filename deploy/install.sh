#!/usr/bin/env bash
set -euo pipefail

QUOD_REPO="${QUOD_REPO:-EricTianC/quod}"
QUOD_VERSION="${QUOD_VERSION:-latest}"
QUOD_INSTALL_DIR="${QUOD_INSTALL_DIR:-$HOME/.local/bin}"

usage() {
  cat <<'EOF'
Usage: install.sh [--repo <owner/repo>] [--version <tag|latest>] [--install-dir <dir>]

Environment variables:
  QUOD_REPO         GitHub repository hosting releases (default: EricTianC/quod)
  QUOD_VERSION      Release tag (e.g. v0.1.0) or "latest" (default: latest)
  QUOD_INSTALL_DIR  Destination directory for the `quod` binary (default: $HOME/.local/bin)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      QUOD_REPO="$2"
      shift 2
      ;;
    --version)
      QUOD_VERSION="$2"
      shift 2
      ;;
    --install-dir)
      QUOD_INSTALL_DIR="$2"
      shift 2
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
done

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}" in
    Linux)
      case "${arch}" in
        x86_64|amd64) echo "x86_64-unknown-linux-musl" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-musl" ;;
        *)
          echo "Unsupported Linux architecture: ${arch}" >&2
          exit 1
          ;;
      esac
      ;;
    Darwin)
      case "${arch}" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *)
          echo "Unsupported macOS architecture: ${arch}" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      echo "Unsupported operating system: ${os}" >&2
      exit 1
      ;;
  esac
}

verify_sha256() {
  local file="$1"
  local expected="$2"
  local actual=""
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${file}" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${file}" | awk '{print $1}')"
  else
    echo "[quod] Warning: neither sha256sum nor shasum found; skipping checksum verification." >&2
    return 0
  fi
  if [ "${actual}" != "${expected}" ]; then
    echo "[quod] Checksum mismatch for ${file}: expected ${expected}, got ${actual}" >&2
    exit 1
  fi
}

TARGET="$(detect_target)"
ASSET="quod-${TARGET}.tar.gz"
if [ "${QUOD_VERSION}" = "latest" ]; then
  BASE_URL="https://github.com/${QUOD_REPO}/releases/latest/download"
else
  BASE_URL="https://github.com/${QUOD_REPO}/releases/download/${QUOD_VERSION}"
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

echo "[quod] Downloading ${ASSET} from ${BASE_URL}..."
curl -fsSL "${BASE_URL}/${ASSET}" -o "${TMP_DIR}/${ASSET}"

if curl -fsSL "${BASE_URL}/SHA256SUMS" -o "${TMP_DIR}/SHA256SUMS" 2>/dev/null; then
  EXPECTED_HASH="$(awk -v asset="${ASSET}" '$2 == asset || $2 == ("./" asset) {print $1; exit}' "${TMP_DIR}/SHA256SUMS")"
  if [ -n "${EXPECTED_HASH}" ]; then
    verify_sha256 "${TMP_DIR}/${ASSET}" "${EXPECTED_HASH}"
    echo "[quod] Verified SHA-256 checksum."
  fi
fi

tar -xzf "${TMP_DIR}/${ASSET}" -C "${TMP_DIR}"
if [ ! -f "${TMP_DIR}/quod" ]; then
  echo "[quod] Archive ${ASSET} did not contain expected 'quod' binary." >&2
  exit 1
fi

mkdir -p "${QUOD_INSTALL_DIR}"
install -m 755 "${TMP_DIR}/quod" "${QUOD_INSTALL_DIR}/quod"
echo "[quod] Installed quod to ${QUOD_INSTALL_DIR}/quod"

case ":${PATH}:" in
  *":${QUOD_INSTALL_DIR}:"*) ;;
  *)
    echo "[quod] Add ${QUOD_INSTALL_DIR} to your PATH:"
    echo "       export PATH=\"${QUOD_INSTALL_DIR}:\$PATH\""
    ;;
esac

"${QUOD_INSTALL_DIR}/quod" --version || true

if ! command -v lake >/dev/null 2>&1; then
  echo "[quod] Note: Lean 4 ('lake') was not found on PATH."
  echo "       Install elan + Lean 4 if you plan to run proofs locally:"
  echo "       curl -sSf https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh | sh -s -- -y --default-toolchain leanprover/lean4:v4.36.0"
fi
