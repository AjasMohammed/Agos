#!/usr/bin/env bash
# setup.sh — One-command Raspberry Pi hardware setup for AgentOS.
#
# Enables I2C, SPI, and serial interfaces; adds the current user to
# the required hardware groups; installs the MCP server Python package.
#
# Usage:
#   sudo ./setup.sh          # Full setup (enables interfaces, installs deps)
#   ./setup.sh --check       # Check current hardware status (no changes)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_ONLY=false

if [[ "${1:-}" == "--check" ]]; then
    CHECK_ONLY=true
fi

# Colors for output.
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

ok()   { echo -e "  ${GREEN}[OK]${NC} $1"; }
warn() { echo -e "  ${YELLOW}[WARN]${NC} $1"; }
fail() { echo -e "  ${RED}[FAIL]${NC} $1"; }

echo "========================================="
echo "  AgentOS Pi Hardware Setup"
echo "========================================="
echo ""

# ---------------------------------------------------------------------------
# 1. Detect platform
# ---------------------------------------------------------------------------
echo "1. Checking platform..."

if [[ -f /proc/device-tree/model ]]; then
    MODEL=$(tr -d '\0' < /proc/device-tree/model)
    ok "Detected: $MODEL"
else
    warn "Not a Raspberry Pi (or /proc/device-tree/model missing)"
    MODEL="unknown"
fi

ARCH=$(uname -m)
ok "Architecture: $ARCH"

# ---------------------------------------------------------------------------
# 2. Check/enable kernel interfaces
# ---------------------------------------------------------------------------
echo ""
echo "2. Checking hardware interfaces..."

check_interface() {
    local name="$1"
    local config_key="$2"
    local device_path="$3"

    if [[ -e "$device_path" ]]; then
        ok "$name is enabled ($device_path exists)"
        return 0
    else
        if $CHECK_ONLY; then
            fail "$name is NOT enabled"
            return 1
        fi

        warn "$name not enabled — enabling via raspi-config..."
        if command -v raspi-config &>/dev/null; then
            raspi-config nonint "$config_key" 0
            ok "$name enabled (reboot required)"
        else
            fail "raspi-config not found — enable $name manually"
            return 1
        fi
    fi
}

NEED_REBOOT=false

check_interface "I2C"    "do_i2c"    "/dev/i2c-1"    || NEED_REBOOT=true
check_interface "SPI"    "do_spi"    "/dev/spidev0.0" || NEED_REBOOT=true
check_interface "Serial" "do_serial_hw" "/dev/ttyAMA0" || NEED_REBOOT=true

# GPIO is always available via /sys/class/gpio or gpiochip.
if [[ -d /sys/class/gpio ]]; then
    ok "GPIO sysfs is available"
else
    warn "GPIO sysfs not found (may use gpiochip instead)"
fi

# ---------------------------------------------------------------------------
# 3. Check/add user to hardware groups
# ---------------------------------------------------------------------------
echo ""
echo "3. Checking user groups..."

USER="${SUDO_USER:-$USER}"
GROUPS_NEEDED=(gpio i2c spi dialout)

for group in "${GROUPS_NEEDED[@]}"; do
    if id -nG "$USER" | grep -qw "$group"; then
        ok "$USER is in group '$group'"
    else
        if $CHECK_ONLY; then
            fail "$USER is NOT in group '$group'"
        else
            if getent group "$group" &>/dev/null; then
                usermod -aG "$group" "$USER"
                ok "Added $USER to group '$group' (re-login required)"
            else
                warn "Group '$group' does not exist on this system"
            fi
        fi
    fi
done

# ---------------------------------------------------------------------------
# 4. Install Python MCP server
# ---------------------------------------------------------------------------
echo ""
echo "4. Installing Pi Hardware MCP server..."

if $CHECK_ONLY; then
    if python3 -c "import pi_hardware_mcp" 2>/dev/null; then
        ok "pi-hardware-mcp is installed"
    else
        fail "pi-hardware-mcp is NOT installed"
    fi
else
    echo "   Installing from $SCRIPT_DIR ..."
    pip3 install -e "$SCRIPT_DIR[pi]" --break-system-packages 2>/dev/null \
        || pip3 install -e "$SCRIPT_DIR[pi]"
    ok "pi-hardware-mcp installed"
fi

# ---------------------------------------------------------------------------
# 5. Verify MCP server starts
# ---------------------------------------------------------------------------
echo ""
echo "5. Testing MCP server..."

INIT_REQ='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
RESPONSE=$(echo "$INIT_REQ" | timeout 5 python3 -m pi_hardware_mcp.server 2>/dev/null || true)

if echo "$RESPONSE" | python3 -c "import sys,json; r=json.load(sys.stdin); assert 'result' in r" 2>/dev/null; then
    ok "MCP server responds to initialize"
    # Count tools.
    LIST_REQ='{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
    TOOLS_RESPONSE=$(echo -e "$INIT_REQ\n$LIST_REQ" | timeout 5 python3 -m pi_hardware_mcp.server 2>/dev/null | tail -1)
    TOOL_COUNT=$(echo "$TOOLS_RESPONSE" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['result']['tools']))" 2>/dev/null || echo "?")
    ok "Registered $TOOL_COUNT tools"
else
    fail "MCP server did not respond correctly"
fi

# ---------------------------------------------------------------------------
# 6. Create AgentOS data directories
# ---------------------------------------------------------------------------
echo ""
echo "6. Creating AgentOS data directories..."

if ! $CHECK_ONLY; then
    DIRS=(
        "/home/$USER/.agentos/data"
        "/home/$USER/.agentos/vault"
        "/home/$USER/.agentos/tools/core"
        "/home/$USER/.agentos/tools/user"
        "/home/$USER/.agentos/logs"
        "/home/$USER/.agentos/models"
    )
    for dir in "${DIRS[@]}"; do
        mkdir -p "$dir"
        chown "$USER:$USER" "$dir"
    done
    ok "Created ~/.agentos/ directories"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "========================================="
echo "  Setup Complete"
echo "========================================="

if $NEED_REBOOT; then
    echo ""
    warn "A REBOOT is required to enable hardware interfaces."
    echo "       Run: sudo reboot"
fi

echo ""
echo "  To start AgentOS on the Pi:"
echo "    AGENTOS_CONFIG=config/pi.toml ./dev.sh"
echo ""
echo "  To check MCP status after boot:"
echo "    agentos mcp status"
echo ""
