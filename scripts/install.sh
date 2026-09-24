#!/usr/bin/env bash
# install.sh — one-line AgentOS installer.
#
#   curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | bash
#
# Env overrides:
#   AGENTOS_VERSION       release tag (default: latest), e.g. v1.0.0
#   AGENTOS_INSTALL_DIR   install dir   (default: ~/.local/bin)
#   AGENTOS_FLAVOR        "lite" for the no-embeddings build (linux-amd64 only)
#   AGENTOS_SKIP_SIG_VERIFY=1  install on checksum alone when no minisign/rsign
#                              is available (not recommended — see below)
#
# Always verifies the SHA-256 checksum, requires the .sig asset, and verifies
# the minisign signature against the pinned public key. Without minisign or
# rsign installed the signature cannot be checked and the install fails closed.
set -euo pipefail

REPO="AjasMohammed/Agos"
VERSION="${AGENTOS_VERSION:-latest}"
INSTALL_DIR="${AGENTOS_INSTALL_DIR:-$HOME/.local/bin}"
# Release signing public key (key id 0692DEA1023C9472), pinned here so a
# compromised repo cannot swap it; must match packaging/signing/agentos-release.pub.
PUBKEY="RWRylDwCod6SBrcNGIz6wZsrWW5Y9o3I+OT/opftcrq4tK/KhgXvtdKl"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required."
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 \
  || die "sha256sum (or shasum) is required."
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# --- detect platform ----------------------------------------------------------
OS="$(uname -s)"; ARCH="$(uname -m)"
case "$OS" in
  Linux)  os=linux ;;
  Darwin) os=darwin ;;
  *) die "Unsupported OS '$OS'. On Windows use WSL2 (recommended) or scripts/install.ps1 (beta)." ;;
esac
case "$ARCH" in
  x86_64|amd64)  arch=amd64 ;;
  arm64|aarch64) arch=arm64 ;;
  *) die "Unsupported arch '$ARCH'." ;;
esac
ASSET="agentos-${os}-${arch}"
# AGENTOS_FLAVOR=lite: no ONNX/MiniLM vector search (FTS5 only), linux-amd64 only.
if [ "${AGENTOS_FLAVOR:-}" = lite ]; then
  [ "$ASSET" = agentos-linux-amd64 ] || die "The lite build is published for linux-amd64 only."
  ASSET="agentos-lite-linux-amd64"
fi

if [ "$os" != "linux" ]; then
  warn "Linux is the primary target. On macOS, seccomp sandboxing and most HAL"
  warn "drivers are unavailable; shell-exec and hardware tools degrade gracefully."
fi

# --- resolve release base url -------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  # Newest final release wins; a pre-release (v1.0.0-rc.1) is used only while
  # no final exists, because GitHub's /releases/latest skips pre-releases and
  # would 404 during an rc window. Falls back to /latest if the API is unreachable.
  tags="$(curl --proto '=https' --tlsv1.2 -fsSL "https://api.github.com/repos/${REPO}/releases?per_page=20" 2>/dev/null \
    | sed -nE 's/.*"tag_name": *"([^"]+)".*/\1/p' || true)"
  VERSION="$(printf '%s\n' "$tags" | grep -v -e '-' | head -n1 || true)"
  [ -n "$VERSION" ] || VERSION="$(printf '%s\n' "$tags" | head -n1)"
  case "$VERSION" in v[0-9]*) ;; *) VERSION="" ;; esac
  if [ -n "$VERSION" ]; then
    BASE="https://github.com/${REPO}/releases/download/${VERSION}"
  else
    VERSION=latest
    BASE="https://github.com/${REPO}/releases/latest/download"
  fi
else
  BASE="https://github.com/${REPO}/releases/download/${VERSION}"
fi

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
info "Downloading $ASSET ($VERSION)"
curl --proto '=https' --tlsv1.2 -fsSL "$BASE/$ASSET"        -o "$tmp/agentos"        || die "Download failed for $ASSET."
curl --proto '=https' --tlsv1.2 -fsSL "$BASE/$ASSET.sha256" -o "$tmp/agentos.sha256" || die "Checksum file missing for $ASSET."
curl --proto '=https' --tlsv1.2 -fsSL "$BASE/$ASSET.sig"    -o "$tmp/agentos.sig"    || die "Signature missing for $ASSET — refusing to install."

# --- verify checksum (mandatory) ----------------------------------------------
info "Verifying checksum"
( cd "$tmp" && sed "s|$ASSET|agentos|" agentos.sha256 | sha256 -c - ) \
  || die "Checksum verification failed — refusing to install."

# --- verify signature (mandatory) ---------------------------------------------
# The checksum above proves integrity, not authenticity: agentos.sha256 comes
# from the same URL as the binary, so whoever can serve one can serve the other.
# The minisign signature against the pinned pubkey is the only check an attacker
# controlling the download cannot forge — so a missing verifier fails the
# install instead of warning and running the binary anyway.
if command -v minisign >/dev/null 2>&1; then
  info "Verifying signature (minisign)"
  minisign -V -P "$PUBKEY" -x "$tmp/agentos.sig" -m "$tmp/agentos" \
    || die "Signature verification failed — refusing to install."
elif command -v rsign >/dev/null 2>&1; then
  info "Verifying signature (rsign)"
  rsign verify -P "$PUBKEY" -x "$tmp/agentos.sig" "$tmp/agentos" \
    || die "Signature verification failed — refusing to install."
elif [ "${AGENTOS_SKIP_SIG_VERIFY:-}" = 1 ]; then
  warn "AGENTOS_SKIP_SIG_VERIFY=1 — installing on checksum alone."
  warn "The checksum comes from the same server as the binary; this does NOT"
  warn "prove the release was signed by the AgentOS release key."
else
  warn "No signature verifier found. Install one and re-run:"
  warn "  Debian/Ubuntu:  sudo apt install minisign"
  warn "  macOS:          brew install minisign"
  warn "  Fedora:         sudo dnf install minisign"
  warn "  Any platform:   cargo install rsign2"
  die "Cannot verify the release signature — refusing to install. Set AGENTOS_SKIP_SIG_VERIFY=1 to install on checksum alone (not recommended)."
fi

# --- install ------------------------------------------------------------------
mkdir -p "$INSTALL_DIR"
install -m 0755 "$tmp/agentos" "$INSTALL_DIR/agentos"
info "Installed to $INSTALL_DIR/agentos"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) warn "Add $INSTALL_DIR to your PATH:  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

info "Verifying install"
"$INSTALL_DIR/agentos" --version || true
"$INSTALL_DIR/agentos" doctor    || warn "Doctor reported issues — see output above."

cat <<EOF

AgentOS installed. Next steps:
  agentos onboard          # interactive setup (no API keys written to disk)
  agentos start            # boot the kernel (REST API on :8080 when [api] enabled = true)

On Linux, install bubblewrap for the shell-exec sandbox:  sudo apt install bubblewrap
Docs: https://ajasmohammed.github.io/Agos
EOF
