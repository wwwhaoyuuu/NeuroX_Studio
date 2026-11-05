[CmdletBinding()]
param([string]$Version = "2026.11.05")

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ($Version -notmatch '^\d{4}\.\d{2}\.\d{2}(-[a-z0-9.-]+)?$') { throw "Invalid release version" }
$name = "NeuroX-Studio-$Version-windows-x64"
$directory = Join-Path $repo "releases\$name"
$zip = "$directory.zip"
$sourceZip = Join-Path $repo "releases\NeuroX-Studio-$Version-source.zip"
$sums = Join-Path $repo "releases\NeuroX-Studio-$Version-SHA256SUMS.txt"
foreach ($output in @($directory, $zip, $sourceZip, $sums)) {
    if (Test-Path -LiteralPath $output) { throw "Refusing to overwrite release output: $output" }
}
Push-Location $repo
try {
    $dirty = @(& git status --porcelain=v1 --untracked-files=normal)
    if ($LASTEXITCODE -ne 0 -or $dirty.Count -gt 0) { throw "Commit all intended source changes before packaging" }
    $commit = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw "Cannot resolve source commit" }
    $exe = Join-Path $repo "target\release\rhs-desktop.exe"
    if (-not (Test-Path -LiteralPath $exe)) { throw "Build the custom-protocol release first" }
    foreach ($line in Get-Content -LiteralPath (Join-Path $PSScriptRoot "local-env.bat")) {
        if ($line -match '^set\s+"FRONTPANEL_SDK=(?<value>.*)"\s*$') {
            $env:FRONTPANEL_SDK = $Matches.value.Replace("%~dp0", "$PSScriptRoot\")
        }
    }
    $dll = Join-Path $env:FRONTPANEL_SDK "API\lib\x64\okFrontPanel.dll"
    $license = Join-Path $env:FRONTPANEL_SDK "LICENSE.txt"
    $lucideLicense = Join-Path $repo "apps\rhs-desktop\node_modules\lucide\LICENSE"
    foreach ($file in @($dll, $license, $lucideLicense)) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing release dependency: $file" }
    }
    $dllVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($dll).FileVersion
    if ($dllVersion -ne "5.2.10.0") { throw "Review licensing/dependencies before packaging another FrontPanel version" }
    New-Item -ItemType Directory -Path (Join-Path $directory "licenses") | Out-Null
    Copy-Item -LiteralPath $exe -Destination (Join-Path $directory "NeuroX Studio.exe")
    Copy-Item -LiteralPath $dll -Destination $directory
    Copy-Item -LiteralPath $license -Destination (Join-Path $directory "licenses\FrontPanel-LICENSE.txt")
    Copy-Item -LiteralPath $lucideLicense -Destination (Join-Path $directory "licenses\Lucide-LICENSE")
    foreach ($doc in @("README.md", "COPYING", "THIRD_PARTY_NOTICES.md", "STIMULATION_BENCH.md", "SPIKES.md", "VALIDATION.md")) {
        Copy-Item -LiteralPath (Join-Path $repo $doc) -Destination $directory
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "Check-Dependencies.ps1") -Destination $directory
    $files = @(Get-ChildItem -LiteralPath $directory -Recurse -File | Sort-Object FullName | ForEach-Object {
        [ordered]@{ name = $_.FullName.Substring($directory.Length + 1).Replace('\', '/'); bytes = $_.Length;
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    [ordered]@{ version = $Version; sourceCommit = $commit; architecture = "windows-x64";
        customProtocol = $true; frontPanelVersion = $dllVersion; codeSigned = $false;
        boardFirmwareIncluded = $false; files = $files } | ConvertTo-Json -Depth 5 |
        Set-Content -LiteralPath (Join-Path $directory "manifest.json") -Encoding UTF8
    & git archive --format=zip "--output=$sourceZip" HEAD
    if ($LASTEXITCODE -ne 0) { throw "Could not archive corresponding source" }
    Compress-Archive -LiteralPath $directory -DestinationPath $zip -CompressionLevel Optimal
    $lines = @($zip, $sourceZip) | ForEach-Object {
        $hash = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $([IO.Path]::GetFileName($_))"
    }
    $lines | Set-Content -LiteralPath $sums -Encoding ASCII
    Get-Item -LiteralPath $zip, $sourceZip, $sums | Select-Object FullName, Length
} finally { Pop-Location }
