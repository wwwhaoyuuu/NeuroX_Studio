[CmdletBinding()]
param([string]$Version = "2026.11.05-hardware1")

$ErrorActionPreference = "Stop"
if ($Version -notmatch '^\d{4}\.\d{2}\.\d{2}(-[a-z0-9.-]+)?$') { throw "Invalid version" }
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$name = "NeuroX-Studio-$Version-windows-x64"
$directory = Join-Path $repo "releases\$name"
$sourceName = "NeuroX-Studio-$Version-source.zip"
$releaseRoot = Split-Path $directory -Parent
Add-Type -AssemblyName System.IO.Compression.FileSystem
function Assert([bool]$Okay, [string]$Message) {
    if (-not $Okay) { throw $Message }
}
function Entry-Hash($Entry) {
    $stream = $Entry.Open()
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($sha.ComputeHash($stream)).Replace("-", "").ToLowerInvariant() }
    finally { $sha.Dispose(); $stream.Dispose() }
}
function Entry-Text($Entry) {
    $reader = [IO.StreamReader]::new($Entry.Open())
    try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
}

$zip = [IO.Compression.ZipFile]::OpenRead("$directory.zip")
try {
    $entries = @{}
    foreach ($entry in $zip.Entries) {
        $path = $entry.FullName.Replace('\', '/')
        Assert ($path.StartsWith("$name/") -and $path -notmatch '(^|/)\.\.(/|$)' -and $path -notmatch ':') "Unsafe ZIP path: $path"
        if ($path.EndsWith('/')) { continue }
        $relative = $path.Substring($name.Length + 1)
        Assert (-not $entries.ContainsKey($relative)) "Duplicate ZIP entry: $relative"
        $entries[$relative] = $entry
    }
    Assert ($entries.ContainsKey('manifest.json')) "Missing manifest"
    $manifest = Entry-Text $entries['manifest.json'] | ConvertFrom-Json
    Assert ($manifest.version -eq $Version -and $manifest.customProtocol -and $manifest.architecture -eq 'windows-x64') "Wrong release metadata"
    Assert ($entries.Count -eq $manifest.files.Count + 1) "ZIP has missing or unlisted files"
    $listed = @{}
    foreach ($file in $manifest.files) {
        Assert (-not $listed.ContainsKey($file.name)) "Duplicate manifest file"
        $listed[$file.name] = $true
        Assert ($entries.ContainsKey($file.name)) "Missing ZIP file: $($file.name)"
        Assert ($entries[$file.name].Length -eq $file.bytes) "Wrong byte count: $($file.name)"
        Assert ((Entry-Hash $entries[$file.name]) -eq $file.sha256) "ZIP hash mismatch: $($file.name)"
        Assert ((Get-FileHash -LiteralPath (Join-Path $directory $file.name)).Hash -eq $file.sha256) "Unpacked file differs: $($file.name)"
    }
    $hardwareNames = @('ConfigRHSController.bit', 'Install-USB-Driver.cmd', 'hardware-payload.json')
    if ($manifest.boardFirmwareIncluded) {
        Assert ($manifest.usbDriverIncluded) "Hardware bundle missing driver flag"
        $payload = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'hardware-payload.json') -Raw | ConvertFrom-Json
        Assert ($manifest.hardwareProfile -eq $payload.profile) "Wrong hardware profile"
        foreach ($file in $payload.files) {
            Assert ($entries.ContainsKey($file.name)) "Missing pinned hardware file: $($file.name)"
            Assert ((Entry-Hash $entries[$file.name]) -eq $file.sha256) "Pinned hardware hash differs: $($file.name)"
        }
        foreach ($file in $hardwareNames) { Assert ($entries.ContainsKey($file)) "Missing hardware helper: $file" }
        foreach ($catalog in @('okusb.cat', 'okuart.cat')) {
            $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $directory "drivers\amd64\$catalog")
            Assert ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like '*Opal Kelly Incorporated*') "Invalid driver catalog: $catalog"
        }
        & (Join-Path $directory 'Install-USB-Driver.cmd') --check
        Assert ($LASTEXITCODE -eq 0) "Driver helper dry-run failed"
    } else {
        Assert (-not $manifest.usbDriverIncluded) "Minimal package has driver flag"
        foreach ($file in $hardwareNames) { Assert (-not $entries.ContainsKey($file)) "Unexpected hardware file: $file" }
        Assert (@($entries.Keys | Where-Object { $_ -like 'drivers/*' }).Count -eq 0) "Unexpected drivers in minimal package"
    }
    Write-Output "PASS: $($manifest.files.Count) runtime files match hashes, lengths and package policy."
} finally { $zip.Dispose() }

Push-Location $repo
try {
    Assert ((& git rev-parse HEAD).Trim() -eq $manifest.sourceCommit) "Package source differs from HEAD"
    $sourceFiles = @(& git ls-tree -r --name-only $manifest.sourceCommit)
    Assert ($LASTEXITCODE -eq 0) "Cannot list corresponding source"
    $sourceZip = [IO.Compression.ZipFile]::OpenRead((Join-Path $releaseRoot $sourceName))
    try {
        $archiveFiles = @($sourceZip.Entries | Where-Object { -not $_.FullName.EndsWith('/') } | ForEach-Object { $_.FullName })
        Assert (@(Compare-Object $sourceFiles $archiveFiles).Count -eq 0) "Source archive differs from tracked file list"
        Assert (@($archiveFiles | Where-Object { $_ -match '(^|/)(work|releases|local-assets|target|node_modules)/|(^|/)local-env\.bat$|\.(bit|exe|dll|rhsraw|rhs|rhd)$' }).Count -eq 0) "Source archive contains private/generated payload"
    } finally { $sourceZip.Dispose() }
} finally { Pop-Location }
foreach ($line in Get-Content -LiteralPath (Join-Path $releaseRoot "NeuroX-Studio-$Version-SHA256SUMS.txt")) {
    Assert ($line -match '^([a-f0-9]{64})  (.+)$') "Malformed checksum line"
    $hash = $Matches[1]
    $fileName = $Matches[2]
    Assert ($fileName -in @("$name.zip", $sourceName)) "Unexpected checksum target"
    Assert ((Get-FileHash -LiteralPath (Join-Path $releaseRoot $fileName)).Hash -eq $hash) "Asset checksum differs: $fileName"
}
Write-Output "PASS: corresponding source file list and release ZIP checksums. No board access or driver installation."
