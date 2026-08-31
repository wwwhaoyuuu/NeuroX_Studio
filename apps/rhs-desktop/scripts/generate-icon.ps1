Add-Type -AssemblyName System.Drawing

$size = 256
$bitmap = New-Object System.Drawing.Bitmap($size, $size)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$graphics.Clear([System.Drawing.Color]::FromArgb(246, 248, 248))

$background = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(29, 108, 104))
$chip = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(18, 25, 28))
$pin = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(136, 154, 158))
$wave = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(66, 211, 176), 11)
$wave.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$wave.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$wave.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round

$graphics.FillRectangle($background, 12, 12, 232, 232)
$graphics.FillRectangle($chip, 62, 62, 132, 132)

foreach ($offset in 0, 1, 2, 3, 4, 5) {
    $position = 72 + $offset * 22
    $graphics.FillRectangle($pin, 40, $position, 22, 10)
    $graphics.FillRectangle($pin, 194, $position, 22, 10)
    $graphics.FillRectangle($pin, $position, 40, 10, 22)
    $graphics.FillRectangle($pin, $position, 194, 10, 22)
}

$points = [System.Drawing.Point[]]@(
    (New-Object System.Drawing.Point(76, 132)),
    (New-Object System.Drawing.Point(96, 132)),
    (New-Object System.Drawing.Point(110, 99)),
    (New-Object System.Drawing.Point(129, 164)),
    (New-Object System.Drawing.Point(146, 117)),
    (New-Object System.Drawing.Point(160, 132)),
    (New-Object System.Drawing.Point(180, 132))
)
$graphics.DrawLines($wave, $points)

$iconDir = Join-Path $PSScriptRoot '..\src-tauri\icons'
$pngPath = Join-Path $iconDir 'icon.png'
$icoPath = Join-Path $iconDir 'icon.ico'
$bitmap.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)

$pngBytes = [System.IO.File]::ReadAllBytes($pngPath)
$stream = [System.IO.File]::Create($icoPath)
$writer = New-Object System.IO.BinaryWriter($stream)
$writer.Write([UInt16]0)
$writer.Write([UInt16]1)
$writer.Write([UInt16]1)
$writer.Write([Byte]0)
$writer.Write([Byte]0)
$writer.Write([Byte]0)
$writer.Write([Byte]0)
$writer.Write([UInt16]1)
$writer.Write([UInt16]32)
$writer.Write([UInt32]$pngBytes.Length)
$writer.Write([UInt32]22)
$writer.Write($pngBytes)
$writer.Dispose()
$stream.Dispose()

$graphics.Dispose()
$bitmap.Dispose()
$background.Dispose()
$chip.Dispose()
$pin.Dispose()
$wave.Dispose()

Write-Output "Generated $icoPath"
