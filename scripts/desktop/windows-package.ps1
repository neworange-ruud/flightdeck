# Build an MSI of flightdeck-desktop with cargo-wix (WiX v3). Windows only.
# NOT run in the authoring environment (no Windows machine); exercised by the
# `package` job in .github/workflows/desktop.yml. See desktop/PACKAGING.md.
#
# Needs: MSVC + Windows SDK (the GUI build compiles HLSL with fxc.exe), WiX v3 toolset
# (candle/light on PATH or WIX env var), and `cargo install cargo-wix`.
#
# Signing is opt-in and skipped when unset:
#   WINDOWS_SIGN_PFX_PATH   path to a code-signing .pfx
#   WINDOWS_SIGN_PFX_PASSWORD  its password (pass from a CI secret; never commit)
#   WINDOWS_SIGN_TIMESTAMP_URL default http://timestamp.digicert.com
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root

if (-not $env:SKIP_BUILD) {
    cargo build -p flightdeck-desktop --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
}

New-Item -ItemType Directory -Force target\desktop-dist | Out-Null
cargo wix -p flightdeck-desktop --nocapture --no-build --output target\desktop-dist\FlightDeck-windows-x64.msi
if ($LASTEXITCODE -ne 0) { throw 'cargo wix failed' }
$msi = 'target\desktop-dist\FlightDeck-windows-x64.msi'

if ($env:WINDOWS_SIGN_PFX_PATH) {
    $ts = if ($env:WINDOWS_SIGN_TIMESTAMP_URL) { $env:WINDOWS_SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    signtool sign /fd SHA256 /f $env:WINDOWS_SIGN_PFX_PATH /p $env:WINDOWS_SIGN_PFX_PASSWORD /tr $ts /td SHA256 $msi
    if ($LASTEXITCODE -ne 0) { throw 'signtool failed' }
} else {
    Write-Host 'signing: skipped (WINDOWS_SIGN_PFX_PATH unset)'
}
Write-Host "msi: $msi"

# --- portable zip (what the self-updater installs from) ---------------------
# Contract with desktop/src/selfupdate/release.rs:
#   FlightDeck-<version>-windows-<arch>-portable.zip, flightdeck-desktop.exe at the zip root,
#   plus <asset>.sha256 in `sha256sum` format ("<64 hex>  <name>").
$version = (Select-String -Path desktop\Cargo.toml -Pattern '^version\s*=\s*"(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$arch = switch ($osArch) {
    'X64'   { 'x86_64' }
    'Arm64' { 'aarch64' }
    default { throw "unsupported architecture: $osArch" }
}
$zipName = "FlightDeck-$version-windows-$arch-portable.zip"
$zip = Join-Path target\desktop-dist $zipName
$stage = Join-Path target\desktop-dist portable
Remove-Item -Recurse -Force $stage, $zip -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\flightdeck-desktop.exe $stage
if ($env:WINDOWS_SIGN_PFX_PATH) {
    signtool sign /fd SHA256 /f $env:WINDOWS_SIGN_PFX_PATH /p $env:WINDOWS_SIGN_PFX_PASSWORD /tr $ts /td SHA256 (Join-Path $stage flightdeck-desktop.exe)
    if ($LASTEXITCODE -ne 0) { throw 'signtool failed' }
}
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
# sha256sum format, LF line ending, no BOM.
[System.IO.File]::WriteAllText("$((Resolve-Path target\desktop-dist).Path)\$zipName.sha256", "$hash  $zipName`n")
Write-Host "portable zip: $zip"
