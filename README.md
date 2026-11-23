# GlassDock

A lightweight liquid-glass dock that replaces the Windows 11 taskbar, written in Rust with plain Win32 + Direct2D (no web view, no framework).

- **Centered glass dock** at the bottom of the screen: pinned and running apps, open/active indicators, auto-hide (slides up when the cursor reaches the bottom middle of the screen).
- **iPhone-style status**: Wi-Fi icon inside a battery ring, battery percentage underneath, `HH:mm` clock and date.
- **Glass panels** that open above the dock with a fade/slide animation and a blurred background:
  - **Start menu**: search-as-you-type plus a grid of all installed apps. The Windows key opens this instead of the built-in Start.
  - **Control Center**: Wi-Fi/Bluetooth/airplane/network buttons, battery ring, Night light and Battery saver tiles, vertical brightness and volume sliders, Settings/Screenshot/Lock/Power.
  - **Calendar**: month view with today highlighted.
- Hides the Windows taskbar while running and restores it on exit.
- Dark right-click menus, three glass styles (Clear, Frosted, Dark).
- Small: ~400 KB executable, roughly 15–60 MB of RAM depending on which panels have been used.

## Requirements

- Windows 11 (Windows 10 mostly works, but is untested)
- [Rust](https://rustup.rs) (stable, MSVC toolchain)
- Visual Studio Build Tools with the **Desktop development with C++** workload (for `link.exe` and the Windows SDK)

## Build

```powershell
cargo build --release
```

The executable is written to `target\release\glassdock.exe`.

## Install / uninstall

```powershell
.\install.ps1
```

This copies the executable to `%LOCALAPPDATA%\GlassDock`, registers it to start at login (current user only, no admin rights needed), and starts it. Run it again after every rebuild to update the installed copy.

```powershell
.\uninstall.ps1
```

This stops auto-start, closes the dock and brings the Windows taskbar back.

If the dock is ever killed (for example with *End task*), the Windows taskbar stays hidden. Run `restore-taskbar.ps1` to bring it back.

## Usage

| Action | Result |
| --- | --- |
| Click an app icon | Open it, switch to its window, or minimize it |
| Middle-click an app icon | Open a new window |
| Right-click an app icon | Open new window, close all windows, pin/unpin, move left/right |
| Right-click the dock | Auto-hide, glass style, show/hide Windows taskbar, quit |
| Click Start / press the Windows key | Glass Start menu (type to search, Enter to open the first result) |
| Click the Wi-Fi/battery ring | Control Center |
| Click the clock | Calendar |

## Settings files

Stored in `%APPDATA%\GlassDock`:

- `pins.txt`: pinned apps, one per line, in dock order. `lnk:<name>` = shortcut in the taskbar pin folder, `app:<AppID>` = Store app (list AppIDs with `Get-StartApps`), `exe:<path>` = program.
- `style.txt`: `clear`, `frosted` or `dark`.
- `autohide.txt`: `1` or `0`.

The default pin list used on first run is the `PINS` constant in `src/main.rs`.

## Project layout

| File | Contents |
| --- | --- |
| `src/main.rs` | Dock window, layout, drawing, auto-hide, taskbar hiding, Windows-key hook |
| `src/panels.rs` | Start menu, Control Center and Calendar panels |
| `src/apps.rs` | Pins, running windows, app icons, launching |
| `src/sys.rs` | Battery, Wi-Fi, volume (Core Audio), brightness (WMI), acrylic, dark menus |

## Known limitations

- Tray icons from other apps (the area at the right end of the Windows taskbar) are not shown in the dock. Use *Show Windows taskbar* from the dock's right-click menu when you need them.
- Brightness control works for built-in laptop screens only (WMI), not external monitors.
- Primary monitor only.
- Uses a few undocumented Windows functions (`SetWindowCompositionAttribute` for acrylic, uxtheme ordinals 135/136 for dark menus) that a future Windows update could change.
