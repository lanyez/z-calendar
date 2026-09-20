param([string]$in, [string]$out)
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Bitmap]::FromFile($in)
$dst = New-Object System.Drawing.Bitmap($src.Width + 80, $src.Height + 80)
$g = [System.Drawing.Graphics]::FromImage($dst)
$g.Clear([System.Drawing.Color]::FromArgb(255, 16, 21, 28))
$g.DrawImage($src, 40, 40, $src.Width, $src.Height)
$dst.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output $out
