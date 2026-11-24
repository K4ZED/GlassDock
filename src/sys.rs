// Sensor sistem (baterai, Wi-Fi, LAN) + efek kaca Acrylic

use std::{ffi::c_void, ptr::null_mut};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        NetworkManagement::{IpHelper::*, WiFi::*},
        System::{
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
        },
    },
};

#[derive(PartialEq, Clone, Copy, Default)]
pub struct Battery { pub pct: u8, pub plugged: bool, pub saver: bool, pub present: bool }

/// Wifi(q) = kualitas sinyal 0-100
#[derive(PartialEq, Clone, Copy)]
pub enum Net { Wifi(u32), Ethernet, Offline }

pub fn battery() -> Battery {
    let mut s = SYSTEM_POWER_STATUS::default();
    let ok = unsafe { GetSystemPowerStatus(&mut s).is_ok() };
    Battery {
        pct: s.BatteryLifePercent.min(100),
        plugged: s.ACLineStatus == 1,
        saver: s.SystemStatusFlag == 1,
        present: ok && s.BatteryFlag != 128 && s.BatteryLifePercent != 255,
    }
}

pub fn network(wlan: &mut HANDLE) -> Net {
    let (q, _) = wifi_signal(wlan);
    if q >= 0 {
        Net::Wifi(q.clamp(0, 100) as u32)
    } else if ethernet_up() {
        Net::Ethernet
    } else {
        Net::Offline
    }
}

/// Nama jaringan Wi-Fi yang tersambung
pub fn wifi_name(wlan: &mut HANDLE) -> Option<String> {
    let (q, name) = wifi_signal(wlan);
    if q >= 0 && !name.is_empty() { Some(name) } else { None }
}

/// (kualitas sinyal 0-100 atau -1 jika tidak tersambung, nama jaringan)
fn wifi_signal(h: &mut HANDLE) -> (i32, String) {
    unsafe {
        if h.0.is_null() {
            let mut ver = 0u32;
            if WlanOpenHandle(2, None, &mut ver, h) != 0 { *h = HANDLE::default(); return (-1, String::new()); }
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
        if WlanEnumInterfaces(*h, None, &mut list) != 0 { return (-1, String::new()); }
        let n = (*list).dwNumberOfItems as usize;
        let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
        let mut res = (-1, String::new());
        for it in items {
            if it.isState != wlan_interface_state_connected { continue; }
            let mut size = 0u32;
            let mut data: *mut c_void = null_mut();
            if WlanQueryInterface(*h, &it.InterfaceGuid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None) == 0 {
                let attr = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                let a = &attr.wlanAssociationAttributes;
                let len = (a.dot11Ssid.uSSIDLength as usize).min(32);
                res = (a.wlanSignalQuality as i32, String::from_utf8_lossy(&a.dot11Ssid.ucSSID[..len]).to_string());
                WlanFreeMemory(data);
                break;
            }
        }
        WlanFreeMemory(list as *const c_void);
        res
    }
}

// ---- Volume (Core Audio) ----
use windows::Win32::Media::Audio::{eConsole, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, CLSCTX_INPROC_SERVER};

pub struct Audio { ep: Option<IAudioEndpointVolume> }

impl Audio {
    /// Speaker/headset default saat ini
    pub fn open() -> Self {
        let ep = unsafe {
            (|| -> Result<IAudioEndpointVolume> {
                let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
                dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            })()
            .ok()
        };
        Self { ep }
    }
    /// (volume 0..1, mute)
    pub fn get(&self) -> Option<(f32, bool)> {
        let ep = self.ep.as_ref()?;
        unsafe { Some((ep.GetMasterVolumeLevelScalar().ok()?, ep.GetMute().ok()?.as_bool())) }
    }
    pub fn set(&self, v: f32) {
        if let Some(ep) = &self.ep { unsafe { let _ = ep.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null()); } }
    }
    pub fn set_mute(&self, m: bool) {
        if let Some(ep) = &self.ep { unsafe { let _ = ep.SetMute(m, std::ptr::null()); } }
    }
}

// ---- Kecerahan layar laptop (WMI: WmiMonitorBrightness) ----
use windows::Win32::System::Com::{CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE};
use windows::Win32::System::Wmi::*;

pub struct Brightness { svc: IWbemServices }

