# Pasang GlassDock: salin exe ke lokasi tetap dan jalankan otomatis setiap login.
# Jalankan lagi setiap kali GlassDock di-build ulang agar versi terpasang ikut diperbarui.
$ErrorActionPreference = 'Stop'
$src  = "$PSScriptRoot\target\release\glassdock.exe"
$dir  = "$env:LOCALAPPDATA\GlassDock"
$exe  = "$dir\glassdock.exe"

# tutup dock yang sedang berjalan (dengan rapi, agar taskbar & work area dikembalikan)
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class GdInstall {
  [DllImport("user32.dll")] public static extern IntPtr FindWindow(string c, string w);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
$h = [GdInstall]::FindWindow("GlassDock", [NullString]::Value)
if ($h -ne [IntPtr]::Zero) { [GdInstall]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
$t = 0; while ((Get-Process glassdock -ErrorAction SilentlyContinue) -and $t -lt 50) { Start-Sleep -Milliseconds 100; $t++ }

New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $src $exe -Force
Copy-Item "$PSScriptRoot\restore-taskbar.ps1" "$dir\restore-taskbar.ps1" -Force

# jalan otomatis saat login (hanya untuk akun ini, tanpa hak admin)
Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "GlassDock" -Value "`"$exe`""

Start-Process $exe
"GlassDock terpasang di $exe dan akan berjalan otomatis setiap login."
