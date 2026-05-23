#!/bin/sh
# warren installer: download the right prebuilt binary for this OS/arch and drop it on PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/Yok4ai/warren/master/install.sh | sh
#
# Honoured environment variables:
#   WARREN_INSTALL_DIR   where to install (default: $HOME/.local/bin)
#   WARREN_VERSION       a release tag like v0.1.0 (default: latest)
set -eu

REPO="Yok4ai/warren"
BIN="warren"
INSTALL_DIR="${WARREN_INSTALL_DIR:-$HOME/.local/bin}"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }

# --- detect platform -> Rust target triple (must match the release asset names) ---------------
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) err "unsupported OS '$os' (warren ships Linux and macOS binaries)" ;;
esac

case "$arch" in
  x86_64|amd64)  arch_part="x86_64" ;;
  aarch64|arm64) arch_part="aarch64" ;;
  *) err "unsupported architecture '$arch'" ;;
esac

target="${arch_part}-${os_part}"

# --- resolve download URL ----------------------------------------------------------------------
asset="${BIN}-${target}.tar.gz"
if [ "${WARREN_VERSION:-latest}" = "latest" ]; then
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
  url="https://github.com/${REPO}/releases/download/${WARREN_VERSION}/${asset}"
fi

# --- fetch + unpack ----------------------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  err "need curl or wget to download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf 'downloading %s ...\n' "$asset"
fetch "$url" "$tmp/$asset" || err "download failed: $url
(is there a published release with an asset for $target yet?)"

tar -xzf "$tmp/$asset" -C "$tmp" || err "could not extract $asset"

# The archive may contain the binary at the root or in a subdir; find it.
binpath="$(find "$tmp" -type f -name "$BIN" -perm -u+x 2>/dev/null | head -n1)"
[ -z "$binpath" ] && binpath="$(find "$tmp" -type f -name "$BIN" 2>/dev/null | head -n1)"
[ -z "$binpath" ] && err "archive did not contain a '$BIN' binary"

# --- install -----------------------------------------------------------------------------------
mkdir -p "$INSTALL_DIR"
install -m 0755 "$binpath" "$INSTALL_DIR/$BIN" 2>/dev/null || {
  cp "$binpath" "$INSTALL_DIR/$BIN" && chmod 0755 "$INSTALL_DIR/$BIN"
}

printf '\ninstalled %s -> %s\n' "$BIN" "$INSTALL_DIR/$BIN"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) printf 'run: %s\n' "$BIN" ;;
  *)
    printf '\n%s is not on your PATH. Add this to your shell profile:\n' "$INSTALL_DIR"
    printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    ;;
esac
