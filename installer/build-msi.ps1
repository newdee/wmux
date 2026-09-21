<#
.SYNOPSIS
  Build the wmux MSI from an already built release binary.

.DESCRIPTION
  Stages the files the installer ships (wmux.exe, README.md, LICENSE,
  wmux.conf.example) into a temporary directory and runs WiX over
  installer/wmux.wxs. WiX 3.14 is used from -WixBin, from PATH, from the usual
  "WiX Toolset v3.x" install, or downloaded (no install) when none is found.

.EXAMPLE
  cargo build --release
  pwsh -File installer/build-msi.ps1 -Version 0.4.0
#>
[CmdletBinding()]
param(
    # Product version; must be x.y.z (MSI does not take anything else).
    [string]$Version,
    # Directory holding the built wmux.exe.
    [string]$ExeDir = "target/release",
    # Where the .msi lands.
    [string]$OutDir = "target",
    # Directory holding candle.exe / light.exe.
    [string]$WixBin
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if (-not $Version) {
    $Version = (Select-String -Path "$root/Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "version must be x.y.z, got '$Version'"
}

function Resolve-Wix {
    if ($WixBin) { return $WixBin }
    $candle = Get-Command candle.exe -ErrorAction SilentlyContinue
    if ($candle) { return (Split-Path -Parent $candle.Source) }
    $installed = Get-ChildItem "${env:ProgramFiles(x86)}" -Filter "WiX Toolset v3.*" -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($installed -and (Test-Path "$($installed.FullName)/bin/candle.exe")) { return "$($installed.FullName)/bin" }
    # No WiX anywhere: use the official binaries zip, unpacked next to the build.
    $dir = Join-Path ([System.IO.Path]::GetTempPath()) "wix314-binaries"
    if (-not (Test-Path "$dir/candle.exe")) {
        $zip = "$dir.zip"
        Write-Host "downloading WiX 3.14 binaries..."
        Invoke-WebRequest -Uri "https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip" -OutFile $zip
        Expand-Archive -Path $zip -DestinationPath $dir -Force
    }
    return $dir
}

$wix = Resolve-Wix
$exe = Join-Path $root $ExeDir "wmux.exe"
if (-not (Test-Path $exe)) { throw "no wmux.exe at $exe (cargo build --release first)" }

# Everything the MSI ships, in one directory, so the .wxs has a single root.
$stage = Join-Path ([System.IO.Path]::GetTempPath()) "wmux-msi-stage-$PID"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $exe $stage
foreach ($f in @("README.md", "LICENSE", "wmux.conf.example")) { Copy-Item (Join-Path $root $f) $stage }

$obj = Join-Path $stage "wmux.wixobj"
$outDirFull = Join-Path $root $OutDir
New-Item -ItemType Directory -Force $outDirFull | Out-Null
$msi = Join-Path $outDirFull "wmux-$Version-windows-x86_64.msi"

try {
    & "$wix/candle.exe" -nologo -arch x64 "-dVersion=$Version" "-dSourceDir=$stage" `
        (Join-Path $root "installer/wmux.wxs") -o $obj
    if ($LASTEXITCODE -ne 0) { throw "candle failed" }
    # ICE61 fires on same-version upgrades, which MajorUpgrade allows on purpose.
    & "$wix/light.exe" -nologo -ext WixUIExtension -sice:ICE61 -spdb -b $root $obj -o $msi
    if ($LASTEXITCODE -ne 0) { throw "light failed" }
}
finally {
    # A failed build must not leave a copy of the staged files behind.
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}

Write-Host "built $msi"
