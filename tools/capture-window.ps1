param([long]$hwnd, [string]$out)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public class PC {
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  public struct R { public int L, T, Rt, B; }
}
'@
$r = New-Object PC+R
[PC]::GetWindowRect([IntPtr]$hwnd, [ref]$r) | Out-Null
$w = $r.Rt - $r.L; $h = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
$ok = [PC]::PrintWindow([IntPtr]$hwnd, $dc, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($dc)
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "captured ${w}x${h} ok=$ok -> $out"
