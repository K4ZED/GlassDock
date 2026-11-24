// ============================================================
//  GlassDock (Rust) - dock kaca di tengah bawah, pengganti taskbar
//  Win32 + Direct2D + DirectWrite langsung, tanpa framework.
//
//  [ Start | aplikasi pin & yang terbuka | ^ | Wi-Fi  baterai | jam ]
//
//  Klik ikon        : buka / pindah ke jendela / minimize
//  Klik tengah ikon : buka jendela baru
//  Klik kanan       : menu (tutup jendela, tampilkan taskbar asli, keluar)
//  Klik ^           : munculkan taskbar asli sementara (untuk ikon tray)
//  Klik Wi-Fi/baterai -> Quick Settings, klik jam -> Notifikasi
//
//  Taskbar Windows disembunyikan selama dock berjalan dan
//  dikembalikan otomatis saat dock ditutup.
// ============================================================
#![windows_subsystem = "windows"]

mod apps;
mod osd;
mod panels;
mod sys;
mod update;

use panels::PanelKind;

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::c_void,
    sync::atomic::{AtomicU32, Ordering::Relaxed},
};
use sys::{Battery, Net};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Gdi::*,
            Imaging::*,
        },
        System::{
            Com::*,
            LibraryLoader::GetModuleHandleW,
            SystemInformation::GetLocalTime,
            Threading::CreateMutexW,
        },
        UI::{
            Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
            HiDpi::*,
            Input::KeyboardAndMouse::*,
            WindowsAndMessaging::*,
        },
    },
};

// ---------------- PENGATURAN (silakan ubah) ----------------
const DOCK_H: f32 = 56.0; // tinggi dock (DIP)
const MARGIN_B: f32 = 10.0; // jarak dari bawah layar
const SHADOW_M: f32 = 14.0; // ruang bayangan di sekeliling dock
const RADIUS: f32 = 20.0; // kelengkungan sudut
const CELL: f32 = 44.0; // lebar satu ikon
const ICON: f32 = 32.0; // ukuran gambar ikon
const PAD: f32 = 8.0;
const HIDE_TASKBAR: bool = true;
const FONT: PCWSTR = w!("Segoe UI Variable Display");
const GREEN: u32 = 0x34C759;
const YELLOW: u32 = 0xFFCC00;
const RED: u32 = 0xFF3B30;

// Urutan pin. "lnk:<nama>" = shortcut di folder pin taskbar,
// "app:<AppID>" = aplikasi Store (lihat AppID dengan: Get-StartApps)
const PINS: &[&str] = &[
    "lnk:File Explorer",
    "app:4DF9E0F8.Netflix_mcm4njqhnhss8!Netflix.App",
    "lnk:Google Chrome",
    "app:MSTeams_8wekyb3d8bbwe!MSTeams",
    "app:Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows",
    "app:5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App",
    "lnk:CapCut",
    "lnk:Word",
    "lnk:Excel",
    "app:Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
    "app:Claude_pzs8sxrjxfjjc!Claude",
    "app:TelegramMessengerLLP.TelegramDesktop_t4vj0pshhgkwm!Telegram.TelegramDesktop.Store",
    "lnk:openttd",
];
// -----------------------------------------------------------

const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_TICK: usize = 1;
const TIMER_REFRESH: usize = 2;
const TIMER_ANIM: usize = 3;
const RING_R: f32 = 12.5; // jari-jari lingkaran Wi-Fi + baterai
static SHELLHOOK_MSG: AtomicU32 = AtomicU32::new(0);
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);
static AUTOHIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
const TIMER_HOVER: usize = 4; // cek posisi kursor (sembunyi otomatis)
const TIMER_SLIDE: usize = 5; // animasi geser naik/turun
const HIDE_DELAY_MS: u128 = 700; // jeda sebelum dock turun setelah kursor pergi
thread_local! { static APP: RefCell<Option<App>> = RefCell::new(None); }

fn col(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F { D2D1_COLOR_F { r, g, b, a } }
fn hex(h: u32) -> D2D1_COLOR_F {
    col(((h >> 16) & 255) as f32 / 255.0, ((h >> 8) & 255) as f32 / 255.0, (h & 255) as f32 / 255.0, 1.0)
}
fn white(a: f32) -> D2D1_COLOR_F { col(1.0, 1.0, 1.0, a) }
fn rc(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F { D2D_RECT_F { left: l, top: t, right: r, bottom: b } }
fn pt(x: f32, y: f32) -> D2D_POINT_2F { D2D_POINT_2F { x, y } }
fn rrect(r: D2D_RECT_F, rad: f32) -> D2D1_ROUNDED_RECT { D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad } }
fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool { x >= r.left && x < r.right && y >= r.top && y < r.bottom }

#[derive(Clone, Copy, PartialEq)]
enum Hit { Start, App(usize), Status, Clock }

// ================= Gaya kaca =================
#[derive(Clone, Copy, PartialEq)]
enum Style { Clear, Frosted, Dark }

struct Look {
    accent: i32,    // 0 = tanpa blur (benar-benar bening), 4 = acrylic (blur)
    tint: u32,      // 0xAABBGGRR (hanya untuk acrylic)
    base: f32,      // lapisan gelap tipis agar teks putih terbaca
    fill: (f32, f32),  // kilau putih atas -> bawah
    rim: (f32, f32),   // tepi berkilau atas -> bawah
    shine: f32,     // pantulan cahaya di setengah atas
    depth: f32,     // bayangan dalam di tepi bawah (ilusi ketebalan)
    shadow: f32,    // bayangan teks agar terbaca di latar terang
    blur: Option<D2D1_COLOR_F>, // Some = latar diburamkan sendiri + lapisan warna ini (Frosted/Dark)
}

impl Style {
    fn look(self) -> Look {
        match self {
            // Liquid Glass bening: tanpa blur, latar terlihat jelas, tepi & pantulan berkilau
            Style::Clear => Look { accent: 0, tint: 0, base: 0.28, fill: (0.16, 0.05), rim: (0.90, 0.22), shine: 0.18, depth: 0.30, shadow: 0.60, blur: None },
            // Kaca buram (blur Acrylic)
            // (dulu memakai acrylic Windows, tapi itu selalu berbentuk kotak penuh termasuk area bayangan)
            Style::Frosted => Look { accent: 0, tint: 0, base: 0.0, fill: (0.10, 0.02), rim: (0.65, 0.14), shine: 0.09, depth: 0.18, shadow: 0.35, blur: Some(col(0.10, 0.10, 0.12, 0.40)) },
            // Kaca gelap (pekat)
            Style::Dark => Look { accent: 0, tint: 0, base: 0.0, fill: (0.08, 0.01), rim: (0.40, 0.08), shine: 0.05, depth: 0.0, shadow: 0.0, blur: Some(col(0.06, 0.06, 0.07, 0.74)) },
        }
    }
    fn name(self) -> &'static str { match self { Style::Clear => "clear", Style::Frosted => "frosted", Style::Dark => "dark" } }
    fn from_name(s: &str) -> Style { match s.trim() { "frosted" => Style::Frosted, "dark" => Style::Dark, _ => Style::Clear } }
}

/// Lapisan kaca bersama untuk dock & panel: warna dasar, kilau, pantulan, ketebalan, tepi berkilau.
/// `min_base` = lapisan gelap minimum (panel butuh lebih gelap agar teks panjang tetap terbaca)
fn draw_glass(rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, w: f32, h: f32, radius: f32, lk: &Look, min_base: f32) -> Result<()> {
    unsafe {
        let body = rrect(rc(0.0, 0.0, w, h), radius);
        let base = lk.base.max(if lk.accent == 0 { min_base } else { 0.0 });
        if base > 0.0 {
            br.SetColor(&col(0.0, 0.0, 0.0, base));
            rt.FillRoundedRectangle(&body, br);
        }
        rt.FillRoundedRectangle(&body, &App::gradient(rt, 0.0, h, lk.fill.0, lk.fill.1)?);
        // pantulan cahaya: pita terang di bagian atas yang memudar
        if lk.shine > 0.0 {
            let bh = (h * 0.55).min(60.0);
            let band = rrect(rc(3.0, 2.0, w - 3.0, bh), radius - 3.0);
            rt.FillRoundedRectangle(&band, &App::gradient(rt, 2.0, bh, lk.shine, 0.0)?);
        }
        // ketebalan kaca: bayangan dalam tipis di tepi bawah
        if lk.depth > 0.0 {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: col(0.0, 0.0, 0.0, 0.0) },
                D2D1_GRADIENT_STOP { position: 1.0, color: col(0.0, 0.0, 0.0, lk.depth) },
            ];
            let sc = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let dark = rt.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(0.0, (h - 30.0).max(h * 0.6)), endPoint: pt(0.0, h) }, None, &sc)?;
            let inner = rrect(rc(1.5, 1.5, w - 1.5, h - 1.5), radius - 1.5);
            rt.DrawRoundedRectangle(&inner, &dark, 2.0, None);
        }
        // tepi berkilau (specular rim): terang di atas, redup di bawah
        let edge = rrect(rc(0.5, 0.5, w - 0.5, h - 0.5), radius - 0.5);
        rt.DrawRoundedRectangle(&edge, &App::gradient(rt, 0.0, h, lk.rim.0, lk.rim.1)?, 1.0, None);
        // garis kilau kedua di dalam (efek pembiasan di tepi)
        let edge2 = rrect(rc(1.5, 1.5, w - 1.5, h - 1.5), radius - 1.5);
        rt.DrawRoundedRectangle(&edge2, &App::gradient(rt, 0.0, (h * 0.5).min(40.0), lk.rim.0 * 0.35, 0.0)?, 1.0, None);
        Ok(())
    }
}

// ================= Pengaturan tersimpan (%APPDATA%\GlassDock) =================
fn cfg_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("GlassDock").join(name)
}
fn cfg_read(name: &str) -> Option<String> { std::fs::read_to_string(cfg_path(name)).ok() }
/// Tulis aman: ke file sementara dulu lalu ganti, jadi file tidak pernah setengah tertulis
fn cfg_write(name: &str, text: &str) {
    let f = cfg_path(name);
    if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    let tmp = f.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &f).is_ok() { return; }
    let _ = std::fs::write(&f, text);
}

// ================= Kalau crash: catat penyebabnya & kembalikan taskbar =================
static ORIG_WORK: std::sync::Mutex<Option<[i32; 4]>> = std::sync::Mutex::new(None);

/// Tambahkan satu baris bertanggal ke %TEMP%\glassdock-crash.log
pub(crate) fn log_line(text: &str) {
    let t = unsafe { GetLocalTime() };
    let line = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} {text}\n", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    let path = std::env::temp_dir().join("glassdock-crash.log");
    let mut prev = std::fs::read_to_string(&path).unwrap_or_default();
    // jangan tumbuh tanpa batas: di atas 64 KB, simpan separuh terakhir saja
    if prev.len() > 64 * 1024 {
        let cut = prev.len() - 32 * 1024;
        let cut = prev[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
        prev = prev[cut..].to_string();
    }
    let _ = std::fs::write(&path, prev + &line);
}
fn install_crash_handler() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{} GlassDock crash: {info}\n", unsafe { let t = GetLocalTime(); format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond) });
        let path = std::env::temp_dir().join("glassdock-crash.log");
        let prev = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::write(&path, prev + &msg);
        // jangan biarkan pengguna tanpa taskbar
        let orig = ORIG_WORK.lock().ok().and_then(|g| *g);
        if let Some([l, t, r, b]) = orig { restore_system(RECT { left: l, top: t, right: r, bottom: b }); } else { set_taskbar_visible(true); }
    }));
}
fn load_style() -> Style { Style::from_name(&cfg_read("style.txt").unwrap_or_default()) }
fn save_style(s: Style) { cfg_write("style.txt", s.name()); }
#[derive(Clone, Copy, PartialEq)]
enum HideMode { Never, Always, Smart }
/// "0" = tidak pernah, "1" = selalu, "smart" = hanya saat ada jendela yang menutupi dock (bawaan)
fn load_hide_mode() -> HideMode {
    match cfg_read("autohide.txt").as_deref().map(str::trim) { Some("0") => HideMode::Never, Some("1") => HideMode::Always, _ => HideMode::Smart }
}
fn save_hide_mode(m: HideMode) { cfg_write("autohide.txt", match m { HideMode::Never => "0", HideMode::Always => "1", HideMode::Smart => "smart" }); }


