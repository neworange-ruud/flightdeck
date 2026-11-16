<#
.SYNOPSIS
  Run the session browser's Windows spike (remote-control-cefv.8) and collect
  every result in one file.

.DESCRIPTION
  Builds desktop/examples/browser_spike.rs and runs it once per mode. Each run
  opens a small window for 15-40 seconds, loads a few pages, prints what each
  stand-in server saw and closes by itself. Don't click in the windows.

    wry        wry's own SOCKS5 proxy setting: does loopback go direct?
    loopback   --proxy-bypass-list=<-loopback>: does loopback go through the proxy?
    http-auth  an HTTP proxy that wants a password: does WebView2 answer it?
    pid        which process opens the relay's connections; then a curl.exe
               connection from outside, which must come out FOREIGN
    profiles   two profiles behind one proxy (cookies kept apart?), and a
               second proxy port in the same data folder vs. its own folder

  Needs what any FlightDeck desktop build on Windows needs: Rust (MSVC
  toolchain), Visual Studio Build Tools with "Desktop development with C++"
  and a Windows SDK, and the WebView2 runtime (part of Windows 10/11).
  curl.exe ships with Windows 10 1803 and later.

  Results: target\browser-spike\results.txt (also one file per mode). Send
  results.txt back; it goes into specs/SESSION_BROWSER_PRD.md section 4.1.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\desktop\windows-browser-spike.ps1
.EXAMPLE
  scripts\desktop\windows-browser-spike.ps1 -Modes pid,profiles -SkipBuild
#>
param(
    [string[]] $Modes = @('wry', 'loopback', 'http-auth', 'pid', 'profiles'),
    [switch] $SkipBuild,
    [int] $TimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
Set-Location $repo
$out = Join-Path $repo 'target\browser-spike'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$exe = Join-Path $repo 'target\debug\examples\browser_spike.exe'
$portFile = Join-Path $env:TEMP 'flightdeck-browser-spike-socks-port'

function Get-WebView2Version {
    $guid = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $keys = @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$guid",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$guid",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$guid"
    )
    foreach ($key in $keys) {
        $item = Get-ItemProperty -Path $key -Name pv -ErrorAction SilentlyContinue
        if ($item -and $item.pv -and $item.pv -ne '0.0.0.0') { return $item.pv }
    }
    return 'not found'
}

$os = Get-CimInstance Win32_OperatingSystem
$header = @(
    "FlightDeck session browser - Windows spike (remote-control-cefv.8)",
    "date:     $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz')",
    "commit:   $(git rev-parse --short HEAD 2>$null)",
    "windows:  $($os.Caption) $($os.Version) (build $($os.BuildNumber)) $($env:PROCESSOR_ARCHITECTURE)",
    "webview2: $(Get-WebView2Version)",
    "rustc:    $(rustc --version 2>$null)",
    "modes:    $($Modes -join ', ')",
    ''
)
$header | ForEach-Object { Write-Host $_ }

if (-not $SkipBuild) {
    Write-Host 'Building the spike (the first build takes a while)...'
    cargo build -p flightdeck-desktop --example browser_spike --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
}
if (-not (Test-Path $exe)) { throw "No spike at $exe; build it first (drop -SkipBuild)." }

# A file's lines, as a list (empty for a missing or empty file). A list,
# because PowerShell turns an empty array returned from a function into $null.
function Read-Lines([string] $path, [int] $last = 0) {
    $list = New-Object System.Collections.Generic.List[string]
    if (Test-Path $path) {
        $content = @(Get-Content $path)
        if ($last -gt 0) { $content = @($content | Select-Object -Last $last) }
        foreach ($line in $content) { $list.Add([string] $line) }
    }
    return , $list
}

$results = New-Object System.Collections.Generic.List[string]
foreach ($line in $header) { $results.Add([string] $line) }

foreach ($mode in $Modes) {
    Write-Host ''
    Write-Host "=== $mode ==="
    $stdout = Join-Path $out "$mode.txt"
    $stderr = Join-Path $out "$mode.stderr.txt"
    Remove-Item -Force -ErrorAction SilentlyContinue $portFile
    $proc = Start-Process -FilePath $exe -ArgumentList $mode -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $null = $proc.Handle  # without this, ExitCode stays empty in Windows PowerShell
    $curl = $null

    if ($mode -eq 'pid') {
        # The spike writes its relay's port once the pages are done, then
        # waits 20 seconds for this connection from another program.
        $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
        while (-not (Test-Path $portFile) -and -not $proc.HasExited -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 500
        }
        if (Test-Path $portFile) {
            $port = (Get-Content $portFile -Raw).Trim()
            Write-Host "connecting from curl.exe through 127.0.0.1:$port"
            $curl = (& curl.exe -s -m 5 --socks5-hostname "127.0.0.1:$port" 'http://foreign-program.invalid/' 2>&1 | Out-String).Trim()
            Write-Host "curl.exe got: $curl"
        } else {
            $curl = 'the spike never wrote its port file'
        }
    }

    if (-not $proc.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Add-Content -Path $stdout -Value "(killed after $TimeoutSeconds s)"
    }

    # Hex too: a crash before main is an NTSTATUS such as 0xC0000135
    # (a DLL not found).
    $code = $proc.ExitCode
    $shown = if ($null -eq $code) { 'unknown' } else { '{0} (0x{1:X8})' -f $code, $code }
    $lines = Read-Lines $stdout
    $errors = Read-Lines $stderr 40

    $results.Add("===== mode: $mode (exit $shown) =====")
    foreach ($line in $lines) { $results.Add($line) }
    if ($lines.Count -eq 0) { $results.Add('(the spike printed nothing)') }
    if ($curl) { $results.Add("curl.exe (a program that is not the spike) got: $curl") }
    if ($errors.Count -gt 0) {
        $results.Add('--- stderr ---')
        foreach ($line in $errors) { $results.Add($line) }
    }
    $results.Add('')

    $lines | Write-Host
    Write-Host "exit: $shown"
    if ($lines.Count -eq 0) { Write-Host '(the spike printed nothing)' }
    if ($errors.Count -gt 0) {
        Write-Host '--- stderr ---'
        $errors | Write-Host
    }
}

# The profiles mode's data folders.
Get-ChildItem -Path $env:TEMP -Filter 'flightdeck-browser-spike-*' -Directory -ErrorAction SilentlyContinue |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue

$all = Join-Path $out 'results.txt'
$results | Set-Content -Path $all -Encoding UTF8
Write-Host ''
Write-Host "Done. Send this file back: $all"
