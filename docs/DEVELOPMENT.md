# GlassDock: development notes

## Requirements

- Windows 10/11
- [Rust](https://rustup.rs) (stable, MSVC toolchain)
- Visual Studio Build Tools with the **Desktop development with C++** workload
- For the installer: [Inno Setup 6](https://jrsoftware.org/isinfo.php) (`winget install JRSoftware.InnoSetup`)

## Build

```powershell
cargo build --release          # target\release\glassdock.exe
.\build-installer.ps1          # installer\Output\GlassDock-Setup-<version>.exe
```

## Install without the installer

```powershell
.\install.ps1     # copy to %LOCALAPPDATA%\GlassDock, start at sign-in, start now (run again after each rebuild)
.\uninstall.ps1   # stop auto-start, close the dock, bring the Windows taskbar back
```

## Publishing a release

1. Raise `version` in `Cargo.toml` and `AppVersion` in `installer\GlassDock.iss` (they must match).
2. Run `.\build-installer.ps1`.
3. On GitHub, create a release with the tag `v<version>` (not a pre-release) and attach `installer\Output\GlassDock-Setup-<version>.exe`.
4. In the README's Download table, change the version in the file name and the link, and the size if it changed.

Installed docks only offer releases whose tag is newer than their own version and that include a `GlassDock-Setup-*.exe` file. To test the update flow without publishing, put the path of a JSON file shaped like the GitHub release API answer into `%APPDATA%\GlassDock\update-feed.txt` (delete it afterwards).

## Settings files (`%APPDATA%\GlassDock`)

- `pins.txt`: pinned apps in dock order. `lnk:<name>` = shortcut in the taskbar pin folder, `app:<AppID>` = Store app (`Get-StartApps` lists AppIDs), `exe:<path>` = program. First-run defaults: the `PINS` constant in `src/main.rs`.
- `style.txt`: `clear`, `frosted` or `dark`.
- `autohide.txt`: `smart` (default), `1` (always) or `0` (never).
- `update-check.txt`: time of the last update check.

## Logs

Starts, failed start attempts (it retries for about a minute at sign-in) and crashes go to `%TEMP%\glassdock-crash.log`. A crash brings the Windows taskbar back. If the dock is killed from outside (*End task*), run **GlassDock - restore Windows taskbar** from the Start Menu or `restore-taskbar.ps1`.

## Project layout

| File | Contents |
| --- | --- |
| `src/main.rs` | Dock window, layout, drawing, auto-hide, taskbar hiding, keyboard hook, watchdog |
| `src/panels.rs` | Start menu, Control Center, Calendar, power card, right-click menu |
| `src/osd.rs` | Glass volume and brightness indicators |
| `src/update.rs` | Daily update check (GitHub Releases), update notice, one-click install |
| `src/apps.rs` | Pins, running windows (incl. Store and web apps hosted by a browser), icons, launching |
| `src/sys.rs` | Battery, Wi-Fi, radios (WinRT), Battery saver, volume (Core Audio), brightness (WMI), memory |

Undocumented Windows functions in use: `SetWindowCompositionAttribute` (acrylic) and uxtheme ordinals 135/136 (dark menus). A future Windows update could change them.