impl Brightness {
    /// None jika layar tidak mendukung (mis. monitor eksternal)
    pub fn open() -> Option<Self> {
        unsafe {
            let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
            let svc = loc.ConnectServer(&BSTR::from("ROOT\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None).ok()?;
            CoSetProxyBlanket(&svc, 10 /* WINNT */, 0, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE).ok()?;
            let b = Self { svc };
            b.get()?; // pastikan benar-benar didukung
            Some(b)
        }
    }

    fn first(&self, query: &str) -> Option<IWbemClassObject> {
        unsafe {
            let en = self.svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(query), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None).ok()?;
            let mut objs = [None];
            let mut n = 0u32;
            let _ = en.Next(-1, &mut objs, &mut n);
            if n == 0 { return None; }
            objs[0].take()
        }
    }

    /// Kecerahan 0..1
    pub fn get(&self) -> Option<f32> {
        unsafe {
            let obj = self.first("SELECT CurrentBrightness FROM WmiMonitorBrightness")?;
            let mut v = VARIANT::default();
            obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None).ok()?;
            let b = i32::try_from(&v).ok()?;
            Some(b.clamp(0, 100) as f32 / 100.0)
        }
    }

    pub fn set(&self, level: f32) {
        unsafe {
            let _ = (|| -> Option<()> {
                let inst = self.first("SELECT * FROM WmiMonitorBrightnessMethods")?;
                let mut pv = VARIANT::default();
                inst.Get(w!("__PATH"), 0, &mut pv, None, None).ok()?;
                let path = BSTR::try_from(&pv).ok()?;
                let mut cls = None;
                self.svc.GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut cls), None).ok()?;
                let mut in_def = None;
                cls?.GetMethod(w!("WmiSetBrightness"), 0, &mut in_def, std::ptr::null_mut()).ok()?;
                let args = in_def?.SpawnInstance(0).ok()?;
                args.Put(w!("Timeout"), 0, &VARIANT::from(0i32), 0).ok()?;
                args.Put(w!("Brightness"), 0, &VARIANT::from((level.clamp(0.0, 1.0) * 100.0).round() as u8), 0).ok()?;
                self.svc.ExecMethod(&path, &BSTR::from("WmiSetBrightness"), WBEM_GENERIC_FLAG_TYPE(0), None, &args, None, None).ok()?;
                Some(())
            })();
        }
    }
}

fn ethernet_up() -> bool {
    const SKIP: [&str; 8] = ["virtual", "vmware", "hyper-v", "loopback", "vpn", "tap-", "bluetooth", "wsl"];
    unsafe {
        let mut size: u32 = 16 * 1024;
        let mut buf: Vec<u64> = vec![0; size as usize / 8];
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size);
        if r == ERROR_BUFFER_OVERFLOW.0 {
            buf = vec![0; size as usize / 8 + 1];
            r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size);
        }
        if r != 0 { return false; }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            // 6 = IF_TYPE_ETHERNET_CSMACD, 1 = IfOperStatusUp
            if a.IfType == 6 && a.OperStatus.0 == 1 {
                let d = a.Description.to_string().unwrap_or_default().to_lowercase();
                if !SKIP.iter().any(|k| d.contains(k)) { return true; }
            }
            p = a.Next;
        }
        false
    }
}

// ---- Radio Wi-Fi & Bluetooth (WinRT Windows.Devices.Radios) ----
/// None = perangkat tidak punya radio itu
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Radios { pub wifi: Option<bool>, pub bt: Option<bool> }

impl Radios {
    /// "Airplane" = semua radio yang ada sedang mati
    pub fn airplane(&self) -> bool { self.wifi != Some(true) && self.bt != Some(true) && (self.wifi.is_some() || self.bt.is_some()) }
}

/// Perintah ke thread radio: 0 = toggle Wi-Fi, 1 = toggle Bluetooth, 2 = toggle airplane (semua radio)
pub type RadioCmd = u8;

