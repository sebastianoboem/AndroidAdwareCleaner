#!/usr/bin/env bash
# Download Android platform-tools for bundling (macOS / Linux dev).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/resources/platform-tools"
mkdir -p "$DEST"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS-$ARCH" in
  Darwin-arm64|Darwin-aarch64)
    URL="https://dl.google.com/android/repository/platform-tools-latest-darwin.zip"
    ;;
  Darwin-x86_64)
    URL="https://dl.google.com/android/repository/platform-tools-latest-darwin.zip"
    ;;
  Linux-x86_64|Linux-amd64)
    URL="https://dl.google.com/android/repository/platform-tools-latest-linux.zip"
    ;;
  *)
    echo "Unsupported platform: $OS $ARCH — download Windows zip manually for CI"
    exit 1
    ;;
esac

TMP="$(mktemp -d)"
curl -fsSL "$URL" -o "$TMP/platform-tools.zip"
unzip -q "$TMP/platform-tools.zip" -d "$TMP"
cp -R "$TMP/platform-tools/"* "$DEST/"
chmod +x "$DEST/adb" 2>/dev/null || true
echo "platform-tools installed to $DEST"
