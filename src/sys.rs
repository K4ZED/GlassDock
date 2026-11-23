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
