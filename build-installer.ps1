# Build GlassDock and its installer: installer\Output\GlassDock-Setup-<version>.exe
# Needs Rust (cargo) and Inno Setup 6 (winget install JRSoftware.InnoSetup).
# 'Continue': PowerShell 5.1 treats cargo's progress output on stderr as an error under 'Stop';
# success is checked through the exit codes instead
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot

cargo build --release 2>&1 | ForEach-Object { "$_" }
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$iscc = @(
  "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
  "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
  "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup" }

& $iscc /Qp installer\GlassDock.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Get-ChildItem installer\Output\*.exe | Select-Object Name, @{ n = 'KB'; e = { [math]::Round($_.Length / 1KB) } }
