$ErrorActionPreference = 'Continue'
$exe = 'D:\ai-projects\mouse-sensitivity-switcher\target\release\mouse-speed-switcher.exe'
Get-Process mouse-speed-switcher -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WE {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }

  public static string ListWindows(uint pid) {
    var sb = new StringBuilder();
    EnumWindows((h, l) => {
      uint wpid;
      GetWindowThreadProcessId(h, out wpid);
      if (wpid == pid && IsWindowVisible(h)) {
        RECT r;
        GetWindowRect(h, out r);
        sb.AppendLine(string.Format("hwnd=0x{0:X} rect=({1},{2})-({3},{4}) size={5}x{6}", h.ToInt64(), r.L, r.T, r.R, r.B, r.R-r.L, r.B-r.T));
      }
      return true;
    }, IntPtr.Zero);
    return sb.ToString();
  }
}
'@
[WE]::SetProcessDPIAware() | Out-Null

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exe
$psi.UseShellExecute = $false
$psi.EnvironmentVariables['MSS_DEBUG_MENU'] = '1'
$psi.EnvironmentVariables['MSS_DEBUG_SUB'] = '1'
$p = [System.Diagnostics.Process]::Start($psi)
Start-Sleep -Milliseconds 2500
if ($p.HasExited) { Write-Output '!!! EXITED'; exit 1 }
Write-Output ([WE]::ListWindows([uint32]$p.Id))
Write-Output '期望: 菜单 640x548 + 子窗口 360x208 位于 (L+640, T+192)'
Stop-Process -Id $p.Id -Force
