# install.ps1 — AgentOS Windows installer (EARLY BETA).
#
#   iwr -useb https://raw.githubusercontent.com/agentos/agentos/main/scripts/install.ps1 | iex
#
# Native Windows is early-beta: seccomp sandboxing and most HAL hardware drivers
# are Linux-only and are unavailable here. The SUPPORTED path is WSL2 +
# scripts/install.sh. This installer is provided for convenience only.
$ErrorActionPreference = "Stop"

Write-Host "AgentOS on native Windows is EARLY-BETA. WSL2 is the supported path." -ForegroundColor Yellow
Write-Host "  (run scripts/install.sh inside WSL2 for the fully-tested experience)" -ForegroundColor Yellow

$repo    = "agentos/agentos"
$version = if ($env:AGENTOS_VERSION) { $env:AGENTOS_VERSION } else { "latest" }
$asset   = "agentos-windows-amd64.exe"
$dir     = "$env:LOCALAPPDATA\AgentOS\bin"
$base    = if ($version -eq "latest") {
    "https://github.com/$repo/releases/latest/download"
} else {
    "https://github.com/$repo/releases/download/$version"
}

New-Item -ItemType Directory -Force -Path $dir | Out-Null
$exe = "$dir\agentos.exe"
$sha = "$env:TEMP\agentos.sha256"

Write-Host "==> Downloading $asset ($version)"
Invoke-WebRequest "$base/$asset"        -OutFile $exe
Invoke-WebRequest "$base/$asset.sha256" -OutFile $sha

# --- verify checksum ----------------------------------------------------------
Write-Host "==> Verifying checksum"
$expected = (Get-Content $sha).Split(" ")[0].Trim().ToLower()
$actual   = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower()
if ($expected -ne $actual) {
    Remove-Item $exe -Force
    throw "Checksum verification failed — refusing to install."
}

# --- add to user PATH ---------------------------------------------------------
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$dir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
    Write-Host "==> Added $dir to your user PATH (restart the shell to pick it up)."
}

Write-Host "==> Installed to $exe"
& $exe --version
Write-Host ""
Write-Host "Next: agentos onboard  then  agentos web serve"
Write-Host "Docs: https://agentos.github.io/agentos"
