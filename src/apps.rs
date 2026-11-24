// Daftar aplikasi untuk dock: pin, jendela yang sedang terbuka, ikon, dan aksi klik

use std::{collections::HashSet, ffi::c_void, path::PathBuf, ptr::null_mut};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
            Gdi::{DeleteObject, HBITMAP},
            Imaging::*,
        },
        Storage::Packaging::Appx::GetApplicationUserModelId,
        System::{Com::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

/// `spec` = cara pin disimpan di pins.txt: "lnk:<nama>", "app:<AppID>", atau "exe:<path>"
pub struct Pin { pub key: String, pub launch: String, pub icon_src: String, pub spec: String }

#[derive(Clone)]
pub struct Ident { pub key: String, pub launch: String, pub icon_src: String }

pub struct AppItem {
    pub key: String,
    pub launch: String,
    pub icon_src: String,
    pub pinned: bool,
    pub windows: Vec<HWND>, // urutan z-order: [0] = paling baru dipakai
}

fn explorer_key() -> String {
    format!("{}\\explorer.exe", std::env::var("WINDIR").unwrap_or("C:\\Windows".into())).to_lowercase()
}

fn app_pin(aumid: &str) -> Pin {
    let s = format!("shell:AppsFolder\\{}", aumid);
    Pin { key: format!("aumid:{}", aumid.to_lowercase()), launch: s.clone(), icon_src: s, spec: format!("app:{}", aumid) }
}

fn exe_pin(path: &str) -> Pin {
    Pin { key: path.to_lowercase(), launch: path.to_string(), icon_src: path.to_string(), spec: format!("exe:{}", path) }
}

fn pinned_lnks() -> Vec<(String, PathBuf)> {
    let dir = PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
        .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
    let mut lnks: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("lnk")).unwrap_or(false))
                .map(|p| (p.file_stem().unwrap_or_default().to_string_lossy().to_string(), p))
                .collect()
        })
        .unwrap_or_default();
    lnks.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    lnks
}

/// Daftar awal (dipakai sekali jika pins.txt belum ada): PINS lalu sisa .lnk di folder pin taskbar
pub fn default_specs(spec: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = spec.iter().map(|s| s.to_string()).collect();
    for (name, _) in pinned_lnks() {
        let s = format!("lnk:{}", name);
        if !v.iter().any(|x| x.eq_ignore_ascii_case(&s)) { v.push(s); }
    }
    v
}

pub fn pins_from_specs(specs: &[String]) -> Vec<Pin> {
    let lnks = pinned_lnks();
    let mut pins = Vec::new();
    for s in specs {
        if let Some(n) = s.strip_prefix("lnk:") {
            if let Some((name, p)) = lnks.iter().find(|(name, _)| name.eq_ignore_ascii_case(n)) {
                pins.push(lnk_pin(name, p));
            }
        } else if let Some(a) = s.strip_prefix("app:") {
            pins.push(app_pin(a));
        } else if let Some(e) = s.strip_prefix("exe:") {
            pins.push(exe_pin(e));
        }
    }
    let mut seen = HashSet::new();
    pins.retain(|p| seen.insert(p.key.clone()));
    pins
}

/// Pin baru dari aplikasi yang sedang terbuka
pub fn pin_from_item(it: &AppItem) -> Pin {
    match it.launch.strip_prefix("shell:AppsFolder\\") {
        Some(aumid) => app_pin(aumid),
        None => exe_pin(&it.launch),
    }
}

fn lnk_pin(name: &str, p: &PathBuf) -> Pin {
    let ps = p.to_string_lossy().to_string();
    let key = if name.eq_ignore_ascii_case("File Explorer") {
        explorer_key()
    } else {
        resolve_lnk(&ps).map(|t| t.to_lowercase()).unwrap_or_else(|| ps.to_lowercase())
    };
    Pin { key, launch: ps.clone(), icon_src: ps, spec: format!("lnk:{}", name) }
}

