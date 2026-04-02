#!/bin/sh
set -e

REPO="agentos/agentos"
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  ARCH="amd64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *) echo "Unsupported architecture: $ARCH" && exit 1 ;;
esac

ARTIFACT="agentos-${OS}-${ARCH}"
LATEST=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)

if [ -z "$LATEST" ]; then
    echo "Failed to determine latest release" && exit 1
fi

URL="https://github.com/${REPO}/releases/download/${LATEST}/${ARTIFACT}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

echo "Installing AgentOS ${LATEST} for ${OS}/${ARCH}..."
curl -fsSL "$URL" -o /tmp/agentos
chmod +x /tmp/agentos

if [ -w "$INSTALL_DIR" ]; then
    mv /tmp/agentos "${INSTALL_DIR}/agentos"
else
    sudo mv /tmp/agentos "${INSTALL_DIR}/agentos"
fi

echo "Installed agentos to ${INSTALL_DIR}/agentos"
agentos --version
