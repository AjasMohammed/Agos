#!/usr/bin/env bash
# install.sh — one-line AgentOS installer.
#
#   curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | bash
#
# Env overrides:
#   AGENTOS_VERSION       release tag (default: latest), e.g. v1.0.0
#   AGENTOS_INSTALL_DIR   install dir   (default: ~/.local/bin)
#
# Always verifies the SHA-256 checksum, and verifies the minisign signature when
# both the signature asset and the repo public key are available (enforced once
# Phase 08 signing is live; gracefully reported as skipped before then).
set -euo pipefail

REPO="AjasMohammed/Agos"
VERSION="${AGENTOS_VERSION:-latest}"
INSTALL_DIR="${AGENTOS_INSTALL_DIR:-$HOME/.local/bin}"
PUBKEY_URL="https://raw.githubusercontent.com/${REPO}/main/packaging/signing/agentos-release.pub"

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

if [ "$os" != "linux" ]; then
  warn "Linux is the primary target. On macOS, seccomp sandboxing and most HAL"
  warn "drivers are unavailable; shell-exec and hardware tools degrade gracefully."
fi

# --- resolve release base url -------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  BASE="https://github.com/${REPO}/releases/latest/download"
else
  BASE="https://github.com/${REPO}/releases/download/${VERSION}"
fi

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
info "Downloading $ASSET ($VERSION)"
curl -fsSL "$BASE/$ASSET"        -o "$tmp/agentos"        || die "Download failed for $ASSET."
curl -fsSL "$BASE/$ASSET.sha256" -o "$tmp/agentos.sha256" || die "Checksum file missing for $ASSET."
# Signature + pubkey are best-effort until Phase 08 signing is published.
sig_ok=0
curl -fsSL "$BASE/$ASSET.sig" -o "$tmp/agentos.sig" 2>/dev/null && sig_ok=1 || true
curl -fsSL "$PUBKEY_URL"      -o "$tmp/agentos.pub" 2>/dev/null || true

# --- verify checksum (mandatory) ----------------------------------------------
info "Verifying checksum"
( cd "$tmp" && sed "s|$ASSET|agentos|" agentos.sha256 | sha256 -c - ) \
  || die "Checksum verification failed — refusing to install."

# --- verify signature (enforced when available) -------------------------------
if [ "$sig_ok" = 1 ] && [ -s "$tmp/agentos.pub" ]; then
  if command -v minisign >/dev/null 2>&1; then
    info "Verifying signature"
    minisign -V -p "$tmp/agentos.pub" -x "$tmp/agentos.sig" -m "$tmp/agentos" \
      || die "Signature verification failed — refusing to install."
  else
    warn "Signature present but 'minisign' is not installed; checksum verified, signature NOT."
    warn "Install minisign and re-run for full supply-chain verification."
  fi
else
  warn "No published signature yet for this release; checksum verified only."
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
  agentos web serve        # start the web UI on http://127.0.0.1:8080

On Linux, install bubblewrap for the shell-exec sandbox:  sudo apt install bubblewrap
Docs: https://ajasmohammed.github.io/Agos
EOF
