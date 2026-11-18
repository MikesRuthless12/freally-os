# SPDX-License-Identifier: Apache-2.0
<#
.SYNOPSIS
  Installs a Freally OS release build where it's used (closeout step 6): the
  FREALLYBOOT test USB stick, and the VM folder %USERPROFILE%\Freally OS VMs\.

.DESCRIPTION
  Never touches a drive that isn't the test stick. A drive is the stick only
  when its volume is labelled FREALLYBOOT *and* it holds the marker file
  \FREALLY\.freally-boot-stick. The script prints the drive, label and marker
  it found, and refuses otherwise. It removes only Freally OS's own files from
  the stick, copies the release, and verifies every file's SHA-256 against the
  release's SHA256SUMS.txt.

  -Format prepares a new stick once (FAT32, label FREALLYBOOT, the marker). It
  asks for administrator rights (UAC), accepts only a removable USB disk that
  isn't the system or boot disk, and asks you to type the drive letter again.

.EXAMPLE
  scripts\install-release.ps1 -Version 0.1.0
  scripts\install-release.ps1 -Version 0.1.0 -DryRun
  scripts\install-release.ps1 -Format -Drive E
#>
param(
    [string]$Version = "",
    [string]$ReleaseDir = "",
    [string]$Drive = $env:FREALLY_USB,
    [switch]$Format,
    [switch]$DryRun,
    [switch]$SkipStick
)

$ErrorActionPreference = "Stop"
$Label = "FREALLYBOOT"
$Marker = "FREALLY\.freally-boot-stick"
$Repo = Split-Path -Parent $PSScriptRoot

function Fail([string]$Why) {
    Write-Host "install-release: refused: $Why" -ForegroundColor Red
    exit 1
}

function Find-Stick {
    $vols = @(Get-Volume | Where-Object { $_.FileSystemLabel -eq $Label -and $_.DriveLetter })
    if ($Drive) { $vols = @($vols | Where-Object { $_.DriveLetter -eq $Drive.TrimEnd(':') }) }
    if ($vols.Count -eq 0) { return $null }
    if ($vols.Count -gt 1) { Fail "more than one volume is labelled $Label; plug in only the test stick" }
    return $vols[0]
}

if ($Format) {
    if (-not $Drive) { Fail "-Format needs -Drive <letter>" }
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Host "install-release: formatting needs administrator rights; asking (UAC)."
        Start-Process powershell -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -Format -Drive $Drive" -Wait
        exit $LASTEXITCODE
    }
    $letter = $Drive.TrimEnd(':')
    $part = Get-Partition -DriveLetter $letter -ErrorAction SilentlyContinue
    if (-not $part) { Fail "there's no drive $letter`:" }
    $disk = Get-Disk -Number $part.DiskNumber
    if ($disk.BusType -ne "USB") { Fail "drive $letter`: is on a $($disk.BusType) disk, not USB" }
    if ($disk.IsSystem -or $disk.IsBoot) { Fail "drive $letter`: is on the system or boot disk" }
    Write-Host "install-release: drive $letter`: is '$($disk.FriendlyName)', $([math]::Round($disk.Size/1GB,1)) GB, USB."
    Write-Host "Everything on it will be erased. Type the drive letter again to format it:"
    $again = Read-Host
    if ($again.TrimEnd(':') -ne $letter) { Fail "the letters didn't match; nothing was formatted" }
    Format-Volume -DriveLetter $letter -FileSystem FAT32 -NewFileSystemLabel $Label -Confirm:$false | Out-Null
    New-Item -ItemType Directory -Force -Path "$letter`:\FREALLY" | Out-Null
    Set-Content -Path "$letter`:\$Marker" -Value "This drive is Freally OS's test stick. install-release.ps1 writes only here." -Encoding utf8
    Write-Host "install-release: $letter`: is now $Label, with its marker."
    exit 0
}

if (-not $Version) {
    $toml = Get-Content (Join-Path $Repo "Cargo.toml") -Raw
    if ($toml -match 'version = "([0-9.]+[^"]*)"') { $Version = $Matches[1] } else { Fail "no -Version and none in Cargo.toml" }
}
if (-not $ReleaseDir) { $ReleaseDir = Join-Path $Repo "target\release\Freally-OS-v$Version" }
if (-not (Test-Path (Join-Path $ReleaseDir "SHA256SUMS.txt"))) { Fail "no release at $ReleaseDir (run cargo xtask release-local)" }

# The release's own checksums first: a damaged release is never installed.
$sums = @{}
foreach ($line in Get-Content (Join-Path $ReleaseDir "SHA256SUMS.txt")) {
    if ($line -match '^([0-9a-f]{64})  (.+)$') { $sums[$Matches[2]] = $Matches[1] }
}
foreach ($name in $sums.Keys) {
    $h = (Get-FileHash -Algorithm SHA256 (Join-Path $ReleaseDir $name)).Hash.ToLower()
    if ($h -ne $sums[$name]) { Fail "$name in the release doesn't match SHA256SUMS.txt" }
}
Write-Host "install-release: release $ReleaseDir verified ($($sums.Count) files)."

$Places = @{ "BOOTX64.EFI" = "EFI\BOOT\BOOTX64.EFI"; "SYSTEM_A.FSI" = "FREALLY\SYSTEM_A.FSI" }

function Install-To([string]$Root, [string]$What) {
    foreach ($name in $sums.Keys) {
        $dest = Join-Path $Root $Places[$name]
        if ($DryRun) { Write-Host "  would copy $name -> $dest"; continue }
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
        Copy-Item -Force (Join-Path $ReleaseDir $name) $dest
        $h = (Get-FileHash -Algorithm SHA256 $dest).Hash.ToLower()
        if ($h -ne $sums[$name]) { Fail "$dest doesn't match after copying" }
    }
    Write-Host "install-release: $What $(if ($DryRun) { 'checked (dry run)' } else { 'installed and verified' })."
}

# The VM folder (always).
$vmDir = Join-Path $env:USERPROFILE "Freally OS VMs\Freally-OS-v$Version"
Install-To $vmDir "the VM folder $vmDir"

# The stick (only the marked FREALLYBOOT volume).
if ($SkipStick) { Write-Host "install-release: the stick skipped (-SkipStick)."; exit 0 }
$vol = Find-Stick
if (-not $vol) { Fail "no volume labelled $Label is plugged in (a new stick: -Format -Drive <letter>)" }
$root = "$($vol.DriveLetter):\"
$hasMarker = Test-Path (Join-Path $root $Marker)
Write-Host "install-release: found $root label '$($vol.FileSystemLabel)', marker $(if ($hasMarker) { 'present' } else { 'MISSING' })."
if (-not $hasMarker) { Fail "$root is labelled $Label but has no $Marker; it isn't the test stick" }
if (-not $DryRun) {
    foreach ($old in @("EFI\BOOT\BOOTX64.EFI", "FREALLY\SYSTEM_A.FSI", "FREALLY\SYSTEM_B.FSI")) {
        $p = Join-Path $root $old
        if (Test-Path $p) { Remove-Item -Force $p }
    }
}
Install-To $root "the stick $root"
