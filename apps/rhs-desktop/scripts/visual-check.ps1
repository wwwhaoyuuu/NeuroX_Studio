$ErrorActionPreference = "Stop"

$edgePath = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
$outputDirectory = Join-Path $PSScriptRoot "..\artifacts\visual-check"
$profileDirectory = Join-Path $PSScriptRoot "..\artifacts\edge-visual-profile"
$targetUrl = "http://127.0.0.1:1420/"
$debugPort = 9233
$script:nextCdpId = 1
$script:browserErrors = [System.Collections.Generic.List[string]]::new()

New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
New-Item -ItemType Directory -Force -Path $profileDirectory | Out-Null

$edgeArguments = @(
    "--headless=new",
    "--disable-gpu",
    "--disable-extensions",
    "--disable-sync",
    "--hide-scrollbars",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-features=msEdgeFirstRunExperience",
    "--remote-debugging-port=$debugPort",
    "--user-data-dir=$profileDirectory",
    "--window-size=1460,920",
    $targetUrl
)
$edge = Start-Process -FilePath $edgePath -ArgumentList $edgeArguments -PassThru -WindowStyle Hidden

function Get-PageTarget {
    for ($attempt = 0; $attempt -lt 200; $attempt++) {
        try {
            $pages = Invoke-RestMethod "http://127.0.0.1:$debugPort/json/list"
            $page = $pages | Where-Object { $_.type -eq "page" -and $_.url.StartsWith($targetUrl) } | Select-Object -First 1
            if ($page) { return $page }
        } catch {
            # Edge may still be starting.
        }
        Start-Sleep -Milliseconds 100
    }
    throw "Edge DevTools endpoint did not expose the NeuroX Studio page"
}

function Receive-WebSocketMessage {
    $stream = [System.IO.MemoryStream]::new()
    $buffer = New-Object byte[] 65536
    do {
        $segment = [ArraySegment[byte]]::new($buffer)
        $result = $script:socket.ReceiveAsync($segment, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
        if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
            throw "Edge closed the DevTools WebSocket"
        }
        $stream.Write($buffer, 0, $result.Count)
    } while (-not $result.EndOfMessage)
    [Text.Encoding]::UTF8.GetString($stream.ToArray())
}

function Send-CdpCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Method,
        [hashtable]$Parameters = @{}
    )
    $id = $script:nextCdpId
    $script:nextCdpId++
    $payload = @{ id = $id; method = $Method; params = $Parameters } | ConvertTo-Json -Compress -Depth 30
    $bytes = [Text.Encoding]::UTF8.GetBytes($payload)
    $segment = [ArraySegment[byte]]::new($bytes)
    $null = $script:socket.SendAsync(
        $segment,
        [System.Net.WebSockets.WebSocketMessageType]::Text,
        $true,
        [Threading.CancellationToken]::None
    ).GetAwaiter().GetResult()

    while ($true) {
        $message = Receive-WebSocketMessage | ConvertFrom-Json
        if ($message.method -eq "Runtime.exceptionThrown") {
            $script:browserErrors.Add([string]$message.params.exceptionDetails.text)
        }
        if ($message.method -eq "Log.entryAdded" -and $message.params.entry.level -eq "error") {
            $script:browserErrors.Add([string]$message.params.entry.text)
        }
        if ($message.id -ne $id) { continue }
        if ($message.error) { throw [string]$message.error.message }
        return $message.result
    }
}

function Invoke-PageExpression {
    param([Parameter(Mandatory = $true)][string]$Expression)
    $response = Send-CdpCommand "Runtime.evaluate" @{
        expression = $Expression
        awaitPromise = $true
        returnByValue = $true
    }
    if ($response.exceptionDetails) { throw [string]$response.exceptionDetails.text }
    $response.result.value
}

function Set-Viewport {
    param([int]$Width, [int]$Height)
    Send-CdpCommand "Emulation.setDeviceMetricsOverride" @{
        width = $Width
        height = $Height
        deviceScaleFactor = 1
        mobile = ($Width -le 680)
    } | Out-Null
    Start-Sleep -Milliseconds 250
}

function Save-Screenshot {
    param([Parameter(Mandatory = $true)][string]$Name)
    $response = Send-CdpCommand "Page.captureScreenshot" @{ format = "png"; fromSurface = $true }
    $file = Join-Path $outputDirectory $Name
    [IO.File]::WriteAllBytes($file, [Convert]::FromBase64String([string]$response.data))
    $file
}

$page = Get-PageTarget
$script:socket = [System.Net.WebSockets.ClientWebSocket]::new()
$null = $script:socket.ConnectAsync([Uri]$page.webSocketDebuggerUrl, [Threading.CancellationToken]::None).GetAwaiter().GetResult()

