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
# mark it Enabled in Settings > Apps > Startup (Windows can skip Run entries without this)
$sa = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
if (-not (Test-Path $sa)) { New-Item -Path $sa -Force | Out-Null }
Set-ItemProperty -Path $sa -Name "GlassDock" -Type Binary -Value ([byte[]](2,0,0,0,0,0,0,0,0,0,0,0))
# backup: shortcut in the Startup folder (a second launch exits right away)
$lnk = Join-Path ([Environment]::GetFolderPath('Startup')) "GlassDock.lnk"
$sh = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk); $sh.TargetPath = $exe; $sh.WorkingDirectory = Split-Path $exe; $sh.Save()
# fastest: a per-user task that runs right at sign-in, without the Startup apps delay
$me = "$env:USERDOMAIN\$env:USERNAME"
Register-ScheduledTask -TaskName "GlassDock" -Force `
  -Action (New-ScheduledTaskAction -Execute $exe -WorkingDirectory (Split-Path $exe)) `
  -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $me) `
  -Principal (New-ScheduledTaskPrincipal -UserId $me -LogonType Interactive -RunLevel Limited) `
  -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Priority 4) | Out-Null

Start-Process $exe
"GlassDock terpasang di $exe dan akan berjalan otomatis setiap login."
