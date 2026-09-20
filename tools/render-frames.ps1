<#
.SYNOPSIS
  Render the frames recorded by tests/demo_frames.rs as PNGs.

.DESCRIPTION
  Each frame is a JSON grid of coloured text runs taken from a real wmux
  session. This draws them with a monospace font inside a terminal-looking
  window, so the result is a picture of what the terminal actually showed.

.EXAMPLE
  $env:WMUX_DEMO_OUT = "target/demo-frames"
  cargo test --release --test demo_frames -- --ignored --nocapture
  pwsh -File tools/render-frames.ps1 -In target/demo-frames -Out target/demo-png
#>
[CmdletBinding()]
param(
    [string]$In = "target/demo-frames",
    [string]$Out = "target/demo-png",
    [string]$FontName = "Cascadia Mono",
    [double]$FontSize = 15,
    # Only render these frame numbers (1-based). Empty means all of them.
    [int[]]$Only = @(),
    [string]$Title = "wmux"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

# One Half Dark, the palette Windows Terminal ships with.
$palette = @(
    "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#dcdfe4",
    "#5a6374", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#dcdfe4"
)
$defaultFg = "#dcdfe4"
$defaultBg = "#282c34"
$chromeBg = "#21252b"

function To-Color([string]$spec, [string]$fallback) {
    if (-not $spec -or $spec -eq "default") { $spec = $fallback }
    if ($spec.StartsWith("#")) { return [System.Drawing.ColorTranslator]::FromHtml($spec) }
    $i = [int]$spec
    if ($i -lt 16) { return [System.Drawing.ColorTranslator]::FromHtml($palette[$i]) }
    if ($i -ge 232) {
        # 24-step grey ramp
        $v = 8 + 10 * ($i - 232)
        return [System.Drawing.Color]::FromArgb($v, $v, $v)
    }
    # 6x6x6 colour cube
    $n = $i - 16
    $steps = @(0, 95, 135, 175, 215, 255)
    return [System.Drawing.Color]::FromArgb($steps[[math]::Floor($n / 36)], $steps[[math]::Floor(($n % 36) / 6)], $steps[$n % 6])
}

$inDir = (Resolve-Path $In).Path
$frames = Get-ChildItem $inDir -Filter "f*.json" | Sort-Object Name
if ($frames.Count -eq 0) { throw "no frames in $inDir" }
New-Item -ItemType Directory -Force $Out | Out-Null
$outDir = (Resolve-Path $Out).Path

# Cell metrics: a monospace font, measured once on a long run of characters.
$probe = New-Object System.Drawing.Bitmap 10, 10
$pg = [System.Drawing.Graphics]::FromImage($probe)
$pg.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$font = New-Object System.Drawing.Font $FontName, $FontSize, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
$fontBold = New-Object System.Drawing.Font $FontName, $FontSize, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
$fmt = [System.Drawing.StringFormat]::GenericTypographic
$sample = "M" * 40
$cellW = [math]::Round($pg.MeasureString($sample, $font, [System.Drawing.PointF]::new(0, 0), $fmt).Width / 40, 2)
$cellH = [math]::Ceiling($font.GetHeight($pg)) + 2
$pg.Dispose(); $probe.Dispose()

$pad = 18            # padding around the grid
$barH = 30           # window title bar

$first = Get-Content $frames[0].FullName -Raw | ConvertFrom-Json
$gridW = [int][math]::Ceiling($cellW * $first.cols)
$gridH = [int]($cellH * $first.rows)
$imgW = $gridW + 2 * $pad
$imgH = $gridH + 2 * $pad + $barH

$n = 0
foreach ($file in $frames) {
    $n++
    if ($Only.Count -gt 0 -and $Only -notcontains $n) { continue }
    $frame = Get-Content $file.FullName -Raw | ConvertFrom-Json
    $bmp = New-Object System.Drawing.Bitmap $imgW, $imgH
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias

    # Window chrome: title bar with the usual three dots.
    $g.Clear([System.Drawing.ColorTranslator]::FromHtml($chromeBg))
    $bgBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml($defaultBg))
    $g.FillRectangle($bgBrush, 0, $barH, $imgW, $imgH - $barH)
    foreach ($dot in @(@{x = 18; c = "#e06c75" }, @{x = 38; c = "#e5c07b" }, @{x = 58; c = "#98c379" })) {
        $b = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml($dot.c))
        $g.FillEllipse($b, $dot.x - 5, ($barH / 2) - 5, 10, 10)
        $b.Dispose()
    }
    $titleBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml("#8a919e"))
    $titleFont = New-Object System.Drawing.Font "Segoe UI", 12, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
    $titleSize = $g.MeasureString($Title, $titleFont)
    $g.DrawString($Title, $titleFont, $titleBrush, ($imgW - $titleSize.Width) / 2, ($barH - $titleSize.Height) / 2)

    $y = 0
    foreach ($line in $frame.lines) {
        $x = 0.0
        foreach ($run in $line) {
            $text = $run.t
            if (-not $text) { $x += 0; continue }
            $fg = To-Color $run.fg $defaultFg
            $bg = To-Color $run.bg $defaultBg
            if ($run.i) { $tmp = $fg; $fg = $bg; $bg = $tmp }
            $w = $cellW * $text.Length
            if ("$($run.bg)" -ne "default" -or $run.i) {
                $b = New-Object System.Drawing.SolidBrush $bg
                $g.FillRectangle($b, [float]($pad + $x), [float]($barH + $pad + $y * $cellH), [float]$w, [float]$cellH)
                $b.Dispose()
            }
            $b = New-Object System.Drawing.SolidBrush $fg
            $f = if ($run.b) { $fontBold } else { $font }
            $g.DrawString($text, $f, $b, [float]($pad + $x), [float]($barH + $pad + $y * $cellH), $fmt)
            $b.Dispose()
            $x += $w
        }
        $y++
    }

    # A block cursor, so the recording looks alive.
    if ($frame.cursor_visible) {
        $cx = $frame.cursor[0]; $cy = $frame.cursor[1]
        $b = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml("#dcdfe4"))
        $g.FillRectangle($b, [float]($pad + $cellW * $cx), [float]($barH + $pad + $cy * $cellH), [float]$cellW, [float]$cellH)
        $b.Dispose()
    }

    $g.Dispose()
    $bmp.Save((Join-Path $outDir ("f{0:d4}.png" -f $n)), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

Write-Host "rendered $(@(Get-ChildItem $outDir -Filter *.png).Count) frames at ${imgW}x${imgH} into $outDir"
