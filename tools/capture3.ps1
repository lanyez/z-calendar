param([string]$out = "E:\git\calendar\shots\screen3.png")
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public class C3 {
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr dst, int dx, int dy, int w, int h2, IntPtr src, int sx, int sy, int rop);
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
}
'@
$w=[C3]::GetSystemMetrics(0); $h=[C3]::GetSystemMetrics(1)
$sdc=[C3]::GetDC([IntPtr]::Zero)
$bmp=New-Object System.Drawing.Bitmap($w,$h)
$g=[System.Drawing.Graphics]::FromImage($bmp)
$hd=$g.GetHdc()
$ok=[C3]::BitBlt($hd,0,0,$w,$h,$sdc,0,0,0x00CC0020)
$g.ReleaseHdc($hd)
[C3]::ReleaseDC([IntPtr]::Zero,$sdc) | Out-Null
$bmp.Save($out,[System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "bitblt=$ok size=${w}x${h}"
$c=$bmp.GetPixel(960,600); Write-Output "center px: $($c.R),$($c.G),$($c.B)"