try {
    Send-CdpCommand "Runtime.enable" | Out-Null
    Send-CdpCommand "Page.enable" | Out-Null
    Send-CdpCommand "Log.enable" | Out-Null
    Start-Sleep -Seconds 1

    Set-Viewport 1460 920
    $desktop = Save-Screenshot "desktop-waveform.png"

    Invoke-PageExpression @'
(() => {
  const select = document.getElementById('mea-profile');
  select.value = 'mea-ab-64';
  select.dispatchEvent(new Event('change', { bubbles: true }));
  document.getElementById('open-stim-config').click();
})()
'@ | Out-Null
    Start-Sleep -Milliseconds 300
    $stimulation64 = Save-Screenshot "desktop-64-stimulation.png"
    $stimulationReport = Invoke-PageExpression @'
(() => {
  const dialog = document.getElementById('stim-config-dialog');
  return {
    open: dialog.open,
    electrodes: document.querySelectorAll('#stim-mea-grid button').length,
    previewOutputLocked: document.getElementById('stim-real-output').disabled && !document.getElementById('stim-real-output').checked,
    fits: dialog.getBoundingClientRect().right <= innerWidth && dialog.getBoundingClientRect().bottom <= innerHeight
  };
})()
'@
    Invoke-PageExpression "document.getElementById('close-stim-config').click()" | Out-Null

    Invoke-PageExpression "document.querySelector('[data-view=`"device-tools-view`"]')?.click()" | Out-Null
    Start-Sleep -Milliseconds 250
    $deviceTools = Save-Screenshot "desktop-device-tools.png"
    $deviceReport = Invoke-PageExpression @'
(() => {
  const panel = document.getElementById('device-tools-view');
  return {
    active: panel.classList.contains('active'),
    sectionCount: panel.querySelectorAll('.device-tool-section').length,
    ioInputBits: document.querySelectorAll('#digital-input-bits .io-bit').length,
    ioOutputBits: document.querySelectorAll('#digital-output-bits .io-bit').length,
    remoteTokenLength: document.getElementById('remote-api-token').value.length,
    overflowingControls: [...panel.querySelectorAll('button,input,select')]
      .filter((element) => element.scrollWidth > element.clientWidth + 2)
      .map((element) => element.id || element.textContent.trim()).slice(0, 20)
  };
})()
'@

    Invoke-PageExpression @'
(() => {
  const select = document.getElementById('mea-profile');
  select.value = 'mea-abcd-120';
  select.dispatchEvent(new Event('change', { bubbles: true }));
  document.querySelector('[data-view="waveform-view"]').click();
})()
'@ | Out-Null
    Start-Sleep -Seconds 1
    $channels120 = Save-Screenshot "desktop-120-channels.png"
    $channelReport = Invoke-PageExpression @'
(() => {
  const canvas = document.getElementById('waveform');
  const pixels = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
  let nonBackgroundPixels = 0;
  for (let index = 0; index < pixels.length; index += 4) {
    if (pixels[index] !== 17 || pixels[index + 1] !== 23 || pixels[index + 2] !== 25) nonBackgroundPixels += 1;
  }
  const firstPage = document.getElementById('channel-page-label').textContent;
  document.getElementById('next-channel-page').click();
  return {
    firstPage,
    secondPage: document.getElementById('channel-page-label').textContent,
    labels: document.querySelectorAll('#channel-labels .channel-label').length,
    nonBackgroundPixels
  };
})()
'@

    Set-Viewport 680 1000
    Invoke-PageExpression "document.querySelector('[data-view=`"device-tools-view`"]')?.click()" | Out-Null
    Start-Sleep -Milliseconds 300
    $narrow = Save-Screenshot "narrow-device-tools.png"
    $narrowReport = Invoke-PageExpression @'
(() => ({
  viewportWidth: innerWidth,
  documentWidth: document.documentElement.scrollWidth,
  bodyWidth: document.body.scrollWidth,
  appWidth: document.querySelector('.app-shell').scrollWidth,
  visibleSections: [...document.querySelectorAll('#device-tools-view .device-tool-section')]
    .filter((element) => {
      const style = getComputedStyle(element);
      return style.display !== 'none' && style.visibility !== 'hidden';
    }).length
}))()
'@

    $pageSeparator = [char]0x2013
    $checks = [ordered]@{
        devicePanelActive = [bool]$deviceReport.active
        stimulationDialogOpen = [bool]$stimulationReport.open
        stimulation64Electrodes = $stimulationReport.electrodes -eq 64
        previewOutputLocked = [bool]$stimulationReport.previewOutputLocked
        stimulationDialogFits = [bool]$stimulationReport.fits
        deviceSectionCount = $deviceReport.sectionCount -eq 4
        ioInputBitCount = $deviceReport.ioInputBits -eq 16
        ioOutputBitCount = $deviceReport.ioOutputBits -eq 16
        tokenLength = $deviceReport.remoteTokenLength -ge 12
        controlsFit = @($deviceReport.overflowingControls).Count -eq 0
        firstChannelPage = $channelReport.firstPage -eq "1${pageSeparator}16 / 120"
        secondChannelPage = $channelReport.secondPage -eq "17${pageSeparator}32 / 120"
        waveformPixels = $channelReport.nonBackgroundPixels -ge 1000
        narrowWidth = $narrowReport.documentWidth -le ($narrowReport.viewportWidth + 2)
        narrowSections = $narrowReport.visibleSections -eq 4
        noBrowserErrors = $script:browserErrors.Count -eq 0
    }
    $report = [ordered]@{
        targetUrl = $targetUrl
        screenshots = [ordered]@{
            desktop = $desktop
            stimulation64 = $stimulation64
            deviceTools = $deviceTools
            channels120 = $channels120
            narrow = $narrow
        }
        deviceReport = $deviceReport
        stimulationReport = $stimulationReport
        channelReport = $channelReport
        narrowReport = $narrowReport
        browserErrors = @($script:browserErrors)
        checks = $checks
    }
    $reportJson = $report | ConvertTo-Json -Depth 20
    [IO.File]::WriteAllText((Join-Path $outputDirectory "report.json"), $reportJson, [Text.Encoding]::UTF8)
    $reportJson

    if (@($checks.Values | Where-Object { -not $_ }).Count -gt 0) { exit 1 }
} finally {
    try { Send-CdpCommand "Browser.close" | Out-Null } catch { Stop-Process -Id $edge.Id -Force -ErrorAction SilentlyContinue }
    $script:socket.Dispose()
}