fn resolve_lnk(path: &str) -> Option<String> {
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let pf: IPersistFile = sl.cast().ok()?;
        pf.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let mut buf = [0u16; 520];
        sl.GetPath(&mut buf, null_mut(), 0).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let s = String::from_utf16_lossy(&buf[..n]);
        if s.is_empty() { None } else { Some(s) }
    }
}

pub(crate) fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if ok { Some(String::from_utf16_lossy(&buf[..len as usize])) } else { None }
    }
}

/// AUMID untuk aplikasi Store / paket MSIX (WhatsApp, Teams, Claude, dll.)
fn process_aumid(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let r = GetApplicationUserModelId(h, &mut len, PWSTR(buf.as_mut_ptr()));
        let _ = CloseHandle(h);
        if r == ERROR_SUCCESS && len > 1 { Some(String::from_utf16_lossy(&buf[..len as usize - 1])) } else { None }
    }
}

unsafe extern "system" fn child_cb(h: HWND, lp: LPARAM) -> BOOL {
    let data = &mut *(lp.0 as *mut (u32, u32));
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if pid != 0 && pid != data.0 { data.1 = pid; return FALSE; }
    TRUE
}

pub fn identify(h: HWND) -> Option<Ident> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 { return None; }
        let mut path = process_path(pid)?;
        // aplikasi UWP dibungkus ApplicationFrameHost: cari proses aslinya
        if path.to_lowercase().ends_with("applicationframehost.exe") {
            let mut data = (pid, 0u32);
            let _ = EnumChildWindows(h, Some(child_cb), LPARAM(&mut data as *mut _ as isize));
            if data.1 != 0 {
                pid = data.1;
                path = process_path(pid)?;
            }
        }
        if let Some(a) = process_aumid(pid) {
            let p = app_pin(&a);
            return Some(Ident { key: p.key, launch: p.launch, icon_src: p.icon_src });
        }
        Some(Ident { key: path.to_lowercase(), launch: path.clone(), icon_src: path })
    }
}

fn class_name(h: HWND) -> String {
    let mut cls = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut cls) } as usize;
    String::from_utf16_lossy(&cls[..n])
}

/// Jendela yang biasanya tampil di taskbar.
fn is_taskbar_window(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() { return false; }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        let app = ex & WS_EX_APPWINDOW.0 != 0;
        if !app && ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 { return false; }
        let owner = GetWindow(h, GW_OWNER).unwrap_or_default();
        if !app && !owner.0.is_null() { return false; }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, 4);
        if cloaked != 0 { return false; }
        if GetWindowTextLengthW(h) == 0 { return false; }
        !matches!(class_name(h).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
    }
}

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    if is_taskbar_window(h) { v.push(h); }
    TRUE
}

pub fn taskbar_windows() -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe { let _ = EnumWindows(Some(enum_cb), LPARAM(&mut v as *mut _ as isize)); }
    v
}

