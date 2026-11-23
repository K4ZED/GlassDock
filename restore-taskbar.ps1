# Darurat: munculkan lagi taskbar Windows jika GlassDock tertutup paksa (crash / End task)
Get-Process glassdock -ErrorAction SilentlyContinue | Stop-Process -Force
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Tb {
  [DllImport("user32.dll")] public static extern IntPtr FindWindow(string c, string w);
  [DllImport("user32.dll")] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, string w);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
}
"@
$none = [NullString]::Value   # $null biasa jadi "" saat dikirim ke Win32
[Tb]::ShowWindow([Tb]::FindWindow("Shell_TrayWnd", $none), 5) | Out-Null
$h = [IntPtr]::Zero
while (($h = [Tb]::FindWindowEx([IntPtr]::Zero, $h, "Shell_SecondaryTrayWnd", $none)) -ne [IntPtr]::Zero) { [Tb]::ShowWindow($h, 5) | Out-Null }
# restart explorer agar work area (area jendela maximize) dihitung ulang
Stop-Process -Name explorer -Force
"Taskbar restored."
