param([int]$x, [int]$y, [int]$right = 0)
Add-Type @'
using System; using System.Runtime.InteropServices;
public class M {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
}
'@
[M]::SetCursorPos($x,$y) | Out-Null
Start-Sleep -Milliseconds 120
if ($right -eq 1) { [M]::mouse_event(8,0,0,0,[UIntPtr]::Zero); [M]::mouse_event(16,0,0,0,[UIntPtr]::Zero) }
else { [M]::mouse_event(2,0,0,0,[UIntPtr]::Zero); [M]::mouse_event(4,0,0,0,[UIntPtr]::Zero) }
Write-Output "clicked $x,$y right=$right"
