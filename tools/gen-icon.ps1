Add-Type -AssemblyName System.Drawing
$size=64
$bmp=New-Object System.Drawing.Bitmap($size,$size)
$g=[System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode='AntiAlias'; $g.TextRenderingHint='AntiAliasGridFit'
$path=New-Object System.Drawing.Drawing2D.GraphicsPath
$r=14
$path.AddArc(0,0,$r,$r,180,90); $path.AddArc($size-$r,0,$r,$r,270,90); $path.AddArc($size-$r,$size-$r,$r,$r,0,90); $path.AddArc(0,$size-$r,$r,$r,90,90); $path.CloseFigure()
$brush=New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255,62,135,250))
$g.FillPath($brush,$path)
$font=New-Object System.Drawing.Font('Microsoft YaHei',24,[System.Drawing.FontStyle]::Bold)
$fmt=New-Object System.Drawing.StringFormat
$fmt.Alignment='Center'; $fmt.LineAlignment='Center'
$ri=[char]0x65E5
$g.DrawString($ri,$font,[System.Drawing.Brushes]::White,(New-Object System.Drawing.RectangleF(0,-2,$size,$size)),$fmt)
$bmp.Save('E:\git\calendar\app\icon.png',[System.Drawing.Imaging.ImageFormat]::Png)
$ico=[System.Drawing.Icon]::FromHandle($bmp.GetHicon())
$fs=[System.IO.File]::Create('E:\git\calendar\app\icon.ico'); $ico.Save($fs); $fs.Close()
Write-Output 'icon done'