/// Thread khusus pemilik objek Radio. Menjalankan perintah, lalu membaca status tiap 2 detik
/// (juga menangkap perubahan dari luar GlassDock) dan memberi tahu jendela `notify` lewat `msg`.
pub fn spawn_radio_worker(notify: isize, msg: u32) -> (std::sync::mpsc::Sender<RadioCmd>, std::sync::mpsc::Receiver<Radios>) {
    use windows::Devices::Radios::{Radio, RadioKind, RadioState};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<RadioCmd>();
    let (st_tx, st_rx) = std::sync::mpsc::channel::<Radios>();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = Radio::RequestAccessAsync().and_then(|op| op.get());
        let list: Vec<Radio> = Radio::GetRadiosAsync()
            .and_then(|op| op.get())
            .map(|v| (0..v.Size().unwrap_or(0)).filter_map(|i| v.GetAt(i).ok()).collect())
            .unwrap_or_default();
        let find = |k: RadioKind| list.iter().find(|r| r.Kind().map(|x| x == k).unwrap_or(false)).cloned();
        let (wifi, bt) = (find(RadioKind::WiFi), find(RadioKind::Bluetooth));
        let is_on = |r: &Option<Radio>| r.as_ref().and_then(|r| r.State().ok()).map(|s| s == RadioState::On);
        let set = |r: &Option<Radio>, on: bool| {
            if let Some(r) = r { let _ = r.SetStateAsync(if on { RadioState::On } else { RadioState::Off }).and_then(|op| op.get()); }
        };
        let read = || Radios { wifi: is_on(&wifi), bt: is_on(&bt) };
        let mut last: Option<Radios> = None;
        loop {
            match cmd_rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(cmd) => {
                    let now = read();
                    match cmd {
                        0 => set(&wifi, now.wifi != Some(true)),
                        1 => set(&bt, now.bt != Some(true)),
                        _ => { let on = now.airplane(); set(&wifi, on); set(&bt, on); }
                    }
                    last = None; // kirim status baru walau sama
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            let now = read();
            if last != Some(now) {
                last = Some(now);
                if st_tx.send(now).is_err() { break; }
                let _ = PostMessageW(HWND(notify as *mut c_void), msg, WPARAM(0), LPARAM(0));
            }
        }
    });
    (cmd_tx, st_rx)
}

// ---- Battery saver: status asli dari "effective power mode" + saklar langsung ----
static POWER_NOTIFY: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static POWER_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// mode daya efektif terakhir (0 = Battery saver aktif), -1 = belum diketahui
pub static POWER_MODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

unsafe extern "system" fn on_power_mode(mode: windows::Win32::System::Power::EFFECTIVE_POWER_MODE, _ctx: *const c_void) {
    use std::sync::atomic::Ordering::Relaxed;
    POWER_MODE.store(mode.0, Relaxed);
    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(HWND(POWER_NOTIFY.load(Relaxed) as *mut c_void), POWER_MSG.load(Relaxed), WPARAM(0), LPARAM(0));
}

/// Windows memberi tahu setiap kali mode daya berubah (termasuk Battery saver dinyalakan dari luar)
pub fn watch_power_mode(notify: isize, msg: u32) {
    use std::sync::atomic::Ordering::Relaxed;
    POWER_NOTIFY.store(notify, Relaxed);
    POWER_MSG.store(msg, Relaxed);
    unsafe {
        let mut handle: *mut c_void = null_mut();
        let _ = windows::Win32::System::Power::PowerRegisterForEffectivePowerModeNotifications(2 /* V2 */, Some(on_power_mode), None, &mut handle);
    }
}

/// Battery saver aktif? Dari mode daya efektif; cadangan: bendera status daya
pub fn saver_on() -> bool {
    let m = POWER_MODE.load(std::sync::atomic::Ordering::Relaxed);
    if m >= 0 { m == 0 } else { battery().saver }
}

const GUID_ENERGY_SAVER_SUBGROUP: GUID = GUID::from_u128(0xde830923_a562_41af_a086_e3a2c6bad2da);
const GUID_ENERGY_SAVER_THRESHOLD: GUID = GUID::from_u128(0xe69653ca_cf7f_4f05_aa73_cb833fa90ad4);

/// Nyalakan/matikan Battery saver (hanya bisa saat memakai baterai, seperti Quick Settings Windows).
/// Caranya: ambang "nyala otomatis" dijadikan 100% (langsung aktif), lalu dikembalikan ke nilai semula.
/// `saved` = ambang asli pengguna (disimpan pemanggil supaya bisa dikembalikan). Mengembalikan ambang baru.
pub fn set_saver(on: bool, saved: u32) -> Option<u32> {
    use windows::Win32::System::Power::{PowerGetActiveScheme, PowerReadDCValueIndex, PowerSetActiveScheme, PowerWriteDCValueIndex};
    unsafe {
        let mut scheme: *mut GUID = null_mut();
        if PowerGetActiveScheme(None, &mut scheme).0 != 0 || scheme.is_null() { return None; }
        let mut cur = 0u32;
        let _ = PowerReadDCValueIndex(None, Some(scheme), Some(&GUID_ENERGY_SAVER_SUBGROUP), Some(&GUID_ENERGY_SAVER_THRESHOLD), &mut cur);
        let target = if on { 100 } else { saved.min(99) };
        let ok = PowerWriteDCValueIndex(None, scheme, Some(&GUID_ENERGY_SAVER_SUBGROUP), Some(&GUID_ENERGY_SAVER_THRESHOLD), target) == 0
            && PowerSetActiveScheme(None, Some(scheme)).0 == 0;
        let _ = windows::Win32::Foundation::LocalFree(HLOCAL(scheme as *mut c_void));
        if ok { Some(cur) } else { None }
    }
}

