[CmdletBinding()]
param([string]$Name = "neurox-studio-local")

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ($Name -notmatch '^[a-z0-9][a-z0-9-]+$') { throw "Invalid package name" }
$output = Join-Path $repo "releases\$Name"
$archive = "$output.zip"
if ((Test-Path -LiteralPath $output) -or (Test-Path -LiteralPath $archive)) {
    throw "Package already exists; choose a new name instead of overwriting it"
}

foreach ($line in Get-Content -LiteralPath (Join-Path $PSScriptRoot "local-env.bat")) {
    if ($line -match '^set\s+"(?<name>FRONTPANEL_SDK|RHS_BITFILE)=(?<value>.*)"\s*$') {
        Set-Item -Path "Env:$($Matches.name)" -Value $Matches.value.Replace("%~dp0", "$PSScriptRoot\")
    }
}
$exe = Join-Path $repo "target\release\rhs-desktop.exe"
$dlls = @(Get-ChildItem -LiteralPath (Join-Path $env:FRONTPANEL_SDK "API\lib\x64") -Filter "*.dll" -File)
if (!(Test-Path -LiteralPath $exe) -or !(Test-Path -LiteralPath $env:RHS_BITFILE) -or $dlls.Count -eq 0) {
    throw "Build the custom-protocol release and configure the SDK/bitfile first"
}

New-Item -ItemType Directory -Path $output | Out-Null
Copy-Item -LiteralPath $exe -Destination (Join-Path $output "NeuroX Studio.exe")
Copy-Item -LiteralPath $env:RHS_BITFILE -Destination (Join-Path $output "ConfigRHSController.bit")
foreach ($dll in $dlls) { Copy-Item -LiteralPath $dll.FullName -Destination (Join-Path $output $dll.Name) }
foreach ($document in @("STIMULATION_BENCH.md", "VALIDATION.md", "README.md", "SPIKES.md")) {
    Copy-Item -LiteralPath (Join-Path $repo $document) -Destination $output
}
Copy-Item -LiteralPath (Join-Path $repo "vendor\intan-rhythmstim-api-230822\COPYING") -Destination $output

$source = Join-Path $output "source"
Push-Location $repo
try {
    $files = @(& git -c core.quotepath=false ls-files --cached --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) { throw "Could not enumerate source snapshot" }
    foreach ($file in $files | Sort-Object -Unique) {
        if ($file -match '^(releases|work|target|local-assets)/' -or $file -match '\.(zip|rar|rhsraw|bit|dll|exe|pdb)$') { continue }
        $destination = Join-Path $source $file
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
        Copy-Item -LiteralPath (Join-Path $repo $file) -Destination $destination
    }
} finally { Pop-Location }

$manifest = @(Get-ChildItem -LiteralPath $output -File | ForEach-Object {
    [ordered]@{ name = $_.Name; bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
})
$manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $output "manifest.json") -Encoding UTF8
Compress-Archive -LiteralPath $output -DestinationPath $archive -CompressionLevel Optimal
Get-Item -LiteralPath $archive | Select-Object FullName, Length
$hash = Get-FileHash -LiteralPath $archive -Algorithm SHA256
Write-Output "SHA256: $($hash.Hash)"