/// Daftar pin tersimpan di pins.txt (satu baris per aplikasi, urutan = urutan di dock)
fn load_pins() -> Vec<apps::Pin> {
    let specs: Vec<String> = match cfg_read("pins.txt") {
        Some(t) => t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        None => {
            let d = apps::default_specs(PINS);
            cfg_write("pins.txt", &d.join("\n"));
            d
        }
    };
    apps::pins_from_specs(&specs)
}

/// Kanvas 32-bit dengan alpha per piksel untuk UpdateLayeredWindow
struct Surface { dc: HDC, bmp: HBITMAP, old: HGDIOBJ, w: i32, h: i32, bits: *mut u8 }

impl Surface {
    fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(None, screen);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let old = SelectObject(dc, bmp);
            Some(Self { dc, bmp, old, w, h, bits: bits as *mut u8 })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

const WM_APP_PAINT: u32 = WM_APP + 1;

#[derive(Default)]
struct Icon { wic: Option<IWICBitmapSource>, bmp: Option<ID2D1Bitmap>, tried: bool }

struct App {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    wic: IWICImagingFactory,
    f_time: IDWriteTextFormat,
    f_date: IDWriteTextFormat,
    f_bat: IDWriteTextFormat,
    soft: ID2D1StrokeStyle,
    rt: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    surf: Option<Surface>,
    paint_pending: std::cell::Cell<bool>,

    pins: Vec<apps::Pin>,
    items: Vec<apps::AppItem>,
    seen: Vec<String>,
    idents: HashMap<isize, apps::Ident>,
    icons: HashMap<String, Icon>,

    clock: String,
    date: String,
    net: Net,
    bat: Battery,
    wlan: HANDLE,
    ticks: u32,
    anim_bat: f32,
    animating: bool,

    fg: HWND,
    hover: Option<Hit>,
    tracking: bool,
    hits: Vec<(Hit, D2D_RECT_F)>,
    sep_x: f32,
    size_px: (i32, i32),

    taskbar_shown: bool,
    orig_work: RECT,
    hidden_fullscreen: bool,
    show_hook: HWINEVENTHOOK,
    style: Style,

    // sembunyi otomatis
    autohide: bool,
    hide_mode: HideMode,
    overlap: bool,       // mode cerdas: ada jendela yang menutupi area dock
    overlap_tick: u32,
    bg_lum: f32,         // terang rata-rata latar di belakang dock (0..1)
    mem_hist: std::collections::VecDeque<f32>,   // pemakaian RAM 60 detik terakhir (0..1)
    mem: (u64, u64),                             // (terpakai, total) byte
    top_procs: Vec<(String, u64)>,               // aplikasi pemakai RAM terbesar (diisi saat Control Center terbuka)
    geom: (i32, i32, i32, i32), // x, y saat tampil, lebar, tinggi (piksel)
    shown: f32,                 // 0 = tersembunyi, 1 = tampil penuh
    slide_target: f32,
    last_over: std::time::Instant,

    panel: Option<panels::Panel>,
    // cache ikon Start menu (dimuat di thread latar belakang, tetap ada antar pembukaan)
    start_icons: HashMap<String, Option<apps::IconPixels>>,
    icon_requested: std::collections::HashSet<String>,
    icon_loader: Option<(std::sync::mpsc::Sender<(String, i32)>, std::sync::mpsc::Receiver<(String, Option<apps::IconPixels>)>)>,
    // disiapkan di latar belakang sejak dock menyala, agar panel langsung muncul saat diklik
    start_list: Option<Vec<(String, String)>>,
    start_rx: Option<std::sync::mpsc::Receiver<Vec<(String, String)>>>,
    bright_tx: Option<std::sync::mpsc::Sender<f32>>,
    bright_rx: Option<std::sync::mpsc::Receiver<Option<f32>>>,
    bright_val: Option<f32>, // None = layar tidak mendukung pengaturan kecerahan
    radios: sys::Radios,     // status Wi-Fi/Bluetooth asli (dibaca thread radio)
    radio_tx: Option<std::sync::mpsc::Sender<sys::RadioCmd>>,
    radio_rx: Option<std::sync::mpsc::Receiver<sys::Radios>>,
    osd: Option<osd::Osd>,             // indikator volume kaca
    osd_audio: Option<sys::Audio>,     // speaker default untuk tombol volume
    osd_bright_last: Option<u32>,      // kecerahan terakhir dari notifikasi daya (untuk OSD kecerahan)
    upd_tx: Option<std::sync::mpsc::Sender<update::Req>>,
    upd_rx: Option<std::sync::mpsc::Receiver<update::Resp>>,
    update: Option<update::Release>,   // versi baru yang tersedia (kalau ada)
    toast: Option<update::Toast>,      // pemberitahuan update kaca
    menu_items: Vec<panels::MenuItem>, // isi menu klik kanan kaca yang sedang terbuka
    menu_target: Option<Hit>,          // yang diklik kanan (ikon app atau dock)
    dock_bg: Option<(u32, u32, Vec<u8>)>, // latar buram di belakang dock (gaya Frosted/Dark)
    dock_bg_bmp: Option<ID2D1Bitmap>,
    bg_sig: Option<Vec<u8>>,           // sidik strip di atas dock, untuk tahu kapan latar berubah
}

const WM_APP_ICON: u32 = WM_APP + 3;
const WM_APP_START: u32 = WM_APP + 4;
const WM_APP_STARTAPPS: u32 = WM_APP + 5; // daftar aplikasi Start selesai dibaca
const WM_APP_BRIGHT: u32 = WM_APP + 6; // nilai kecerahan dari thread WMI
const WM_APP_RADIO: u32 = WM_APP + 7;  // status radio Wi-Fi/Bluetooth berubah
const WM_APP_POWER: u32 = WM_APP + 8;  // mode daya (Battery saver) berubah
const WM_APP_VOLKEY: u32 = WM_APP + 9; // tombol volume keyboard (wparam = kode tombol)
const WM_APP_UPDATE: u32 = WM_APP + 10; // hasil cek / unduh update
const WM_APP_KBLIGHT: u32 = WM_APP + 11; // lampu keyboard ASUS berubah (wparam = level 0..3)

// ================= Tombol Windows -> Start menu kaca =================
/// penjaga awal yang terus menyembunyikan taskbar lama; dimatikan bila pengguna memilih menampilkannya
static TASKBAR_GUARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static DOCK_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

// ---- watchdog: catat bila thread utama terjebak banjir pesan/event (timer tidak jalan lagi) ----
use std::sync::atomic::{AtomicIsize, AtomicU64};
static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static LAST_TICK_MS: AtomicU64 = AtomicU64::new(0);
static MSG_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_MSG: AtomicU32 = AtomicU32::new(0);
static LAST_MSG_HWND: AtomicIsize = AtomicIsize::new(0);
static SHOW_EVENTS: AtomicU64 = AtomicU64::new(0);
static FG_EVENTS: AtomicU64 = AtomicU64::new(0);
fn now_ms() -> u64 { T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 }

fn spawn_watchdog() {
    now_ms();
    LAST_TICK_MS.store(now_ms(), Relaxed);
    std::thread::spawn(|| {
        let (mut m0, mut s0, mut f0) = (0u64, 0u64, 0u64);
        let mut last_log = 0u64;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let (m, s, f) = (MSG_COUNT.load(Relaxed), SHOW_EVENTS.load(Relaxed), FG_EVENTS.load(Relaxed));
            let (dm, ds, df) = (m - m0, s - s0, f - f0);
            (m0, s0, f0) = (m, s, f);
            let now = now_ms();
            let stale = now.saturating_sub(LAST_TICK_MS.load(Relaxed));
            // timer 1 detik tidak jalan padahal thread sibuk = banjir (setelah sleep laptop, aktivitasnya kecil)
            if stale > 5000 && dm + ds + df > 400 && now.saturating_sub(last_log) > 30_000 {
                last_log = now;
                let h = HWND(LAST_MSG_HWND.load(Relaxed) as *mut c_void);
                log_line(&format!(
                    "UI thread flooded for {}s: {dm} msgs (last 0x{:X} to '{}'), {ds} taskbar-show events, {df} foreground events in 2s",
                    stale / 1000, LAST_MSG.load(Relaxed), window_class(h)
                ));
            }
        }
    });
}

/// WM_PAINT untuk jendela layered (isinya dari UpdateLayeredWindow). Harus BeginPaint/EndPaint:
/// ValidateRect tidak menghapus tanda "internal paint", sehingga WM_PAINT datang terus tanpa henti
/// (thread utama sibuk 100%, timer tidak jalan, panel tak terlihat & input terasa macet).
pub(crate) fn paint_noop(hwnd: HWND) {
    unsafe {
        let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
        let _ = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
        let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
    }
}

// ---- lampu keyboard ASUS (Fn+F4): level dibaca dari registry driver ASUS, OSD ASUS diganti OSD kaca ----
const ASUS_KEY: PCWSTR = w!("SOFTWARE\\ASUS\\ASUS System Control Interface\\AsusOptimization\\ASUS Keyboard Hotkeys");
static KB_LEVEL: AtomicU32 = AtomicU32::new(u32::MAX); // nilai mentah terakhir
static KB_AT: AtomicU64 = AtomicU64::new(0);           // kapan terakhir berubah (now_ms)
static BRIGHT_AT: AtomicU64 = AtomicU64::new(0);       // kapan kecerahan terakhir berubah (now_ms)
static ASUS_OSD_DENIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe fn read_kb_level(key: windows::Win32::System::Registry::HKEY) -> Option<u32> {
    use windows::Win32::System::Registry::*;
    let mut v = 0u32;
    let mut len = 4u32;
    let r = RegQueryValueExW(key, w!("KeybdLightLevel"), None, None, Some(&mut v as *mut u32 as *mut u8), Some(&mut len));
    if r.is_ok() { Some(v) } else { None }
}

