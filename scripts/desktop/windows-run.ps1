<#
.SYNOPSIS
  Build the FlightDeck desktop app from source on Windows and run it.

.DESCRIPTION
  A debug build by default. It must be built on the machine that runs it:
  GPUI's debug build compiles its shaders when the app starts, from GPUI's
  source folder as the build recorded it. A release build (-Release) has
  them built in, which needs the Windows SDK's shader compiler (fxc.exe).

  With the GNU toolchain (x86_64-pc-windows-gnu) webview2-com-sys links
  WebView2's loader as WebView2Loader.dll, which the app needs beside it;
  this copies it there. MSVC builds have it linked in.

  Any arguments after the switches go to the app.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\desktop\windows-run.ps1 --spike-browser
.EXAMPLE
  scripts\desktop\windows-run.ps1
.EXAMPLE
  scripts\desktop\windows-run.ps1 -Release
#>
param(
    [switch] $Release,
    [switch] $SkipBuild,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $AppArgs = @()
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
Set-Location $repo
$buildProfile = if ($Release) { 'release' } else { 'debug' }
$exe = Join-Path $repo "target\$buildProfile\flightdeck-desktop.exe"

if (-not $SkipBuild) {
    $cargoArgs = @('build', '-p', 'flightdeck-desktop', '--locked')
    if ($Release) { $cargoArgs += '--release' }
    Write-Host "cargo $($cargoArgs -join ' ')"
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
}
if (-not (Test-Path $exe)) { throw "No app at $exe; build it first (drop -SkipBuild)." }

$triple = ((rustc -vV 2>$null | Select-String '^host:').ToString().Substring(5)).Trim()
if ($triple -like '*-gnu') {
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $loader = Get-ChildItem -Path (Join-Path $repo "target\$buildProfile\build") -Directory -Filter 'webview2-com-sys-*' -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName "out\$arch\WebView2Loader.dll" } |
        Where-Object { Test-Path $_ } |
        Sort-Object { (Get-Item $_).LastWriteTime } -Descending |
        Select-Object -First 1
    if ($loader) {
        Copy-Item -Force $loader (Split-Path -Parent $exe)
        Write-Host "Copied WebView2Loader.dll beside the app ($triple links it as a DLL)."
    } else {
        Write-Host "No WebView2Loader.dll found in the build output; the app may not start." -ForegroundColor Yellow
    }
}

Write-Host "Running $exe $($AppArgs -join ' ')"
& $exe @AppArgs
exit $LASTEXITCODE
