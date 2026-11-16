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

  Before running, it checks that the DLLs the spike imports are on this
  machine (VCRUNTIME140*.dll come with the Visual C++ Redistributable:
  https://aka.ms/vs/17/release/vc_redist.x64.exe).

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

# The DLL names a PE file imports, read from its import and delay-import
# tables (no dumpbin needed).
function Get-PeImports([string] $path) {
    $b = [System.IO.File]::ReadAllBytes($path)
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    if ([BitConverter]::ToUInt32($b, $pe) -ne 0x4550) { throw "$path is not a PE file" }
    $coff = $pe + 4
    $sections = [BitConverter]::ToUInt16($b, $coff + 2)
    $optional = $coff + 20
    $optionalSize = [BitConverter]::ToUInt16($b, $coff + 16)
    $directories = if ([BitConverter]::ToUInt16($b, $optional) -eq 0x20B) { $optional + 112 } else { $optional + 96 }
    $table = $optional + $optionalSize
    $toOffset = {
        param([uint32] $rva)
        for ($i = 0; $i -lt $sections; $i++) {
            $s = $table + 40 * $i
            $va = [BitConverter]::ToUInt32($b, $s + 12)
            $size = [Math]::Max([BitConverter]::ToUInt32($b, $s + 8), [BitConverter]::ToUInt32($b, $s + 16))
            if ($rva -ge $va -and $rva -lt $va + $size) {
                return [int]($rva - $va + [BitConverter]::ToUInt32($b, $s + 20))
            }
        }
        return -1
    }
    $names = New-Object System.Collections.Generic.List[string]
    # (data directory, descriptor size, offset of the DLL name in it):
    # imports, then delay-load imports.
    foreach ($kind in @(@(1, 20, 12), @(13, 32, 4))) {
        $rva = [BitConverter]::ToUInt32($b, $directories + 8 * $kind[0])
        if ($rva -eq 0) { continue }
        $at = & $toOffset $rva
        while ($at -ge 0) {
            $nameRva = [BitConverter]::ToUInt32($b, $at + $kind[2])
            if ($nameRva -eq 0) { break }
            $start = & $toOffset $nameRva
            $end = $start
            while ($b[$end] -ne 0) { $end++ }
            $names.Add([System.Text.Encoding]::ASCII.GetString($b, $start, $end - $start))
            $at += $kind[1]
        }
    }
    return , $names
}

# Load each DLL `$path` imports, as the loader would; for one that fails,
# say why, and follow its own imports (a DLL that is there can still miss
# one of its dependencies). Windows only.
function Find-MissingDlls([string] $path, [int] $depth = 0, $seen = $null) {
    if ($null -eq $seen) { $seen = New-Object System.Collections.Generic.HashSet[string] }
    if (-not ('FlightDeck.Native' -as [type])) {
        Add-Type -Namespace FlightDeck -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr LoadLibraryExW(string name, IntPtr file, uint flags);
[DllImport("kernel32.dll")]
public static extern bool FreeLibrary(IntPtr module);
'@
    }
    $found = New-Object System.Collections.Generic.List[string]
    $indent = '  ' * ($depth + 1)
    $dir = Split-Path -Parent $path
    foreach ($name in (Get-PeImports $path)) {
        if (-not $seen.Add($name.ToLowerInvariant())) { continue }
        $local = Join-Path $dir $name
        $target = if (Test-Path $local) { $local } else { $name }
        $module = [FlightDeck.Native]::LoadLibraryExW($target, [IntPtr]::Zero, 0)
        if ($module -ne [IntPtr]::Zero) {
            [void] [FlightDeck.Native]::FreeLibrary($module)
            continue
        }
        $code = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
        $file = @($local, (Join-Path $env:windir "System32\$name")) | Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($file) {
            $found.Add("$indent$name cannot load (error $code), though it is at $file; its imports:")
            if ($depth -lt 3) {
                foreach ($line in (Find-MissingDlls $file ($depth + 1) $seen)) { $found.Add($line) }
            }
        } else {
            $found.Add("$indent$name NOT FOUND (error $code)")
        }
    }
    return , $found
}

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

$os = Get-CimInstance Win32_OperatingSystem
$header = @(
    "FlightDeck session browser - Windows spike (remote-control-cefv.8)",
    "date:     $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz')",
    "commit:   $(git rev-parse --short HEAD 2>$null)",
    "windows:  $($os.Caption) $($os.Version) (build $($os.BuildNumber)) $($env:PROCESSOR_ARCHITECTURE)",
    "webview2: $(Get-WebView2Version)",
    "rustc:    $(rustc --version 2>$null) ($((rustc -vV 2>$null | Select-String '^host:').ToString().Substring(6)))",
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

# With the GNU toolchain (x86_64-pc-windows-gnu), webview2-com-sys links
# WebView2's loader as WebView2Loader.dll instead of statically, and the .exe
# needs it beside it; the crate's build only copies it into its own output.
if ((Get-PeImports $exe) -contains 'WebView2Loader.dll') {
    $beside = Join-Path (Split-Path -Parent $exe) 'WebView2Loader.dll'
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $loader = Get-ChildItem -Path (Join-Path $repo 'target\debug\build') -Directory -Filter 'webview2-com-sys-*' -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName "out\$arch\WebView2Loader.dll" } |
        Where-Object { Test-Path $_ } |
        Sort-Object { (Get-Item $_).LastWriteTime } -Descending |
        Select-Object -First 1
    if ($loader) {
        Copy-Item -Force $loader $beside
        $header += "loader:   WebView2Loader.dll copied beside the spike (GNU toolchain)"
        Write-Host "Copied WebView2Loader.dll beside the spike (GNU toolchain links it as a DLL)."
    } else {
        Write-Host 'The spike needs WebView2Loader.dll and no build of webview2-com-sys has one.' -ForegroundColor Yellow
    }
}

# The spike's imports that are not part of every Windows (API sets such as
# api-ms-win-* are, and are left out). Read from the cross-built .exe.
$imports = @(
    'VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'd3d11.dll', 'd3dcompiler_47.dll',
    'dcomp.dll', 'dwrite.dll', 'dxgi.dll', 'icuuc.dll', 'uiautomationcore.dll',
    'comctl32.dll', 'dwmapi.dll', 'iphlpapi.dll', 'bcryptprimitives.dll'
)
$system32 = Join-Path $env:windir 'System32'
$missing = @($imports | Where-Object { -not (Test-Path (Join-Path $system32 $_)) })
if ($missing.Count -gt 0) {
    $header += "missing:  $($missing -join ', ') (not in $system32)"
    Write-Host "Missing DLLs: $($missing -join ', ')" -ForegroundColor Yellow
    if ($missing | Where-Object { $_ -like 'VCRUNTIME*' }) {
        Write-Host 'Install the Visual C++ Redistributable: https://aka.ms/vs/17/release/vc_redist.x64.exe' -ForegroundColor Yellow
    }
} else {
    $header += 'missing:  none of the DLLs the spike imports'
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

    $diagnosis = New-Object System.Collections.Generic.List[string]
    if ($code -eq -1073741515) {
        # 0xC0000135: a DLL it needs could not be found. Which one?
        $diagnosis.Add('DLLs the spike could not load (0xC0000135):')
        try {
            $missingDlls = Find-MissingDlls $exe
            if ($missingDlls.Count -eq 0) { $diagnosis.Add('  (every import loads from here; see the stderr)') }
            foreach ($line in $missingDlls) { $diagnosis.Add($line) }
        } catch {
            $diagnosis.Add("  could not check: $_")
        }
    }

    $results.Add("===== mode: $mode (exit $shown) =====")
    foreach ($line in $diagnosis) { $results.Add($line) }
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
    $diagnosis | Write-Host
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
