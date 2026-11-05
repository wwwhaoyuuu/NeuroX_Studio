[CmdletBinding()]
param(
    [string]$AppDirectory = $PSScriptRoot,
    [switch]$SkipFirmware
)

$ErrorActionPreference = "Stop"
$script:failures = 0
function Report([bool]$Okay, [string]$Name, [string]$Detail) {
    if ($Okay) { Write-Output "[OK] $Name : $Detail" }
    else { Write-Output "[MISSING] $Name : $Detail"; $script:failures++ }
}
function Is-X64Binary([string]$File) {
    if (-not (Test-Path -LiteralPath $File -PathType Leaf)) { return $false }
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($File))
    try {
        if ($reader.ReadUInt16() -ne 0x5A4D) { return $false }
        $reader.BaseStream.Position = 0x3C
        $offset = $reader.ReadUInt32()
        if ($offset -gt $reader.BaseStream.Length - 6) { return $false }
        $reader.BaseStream.Position = $offset
        return ($reader.ReadUInt32() -eq 0x4550 -and $reader.ReadUInt16() -eq 0x8664)
    } catch { return $false } finally { $reader.Dispose() }
}

$directory = (Resolve-Path -LiteralPath $AppDirectory).Path
Report ([Environment]::Is64BitOperatingSystem -and [Environment]::Is64BitProcess) "Windows x64 / PowerShell x64" "Use the 64-bit Windows package and 64-bit PowerShell"
Report (Is-X64Binary (Join-Path $directory "NeuroX Studio.exe")) "NeuroX Studio.exe" "A complete x64 executable must be beside this checker"
Report (Is-X64Binary (Join-Path $directory "okFrontPanel.dll")) "FrontPanel API x64" "Keep the bundled 5.2.10 okFrontPanel.dll beside the EXE"

$webview = @()
foreach ($root in @("HKCU:\Software\Microsoft\EdgeUpdate\Clients", "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients", "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients")) {
    if (Test-Path -LiteralPath $root) {
        $webview += @(Get-ChildItem -LiteralPath $root -ErrorAction SilentlyContinue | ForEach-Object {
            Get-ItemProperty -LiteralPath $_.PSPath -ErrorAction SilentlyContinue
        } | Where-Object { $_.name -like "*WebView2*" -and $_.pv -and $_.pv -ne "0.0.0.0" })
    }
}
Report ($webview.Count -gt 0) "WebView2 Evergreen Runtime" "Install from https://developer.microsoft.com/microsoft-edge/webview2/ if not registered"

$vc = Get-ItemProperty -LiteralPath "HKLM:\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" -ErrorAction SilentlyContinue
$version = if ($vc) { $vc.Version } else { "not registered" }
Report ($null -ne $vc -and $vc.Installed -eq 1) "Visual C++ v14 x64 Runtime" "Detected: $version . Install the latest x64 package: https://aka.ms/vc14/vc_redist.x64.exe"
if (-not $SkipFirmware) {
    Report (Test-Path -LiteralPath (Join-Path $directory "ConfigRHSController.bit") -PathType Leaf) "FPGA bitfile for hardware" "Obtain a matching bitfile from the board provider, or choose its path in FPGA settings"
}
Write-Output ""
Write-Output "USB driver/device availability and firmware compatibility are NOT verified by this read-only check."
Write-Output "Install the official FrontPanel USB driver before connecting hardware; close other apps using the board."
Write-Output "No board was opened, no firmware was uploaded, and no stimulation was sent."
if ($script:failures -gt 0) { exit 1 }
exit 0