/// Sembunyikan OSD bawaan ASUS ("ATKOSD2") yang sedang tampil
unsafe fn hide_asus_osd(h: Option<HWND>) {
    let h = match h { Some(h) => h, None => match FindWindowW(w!("CNewOsd"), PCWSTR::null()) { Ok(h) if !h.0.is_null() => h, _ => return } };
    // di sebagian laptop AsusOSD berjalan sebagai admin -> Windows menolak (akses ditolak); di yang lain berhasil
    if !ShowWindowAsync(h, SW_HIDE).as_bool() {
        if !ASUS_OSD_DENIED.swap(true, Relaxed) { log_line("ASUS OSD runs elevated; it cannot be hidden, so both OSDs may show"); }
        return;
    }
    let _ = SetWindowPos(h, None, -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
}

fn spawn_kb_light_watcher(notify: isize) {
    std::thread::spawn(move || unsafe {
        use windows::Win32::System::Registry::*;
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, ASUS_KEY, 0, KEY_QUERY_VALUE | KEY_NOTIFY, &mut key).is_err() { return; } // bukan laptop ASUS
        let Some(first) = read_kb_level(key) else { return };
        KB_LEVEL.store(first, Relaxed);
        loop {
            if RegNotifyChangeKeyValue(key, false, REG_NOTIFY_CHANGE_LAST_SET, None, false).is_err() { return; }
            let Some(v) = read_kb_level(key) else { continue };
            let old = KB_LEVEL.swap(v, Relaxed);
            // bit tinggi hanya penanda dari driver; level ada di 2 bit bawah (0 = mati .. 3 = paling terang)
            if (old & 0x7F) == (v & 0x7F) { continue; }
            KB_AT.store(now_ms(), Relaxed);
            // beri waktu OSD ASUS muncul dulu, supaya ketahuan bisa disembunyikan atau tidak
            std::thread::sleep(std::time::Duration::from_millis(200));
            hide_asus_osd(None);
            // OSD ASUS tidak bisa disembunyikan (berjalan sebagai admin): biarkan OSD ASUS saja, jangan dobel
            if ASUS_OSD_DENIED.load(Relaxed) { continue; }
            let _ = PostMessageW(HWND(notify as *mut c_void), WM_APP_KBLIGHT, WPARAM((v & 0x7F) as usize), LPARAM(0));
        }
    });
}

/// Jendela lain muncul: OSD ASUS untuk lampu keyboard disembunyikan (OSD kaca yang tampil)
unsafe extern "system" fn on_shell_event(_: HWINEVENTHOOK, ev: u32, h: HWND, obj: i32, _: i32, _: u32, _: u32) {
    if obj != 0 || h.0.is_null() { return; }
    let cls = window_class(h);
    if cls == "CNewOsd" && (ev == EVENT_OBJECT_SHOW || ev == 0x8018) {
        // OSD ASUS juga dipakai tombol lain (touchpad, kamera...): hanya sembunyikan bila lampu keyboard baru berubah
        let fresh = now_ms().saturating_sub(KB_AT.load(Relaxed)) < 1500;
        let changed = {
            use windows::Win32::System::Registry::*;
            let mut key = HKEY::default();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, ASUS_KEY, 0, KEY_QUERY_VALUE, &mut key).is_ok() {
                let v = read_kb_level(key);
                let _ = RegCloseKey(key);
                v.map(|v| (v & 0x7F) != (KB_LEVEL.load(Relaxed) & 0x7F)).unwrap_or(false)
            } else { false }
        };
        if fresh || changed { hide_asus_osd(Some(h)); }
        return;
    }
    // flyout kecerahan bawaan Windows muncul saat kecerahan berubah: sembunyikan (OSD kaca yang tampil)
    if (ev == EVENT_OBJECT_SHOW || ev == 0x8018) && now_ms().saturating_sub(BRIGHT_AT.load(Relaxed)) < 1500 && is_brightness_flyout(h, &cls) {
        let _ = ShowWindowAsync(h, SW_HIDE);
    }
}

/// Flyout kecerahan/volume Windows 11: jendela XAML Explorer kecil di tengah bawah layar
unsafe fn is_brightness_flyout(h: HWND, cls: &str) -> bool {
    if cls != "XamlExplorerHostIslandWindow" { return false; }
    let mut r = RECT::default();
    if GetWindowRect(h, &mut r).is_err() { return false; }
    let (w, ht) = (r.right - r.left, r.bottom - r.top);
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let cx = (r.left + r.right) / 2;
    (100..=520).contains(&w) && (30..=200).contains(&ht) && r.bottom >= sh - 160 && (cx - sw / 2).abs() < 260
}

/// Dipanggil saat notifikasi kecerahan datang: flyout Windows yang sudah telanjur tampil ikut disembunyikan
unsafe fn hide_brightness_flyout() {
    let mut after = HWND::default();
    while let Ok(h) = FindWindowExW(None, after, w!("XamlExplorerHostIslandWindow"), PCWSTR::null()) {
        if h.0.is_null() { break; }
        if IsWindowVisible(h).as_bool() && is_brightness_flyout(h, "XamlExplorerHostIslandWindow") {
            let _ = ShowWindowAsync(h, SW_HIDE);
        }
        after = h;
    }
}

/// Hook keyboard di thread sendiri: kalau thread utama sibuk, keyboard seluruh Windows tidak ikut macet
fn spawn_keyboard_hook(hinst: isize) {
    std::thread::spawn(move || unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), HINSTANCE(hinst as *mut c_void), 0);
        if let Err(e) = &hook { log_line(&format!("keyboard hook failed: {e}")); }
        // event jendela proses lain (OSD ASUS, flyout Windows) juga ditangani di thread ini, bukan thread utama
        let _ = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE, None, Some(on_shell_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        let _ = SetWinEventHook(0x8017, 0x8018, None, Some(on_shell_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() {}
        if let Ok(h) = hook { let _ = UnhookWindowsHookEx(h); }
    });
}
static WIN_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WIN_COMBO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Tombol "penutup" tanpa fungsi (sama seperti yang dipakai AutoHotkey) agar Windows
/// tidak membuka Start bawaan saat tombol Windows dilepas
const VK_MASK: u16 = 0xE8;
/// Penanda pada tombol yang dikirim dock sendiri, agar tidak diproses ulang oleh hook
const OWN_INPUT: usize = 0x4744_4B21; // "GDK!"

unsafe fn key(vk: u16, up: bool, extended: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if extended { flags |= KEYEVENTF_EXTENDEDKEY; }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: OWN_INPUT } },
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if k.dwExtraInfo == OWN_INPUT { return CallNextHookEx(None, code, wp, lp); } // kiriman dock sendiri
        let vk = k.vkCode;
        let down = wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN;
        let is_win = vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32;
        // tombol volume: dock yang mengatur volume & menampilkan OSD kaca (indikator Windows tidak muncul)
        if (0xAD..=0xAF).contains(&vk) && !WIN_DOWN.load(Relaxed) {
            if down { let _ = PostMessageW(HWND(DOCK_HWND.load(Relaxed) as *mut _), WM_APP_VOLKEY, WPARAM(vk as usize), LPARAM(0)); }
            return LRESULT(1);
        }
        if is_win {
            if down {
                if !WIN_DOWN.swap(true, Relaxed) { WIN_COMBO.store(false, Relaxed); }
            } else {
                WIN_DOWN.store(false, Relaxed);
                if !WIN_COMBO.load(Relaxed) {
                    // Ditekan sendirian (bukan Win+E, Win+L, dll.):
                    // tahan "lepas" yang asli, kirim tombol penutup lalu "lepas" pengganti.
                    // Windows menganggap tombol Windows dipakai untuk kombinasi -> Start bawaan tidak muncul.
                    let inputs = [key(VK_MASK, false, false), key(VK_MASK, true, false), key(vk as u16, true, true)];
                    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                    let _ = PostMessageW(HWND(DOCK_HWND.load(Relaxed) as *mut _), WM_APP_START, WPARAM(0), LPARAM(0));
                    return LRESULT(1);
                }
            }
        } else if down && WIN_DOWN.load(Relaxed) {
            WIN_COMBO.store(true, Relaxed);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn text_format(dw: &IDWriteFactory, family: PCWSTR, weight: DWRITE_FONT_WEIGHT, size: f32) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-US"))?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(f)
    }
}

// ================= Taskbar asli & work area =================
fn tray_windows() -> Vec<HWND> {
    let mut v = Vec::new();
    unsafe {
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            if !h.0.is_null() { v.push(h); }
        }
        let mut after = HWND::default();
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_SecondaryTrayWnd"), PCWSTR::null()) {
            if h.0.is_null() { break; }
            v.push(h);
            after = h;
        }
    }
    v
}

fn set_taskbar_visible(show: bool) {
    for h in tray_windows() {
        unsafe { let _ = ShowWindow(h, if show { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
    }
}

fn scale(hwnd: HWND) -> f32 { unsafe { GetDpiForWindow(hwnd).max(96) as f32 / 96.0 } }

fn get_work_area() -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    r
}

fn set_work_area(mut r: RECT) {
    let old = get_work_area();
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut r as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        // umumkan tanpa menunggu (SPIF_SENDCHANGE bisa macet berdetik-detik di aplikasi yang hang)
        let _ = PostMessageW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(SPI_SETWORKAREA.0 as usize), LPARAM(0));
    }
    fit_maximized_windows(old, r);
}

unsafe extern "system" fn zoomed_cb(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    if IsWindowVisible(h).as_bool() && IsZoomed(h).as_bool() { v.push(h); }
    TRUE
}

/// Sesuaikan jendela yang sedang maximize ke work area baru (asinkron, tidak memblokir)
fn fit_maximized_windows(old: RECT, new: RECT) {
    if old == new { return; }
    unsafe {
        let mut v: Vec<HWND> = Vec::new();
        let _ = EnumWindows(Some(zoomed_cb), LPARAM(&mut v as *mut _ as isize));
        for h in v {
            let mut cur = RECT::default();
            if GetWindowRect(h, &mut cur).is_err() { continue; }
            // hanya jendela di monitor ini (yang menempel ke work area lama)
            if (cur.top - old.top).abs() > 16 || (cur.bottom - old.bottom).abs() > 16 { continue; }
            let _ = SetWindowPos(
                h, None,
                new.left + (cur.left - old.left),
                new.top + (cur.top - old.top),
                (new.right + (cur.right - old.right)) - (new.left + (cur.left - old.left)),
                (new.bottom + (cur.bottom - old.bottom)) - (new.top + (cur.top - old.top)),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }
}

fn desired_work_area(hwnd: HWND) -> RECT {
    let s = scale(hwnd);
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        // sembunyi otomatis: dock tidak memakan tempat, jendela boleh sampai bawah layar
        let reserve = if AUTOHIDE.load(Relaxed) { 0 } else { ((DOCK_H + MARGIN_B + 4.0) * s).round() as i32 };
        RECT { left: 0, top: 0, right: w, bottom: h - reserve }
    }
}

unsafe fn send_keys(keys: &[VIRTUAL_KEY]) {
    let mk = |k: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: k, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 },
        },
    };
    let mut v: Vec<INPUT> = keys.iter().map(|k| mk(*k, false)).collect();
    v.extend(keys.iter().rev().map(|k| mk(*k, true)));
    SendInput(&v, std::mem::size_of::<INPUT>() as i32);
}

