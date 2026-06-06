#!/usr/bin/env bash
#
# install.sh — DOL (Docker Observability Language) binary installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/genc-murat/DockQL/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/genc-murat/DockQL/main/install.sh | bash -s -- 0.7.0
#
# Downloads the pre-built binary from GitHub Releases for the detected
# OS/architecture and installs it to a local bin directory.
#
# Environment variables:
#   DOL_INSTALL_DIR  — custom install directory (default: auto-detected)
#   DOL_VERSION      — version tag to install (default: latest)

set -euo pipefail

REPO="genc-murat/DockQL"
BINARY="dol"
VERSION="${DOL_VERSION:-${1:-latest}}"

# ── Detect OS and architecture ──────────────────────────────────────────

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    linux)   TARGET_OS="unknown-linux-gnu" ;;
    darwin)  TARGET_OS="apple-darwin" ;;
    mingw*|msys*|cygwin*)
        TARGET_OS="pc-windows-msvc"
        ;;
    *)
        echo "❌ Unsupported OS: $OS"
        echo "   Supported: Linux, macOS, Windows (MSYS2/Git Bash)"
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64) TARGET_ARCH="x86_64" ;;
    aarch64|arm64) TARGET_ARCH="aarch64" ;;
    *)
        echo "❌ Unsupported architecture: $ARCH"
        echo "   Supported: x86_64, aarch64"
        exit 1
        ;;
esac

TARGET="${TARGET_ARCH}-${TARGET_OS}"
EXT="tar.gz"

echo "🔍 Detected: $OS ($ARCH) → $TARGET"

# ── Determine install directory ─────────────────────────────────────────

if [ -n "${DOL_INSTALL_DIR:-}" ]; then
    INSTALL_DIR="$DOL_INSTALL_DIR"
elif [ -w "$HOME/.cargo/bin" ]; then
    INSTALL_DIR="$HOME/.cargo/bin"
elif [ -w "/usr/local/bin" ]; then
    INSTALL_DIR="/usr/local/bin"
elif [ -w "$HOME/.local/bin" ]; then
    INSTALL_DIR="$HOME/.local/bin"
else
    INSTALL_DIR="$HOME/.local/bin"
    mkdir -p "$INSTALL_DIR"
fi

# ── Resolve download URL ────────────────────────────────────────────────

if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${BINARY}-${TARGET}.${EXT}"
    VERSION_DISPLAY="latest"
else
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/v${VERSION}/${BINARY}-${TARGET}.${EXT}"
    VERSION_DISPLAY="v${VERSION}"
fi

# ── Download ────────────────────────────────────────────────────────────

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "📥 Downloading ${BINARY} ${VERSION_DISPLAY}..."
curl -fsSL "$DOWNLOAD_URL" -o "${TMP_DIR}/${BINARY}.${EXT}" || {
    echo "❌ Download failed."
    echo "   URL: $DOWNLOAD_URL"
    echo ""
    echo "   Possible reasons:"
    echo "   - The release may not exist yet (run 'cargo build --release' locally)"
    echo "   - No binary for ${TARGET} in this release"
    exit 1
}

# ── Extract ─────────────────────────────────────────────────────────────

echo "📦 Extracting..."
if [ "$OS" = "darwin" ]; then
    tar -xzf "${TMP_DIR}/${BINARY}.${EXT}" -C "$TMP_DIR"
else
    tar -xzf "${TMP_DIR}/${BINARY}.${EXT}" -C "$TMP_DIR"
fi

# The archive from cargo-dist contains the binary at the root
BINARY_PATH=$(find "$TMP_DIR" -maxdepth 2 -name "${BINARY}" -type f | head -1)

if [ -z "$BINARY_PATH" ]; then
    # Fallback: try for Windows .exe
    BINARY_PATH=$(find "$TMP_DIR" -maxdepth 2 -name "${BINARY}.exe" -type f | head -1)
fi

if [ -z "$BINARY_PATH" ]; then
    echo "❌ Could not find ${BINARY} binary in the archive."
    ls -la "$TMP_DIR"
    exit 1
fi

# ── Install ─────────────────────────────────────────────────────────────

echo "📋 Installing to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR"
cp "$BINARY_PATH" "${INSTALL_DIR}/${BINARY}"
chmod +x "${INSTALL_DIR}/${BINARY}"

echo ""
echo "✅ DOL ${VERSION_DISPLAY} installed to ${INSTALL_DIR}/${BINARY}"
echo ""

# ── Post-install message ────────────────────────────────────────────────

if ! command -v "${BINARY}" &>/dev/null; then
    echo "   ⚠️  ${INSTALL_DIR} is not in your PATH."
    echo "   Add it by running:"
    echo ""
    case "$SHELL" in
        *zsh)  echo "       echo 'export PATH=\"\$PATH:${INSTALL_DIR}\"' >> ~/.zshrc" ;;
        *bash) echo "       echo 'export PATH=\"\$PATH:${INSTALL_DIR}\"' >> ~/.bashrc" ;;
        *fish) echo "       fish_add_path ${INSTALL_DIR}" ;;
        *)     echo "       export PATH=\"\$PATH:${INSTALL_DIR}\"" ;;
    esac
    echo ""
fi

echo "   Run '${BINARY} --help' to get started."
