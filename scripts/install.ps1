# install.ps1 — AgentOS Windows installer notice.
#
#   iwr -useb https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.ps1 | iex
#
# Native Windows binaries are NOT published for v1.0.0. Seccomp sandboxing and
# most HAL hardware drivers are Linux-only, so the SUPPORTED path on Windows is
# WSL2 + scripts/install.sh. This script does not download anything; it just
# points you at the supported installer and exits.
$ErrorActionPreference = "Stop"

Write-Host ""
Write-Host "AgentOS does not ship a native Windows binary yet." -ForegroundColor Yellow
Write-Host "The supported path on Windows is WSL2 (Windows Subsystem for Linux)." -ForegroundColor Yellow
Write-Host ""
Write-Host "To install:" -ForegroundColor Cyan
Write-Host "  1. Install WSL2:  wsl --install"
Write-Host "  2. Open your WSL2 (e.g. Ubuntu) shell."
Write-Host "  3. Run the Linux installer:"
Write-Host "       curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | bash"
Write-Host ""
Write-Host "Docs: https://ajasmohammed.github.io/Agos"
Write-Host ""

exit 0
