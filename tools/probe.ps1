Add-Type @'
using System; using System.Runtime.InteropServices;
public class W {
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, string t);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 public struct R { public int L,T,Rt,B; }
}
'@
$tray=[W]::FindWindowEx([IntPtr]::Zero,[IntPtr]::Zero,'Shell_TrayWnd',$null)
$tn=[W]::FindWindowEx($tray,[IntPtr]::Zero,'TrayNotifyWnd',$null)
$clk=[W]::FindWindowEx($tn,[IntPtr]::Zero,'TrayClockWClass',$null)
"tray=$tray traynotify=$tn clock=$clk visible=$([W]::IsWindowVisible($clk))"
if ($clk -ne [IntPtr]::Zero) { $r=New-Object W+R; [W]::GetWindowRect($clk,[ref]$r)|Out-Null; "clock rect: L=$($r.L) T=$($r.T) R=$($r.Rt) B=$($r.B) size=$($r.Rt-$r.L)x$($r.B-$r.T)" }
