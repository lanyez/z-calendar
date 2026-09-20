param([string]$out = "E:\git\calendar\shot.png")
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public class Cap {
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
}
'@
$w=[Cap]::GetSystemMetrics(0); $h=[Cap]::GetSystemMetrics(1)
$bmp=New-Object System.Drawing.Bitmap($w,$h)
$g=[System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen(0,0,0,0,$bmp.Size)
$bmp.Save($out,[System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "saved $out ($w x $h)"