fn window_class(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn is_fullscreen(h: HWND) -> bool {
    unsafe {
        // jendela maximize bukan fullscreen (fullscreen asli tidak berstatus "zoomed")
        if h.0.is_null() || apps::is_desktop(h) || IsZoomed(h).as_bool() { return false; }
        // jendela sistem layar penuh saat login/transisi (layar selamat datang, Explorer) bukan video/game
        let cls = window_class(h);
        if matches!(cls.as_str(), "Windows.UI.Core.CoreWindow" | "ForegroundStaging" | "XamlExplorerHostIslandWindow" | "Shell_TrayWnd" | "LockScreenControllerProxyWindow") { return false; }        // fullscreen asli (video/game) tidak punya title bar
        let style = GetWindowLongW(h, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == WS_CAPTION.0 { return false; }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() { return false; }
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() { return false; }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

impl App {
    fn new(hwnd: HWND) -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let soft = d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_FLAT,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            Ok(Self {
                hwnd,
                f_time: text_format(&dw, FONT, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.5)?,
                f_date: text_format(&dw, FONT, DWRITE_FONT_WEIGHT_NORMAL, 11.0)?,
                f_bat: text_format(&dw, FONT, DWRITE_FONT_WEIGHT_BOLD, 10.0)?,
                d2d,
                dw,
                wic,
                soft,
                rt: None,
                brush: None,
                surf: None,
                paint_pending: std::cell::Cell::new(false),
                pins: load_pins(),
                items: Vec::new(),
                seen: Vec::new(),
                idents: HashMap::new(),
                icons: HashMap::new(),
                clock: String::new(),
                date: String::new(),
                net: Net::Offline,
                bat: Battery::default(),
                wlan: HANDLE::default(),
                ticks: 0,
                anim_bat: 0.0,
                animating: false,
                fg: GetForegroundWindow(),
                hover: None,
                tracking: false,
                hits: Vec::new(),
                sep_x: 0.0,
                size_px: (0, 0),
                taskbar_shown: !HIDE_TASKBAR,
                orig_work: get_work_area(),
                hidden_fullscreen: false,
                show_hook: HWINEVENTHOOK::default(),
                style: load_style(),
                autohide: load_hide_mode() != HideMode::Never,
                hide_mode: load_hide_mode(),
                overlap: false,
                overlap_tick: 0,
                bg_lum: 0.3,
                mem_hist: std::collections::VecDeque::with_capacity(61),
                mem: (0, 0),
                top_procs: Vec::new(),
                geom: (0, 0, 0, 0),
                shown: 1.0,
                slide_target: 1.0,
                // saat baru dibuka, tampil ~1,5 detik dulu baru turun
                last_over: std::time::Instant::now() + std::time::Duration::from_millis(800),
                panel: None,
                start_icons: HashMap::new(),
                icon_requested: std::collections::HashSet::new(),
                icon_loader: None,
                start_list: None,
                start_rx: None,
                bright_tx: None,
                bright_rx: None,
                radios: sys::Radios::default(),
                radio_tx: None,
                radio_rx: None,
                osd: None,
                osd_audio: None,
                osd_bright_last: None,
                upd_tx: None,
                upd_rx: None,
                update: None,
                toast: None,
                menu_items: Vec::new(),
                menu_target: None,
                dock_bg: None,
                dock_bg_bmp: None,
                bg_sig: None,
                bright_val: None,
            })
        }
    }

    /// Minta gambar ulang (digabung: beberapa perubahan = sekali gambar)
    fn invalidate(&self) {
        if !self.paint_pending.replace(true) {
            unsafe { let _ = PostMessageW(self.hwnd, WM_APP_PAINT, WPARAM(0), LPARAM(0)); }
        }
    }

    fn apply_style(&mut self, s: Style) {
        self.style = s;
        let l = s.look();
        unsafe { sys::enable_acrylic(self.hwnd, l.accent, l.tint); }
        self.size_px = (0, 0); // pasang ulang region sesuai gaya
        self.relayout();
        self.dock_bg = None; // paksa ambil ulang latar buram untuk gaya baru
        self.sample_background(); // latar buram langsung siap untuk Frosted/Dark
    }

    // ================= Data =================
    fn tick(&mut self) {
        LAST_TICK_MS.store(now_ms(), Relaxed);
        self.ticks += 1;
        self.check_fullscreen();
        self.sample_background(); // murah: hanya strip kecil, blur diambil ulang bila latar berubah
        self.sample_memory();
        // taskbar asli kadang muncul lagi (mis. dock lama baru selesai menutup): sembunyikan lagi
        if HIDE_TASKBAR && !self.taskbar_shown {
            for h in tray_windows() {
                unsafe { if IsWindowVisible(h).as_bool() { let _ = ShowWindowAsync(h, SW_HIDE); } }
            }
        }
        let mut changed = self.update_clock();
        // jaga area layar untuk dock (bisa direset explorer atau aplikasi lain)
        if self.ticks % 2 == 0 && !self.taskbar_shown {
            let want = desired_work_area(self.hwnd);
            if get_work_area() != want { set_work_area(want); }
        }
        if self.ticks % 5 == 0 {
            let b = sys::battery();
            let n = sys::network(&mut self.wlan);
            let sensors = b != self.bat || n != self.net;
            changed |= sensors;
            self.bat = b;
            self.net = n;
            if sensors { self.start_anim(); }
        }
        if self.ticks % 3 == 0 { self.refresh_apps(); }
        if changed { self.relayout(); }
    }

    fn update_clock(&mut self) -> bool {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let t = unsafe { GetLocalTime() };
        let clock = format!("{:02}:{:02}", t.wHour, t.wMinute);
        let date = format!("{} {} {}", DAYS[t.wDayOfWeek as usize % 7], t.wDay, MONTHS[(t.wMonth as usize + 11) % 12]);
        let changed = clock != self.clock || date != self.date;
        self.clock = clock;
        self.date = date;
        changed
    }

    fn refresh_apps(&mut self) {
        let wins = apps::taskbar_windows();
        let mut alive = HashMap::new();
        let mut groups: Vec<(apps::Ident, Vec<HWND>)> = Vec::new();
        for w in wins {
            if w == self.hwnd { continue; }
            let id = match self.idents.get(&(w.0 as isize)) {
                Some(i) => i.clone(),
                None => match apps::identify(w) { Some(i) => i, None => continue },
            };
            alive.insert(w.0 as isize, id.clone());
            match groups.iter_mut().find(|(g, _)| g.key == id.key) {
                Some((_, v)) => v.push(w),
                None => groups.push((id, vec![w])),
            }
        }
        self.idents = alive;

        let mut items: Vec<apps::AppItem> = self
            .pins
            .iter()
            .map(|p| apps::AppItem { key: p.key.clone(), launch: p.launch.clone(), icon_src: p.icon_src.clone(), pinned: true, windows: Vec::new() })
            .collect();
        let mut extra: Vec<apps::AppItem> = Vec::new();
        for (id, v) in groups {
            if let Some(it) = items.iter_mut().find(|i| i.key == id.key) {
                it.windows = v;
            } else {
                if !self.seen.contains(&id.key) { self.seen.push(id.key.clone()); }
                extra.push(apps::AppItem { key: id.key, launch: id.launch, icon_src: id.icon_src, pinned: false, windows: v });
            }
        }
        // aplikasi yang tidak di-pin: urutan stabil sesuai kapan pertama terlihat
        extra.sort_by_key(|i| self.seen.iter().position(|k| *k == i.key).unwrap_or(usize::MAX));
        items.extend(extra);

        let same = items.len() == self.items.len()
            && items.iter().zip(&self.items).all(|(a, b)| a.key == b.key && a.windows == b.windows);
        if !same {
            self.items = items;
            self.relayout();
        }
    }

    fn on_foreground(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        if fg == self.hwnd { return; }
        self.fg = fg;
        self.check_fullscreen();
        self.refresh_apps();
        self.invalidate();
    }

    /// Sembunyikan dock saat aplikasi aktif fullscreen (video, game, presentasi)
    fn check_fullscreen(&mut self) {
        let fs = is_fullscreen(unsafe { GetForegroundWindow() });
        if fs != self.hidden_fullscreen {
            self.hidden_fullscreen = fs;
            if fs { log_line(&format!("hidden for fullscreen window: {}", window_class(unsafe { GetForegroundWindow() }))); }
            unsafe { let _ = ShowWindow(self.hwnd, if fs { SW_HIDE } else { SW_SHOWNOACTIVATE }); }
        }
    }

    // ================= Tata letak =================
    fn text_width(&self, text: &str, f: &IDWriteTextFormat) -> f32 {
        unsafe {
            let t: Vec<u16> = text.encode_utf16().collect();
            let Ok(l) = self.dw.CreateTextLayout(&t, f, 1000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }

    fn relayout(&mut self) {
        let top = (DOCK_H - CELL) / 2.0;
        let cell = |x: f32, w: f32| rc(x, top, x + w, top + CELL);
        let mut hits = Vec::new();
        let mut x = PAD;
        hits.push((Hit::Start, cell(x, CELL)));
        x += CELL;
        for i in 0..self.items.len() {
            hits.push((Hit::App(i), cell(x, CELL)));
            x += CELL;
        }
        x += 7.0;
        self.sep_x = x;
        x += 6.0;
        let status_w = RING_R * 2.0 + 22.0;
        hits.push((Hit::Status, cell(x, status_w)));
        x += status_w;
        let cw = self.text_width(&self.clock, &self.f_time).max(self.text_width(&self.date, &self.f_date)) + 16.0;
        hits.push((Hit::Clock, cell(x, cw)));
        x += cw + PAD;
        self.hits = hits;
        self.apply_geometry(x);
        self.invalidate();
    }

    /// Jendela = badan dock + margin bayangan di sekelilingnya
    fn apply_geometry(&mut self, width: f32) {
        let s = scale(self.hwnd);
        let m = (SHADOW_M * s).round() as i32;
        let (bw, bh) = ((width * s).round() as i32, (DOCK_H * s).round() as i32);
        let (w, h) = (bw + 2 * m, bh + 2 * m);
        if (w, h) == self.size_px { return; }
        self.size_px = (w, h);
        unsafe {
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let x = (sw - bw) / 2 - m;
            let y = sh - bh - (MARGIN_B * s).round() as i32 - m;
            self.geom = (x, y, w, h);
            self.place();
            if self.style.look().accent == 0 {
                // tanpa blur: bentuk bulat & bayangan dari alpha per piksel (tepi anti-aliased)
                SetWindowRgn(self.hwnd, HRGN::default(), true);
            } else {
                // blur Acrylic perlu dipotong region agar tidak bocor di sudut
                let r2 = (RADIUS * 2.0 * s).round() as i32;
                let rgn = CreateRoundRectRgn(m, m, m + bw + 1, m + bh + 1, r2, r2);
                SetWindowRgn(self.hwnd, rgn, true);
            }
        }
    }

    /// Kotak badan dock (tanpa margin bayangan) saat tampil, dalam piksel layar
    pub(crate) fn body(&self) -> (i32, i32, i32, i32) {
        let m = (SHADOW_M * scale(self.hwnd)).round() as i32;
        let (x, y, w, h) = self.geom;
        (x + m, y + m, (w - 2 * m).max(0), (h - 2 * m).max(0))
    }

    /// Gaya kaca saat ini; untuk gaya bening, lapisan gelap & bayangan teks menyesuaikan terang latar
    pub(crate) fn look(&self) -> Look {
        let mut lk = self.style.look();
        if lk.blur.is_none() {
            let t = ((self.bg_lum - 0.30) / 0.45).clamp(0.0, 1.0);
            lk.base = 0.24 + 0.36 * t;   // latar terang -> kaca lebih gelap agar teks putih terbaca
            lk.shadow = 0.5 + 0.35 * t;
        }
        lk
    }

    /// Pantau latar dock tiap detik.
    /// - Terang latar diukur dari strip tipis tepat DI ATAS dock (tidak pernah berisi dock),
    ///   jadi tidak perlu trik apa pun dan perekam layar tidak terganggu.
    /// - Gaya Frosted/Dark: latar buram diambil ulang hanya bila strip itu berubah (atau belum ada).
    ///   Saat dock tampil, dock ditandai "jangan direkam" sesaat (jarang terjadi); saat tersembunyi tidak perlu.
    fn sample_background(&mut self) {
        let (x, y, w, h) = self.body();
        if w == 0 { return; }
        let s = scale(self.hwnd);
        let strip_h = (10.0 * s).round().max(2.0) as i32;
        let strip_y = (y - (14.0 * s).round() as i32).max(0);
        let sig: Vec<u8> = unsafe {
            let Some(surf) = Surface::new(48, 2) else { return };
            let screen = GetDC(None);
            SetStretchBltMode(surf.dc, HALFTONE);
            let ok = StretchBlt(surf.dc, 0, 0, 48, 2, screen, x, strip_y, w, strip_h, SRCCOPY).as_bool();
            ReleaseDC(None, screen);
            if !ok || surf.bits.is_null() { return; }
            std::slice::from_raw_parts(surf.bits, 48 * 2 * 4).to_vec()
        };
        let mut sum = 0.0f32;
        for p in sig.chunks_exact(4) { sum += 0.0722 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.2126 * p[2] as f32; }
        let lum = sum / (48.0 * 2.0 * 255.0);
        if (lum - self.bg_lum).abs() > 0.04 { self.bg_lum = lum; self.invalidate(); }

        if self.style.look().blur.is_none() {
            if self.dock_bg.is_some() { self.dock_bg = None; self.dock_bg_bmp = None; }
            self.bg_sig = None;
            return;
        }
        let changed = match &self.bg_sig {
            Some(old) => old.iter().zip(&sig).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs()).sum::<u32>() > sig.len() as u32 * 3,
            None => true,
        };
        self.bg_sig = Some(sig);
        if self.dock_bg.is_some() && !changed { return; }
        let on_screen = self.shown > 0.0;
        unsafe {
            if on_screen {
                let _ = SetWindowDisplayAffinity(self.hwnd, WDA_EXCLUDEFROMCAPTURE);
                let _ = windows::Win32::Graphics::Dwm::DwmFlush();
            }
            let new_bg = panels::capture_blur_ex(x, y, w, h, false);
            if on_screen { let _ = SetWindowDisplayAffinity(self.hwnd, WDA_NONE); }
            if new_bg.is_some() {
                self.dock_bg = new_bg;
                self.dock_bg_bmp = None;
                self.invalidate();
            }
        }
    }
    /// Ambil & buramkan layar di (x, y, w, h) TANPA jendela GlassDock sendiri (dock, OSD, pemberitahuan).
    /// Dipakai saat panel/OSD/pemberitahuan muncul (sekali per kemunculan, bukan terus-menerus).
    pub(crate) fn capture_behind(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(u32, u32, Vec<u8>)> {
        let mut ours = vec![self.hwnd];
        if let Some(h) = self.osd_hwnd() { ours.push(h); }
        if let Some(h) = self.toast_hwnd() { ours.push(h); }
        unsafe {
            for h in &ours { let _ = SetWindowDisplayAffinity(*h, WDA_EXCLUDEFROMCAPTURE); }
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
            let bg = panels::capture_blur_ex(x, y, w, h, true);
            for h in &ours { let _ = SetWindowDisplayAffinity(*h, WDA_NONE); }
            bg
        }
    }

    /// RAM dicatat tiap detik sejak dock menyala, jadi grafik di Control Center langsung terisi
    fn sample_memory(&mut self) {
        let (used, total) = sys::memory();
        if total == 0 { return; }
        self.mem = (used, total);
        self.mem_hist.push_back(used as f32 / total as f32);
        while self.mem_hist.len() > 60 { self.mem_hist.pop_front(); }
        let cc_open = self.panel.as_ref().map(|p| p.kind == PanelKind::Control).unwrap_or(false);
        if cc_open {
            // daftar aplikasi cukup diperbarui tiap 2 detik, dan hanya saat panel terbuka
            if self.ticks % 2 == 0 { self.top_procs = sys::top_processes(3); }
            self.refresh_panel();
        }
    }

    /// Mode cerdas: apakah ada jendela (tidak di-minimize) yang menutupi area dock
    fn windows_overlap_dock(&self) -> bool {
        let (x, y, w, h) = self.body();
        unsafe {
            for win in apps::taskbar_windows() {
                if win == self.hwnd || IsIconic(win).as_bool() { continue; }
                let mut r = RECT::default();
                // batas jendela yang terlihat (GetWindowRect ikut menghitung bingkai tak terlihat 7-8 px)
                if DwmGetWindowAttribute(win, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut c_void, std::mem::size_of::<RECT>() as u32).is_err() {
                    let _ = GetWindowRect(win, &mut r);
                }
                if r.left < x + w && r.right > x && r.top < y + h && r.bottom > y - 6 { return true; }
            }
        }
        false
    }

    // ================= Sembunyi otomatis =================
    /// Pasang posisi jendela sesuai `shown` (0 = di bawah layar, 1 = tampil)
    fn place(&self) {
        let (x, y, w, h) = self.geom;
        if w == 0 { return; }
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let t = self.shown.clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3); // ease-out: cepat lalu melambat
        let hidden_y = sh + 2;
        let yy = hidden_y + ((y - hidden_y) as f32 * eased).round() as i32;
        unsafe { let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, yy, w, h, SWP_NOACTIVATE); }
    }

    fn slide_to(&mut self, target: f32) {
        if (self.slide_target - target).abs() < f32::EPSILON && (self.shown - target).abs() < 0.001 { return; }
        self.slide_target = target;
        unsafe { SetTimer(self.hwnd, TIMER_SLIDE, 15, None); }
    }

    fn slide_step(&mut self) {
        let step = if self.slide_target > self.shown { 0.12 } else { -0.10 }; // naik ~125ms, turun ~150ms
        self.shown += step;
        if (step > 0.0 && self.shown >= self.slide_target) || (step < 0.0 && self.shown <= self.slide_target) {
            self.shown = self.slide_target;
            unsafe { let _ = KillTimer(self.hwnd, TIMER_SLIDE); }
        }
        self.place();
    }

    /// Dipanggil tiap 100 ms: munculkan dock saat kursor di tengah bawah layar,
    /// sembunyikan lagi setelah kursor menjauh
    fn hover_check(&mut self) {
        if self.hide_mode == HideMode::Never || self.hidden_fullscreen { return; }
        let mut p = POINT::default();
        unsafe { let _ = GetCursorPos(&mut p); }
        let (x, y, w, _) = self.body();
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let near_x = p.x >= x - 30 && p.x <= x + w + 30;
        let at_edge = near_x && p.y >= sh - 3;
        let over = near_x && self.slide_target > 0.5 && p.y >= y - 12;
        // mode cerdas: cek tumpang-tindih jendela tiap 300 ms; tidak ada yang menutupi -> dock tetap tampil
        if self.hide_mode == HideMode::Smart {
            self.overlap_tick += 1;
            if self.overlap_tick % 3 == 0 { self.overlap = self.windows_overlap_dock(); }
        }
        let keep = self.hide_mode == HideMode::Smart && !self.overlap;
        let now = std::time::Instant::now();
        if at_edge || over || keep || self.panel.is_some() {
            self.last_over = now;
            self.slide_to(1.0);
        } else if self.slide_target > 0.5 && now > self.last_over && (now - self.last_over).as_millis() > HIDE_DELAY_MS {
            self.hover = None;
            self.slide_to(0.0);
        }
    }

    fn set_hide_mode(&mut self, mode: HideMode) {
        self.hide_mode = mode;
        self.overlap = false;
        save_hide_mode(mode);
        self.set_autohide(mode != HideMode::Never);
    }

    fn set_autohide(&mut self, on: bool) {
        self.autohide = on;
        AUTOHIDE.store(on, Relaxed);
        unsafe {
            if on {
                self.last_over = std::time::Instant::now();
                SetTimer(self.hwnd, TIMER_HOVER, 100, None);
            } else {
                let _ = KillTimer(self.hwnd, TIMER_HOVER);
                self.slide_to(1.0);
            }
        }
        if !self.taskbar_shown { set_work_area(desired_work_area(self.hwnd)); }
    }

    fn write_pins(&self) {
        let text: Vec<&str> = self.pins.iter().map(|p| p.spec.as_str()).collect();
        cfg_write("pins.txt", &text.join("\n"));
    }

    fn save_pins(&mut self) {
        self.write_pins(); // tulis dulu, baru perbarui tampilan
        self.refresh_apps();
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().find(|(_, r)| inside(r, x, y)).map(|(h, _)| *h)
    }

    // ================= Aksi =================
    fn click(&mut self, hit: Hit) {
        unsafe {
            match hit {
                Hit::Start | Hit::Status | Hit::Clock => {
                    let cx = self.hits.iter().find(|(h, _)| *h == hit).map(|(_, r)| (r.left + r.right) / 2.0).unwrap_or(0.0);
                    let kind = match hit { Hit::Start => PanelKind::Start, Hit::Status => PanelKind::Control, _ => PanelKind::Calendar };
                    self.toggle_panel(kind, cx);
                }
                Hit::App(i) => {
                    let Some(it) = self.items.get(i) else { return };
                    if it.windows.is_empty() {
                        apps::launch(&it.launch);
                    } else if it.windows.contains(&self.fg) {
                        if it.windows.len() > 1 {
                            apps::activate(*it.windows.last().unwrap()); // ganti ke jendela berikutnya
                        } else {
                            apps::minimize(self.fg);
                        }
                    } else {
                        apps::activate(it.windows[0]);
                    }
                }
            }
        }
    }

    fn toggle_taskbar(&mut self) {
        self.taskbar_shown = !self.taskbar_shown;
        TASKBAR_GUARD.store(false, Relaxed);
        if self.taskbar_shown {
            restore_system(self.orig_work);
        } else {
            set_taskbar_visible(false);
            set_work_area(desired_work_area(self.hwnd));
        }
        self.invalidate();
    }

    /// Isi menu klik kanan. Nomor perintah sama dengan yang dipakai run_menu_cmd.
    fn build_menu(&self, hit: Option<Hit>) -> Vec<panels::MenuItem> {
        use panels::MenuItem::*;
        let mut m = Vec::new();
        if let Some(Hit::App(i)) = hit {
            let has_win = self.items.get(i).map(|it| !it.windows.is_empty()).unwrap_or(false);
            let pinned = self.items.get(i).map(|it| it.pinned).unwrap_or(false);
            m.push(Action { id: 10, glyph: "\u{E8A7}", label: "Open new window".into(), enabled: true });
            if has_win { m.push(Action { id: 11, glyph: "\u{E711}", label: "Close all windows".into(), enabled: true }); }
            m.push(Sep);
            if pinned {
                m.push(Action { id: 12, glyph: "\u{E77A}", label: "Unpin from dock".into(), enabled: true });
                m.push(Action { id: 14, glyph: "\u{E72B}", label: "Move left".into(), enabled: i > 0 });
                m.push(Action { id: 15, glyph: "\u{E72A}", label: "Move right".into(), enabled: i + 1 < self.pins.len() });
            } else {
                m.push(Action { id: 13, glyph: "\u{E718}", label: "Pin to dock".into(), enabled: true });
            }
            m.push(Sep);
        }
        let hide = match self.hide_mode { HideMode::Smart => 0, HideMode::Always => 1, HideMode::Never => 2 };
        m.push(Segmented { id: 22, title: "Auto-hide", options: ["Smart", "Always", "Never"], selected: hide });
        let style = match self.style { Style::Clear => 0, Style::Frosted => 1, Style::Dark => 2 };
        m.push(Segmented { id: 30, title: "Glass style", options: ["Clear", "Frosted", "Dark"], selected: style });
        m.push(Toggle { id: 20, glyph: "\u{E7F4}", label: "Windows taskbar", on: self.taskbar_shown });
        m.push(Sep);
        match &self.update {
            Some(r) => m.push(Action { id: 40, glyph: "\u{E896}", label: format!("Update to {}", r.version), enabled: true }),
            None => m.push(Action { id: 41, glyph: "\u{E895}", label: "Check for updates".into(), enabled: true }),
        }
        m.push(Action { id: 21, glyph: "\u{E7E8}", label: "Quit GlassDock".into(), enabled: true });
        m
    }

    /// Klik kanan: menu kaca di atas dock, sejajar kursor
    fn context_menu(&mut self, hit: Option<Hit>) {
        self.menu_items = self.build_menu(hit);
        self.menu_target = hit;
        let mut p = POINT::default();
        unsafe { let _ = GetCursorPos(&mut p); }
        let (bx, _, _, _) = self.body();
        let anchor = (p.x - bx) as f32 / scale(self.hwnd);
        if self.panel.as_ref().map(|p| p.kind == PanelKind::Menu).unwrap_or(false) { self.destroy_panel(); }
        self.toggle_panel(PanelKind::Menu, anchor);
    }

    /// Jalankan perintah menu klik kanan
    fn run_menu_cmd(&mut self, cmd: u32, hit: Option<Hit>) {
        match (cmd, hit) {
            (10, Some(Hit::App(i))) => { if let Some(it) = self.items.get(i) { apps::launch(&it.launch); } }
            (11, Some(Hit::App(i))) => { if let Some(it) = self.items.get(i) { for w in &it.windows { apps::close(*w); } } }
            (12, Some(Hit::App(i))) => {
                if i < self.pins.len() { self.pins.remove(i); self.save_pins(); }
            }
            (13, Some(Hit::App(i))) => {
                if let Some(it) = self.items.get(i) { self.pins.push(apps::pin_from_item(it)); self.save_pins(); }
            }
            (14, Some(Hit::App(i))) => {
                if i > 0 && i < self.pins.len() { self.pins.swap(i, i - 1); self.save_pins(); }
            }
            (15, Some(Hit::App(i))) => {
                if i + 1 < self.pins.len() { self.pins.swap(i, i + 1); self.save_pins(); }
            }
            (22, _) => self.set_hide_mode(HideMode::Smart),
            (23, _) => self.set_hide_mode(HideMode::Always),
            (24, _) => self.set_hide_mode(HideMode::Never),
            (20, _) => self.toggle_taskbar(),
            (30..=32, _) => {
                let s = [Style::Clear, Style::Frosted, Style::Dark][(cmd - 30) as usize];
                self.apply_style(s);
                save_style(s);
            }
            (21, _) => unsafe { let _ = PostMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)); },
            (40, _) => self.start_update(),
            (41, _) => self.check_updates_now(),
            _ => {}
        }
    }
    // ================= Gambar =================
    fn create_target(&mut self) -> Result<()> {
        unsafe {
            let dpi = GetDpiForWindow(self.hwnd).max(96) as f32;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, // tanpa driver GPU -> RAM kecil
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: dpi,
                dpiY: dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = self.d2d.CreateDCRenderTarget(&props)?;
            self.brush = Some(rt.CreateSolidColorBrush(&white(1.0), None)?);
            self.rt = Some(rt);
            for ic in self.icons.values_mut() { ic.bmp = None; }
            self.dock_bg_bmp = None; // bitmap milik render target lama
            Ok(())
        }
    }

    fn ensure_icons(&mut self, rt: &ID2D1RenderTarget) {
        let px = (ICON * scale(self.hwnd)).round() as i32;
        for it in &self.items {
            let ic = self.icons.entry(it.key.clone()).or_default();
            if !ic.tried {
                ic.tried = true;
                ic.wic = apps::load_icon(&self.wic, &it.icon_src, px);
            }
            if ic.bmp.is_none() {
                if let Some(w) = &ic.wic {
                    ic.bmp = unsafe { rt.CreateBitmapFromWicBitmap(w, None).ok() };
                }
            }
        }
    }

    fn gradient(rt: &ID2D1RenderTarget, y0: f32, y1: f32, a0: f32, a1: f32) -> Result<ID2D1LinearGradientBrush> {
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: white(a0) },
                D2D1_GRADIENT_STOP { position: 1.0, color: white(a1) },
            ];
            let sc = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            rt.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(0.0, y0), endPoint: pt(0.0, y1) }, None, &sc)
        }
    }

    fn render(&mut self) -> Result<()> {
        unsafe {
            self.paint_pending.set(false);
            let (pw, ph) = self.size_px;
            if pw <= 0 || ph <= 0 { return Ok(()); }
            if self.rt.is_none() { self.create_target()?; }
            if self.surf.as_ref().map(|s| (s.w, s.h) != (pw, ph)).unwrap_or(true) {
                self.surf = Surface::new(pw, ph);
            }
            let Some(surf) = self.surf.as_ref() else { return Ok(()) };
            let dc_rt = self.rt.clone().unwrap();
            dc_rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: pw, bottom: ph })?;
            let surf_dc = surf.dc;
            let rt: ID2D1RenderTarget = dc_rt.cast()?;
            let br = self.brush.clone().unwrap();
            self.ensure_icons(&rt);
            let s = scale(self.hwnd);
            let (w, h) = (pw as f32 / s, ph as f32 / s);

            rt.BeginDraw();
            rt.Clear(Some(&col(0.0, 0.0, 0.0, 0.0))); // transparan -> kaca Acrylic terlihat
            rt.SetTransform(&windows::Foundation::Numerics::Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 }); // transform frame sebelumnya tersimpan di render target

            // bayangan berlapis di luar badan dock: tepi tetap terlihat di atas jendela gelap maupun terang
            let m = SHADOW_M;
            let (bw, bh) = (w - 2.0 * m, h - 2.0 * m);
            let lk = self.look();
            {
                for i in 1..=6 {
                    let e = i as f32 * 1.8;
                    br.SetColor(&col(0.0, 0.0, 0.0, 0.055 * (1.0 - i as f32 / 7.0)));
                    rt.FillRoundedRectangle(&rrect(rc(m - e, m - e + i as f32 * 0.9, m + bw + e, m + bh + e + i as f32 * 0.9), RADIUS + e), &br);
                }
                // garis gelap tipis tepat di luar tepi kaca
                br.SetColor(&col(0.0, 0.0, 0.0, 0.32));
                rt.DrawRoundedRectangle(&rrect(rc(m - 0.5, m - 0.5, m + bw + 0.5, m + bh + 0.5), RADIUS + 0.5), &br, 1.0, None);
            }
            rt.SetTransform(&windows::Foundation::Numerics::Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: m, M32: m });
            let (w, h) = (bw, bh);
            if let Some(tint) = lk.blur {
                // latar buram buatan sendiri, dipotong sudut membulat (bukan kotak acrylic)
                if self.dock_bg_bmp.is_none() {
                    if let Some((iw, ih, data)) = &self.dock_bg {
                        let props = D2D1_BITMAP_PROPERTIES {
                            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                            dpiX: 96.0,
                            dpiY: 96.0,
                        };
                        self.dock_bg_bmp = rt.CreateBitmap(D2D_SIZE_U { width: *iw, height: *ih }, Some(data.as_ptr() as *const c_void), iw * 4, &props).ok();
                    }
                }
                let body = rrect(rc(0.0, 0.0, w, h), RADIUS);
                match (&self.dock_bg_bmp, &self.dock_bg) {
                    (Some(bmp), Some((iw, ih, _))) => {
                        let bb = rt.CreateBitmapBrush(
                            bmp,
                            Some(&D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR }),
                            None,
                        )?;
                        bb.SetTransform(&windows::Foundation::Numerics::Matrix3x2 { M11: w / *iw as f32, M12: 0.0, M21: 0.0, M22: h / *ih as f32, M31: 0.0, M32: 0.0 });
                        rt.FillRoundedRectangle(&body, &bb);
                    }
                    _ => { br.SetColor(&col(0.12, 0.12, 0.14, 0.9)); rt.FillRoundedRectangle(&body, &br); }
                }
                br.SetColor(&tint);
                rt.FillRoundedRectangle(&body, &br);
            }
            draw_glass(&rt, &br, w, h, RADIUS, &lk, 0.0)?;
            // garis tepi dalam 1px yang rata di semua sisi (bukan hanya kilau atas)
            br.SetColor(&white(0.16));
            rt.DrawRoundedRectangle(&rrect(rc(1.0, 1.0, w - 1.0, h - 1.0), RADIUS - 1.0), &br, 1.0, None);

            // sorotan saat kursor di atas
            if let Some(hv) = self.hover {
                if let Some((_, r)) = self.hits.iter().find(|(h2, _)| *h2 == hv) {
                    br.SetColor(&white(0.16));
                    rt.FillRoundedRectangle(&rrect(rc(r.left + 2.0, r.top + 2.0, r.right - 2.0, r.bottom - 2.0), 12.0), &br);
                }
            }

            let hits = self.hits.clone();
            for (hit, r) in &hits {
                let cx = (r.left + r.right) / 2.0;
                let cy = (r.top + r.bottom) / 2.0;
                match hit {
                    Hit::Start => self.draw_start(&rt, &br, cx, cy),
                    Hit::App(i) => self.draw_app(&rt, &br, *i, cx, cy),
                    Hit::Status => {
                        self.draw_status(&rt, &br, cx, cy)?;
                    }
                    Hit::Clock => self.draw_clock(&rt, &br, r)?,
                }
            }

            // pemisah
            br.SetColor(&white(0.22));
            rt.FillRectangle(&rc(self.sep_x, h / 2.0 - 13.0, self.sep_x + 1.0, h / 2.0 + 13.0), &br);

            if let Err(e) = rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.brush = None;
                    for ic in self.icons.values_mut() { ic.bmp = None; }
            self.dock_bg_bmp = None; // bitmap milik render target lama
                    self.invalidate();
                    return Ok(());
                }
                return Err(e);
            }

            // kirim kanvas (dengan alpha per piksel) ke jendela
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let size = SIZE { cx: pw, cy: ph };
            let src = POINT { x: 0, y: 0 };
            UpdateLayeredWindow(self.hwnd, None, None, Some(&size), surf_dc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA)?;
            Ok(())
        }
    }

    fn draw_start(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, cx: f32, cy: f32) {
        unsafe {
            let (s, g) = (8.5, 1.6);
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let x0 = if dx < 0.0 { cx - g / 2.0 - s } else { cx + g / 2.0 };
                let y0 = if dy < 0.0 { cy - g / 2.0 - s } else { cy + g / 2.0 };
                br.SetColor(&white(if dy < 0.0 { 0.98 } else { 0.85 }));
                rt.FillRoundedRectangle(&rrect(rc(x0, y0, x0 + s, y0 + s), 1.6), br);
            }
        }
    }

    fn draw_app(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, i: usize, cx: f32, cy: f32) {
        unsafe {
            let it = &self.items[i];
            let r = rc(cx - ICON / 2.0, cy - ICON / 2.0 - 2.0, cx + ICON / 2.0, cy + ICON / 2.0 - 2.0);
            match self.icons.get(&it.key).and_then(|ic| ic.bmp.clone()) {
                Some(b) => rt.DrawBitmap(&b, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                None => {
                    br.SetColor(&white(0.3));
                    rt.FillRoundedRectangle(&rrect(r, 8.0), br);
                }
            }
            // indikator: titik = terbuka, garis = sedang aktif
            let y = cy + ICON / 2.0 + 4.0;
            if it.windows.contains(&self.fg) {
                br.SetColor(&white(1.0));
                rt.FillRoundedRectangle(&rrect(rc(cx - 7.0, y - 1.5, cx + 7.0, y + 1.5), 1.5), br);
            } else if !it.windows.is_empty() {
                br.SetColor(&white(0.65));
                rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, y), radiusX: 2.0, radiusY: 2.0 }, br);
            }
        }
    }

    fn draw_chevron(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, cx: f32, cy: f32) -> Result<()> {
        unsafe {
            let d = if self.taskbar_shown { -1.0 } else { 1.0 }; // ^ atau v
            let g = self.d2d.CreatePathGeometry()?;
            let s = g.Open()?;
            s.BeginFigure(pt(cx - 5.0, cy + 2.5 * d), D2D1_FIGURE_BEGIN_HOLLOW);
            s.AddLine(pt(cx, cy - 2.5 * d));
            s.AddLine(pt(cx + 5.0, cy + 2.5 * d));
            s.EndFigure(D2D1_FIGURE_END_OPEN);
            s.Close()?;
            br.SetColor(&white(0.85));
            rt.DrawGeometry(&g, br, 1.7, &self.soft);
            Ok(())
        }
    }

    /// Busur searah jarum jam pada lingkaran (cx, cy, r), sudut diukur dari arah jam 12
    fn arc_path(&self, cx: f32, cy: f32, r: f32, from: f32, sweep: f32) -> Result<ID2D1PathGeometry> {
        unsafe {
            let p = |deg: f32| { let a = deg.to_radians(); pt(cx + r * a.sin(), cy - r * a.cos()) };
            let g = self.d2d.CreatePathGeometry()?;
            let s = g.Open()?;
            s.BeginFigure(p(from), D2D1_FIGURE_BEGIN_HOLLOW);
            s.AddArc(&D2D1_ARC_SEGMENT {
                point: p(from + sweep),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if sweep > 180.0 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            s.EndFigure(D2D1_FIGURE_END_OPEN);
            s.Close()?;
            Ok(g)
        }
    }

    /// Indikator gabungan ala "iPhone Duo":
    ///  - ikon Wi-Fi di tengah (jumlah pita = kekuatan sinyal)
    ///  - busur di sekelilingnya = isi baterai (jam 8 -> lewat atas -> jam 4)
    ///  - angka persen baterai di celah bawah lingkaran
    /// Warna busur: hijau = dicas, kuning = hemat baterai, merah = di bawah 20%
    fn draw_status(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, cx: f32, cy: f32) -> Result<()> {
        unsafe {
            const START: f32 = 240.0; // arah jam 8
            const SPAN: f32 = 240.0; // sampai arah jam 4
            const W: f32 = 2.0; // tebal busur
            let b = self.bat;

            // jalur redup (bagian baterai yang kosong)
            let track = self.arc_path(cx, cy, RING_R, START, SPAN)?;
            br.SetColor(&white(0.22));
            rt.DrawGeometry(&track, br, W, &self.soft);

            // bagian terisi
            let level = if b.present { self.anim_bat } else { 1.0 };
            if level > 0.005 {
                let color = if !b.present { white(1.0) } else if b.plugged { hex(GREEN) } else if b.saver { hex(YELLOW) } else if b.pct <= 20 { hex(RED) } else { white(1.0) };
                let fill = self.arc_path(cx, cy, RING_R, START, SPAN * level.min(1.0))?;
                br.SetColor(&color);
                rt.DrawGeometry(&fill, br, W, &self.soft);
            }

            // ikon jaringan di tengah (sedikit ke atas, memberi ruang untuk angka)
            if self.net == Net::Ethernet {
                br.SetColor(&white(1.0));
                rt.FillRoundedRectangle(&rrect(rc(cx - 4.0, cy - 6.0, cx + 4.0, cy - 0.5), 1.0), br);
                for k in 0..3 {
                    let lx = cx - 3.2 + k as f32 * 2.6;
                    rt.FillRectangle(&rc(lx, cy - 0.5, lx + 1.2, cy + 2.0), br);
                }
            } else {
                let bars = match self.net { Net::Wifi(q) if q > 75 => 3, Net::Wifi(q) if q >= 40 => 2, Net::Wifi(_) => 1, _ => 0 };
                self.draw_fan(rt, br, cx, cy + 2.2, 0.62, bars)?;
            }

            // angka persen baterai di celah bawah (ikut beranimasi)
            if b.present {
                let shown = (self.anim_bat * 100.0).round() as u32;
                let t: Vec<u16> = format!("{}%", shown).encode_utf16().collect();
                let (bw, bh) = (RING_R * 2.0 + 16.0, 12.0);
                let l = self.dw.CreateTextLayout(&t, &self.f_bat, bw, bh)?;
                // tanda % dibuat lebih kecil supaya angkanya menonjol
                let digits = t.len() as u32 - 1;
                let _ = l.SetFontSize(7.0, DWRITE_TEXT_RANGE { startPosition: digits, length: 1 });
                let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_RANGE { startPosition: digits, length: 1 });
                let (x, y) = (cx - bw / 2.0, cy + RING_R - 2.0 - bh / 2.0);
                let sh = self.look().shadow;
                if sh > 0.0 {
                    br.SetColor(&col(0.0, 0.0, 0.0, sh));
                    rt.DrawTextLayout(pt(x, y + 0.8), &l, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                br.SetColor(&white(1.0));
                rt.DrawTextLayout(pt(x, y), &l, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            Ok(())
        }
    }

    /// Ikon kipas Wi-Fi ala iOS (3 pita), `by` = titik bawah kipas, `k` = skala
    fn draw_fan(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, cx: f32, by: f32, k: f32, bars: u32) -> Result<()> {
        unsafe {
            let p = |r: f32, deg: f32| pt(cx + r * deg.to_radians().cos(), by - r * deg.to_radians().sin());
            let arc = |to: D2D_POINT_2F, r: f32, cw: bool| D2D1_ARC_SEGMENT {
                point: to,
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: if cw { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            // (radius dalam, radius luar)
            let rings = [(0.0f32, 3.9f32 * k), (5.9 * k, 8.6 * k), (10.6 * k, 13.3 * k)];
            for (i, (ri, ro)) in rings.iter().enumerate() {
                let g = self.d2d.CreatePathGeometry()?;
                let s = g.Open()?;
                s.BeginFigure(p(*ro, 135.0), D2D1_FIGURE_BEGIN_FILLED);
                s.AddArc(&arc(p(*ro, 45.0), *ro, true));
                if *ri == 0.0 {
                    s.AddLine(pt(cx, by));
                } else {
                    s.AddLine(p(*ri, 45.0));
                    s.AddArc(&arc(p(*ri, 135.0), *ri, false));
                }
                s.EndFigure(D2D1_FIGURE_END_CLOSED);
                s.Close()?;
                br.SetColor(&white(if bars as usize > i { 1.0 } else { 0.3 }));
                rt.FillGeometry(&g, br, None);
                rt.DrawGeometry(&g, br, 0.7, &self.soft); // sudut sedikit membulat seperti iOS
            }
            Ok(())
        }
    }

    // ================= Animasi busur baterai =================
    fn ring_target(&self) -> f32 {
        // mode demo untuk melihat busur sebagian: set GLASSDOCK_DEMO=1
        if std::env::var("GLASSDOCK_DEMO").is_ok() { return 0.57; }
        self.bat.pct as f32 / 100.0
    }

    fn start_anim(&mut self) {
        if !self.animating {
            self.animating = true;
            unsafe { SetTimer(self.hwnd, TIMER_ANIM, 16, None); }
        }
    }

    fn anim_step(&mut self) {
        let tb = self.ring_target();
        self.anim_bat += (tb - self.anim_bat) * 0.14;
        if (tb - self.anim_bat).abs() < 0.003 {
            self.anim_bat = tb;
            self.animating = false;
            unsafe { let _ = KillTimer(self.hwnd, TIMER_ANIM); }
        }
        self.invalidate();
    }

    fn draw_clock(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, r: &D2D_RECT_F) -> Result<()> {
        unsafe {
            let w = r.right - r.left;
            let mid = (r.top + r.bottom) / 2.0;
            let t: Vec<u16> = self.clock.encode_utf16().collect();
            let lt = self.dw.CreateTextLayout(&t, &self.f_time, w, 18.0)?;
            let d: Vec<u16> = self.date.encode_utf16().collect();
            let ld = self.dw.CreateTextLayout(&d, &self.f_date, w, 16.0)?;
            // bayangan teks agar tetap terbaca saat kaca bening di atas latar terang
            let sh = self.look().shadow;
            if sh > 0.0 {
                br.SetColor(&col(0.0, 0.0, 0.0, sh));
                rt.DrawTextLayout(pt(r.left, mid - 16.2), &lt, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
                rt.DrawTextLayout(pt(r.left, mid + 1.3), &ld, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            br.SetColor(&white(1.0));
            rt.DrawTextLayout(pt(r.left, mid - 17.0), &lt, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
            br.SetColor(&white(0.78));
            rt.DrawTextLayout(pt(r.left, mid + 0.5), &ld, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
            Ok(())
        }
    }
}

// ================= Win32 =================
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

unsafe extern "system" fn on_foreground_event(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    FG_EVENTS.fetch_add(1, Relaxed);
    with_app(|a| a.on_foreground());
}

thread_local! {
    /// (awal jendela hitungan, jumlah sembunyikan, jeda sampai)
    static HIDE_RATE: std::cell::Cell<(u64, u32, u64)> = const { std::cell::Cell::new((0, 0, 0)) };
}

/// Windows kadang memunculkan lagi taskbar (mis. saat tombol Start ditekan): sembunyikan lagi
unsafe extern "system" fn on_show_event(_: HWINEVENTHOOK, _: u32, h: HWND, obj: i32, _: i32, _: u32, _: u32) {
    if obj != 0 { return; } // OBJID_WINDOW
    SHOW_EVENTS.fetch_add(1, Relaxed);
    // Explorer bisa terus memunculkan taskbar (mis. setelah laptop bangun dari sleep).
    // Jangan adu cepat tanpa henti: terlalu sering dalam 2 detik -> berhenti sebentar, penjaga detik-an yang lanjut.
    let now = now_ms();
    let (start, n, pause) = HIDE_RATE.with(|c| c.get());
    if now < pause { return; }
    let (start, n) = if now - start > 2000 { (now, 0) } else { (start, n) };
    if n >= 20 {
        log_line("taskbar keeps reappearing; pausing the instant re-hide for 5s");
        HIDE_RATE.with(|c| c.set((now, 0, now + 5000)));
        return;
    }
    HIDE_RATE.with(|c| c.set((start, n + 1, 0)));
    with_app(|a| {
        if !a.taskbar_shown && tray_windows().contains(&h) {
            // async: ShowWindow biasa menunggu explorer (thread utama bisa tertahan)
            let _ = ShowWindowAsync(h, SW_HIDE);
        }
    });
}

unsafe fn hook_taskbar_show(a: &mut App) {
    if !a.show_hook.0.is_null() { let _ = UnhookWinEvent(a.show_hook); }
    let mut pid = 0u32;
    if let Some(t) = tray_windows().first() { GetWindowThreadProcessId(*t, Some(&mut pid)); }
    if pid != 0 {
        a.show_hook = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, None, Some(on_show_event), pid, 0, WINEVENT_OUTOFCONTEXT);
    }
}

fn mouse_dip(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let s = scale(hwnd);
    let x = (lp.0 & 0xffff) as u16 as i16 as f32;
    let y = ((lp.0 >> 16) & 0xffff) as u16 as i16 as f32;
    (x / s - SHADOW_M, y / s - SHADOW_M) // koordinat badan dock (tanpa margin bayangan)
}

fn restore_system(orig: RECT) {
    TASKBAR_GUARD.store(false, std::sync::atomic::Ordering::Relaxed);
    set_work_area(orig);
    for h in tray_windows() {
        // async: ShowWindow biasa menunggu explorer dan bisa macet ~10 detik
        unsafe { let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE); }
    }
    // minta explorer menghitung ulang work area dari taskbar aslinya (aman juga untuk mode auto-hide)
    for h in tray_windows() {
        let mut abd = windows::Win32::UI::Shell::APPBARDATA {
            cbSize: std::mem::size_of::<windows::Win32::UI::Shell::APPBARDATA>() as u32,
            hWnd: h,
            ..Default::default()
        };
        unsafe { windows::Win32::UI::Shell::SHAppBarMessage(windows::Win32::UI::Shell::ABM_WINDOWPOSCHANGED, &mut abd); }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let shell = SHELLHOOK_MSG.load(Relaxed);
        if shell != 0 && msg == shell {
            // jendela dibuat/ditutup/aktif -> segarkan daftar (ditunda sebentar)
            SetTimer(hwnd, TIMER_REFRESH, 150, None);
            return LRESULT(0);
        }
        let created = TASKBAR_CREATED_MSG.load(Relaxed);
        if created != 0 && msg == created {
            // explorer baru di-restart
            with_app(|a| {
                if !a.taskbar_shown {
                    set_taskbar_visible(false);
                    set_work_area(desired_work_area(hwnd));
                }
                hook_taskbar_show(a);
            });
            return LRESULT(0);
        }
        match msg {
            WM_APP_PAINT => {
                with_app(|a| { let _ = a.render(); });
                LRESULT(0)
            }
            WM_APP_START => {
                // tombol Windows ditekan: munculkan dock lalu buka/tutup Start menu kaca
                with_app(|a| {
                    a.last_over = std::time::Instant::now();
                    a.slide_to(1.0);
                    let cx = a.hits.iter().find(|(h, _)| *h == Hit::Start).map(|(_, r)| (r.left + r.right) / 2.0).unwrap_or(30.0);
                    a.toggle_panel(PanelKind::Start, cx);
                });
                LRESULT(0)
            }
            WM_APP_STARTAPPS => { with_app(|a| a.on_start_list()); LRESULT(0) }
            WM_APP_BRIGHT => { with_app(|a| a.on_brightness()); LRESULT(0) }
            WM_APP_RADIO => { with_app(|a| a.on_radios()); LRESULT(0) }
            WM_APP_POWER => { with_app(|a| a.refresh_panel()); LRESULT(0) }
            WM_POWERBROADCAST if wp.0 == 0x8013 /* PBT_POWERSETTINGCHANGE */ && lp.0 != 0 => {
                let ps = &*(lp.0 as *const windows::Win32::System::Power::POWERBROADCAST_SETTING);
                if ps.PowerSetting == GUID::from_u128(0x8ffee2c6_2d01_46be_adb9_398addc5b4ff) && ps.DataLength >= 4 {
                    let pct = std::ptr::read_unaligned(ps.Data.as_ptr() as *const u32);
                    BRIGHT_AT.store(now_ms(), Relaxed);
                    hide_brightness_flyout();
                    with_app(|a| a.on_brightness_change(pct));
                }
                LRESULT(1)
            }
            WM_APP_KBLIGHT => { with_app(|a| a.on_kb_light(wp.0 as u32)); LRESULT(0) }
            WM_APP_VOLKEY => { with_app(|a| a.on_volume_key(wp.0 as u32)); LRESULT(0) }
            WM_APP_UPDATE => { with_app(|a| a.on_update_msg()); LRESULT(0) }
            WM_APP_ICON => {
                // ikon Start menu selesai dimuat di thread latar belakang
                with_app(|a| a.receive_icons());
                LRESULT(0)
            }
            WM_PAINT => {
                paint_noop(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER => {
                if wp.0 == TIMER_HOVER {
                    with_app(|a| a.hover_check());
                } else if wp.0 == TIMER_SLIDE {
                    with_app(|a| a.slide_step());
                } else if wp.0 == TIMER_ANIM {
                    with_app(|a| a.anim_step());
                } else if wp.0 == TIMER_REFRESH {
                    let _ = KillTimer(hwnd, TIMER_REFRESH);
                    with_app(|a| a.refresh_apps());
                } else {
                    with_app(|a| a.tick());
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                let (x, y) = mouse_dip(hwnd, lp);
                with_app(|a| {
                    if !a.tracking {
                        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        let _ = TrackMouseEvent(&mut t);
                        a.tracking = true;
                    }
                    let h = a.hit_at(x, y);
                    if h != a.hover { a.hover = h; a.invalidate(); }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|a| { a.tracking = false; a.hover = None; a.invalidate(); });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_dip(hwnd, lp);
                with_app(|a| if let Some(h) = a.hit_at(x, y) { a.click(h) });
                LRESULT(0)
            }
            WM_MBUTTONUP => {
                let (x, y) = mouse_dip(hwnd, lp);
                with_app(|a| {
                    if let Some(Hit::App(i)) = a.hit_at(x, y) {
                        if let Some(it) = a.items.get(i) { apps::launch(&it.launch); }
                    }
                });
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (x, y) = mouse_dip(hwnd, lp);
                with_app(|a| { let h = a.hit_at(x, y); a.context_menu(h) });
                LRESULT(0)
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                with_app(|a| {
                    a.rt = None;
                    a.brush = None;
                    a.size_px = (0, 0);
                    for ic in a.icons.values_mut() { *ic = Icon::default(); }
                    a.relayout();
                    if !a.taskbar_shown { set_work_area(desired_work_area(hwnd)); }
                });
                LRESULT(0)
            }
            WM_ENDSESSION | WM_DESTROY => {
                let _ = ShowWindow(hwnd, SW_HIDE); // langsung hilang dari layar
                with_app(|a| a.destroy_panel());
                let orig = with_app(|a| a.orig_work).unwrap_or_default();
                restore_system(orig); // taskbar & work area kembali normal
                if msg == WM_DESTROY { PostQuitMessage(0); }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // GetLastError bisa berisi sisa error lama; kosongkan dulu supaya cek 'sudah berjalan' tidak keliru
        windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::WIN32_ERROR(0));
        let _mutex = CreateMutexW(None, true, w!("GlassDock.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS { log_line("launched, but another GlassDock is already running"); return Ok(()); }
        log_line("launched");
        // Dijalankan lewat Scheduled Task saat login: bisa lebih cepat dari desktop Windows.
        // Tunggu taskbar Windows ada dulu (maks. 2 menit) supaya bisa disembunyikan dengan benar.
        let mut waited = 0;
        while FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).map(|h| h.0.is_null()).unwrap_or(true) && waited < 2400 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited += 1;
        }
        if waited > 0 { log_line(&format!("waited {:.1}s for the Windows desktop", waited as f32 / 20.0)); }
        // Sembunyikan taskbar lama SEKARANG (sebelum dock selesai disiapkan), lalu jaga 20 detik:
        // saat login Explorer bisa menampilkannya lagi beberapa kali.
        if HIDE_TASKBAR {
            std::thread::spawn(|| {
                for _ in 0..200 {
                    if !TASKBAR_GUARD.load(Relaxed) { break; }
                    for h in tray_windows() {
                        if IsWindowVisible(h).as_bool() { let _ = ShowWindowAsync(h, SW_HIDE); }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });
        }
        install_crash_handler();
        let w = get_work_area();
        if let Ok(mut g) = ORIG_WORK.lock() { *g = Some([w.left, w.top, w.right, w.bottom]); }
        if let Err(e) = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() { log_line(&format!("CoInitializeEx failed: {e}")); }
        sys::force_dark_menus(); // menu klik kanan selalu gelap

        let hinst = GetModuleHandleW(None)?;
        let class = w!("GlassDock");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let pc = WNDCLASSW {
            lpfnWndProc: Some(panels::panel_wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("GlassPanel"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&pc);
        osd::register_class(hinst.into());
        update::register_class(hinst.into());
        // Saat login, Windows kadang belum siap (grafis/audio/Wi-Fi) dan pembuatan dock gagal.
        // Jangan langsung keluar diam-diam: catat penyebabnya dan coba lagi sampai ~1 menit.
        let mut attempt = 0;
        let hwnd = loop {
            let made = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                class, w!("GlassDock"), WS_POPUP,
                0, 0, 400, 56, None, None, HINSTANCE(hinst.0), None,
            )
            .and_then(|h| match App::new(h) {
                Ok(app) => Ok((h, app)),
                Err(e) => { let _ = DestroyWindow(h); Err(e) }
            });
            match made {
                Ok((h, app)) => { APP.with(|c| *c.borrow_mut() = Some(app)); break h; }
                Err(e) => {
                    attempt += 1;
                    log_line(&format!("startup attempt {attempt} failed: {e}"));
                    if attempt >= 20 { return Err(e); }
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
            }
        };
        if attempt > 0 { log_line(&format!("started after {} retries", attempt)); }
        with_app(|a| {
            AUTOHIDE.store(a.autohide, Relaxed);
            if a.autohide { SetTimer(hwnd, TIMER_HOVER, 100, None); }
            a.apply_style(a.style);
            a.update_clock();
            a.bat = sys::battery();
            a.net = sys::network(&mut a.wlan);
            a.refresh_apps();
            a.relayout();
            a.start_anim(); // cincin terisi dari 0 saat dock muncul
            a.preload(); // siapkan daftar aplikasi, ikon, dan kecerahan di latar belakang
            a.start_updater(); // cek GitHub Releases sekali sehari
            if HIDE_TASKBAR {
                set_taskbar_visible(false);
                set_work_area(desired_work_area(hwnd));
                hook_taskbar_show(a);
            }
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        SHELLHOOK_MSG.store(RegisterWindowMessageW(w!("SHELLHOOK")), Relaxed);
        TASKBAR_CREATED_MSG.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);
        let _ = RegisterShellHookWindow(hwnd);
        SetTimer(hwnd, TIMER_TICK, 1000, None);
        let fg_hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_foreground_event),
            0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        // tombol Windows di keyboard membuka Start menu kaca
        DOCK_HWND.store(hwnd.0 as isize, Relaxed);
        // kecerahan layar berubah (Fn+F5/F6 dll.) -> WM_POWERBROADCAST -> OSD kaca kecerahan
        let bright_guid = GUID::from_u128(0x8ffee2c6_2d01_46be_adb9_398addc5b4ff); // GUID_VIDEO_CURRENT_MONITOR_BRIGHTNESS
        if let Err(e) = windows::Win32::System::Power::RegisterPowerSettingNotification(
            HANDLE(hwnd.0), &bright_guid, windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE,
        ) { log_line(&format!("brightness notification failed: {e}")); }
        spawn_keyboard_hook(hinst.0 as isize);
        spawn_kb_light_watcher(hwnd.0 as isize);
        spawn_watchdog();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            MSG_COUNT.fetch_add(1, Relaxed);
            LAST_MSG.store(msg.message, Relaxed);
            LAST_MSG_HWND.store(msg.hwnd.0 as isize, Relaxed);
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWinEvent(fg_hook);
        with_app(|a| if !a.show_hook.0.is_null() { let _ = UnhookWinEvent(a.show_hook); });
        APP.with(|c| *c.borrow_mut() = None);
        Ok(())
    }
}