pub fn is_desktop(h: HWND) -> bool {
    matches!(class_name(h).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// Ikon resolusi tinggi lewat Shell (bisa .exe, .lnk, atau shell:AppsFolder\AUMID)
pub fn load_icon(wic: &IWICImagingFactory, src: &str, px: i32) -> Option<IWICBitmapSource> {
    unsafe {
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(src), None).ok()?;
        // ambil 2x lebih besar, lalu perkecil dengan filter halus -> tepi ikon tajam & mulus
        let big = (px * 2).max(64);
        let hbmp = f.GetImage(SIZE { cx: big, cy: big }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let bmp = decode_hbitmap(wic, hbmp);
        let _ = DeleteObject(hbmp);
        let bmp = bmp?;
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&bmp, px as u32, px as u32, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
        // simpan hasil jadi (bukan rantai converter) agar tidak dihitung ulang tiap gambar
        let done = wic.CreateBitmapFromSource(&conv, WICBitmapCacheOnLoad).ok()?;
        done.cast().ok()
    }
}

/// Piksel ikon (BGRA premultiplied) yang bisa dikirim antar thread
pub type IconPixels = (u32, u32, Vec<u8>);

/// Thread latar belakang untuk memuat ikon Start menu (memuat lewat Shell bisa ~0,3 detik per ikon).
/// Kirim (target, ukuran px) ke `Sender`; hasil dikirim balik lewat channel dan
/// jendela `notify` diberi pesan `msg` agar menggambar ulang.
pub fn spawn_icon_loader(notify: isize, msg: u32) -> (std::sync::mpsc::Sender<(String, i32)>, std::sync::mpsc::Receiver<(String, Option<IconPixels>)>) {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<(String, i32)>();
    let (res_tx, res_rx) = std::sync::mpsc::channel::<(String, Option<IconPixels>)>();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // prioritas rendah: memuat ikon tidak boleh mengganggu aplikasi lain
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) else { return };
        while let Ok((target, px)) = req_rx.recv() {
            let pixels = load_icon(&wic, &target, px).and_then(|src| {
                let (mut w, mut h) = (0u32, 0u32);
                src.GetSize(&mut w, &mut h).ok()?;
                let mut buf = vec![0u8; (w * h * 4) as usize];
                src.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
                Some((w, h, buf))
            });
            if res_tx.send((target, pixels)).is_err() { break; }
            let _ = PostMessageW(HWND(notify as *mut c_void), msg, WPARAM(0), LPARAM(0));
        }
    });
    (req_tx, res_rx)
}

/// Shell kadang memberi alpha biasa, kadang alpha premultiplied: deteksi dari pikselnya.
fn decode_hbitmap(wic: &IWICImagingFactory, hbmp: HBITMAP) -> Option<IWICBitmap> {
    unsafe {
        let probe = wic.CreateBitmapFromHBITMAP(hbmp, None, WICBitmapUseAlpha).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        probe.GetSize(&mut w, &mut h).ok()?;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        probe.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
        // pada data premultiplied, warna tidak pernah lebih besar dari alpha
        let straight = buf.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
        if straight { Some(probe) } else { wic.CreateBitmapFromHBITMAP(hbmp, None, WICBitmapUsePremultipliedAlpha).ok() }
    }
}

/// Semua aplikasi terpasang (sama dengan "All apps" di Start Windows): (nama, target buka/ikon)
pub fn start_apps() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    unsafe {
        let Ok(folder) = SHGetKnownFolderItem::<_, IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return v };
        let Ok(en) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return v };
        loop {
            let mut items = [None];
            let mut got = 0u32;
            if en.Next(&mut items, Some(&mut got)).is_err() || got == 0 { break; }
            let Some(it) = items[0].take() else { break };
            let name = it.GetDisplayName(SIGDN_NORMALDISPLAY).ok().map(|p| { let s = p.to_string().unwrap_or_default(); CoTaskMemFree(Some(p.0 as *const c_void)); s });
            let parse = it.GetDisplayName(SIGDN_PARENTRELATIVEPARSING).ok().map(|p| { let s = p.to_string().unwrap_or_default(); CoTaskMemFree(Some(p.0 as *const c_void)); s });
            if let (Some(n), Some(p)) = (name, parse) {
                // lewati shortcut dokumentasi/uninstaller yang biasa ikut terdaftar
                let l = n.to_lowercase();
                if n.is_empty() || l.contains("uninstall") || l.ends_with(" help") || l.contains("readme") || p.starts_with("http") { continue; }
                v.push((n, format!("shell:AppsFolder\\{}", p)));
            }
        }
    }
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    v.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    v
}

/// Jalankan program dengan argumen (mis. shutdown.exe /r /t 0)
pub fn run(exe: &str, args: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(exe), &HSTRING::from(args), PCWSTR::null(), SW_HIDE);
    }
}

pub fn launch(target: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn activate(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() { let _ = ShowWindow(h, SW_RESTORE); }
        SwitchToThisWindow(h, true);
    }
}

pub fn minimize(h: HWND) { unsafe { let _ = ShowWindow(h, SW_MINIMIZE); } }

pub fn close(h: HWND) { unsafe { let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)); } }