// ---- Memori (RAM) ----
/// (terpakai, total) dalam byte
pub fn memory() -> (u64, u64) {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        if GlobalMemoryStatusEx(&mut m).is_ok() { (m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys) } else { (0, 0) }
    }
}

/// Nama yang lebih mudah dikenali untuk proses yang sering muncul
fn friendly_process(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe").trim_end_matches(".EXE");
    match stem.to_lowercase().as_str() {
        "msedgewebview2" => "WebView2 (Edge)".into(),
        "msmpeng" => "Windows Defender".into(),
        "svchost" => "Windows services".into(),
        "code" => "VS Code".into(),
        "claude" => "Claude".into(),
        "chrome" => "Chrome".into(),
        "msedge" => "Edge".into(),
        "explorer" => "File Explorer".into(),
        "dwm" => "Desktop Window Manager".into(),
        "whatsapp.root" => "WhatsApp".into(),
        "ms-teams" => "Teams".into(),
        "olk" => "Outlook".into(),
        _ => {
            let mut c = stem.chars();
            match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => stem.to_string() }
        }
    }
}

/// Aplikasi dengan pemakaian RAM terbesar (private working set, sama seperti kolom Memory di Task Manager),
/// digabung per nama program
pub fn top_processes(n: usize) -> Vec<(String, u64)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                let exe = String::from_utf16_lossy(&e.szExeFile[..len]);
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                    let mut pmc = PROCESS_MEMORY_COUNTERS_EX2 { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32, ..Default::default() };
                    if K32GetProcessMemoryInfo(h, &mut pmc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, pmc.cb).as_bool() {
                        *map.entry(friendly_process(&exe)).or_default() += pmc.PrivateWorkingSetSize as u64;
                    }
                    let _ = CloseHandle(h);
                }
                if Process32NextW(snap, &mut e).is_err() { break; }
            }
        }
        let _ = CloseHandle(snap);
    }
    let mut v: Vec<(String, u64)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}

// ---- Menu klik kanan selalu gelap ----
/// Memakai fungsi uxtheme tanpa nama (ordinal 135 = SetPreferredAppMode, 136 = FlushMenuThemes),
/// cara yang sama dipakai Explorer untuk menu gelap. Mode 2 = ForceDark (selalu gelap,
/// tidak peduli pengaturan tema Windows).
pub fn force_dark_menus() {
    unsafe {
        let Ok(ux) = windows::Win32::System::LibraryLoader::LoadLibraryW(w!("uxtheme.dll")) else { return };
        type SetMode = unsafe extern "system" fn(i32) -> i32;
        type Flush = unsafe extern "system" fn();
        if let Some(f) = GetProcAddress(ux, PCSTR(135usize as *const u8)) {
            let set_mode: SetMode = std::mem::transmute(f);
            set_mode(2);
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136usize as *const u8)) {
            let flush: Flush = std::mem::transmute(f);
            flush();
        }
    }
}

// ---- Acrylic (API tidak terdokumentasi, ada sejak Windows 10) ----
#[repr(C)]
struct AccentPolicy { state: i32, flags: i32, color: u32, anim: i32 }
#[repr(C)]
struct WinCompAttrData { attr: i32, data: *mut c_void, size: usize }
type SetWcaFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> BOOL;

/// state: 3 = blur biasa (lebih bening), 4 = acrylic (blur + tint + noise)
pub unsafe fn enable_acrylic(hwnd: HWND, state: i32, tint: u32) {
    let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
    let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return };
    let set_wca: SetWcaFn = std::mem::transmute(f);
    let mut accent = AccentPolicy { state, flags: 2, color: tint, anim: 0 };
    let mut data = WinCompAttrData {
        attr: 19, // WCA_ACCENT_POLICY
        data: &mut accent as *mut _ as *mut c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    let _ = set_wca(hwnd, &mut data);
}
