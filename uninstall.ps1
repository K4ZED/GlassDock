# Lepas GlassDock: berhenti jalan otomatis, tutup dock, dan kembalikan taskbar Windows.
Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "GlassDock" -ErrorAction SilentlyContinue
Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" -Name "GlassDock" -ErrorAction SilentlyContinue
Remove-Item (Join-Path ([Environment]::GetFolderPath('Startup')) "GlassDock.lnk") -ErrorAction SilentlyContinue
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class GdUninstall {
  [DllImport("user32.dll")] public static extern IntPtr FindWindow(string c, string w);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
$h = [GdUninstall]::FindWindow("GlassDock", [NullString]::Value)
if ($h -ne [IntPtr]::Zero) { [GdUninstall]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
"GlassDock tidak lagi berjalan otomatis. Taskbar Windows sudah kembali."
Unregister-ScheduledTask -TaskName "GlassDock" -Confirm:$false -ErrorAction SilentlyContinue
