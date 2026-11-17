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

# Contract with desktop/src/selfupdate/release.rs (`asset_name`): the updater
# finds its download by name, so both names carry the version and the arch.
$version = (Select-String -Path desktop\Cargo.toml -Pattern '^version\s*=\s*"(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$arch = switch ($osArch) {
    'X64'   { 'x86_64' }
    'Arm64' { 'aarch64' }
    default { throw "unsupported architecture: $osArch" }
}
$dist = (New-Item -ItemType Directory -Force target\desktop-dist).FullName

# sha256sum format ("<64 hex>  <name>"), LF line ending, no BOM.
function Write-Checksum([string] $name) {
    $hash = (Get-FileHash -Algorithm SHA256 (Join-Path $dist $name)).Hash.ToLower()
    [System.IO.File]::WriteAllText((Join-Path $dist "$name.sha256"), "$hash  $name`n")
}

# --- MSI (installs per machine; the updater runs the next one over it) ------
$msiName = "FlightDeck-$version-windows-$arch.msi"
$msi = Join-Path $dist $msiName
cargo wix -p flightdeck-desktop --nocapture --no-build --output $msi
if ($LASTEXITCODE -ne 0) { throw 'cargo wix failed' }

if ($env:WINDOWS_SIGN_PFX_PATH) {
    $ts = if ($env:WINDOWS_SIGN_TIMESTAMP_URL) { $env:WINDOWS_SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    signtool sign /fd SHA256 /f $env:WINDOWS_SIGN_PFX_PATH /p $env:WINDOWS_SIGN_PFX_PASSWORD /tr $ts /td SHA256 $msi
    if ($LASTEXITCODE -ne 0) { throw 'signtool failed' }
} else {
    Write-Host 'signing: skipped (WINDOWS_SIGN_PFX_PATH unset)'
}
# After signing: the checksum is of the file users download.
Write-Checksum $msiName
Write-Host "msi: $msi"

# --- portable zip (what the self-updater installs from) ---------------------
# Contract with desktop/src/selfupdate/release.rs:
#   FlightDeck-<version>-windows-<arch>-portable.zip, flightdeck-desktop.exe at the zip root,
#   plus <asset>.sha256 in `sha256sum` format ("<64 hex>  <name>").
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
Write-Checksum $zipName
Write-Host "portable zip: $zip"
