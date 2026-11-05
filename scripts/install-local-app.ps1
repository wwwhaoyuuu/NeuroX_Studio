[CmdletBinding()]
param(
    [switch]$NoLaunch
)

$ErrorActionPreference = "Stop"

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path
$localEnvPath = Join-Path $scriptRoot "local-env.bat"

if (-not (Test-Path -LiteralPath $localEnvPath)) {
    throw "Missing scripts\local-env.bat. Create it from local-env.example.bat first."
}

foreach ($line in Get-Content -LiteralPath $localEnvPath) {
    if ($line -match '^set\s+"(?<name>FRONTPANEL_SDK|RHS_BITFILE|RHS_VCVARS64)=(?<value>.*)"\s*$') {
        $value = $Matches.value.Replace("%~dp0", "$scriptRoot\")
        Set-Item -Path "Env:$($Matches.name)" -Value $value
    }
}

if ([string]::IsNullOrWhiteSpace($env:FRONTPANEL_SDK)) {
    throw "FRONTPANEL_SDK is not configured in scripts\local-env.bat."
}
if ([string]::IsNullOrWhiteSpace($env:RHS_BITFILE)) {
    throw "RHS_BITFILE is not configured in scripts\local-env.bat."
}
if (-not (Test-Path -LiteralPath $env:RHS_BITFILE -PathType Leaf)) {
    throw "RHS bitfile was not found: $env:RHS_BITFILE"
}

$frontPanelRuntime = Join-Path $env:FRONTPANEL_SDK "API\lib\x64"
$runtimeDlls = @(Get-ChildItem -LiteralPath $frontPanelRuntime -Filter "*.dll" -File)
if ($runtimeDlls.Count -eq 0) {
    throw "No FrontPanel runtime DLLs were found in: $frontPanelRuntime"
}

$running = @(Get-Process -Name "NeuroX Studio", "RHS Studio", "rhs-desktop" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    throw "NeuroX Studio or its previous version is running. Close it and run the installer again."
}

$frontendRoot = Join-Path $repoRoot "apps\rhs-desktop"
$lucidePath = Join-Path $frontendRoot "node_modules\lucide\dist\umd\lucide.js"
if (-not (Test-Path -LiteralPath $lucidePath -PathType Leaf)) {
    Push-Location $frontendRoot
    try {
        & npm.cmd install
        if ($LASTEXITCODE -ne 0) { throw "npm install failed with exit code $LASTEXITCODE." }
    } finally {
        Pop-Location
    }
}

Push-Location $frontendRoot
try {
    & npm.cmd run build
    if ($LASTEXITCODE -ne 0) { throw "Frontend build failed with exit code $LASTEXITCODE." }
} finally {
    Pop-Location
}

& (Join-Path $scriptRoot "dev-cargo.bat") build --release -p rhs-desktop --features custom-protocol
if ($LASTEXITCODE -ne 0) {
    throw "Rust release build failed with exit code $LASTEXITCODE."
}

$releaseExe = Join-Path $repoRoot "target\release\rhs-desktop.exe"
if (-not (Test-Path -LiteralPath $releaseExe -PathType Leaf)) {
    throw "Release executable was not produced: $releaseExe"
}

$legacyInstallDir = Join-Path $env:LOCALAPPDATA "Programs\RHS Studio"
$installDir = Join-Path $env:LOCALAPPDATA "Programs\NeuroX Studio"
# Preserve the working directory of existing recordings and installations.
if ((Test-Path -LiteralPath (Join-Path $legacyInstallDir "RHS Studio.exe") -PathType Leaf) -or
    (Test-Path -LiteralPath (Join-Path $legacyInstallDir "NeuroX Studio.exe") -PathType Leaf)) {
    $installDir = $legacyInstallDir
}
$installedExe = Join-Path $installDir "NeuroX Studio.exe"
$installedBitfile = Join-Path $installDir "ConfigRHSController.bit"
New-Item -ItemType Directory -Path $installDir -Force | Out-Null

Copy-Item -LiteralPath $releaseExe -Destination $installedExe -Force
Copy-Item -LiteralPath $env:RHS_BITFILE -Destination $installedBitfile -Force
foreach ($dll in $runtimeDlls) {
    Copy-Item -LiteralPath $dll.FullName -Destination (Join-Path $installDir $dll.Name) -Force
}

$registryDesktop = Get-ItemPropertyValue `
    -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders" `
    -Name "Desktop"
$desktopCandidates = @(
    (Join-Path $env:USERPROFILE "Desktop"),
    [Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory),
    [Environment]::ExpandEnvironmentVariables($registryDesktop)
)
if (-not [string]::IsNullOrWhiteSpace($env:OneDrive)) {
    $desktopCandidates += (Join-Path $env:OneDrive "Desktop")
    $desktopCandidates += (Join-Path $env:OneDrive "桌面")
}
$desktopDir = $desktopCandidates |
    Where-Object { -not [string]::IsNullOrWhiteSpace($_) -and (Test-Path -LiteralPath $_ -PathType Container) } |
    Select-Object -First 1
if ([string]::IsNullOrWhiteSpace($desktopDir)) {
    throw "Windows desktop directory was not found."
}

$profileRoot = [IO.Path]::GetFullPath($env:USERPROFILE)
$desktopDir = [IO.Path]::GetFullPath($desktopDir)
if (-not $desktopDir.StartsWith($profileRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Resolved desktop is outside the current user profile: $desktopDir"
}

function New-NeuroXStudioShortcut([string]$shortcutPath) {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $installedExe
    $shortcut.Arguments = ""
    $shortcut.WorkingDirectory = $installDir
    $shortcut.IconLocation = "$installedExe,0"
    $shortcut.Description = "Multichannel acquisition, analysis and stimulation workbench"
    $shortcut.Save()
    $saved = $shell.CreateShortcut($shortcutPath)
    if ($saved.TargetPath -ne $installedExe -or $saved.WorkingDirectory -ne $installDir) {
        throw "Shortcut target verification failed: $shortcutPath"
    }
}

$shortcutPath = Join-Path $desktopDir "NeuroX Studio.lnk"
New-NeuroXStudioShortcut $shortcutPath

$startMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
if (-not (Test-Path -LiteralPath $startMenuDir -PathType Container)) {
    throw "Windows Start menu directory was not found: $startMenuDir"
}
$startMenuShortcutPath = Join-Path $startMenuDir "NeuroX Studio.lnk"
New-NeuroXStudioShortcut $startMenuShortcutPath

foreach ($folder in @($desktopDir, $startMenuDir)) {
    $legacyShortcut = Join-Path $folder "RHS Studio.lnk"
    if (-not (Test-Path -LiteralPath $legacyShortcut -PathType Leaf)) { continue }
    $shell = New-Object -ComObject WScript.Shell
    $legacyTarget = $shell.CreateShortcut($legacyShortcut).TargetPath
    if ($legacyTarget -notin @((Join-Path $legacyInstallDir "RHS Studio.exe"), $installedExe)) { continue }
    $backupDir = Join-Path $installDir ("legacy-shortcuts\" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
    Move-Item -LiteralPath $legacyShortcut -Destination (Join-Path $backupDir "RHS Studio.lnk")
}

Write-Host "NeuroX Studio installed to: $installDir"
Write-Host "Desktop shortcut created: $shortcutPath"
Write-Host "Start menu shortcut created: $startMenuShortcutPath"

if (-not $NoLaunch) {
    Start-Process -FilePath $installedExe -WorkingDirectory $installDir
}
