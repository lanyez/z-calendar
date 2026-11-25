param([string]$srcPath, [string]$outPath, [int]$x, [int]$y, [int]$w, [int]$h, [int]$zoom = 3)
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Image]::FromFile($srcPath)
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.DrawImage($src, (New-Object System.Drawing.Rectangle(0, 0, $w, $h)), (New-Object System.Drawing.Rectangle($x, $y, $w, $h)), [System.Drawing.GraphicsUnit]::Pixel)
$g.Dispose()
$big = New-Object System.Drawing.Bitmap ($w * $zoom), ($h * $zoom)
$g2 = [System.Drawing.Graphics]::FromImage($big)
$g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g2.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
$g2.DrawImage($bmp, 0, 0, ($w * $zoom), ($h * $zoom))
$g2.Dispose()
$big.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
$src.Dispose(); $bmp.Dispose(); $big.Dispose()
Write-Output "saved $outPath (${w}x${h} @${zoom}x)"
