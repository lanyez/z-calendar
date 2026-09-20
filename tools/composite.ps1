Add-Type -AssemblyName System.Drawing
$files = Get-ChildItem 'E:\git\calendar\shots\*.png'
foreach ($f in $files) {
  $src = [System.Drawing.Bitmap]::FromFile($f.FullName)
  $bw = $src.Width + 80; $bh = $src.Height + 80
  $dst = New-Object System.Drawing.Bitmap($bw, $bh)
  $g = [System.Drawing.Graphics]::FromImage($dst)
  $g.Clear([System.Drawing.Color]::FromArgb(255, 16, 21, 28))
  $g.DrawImage($src, 40, 40, $src.Width, $src.Height)
  $out = 'E:\git\calendar\shots\view-' + $f.Name
  $dst.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $dst.Dispose(); $src.Dispose()
  Write-Output $out
}
