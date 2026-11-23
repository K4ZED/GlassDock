// ============================================================
//  Panel kaca yang muncul di atas dock:
//   - Control Center (klik lingkaran Wi-Fi/baterai) - tata letak ala iOS
//   - Calendar       (klik jam)
//   - Start menu     (klik tombol Start / tombol Windows)
//
//  Kaca panel dibuat sendiri: latar di belakang panel diambil, diburamkan,
//  dan warnanya dipertajam, lalu digambar dengan sudut anti-aliased (mulus).
//  Panel muncul & hilang dengan animasi fade + geser.
// ============================================================

use super::*;
use std::{cell::Cell, collections::HashMap, ffi::c_void, sync::mpsc, time::Instant};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::System::{
    Power::{GetSystemPowerStatus, SetSuspendState, SYSTEM_POWER_STATUS},
    Shutdown::LockWorkStation,
    Threading::{AttachThreadInput, GetCurrentThreadId},
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};

pub const WM_APP_CLOSE: u32 = WM_APP + 2;
const PANEL_R: f32 = 26.0;
const GLYPH: PCWSTR = w!("Segoe MDL2 Assets");
const ANIM_TIMER: usize = 1;
const BLUE: u32 = 0x0A84FF;

#[derive(Clone, Copy, PartialEq)]
pub enum PanelKind { Control, Calendar, Start }

#[derive(Clone, Copy, PartialEq)]
pub enum PHit { Conn(u8), BatteryModule, Tile(u8), VBright, VVol, Btn(u8), Prev, Next, App(usize), Power }

#[derive(Clone, Copy, PartialEq)]
enum Slider { Volume, Bright }

pub struct Panel {
    pub hwnd: HWND,
    pub kind: PanelKind,
    rt: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    surf: Option<Surface>,
    size_px: (i32, i32),
    pos: (i32, i32),
    w: f32,
    h: f32,
    hover: Option<PHit>,
    hits: Vec<(PHit, D2D_RECT_F)>,
    pending: Cell<bool>,
    anim: f32,
    closing: bool,
    bg: Option<(u32, u32, Vec<u8>)>,
    bg_bmp: Option<ID2D1Bitmap>,
    f_title: IDWriteTextFormat,
    f_text: IDWriteTextFormat,
    f_small: IDWriteTextFormat,
    f_name: IDWriteTextFormat,
    f_big: IDWriteTextFormat,
    f_glyph: IDWriteTextFormat,
    f_glyph_big: IDWriteTextFormat,
    // Control Center
    audio: Option<sys::Audio>,
    volume: f32,
    brightness: f32,
    drag: Option<Slider>,
    ssid: Option<String>,
    // Calendar
    month: (i32, u32),
    today: (i32, u32, u32),
    // Start
    apps: Vec<(String, String)>,
    filtered: Vec<usize>,
    search: String,
    scroll: usize,
    icons: HashMap<String, ID2D1Bitmap>,
}

// ---------- tanggal ----------
const MONTH_NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
    }
}

/// 0 = Minggu (algoritma Sakamoto)
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32) % 7) as u32
}

fn ease_out(t: f32) -> f32 { 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3) }

// ---------- latar kaca buatan sendiri ----------
/// Blur kotak (box blur) satu arah bolak-balik, pada gambar kecil 3 kanal
fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0f32; buf.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    // horizontal
    for y in 0..h {
        for c in 0..3 {
            let mut acc = 0f32;
            for k in 0..=(2 * r) {
                let x = k.saturating_sub(r).min(w - 1);
                acc += buf[(y * w + x) * 3 + c];
            }
            for x in 0..w {
                tmp[(y * w + x) * 3 + c] = acc * norm;
                let out = x.saturating_sub(r);
                let inn = (x + r + 1).min(w - 1);
                acc += buf[(y * w + inn) * 3 + c] - buf[(y * w + out) * 3 + c];
            }
        }
    }
    // vertikal
    for x in 0..w {
        for c in 0..3 {
            let mut acc = 0f32;
            for k in 0..=(2 * r) {
                let y = k.saturating_sub(r).min(h - 1);
                acc += tmp[(y * w + x) * 3 + c];
            }
            for y in 0..h {
                buf[(y * w + x) * 3 + c] = acc * norm;
                let out = y.saturating_sub(r);
                let inn = (y + r + 1).min(h - 1);
                acc += tmp[(inn * w + x) * 3 + c] - tmp[(out * w + x) * 3 + c];
            }
        }
    }
}

/// Ambil layar di belakang panel, perkecil 4x, buramkan, pertajam warna (efek kaca iPhone).
/// Hasil: gambar kecil BGRA yang nanti diperbesar halus saat digambar.
fn capture_blur(x: i32, y: i32, w: i32, h: i32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let surf = Surface::new(w, h)?;
        let screen = GetDC(None);
        let ok = BitBlt(surf.dc, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT).is_ok();
        ReleaseDC(None, screen);
        if !ok || surf.bits.is_null() { return None; }
        let src = std::slice::from_raw_parts(surf.bits, (w * h * 4) as usize);
        const F: i32 = 4;
        let (sw, sh) = ((w + F - 1) / F, (h + F - 1) / F);
        let mut buf = vec![0f32; (sw * sh * 3) as usize];
        for sy in 0..sh {
            for sx in 0..sw {
                let mut acc = [0f32; 3];
                let mut n = 0f32;
                for dy in 0..F {
                    let yy = sy * F + dy;
                    if yy >= h { break; }
                    for dx in 0..F {
                        let xx = sx * F + dx;
                        if xx >= w { break; }
                        let i = ((yy * w + xx) * 4) as usize;
                        acc[0] += src[i] as f32;
                        acc[1] += src[i + 1] as f32;
                        acc[2] += src[i + 2] as f32;
                        n += 1.0;
                    }
                }
                let o = ((sy * sw + sx) * 3) as usize;
                buf[o] = acc[0] / n;
                buf[o + 1] = acc[1] / n;
                buf[o + 2] = acc[2] / n;
            }
        }
        for _ in 0..3 { box_blur(&mut buf, sw as usize, sh as usize, 3); }
        let mut out = vec![0u8; (sw * sh * 4) as usize];
        for i in 0..(sw * sh) as usize {
            let (b, g, r) = (buf[i * 3], buf[i * 3 + 1], buf[i * 3 + 2]);
            let l = 0.114 * b + 0.587 * g + 0.299 * r;
            let sat = |c: f32| (l + (c - l) * 1.35).clamp(0.0, 255.0) as u8;
            out[i * 4] = sat(b);
            out[i * 4 + 1] = sat(g);
            out[i * 4 + 2] = sat(r);
            out[i * 4 + 3] = 255;
        }
        Some((sw as u32, sh as u32, out))
    }
}

/// Jadikan panel jendela aktif (agar bisa langsung mengetik). Windows kadang menolak jika
/// panel dibuka lewat tombol keyboard; cadangannya: ikut "menumpang" input thread jendela aktif.
fn force_foreground(hwnd: HWND) {
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() { return; }
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me {
            let _ = AttachThreadInput(me, fg_thread, true);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(hwnd);
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

// ---------- posisi elemen Control Center (DIP) ----------
const CC_W: f32 = 316.0;
const CC_H: f32 = 390.0;
fn cc_bright_rect() -> D2D_RECT_F { rc(164.0, 164.0, 226.0, 300.0) }
fn cc_vol_rect() -> D2D_RECT_F { rc(238.0, 164.0, 300.0, 300.0) }

impl App {
    // ================= persiapan di latar belakang =================
    /// Dipanggil sekali saat dock menyala: baca daftar aplikasi Start + ikonnya, dan siapkan WMI kecerahan
    pub fn preload(&mut self) {
        self.refresh_start_list();
        let notify = self.hwnd.0 as isize;
        let (cmd_tx, cmd_rx) = mpsc::channel::<f32>();
        let (val_tx, val_rx) = mpsc::channel::<Option<f32>>();
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let post = || { let _ = PostMessageW(HWND(notify as *mut c_void), WM_APP_BRIGHT, WPARAM(0), LPARAM(0)); };
            let b = sys::Brightness::open();
            let _ = val_tx.send(b.as_ref().and_then(|b| b.get()));
            post();
            // nilai < 0 = minta baca ulang; nilai 0..1 = setel kecerahan (hanya nilai terakhir yang dipakai)
            while let Ok(first) = cmd_rx.recv() {
                let mut set: Option<f32> = None;
                let mut read = false;
                for v in std::iter::once(first).chain(cmd_rx.try_iter()) {
                    if v < 0.0 { read = true } else { set = Some(v) }
                }
                if let Some(b) = &b {
                    if let Some(v) = set { b.set(v); }
                    if read { let _ = val_tx.send(b.get()); post(); }
                }
            }
        });
        self.bright_tx = Some(cmd_tx);
        self.bright_rx = Some(val_rx);
    }

    fn refresh_start_list(&mut self) {
        let notify = self.hwnd.0 as isize;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let _ = tx.send(apps::start_apps());
            let _ = PostMessageW(HWND(notify as *mut c_void), WM_APP_STARTAPPS, WPARAM(0), LPARAM(0));
        });
        self.start_rx = Some(rx);
    }

    pub fn on_start_list(&mut self) {
        let Some(list) = self.start_rx.as_ref().and_then(|r| r.try_recv().ok()) else { return };
        // muat semua ikon di latar belakang (urut abjad, jadi yang terlihat duluan selesai duluan)
        let px = (40.0 * scale(self.hwnd)).round() as i32;
        if self.icon_loader.is_none() {
            self.icon_loader = Some(apps::spawn_icon_loader(self.hwnd.0 as isize, WM_APP_ICON));
        }
        for (_, t) in &list {
            if !self.start_icons.contains_key(t) && self.icon_requested.insert(t.clone()) {
                if let Some((tx, _)) = &self.icon_loader { let _ = tx.send((t.clone(), px)); }
            }
        }
        let start_open = self.panel.as_ref().map(|p| p.kind == PanelKind::Start).unwrap_or(false);
        if start_open {
            if let Some(p) = self.panel.as_mut() { p.apps = list.clone(); }
            self.apply_search_keep_scroll();
            self.panel_invalidate();
        }
        self.start_list = Some(list);
    }

    pub fn on_brightness(&mut self) {
        let mut latest = None;
        if let Some(rx) = &self.bright_rx {
            for v in rx.try_iter() { latest = Some(v); }
        }
        let Some(v) = latest else { return };
        self.bright_val = v;
        if let Some(p) = self.panel.as_mut() {
            if p.kind == PanelKind::Control && p.drag.is_none() {
                p.brightness = v.unwrap_or(0.0);
            }
        }
        self.panel_invalidate();
    }

    // ================= buka / tutup =================
    pub fn toggle_panel(&mut self, kind: PanelKind, anchor_dip: f32) {
        if let Some(p) = &self.panel {
            if p.kind == kind && !p.closing {
                self.close_panel();
                return;
            }
            self.destroy_panel();
        }
        let _ = self.open_panel(kind, anchor_dip);
    }

    /// Tutup dengan animasi
    pub fn close_panel(&mut self) {
        if let Some(p) = self.panel.as_mut() {
            if !p.closing {
                p.closing = true;
                p.drag = None;
                p.hover = None;
                unsafe {
                    let _ = ReleaseCapture();
                    SetTimer(p.hwnd, ANIM_TIMER, 15, None);
                }
            }
        }
    }

    /// Tutup langsung tanpa animasi
    pub fn destroy_panel(&mut self) {
        if let Some(p) = self.panel.take() {
            unsafe { let _ = DestroyWindow(p.hwnd); }
        }
    }

    fn open_panel(&mut self, kind: PanelKind, anchor_dip: f32) -> Result<()> {
        unsafe {
            let s = scale(self.hwnd);
            let (w, h) = match kind {
                PanelKind::Control => (CC_W, CC_H),
                PanelKind::Calendar => (300.0, 338.0),
                PanelKind::Start => (600.0, 560.0),
            };
            let (pw, ph) = ((w * s).round() as i32, (h * s).round() as i32);
            // posisi: di atas dock, sejajar dengan tombol yang diklik
            let (dx, dy, _, _) = self.geom;
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let ax = dx + (anchor_dip * s).round() as i32;
            let left = (ax - pw / 2).clamp(8, sw - pw - 8);
            let top = (dy - ph - (12.0 * s).round() as i32).max(8);

            let text = |weight, size| text_format(&self.dw, FONT, weight, size);
            let mut panel = Panel {
                hwnd: HWND::default(),
                kind,
                rt: None,
                brush: None,
                surf: None,
                size_px: (pw, ph),
                pos: (left, top),
                w,
                h,
                hover: None,
                hits: Vec::new(),
                pending: Cell::new(false),
                anim: 0.0,
                closing: false,
                bg: capture_blur(left, top, pw, ph), // diambil sebelum panel tampil
                bg_bmp: None,
                f_title: text(DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0)?,
                f_text: text(DWRITE_FONT_WEIGHT_SEMI_BOLD, 12.5)?,
                f_small: text(DWRITE_FONT_WEIGHT_NORMAL, 11.0)?,
                f_name: {
                    let f = text(DWRITE_FONT_WEIGHT_NORMAL, 11.0)?;
                    f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                    f
                },
                f_big: text(DWRITE_FONT_WEIGHT_SEMI_BOLD, 20.0)?,
                f_glyph: text_format(&self.dw, GLYPH, DWRITE_FONT_WEIGHT_NORMAL, 16.0)?,
                f_glyph_big: text_format(&self.dw, GLYPH, DWRITE_FONT_WEIGHT_NORMAL, 19.0)?,
                audio: None,
                volume: 0.0,
                brightness: self.bright_val.unwrap_or(0.0),
                drag: None,
                ssid: None,
                month: (2000, 1),
                today: (2000, 1, 1),
                apps: Vec::new(),
                filtered: Vec::new(),
                search: String::new(),
                scroll: 0,
                icons: HashMap::new(),
            };
            let t = GetLocalTime();
            panel.today = (t.wYear as i32, t.wMonth as u32, t.wDay as u32);
            panel.month = (t.wYear as i32, t.wMonth as u32);
            match kind {
                PanelKind::Control => {
                    let a = sys::Audio::open();
                    if let Some((v, _)) = a.get() { panel.volume = v; }
                    panel.audio = Some(a);
                    panel.ssid = sys::wifi_name(&mut self.wlan);
                    // baca ulang kecerahan (mungkin diubah lewat tombol Fn) tanpa menunggu
                    if let Some(tx) = &self.bright_tx { let _ = tx.send(-1.0); }
                }
                PanelKind::Start => {
                    match &self.start_list {
                        Some(l) => panel.apps = l.clone(),
                        None => panel.apps = apps::start_apps(),
                    }
                    panel.filtered = (0..panel.apps.len()).collect();
                }
                PanelKind::Calendar => {}
            }
            if kind == PanelKind::Start { self.refresh_start_list(); } // perbarui diam-diam (aplikasi baru)

            let hinst = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("GlassPanel"), w!(""), WS_POPUP,
                left, top, pw, ph, None, None, HINSTANCE(hinst.0), None,
            )?;
            panel.hwnd = hwnd;
            self.panel = Some(panel);
            self.panel_render(); // anim = 0: masih transparan
            let _ = ShowWindow(hwnd, SW_SHOW);
            force_foreground(hwnd);
            SetTimer(hwnd, ANIM_TIMER, 15, None);
            Ok(())
        }
    }

    pub fn panel_anim_step(&mut self) {
        let Some(p) = self.panel.as_mut() else { return };
        if p.closing {
            p.anim -= 0.16; // ~95 ms
            if p.anim <= 0.0 { self.destroy_panel(); return; }
        } else {
            p.anim += 0.11; // ~140 ms
            if p.anim >= 1.0 {
                p.anim = 1.0;
                unsafe { let _ = KillTimer(p.hwnd, ANIM_TIMER); }
            }
        }
        self.panel_present();
    }

    fn panel_invalidate(&self) {
        if let Some(p) = &self.panel {
            if !p.pending.replace(true) {
                unsafe { let _ = PostMessageW(p.hwnd, WM_APP_PAINT, WPARAM(0), LPARAM(0)); }
            }
        }
    }

    // ================= gambar =================
    pub fn panel_render(&mut self) {
        if let Err(e) = self.panel_render_inner() {
            let p = std::env::temp_dir().join("glassdock-panel.log");
            let prev = std::fs::read_to_string(&p).unwrap_or_default();
            let _ = std::fs::write(&p, format!("{prev}render error: {e:?}\n"));
        }
    }

    /// Tampilkan kanvas panel dengan transparansi & posisi sesuai animasi (tanpa menggambar ulang)
    fn panel_present(&self) {
        let Some(p) = self.panel.as_ref() else { return };
        let Some(surf) = p.surf.as_ref() else { return };
        unsafe {
            let s = scale(self.hwnd);
            let e = ease_out(p.anim);
            let offset = ((1.0 - e) * 14.0 * s).round() as i32;
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: (e * 255.0) as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
            let size = SIZE { cx: p.size_px.0, cy: p.size_px.1 };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT { x: p.pos.0, y: p.pos.1 + offset };
            let _ = UpdateLayeredWindow(p.hwnd, None, Some(&dst), Some(&size), surf.dc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }

    fn panel_render_inner(&mut self) -> Result<()> {
        unsafe {
            let s = scale(self.hwnd);
            let mut look = self.style.look();
            look.base = 0.0;
            let Some(p) = self.panel.as_mut() else { return Ok(()) };
            p.pending.set(false);
            let (pw, ph) = p.size_px;
            if p.rt.is_none() {
                let dpi = 96.0 * s;
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: dpi,
                    dpiY: dpi,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = self.d2d.CreateDCRenderTarget(&props)?;
                p.brush = Some(rt.CreateSolidColorBrush(&white(1.0), None)?);
                p.rt = Some(rt);
            }
            if p.surf.is_none() { p.surf = Surface::new(pw, ph); }
            let Some(surf) = p.surf.as_ref() else { return Ok(()) };
            let dc_rt = p.rt.clone().unwrap();
            dc_rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: pw, bottom: ph })?;
            let rt: ID2D1RenderTarget = dc_rt.cast()?;
            if p.bg_bmp.is_none() {
                if let Some((bw, bh, data)) = &p.bg {
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    p.bg_bmp = rt.CreateBitmap(D2D_SIZE_U { width: *bw, height: *bh }, Some(data.as_ptr() as *const c_void), bw * 4, &props).ok();
                }
            }
            let kind = p.kind;
            let br = p.brush.clone().unwrap();
            let (w, h) = (p.w, p.h);
            let bg = p.bg_bmp.clone().zip(p.bg.as_ref().map(|(bw, bh, _)| (*bw as f32, *bh as f32)));

            if kind == PanelKind::Start { self.start_load_icons(&rt); }

            rt.BeginDraw();
            rt.Clear(Some(&col(0.0, 0.0, 0.0, 0.0)));
            let body = rrect(rc(0.0, 0.0, w, h), PANEL_R);
            // latar buram (diperbesar halus dari gambar kecil), sudut anti-aliased
            match bg {
                Some((bmp, (bw, bh))) => {
                    let bb = rt.CreateBitmapBrush(
                        &bmp,
                        Some(&D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR }),
                        None,
                    )?;
                    bb.SetTransform(&Matrix3x2 { M11: w / bw, M12: 0.0, M21: 0.0, M22: h / bh, M31: 0.0, M32: 0.0 });
                    rt.FillRoundedRectangle(&body, &bb);
                }
                None => {
                    br.SetColor(&col(0.1, 0.1, 0.12, 0.92));
                    rt.FillRoundedRectangle(&body, &br);
                }
            }
            // lapisan gelap tipis agar teks putih terbaca, lalu lapisan kaca
            br.SetColor(&col(0.05, 0.05, 0.07, 0.42));
            rt.FillRoundedRectangle(&body, &br);
            draw_glass(&rt, &br, w, h, PANEL_R, &look, 0.0)?;

            let mut hits = Vec::new();
            let res = match kind {
                PanelKind::Control => self.draw_control(&rt, &br, &mut hits),
                PanelKind::Calendar => self.draw_calendar(&rt, &br, w, &mut hits),
                PanelKind::Start => self.draw_start_menu(&rt, &br, w, h, &mut hits),
            };
            let _ = rt.EndDraw(None, None);
            res?;
            if let Some(p) = self.panel.as_mut() { p.hits = hits; }
            self.panel_present();
            Ok(())
        }
    }

    /// Tulis teks di dalam kotak `r` (rata tengah vertikal), kembalikan lebar teks
    fn ptext(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, align: DWRITE_TEXT_ALIGNMENT, color: D2D1_COLOR_F) -> Result<f32> {
        unsafe {
            let t: Vec<u16> = s.encode_utf16().collect();
            let l = self.dw.CreateTextLayout(&t, f, (r.right - r.left).max(1.0), (r.bottom - r.top).max(1.0))?;
            l.SetTextAlignment(align)?;
            br.SetColor(&color);
            rt.DrawTextLayout(pt(r.left, r.top), &l, br, D2D1_DRAW_TEXT_OPTIONS_CLIP);
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m)?;
            Ok(m.widthIncludingTrailingWhitespace)
        }
    }

    fn hovered(&self, h: PHit) -> bool { self.panel.as_ref().map(|p| p.hover == Some(h)).unwrap_or(false) }

    /// Kotak modul ala iOS (latar kaca terang tipis + garis tepi halus)
    fn module_bg(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, r: D2D_RECT_F, radius: f32, hover: bool) {
        unsafe {
            br.SetColor(&white(if hover { 0.17 } else { 0.11 }));
            rt.FillRoundedRectangle(&rrect(r, radius), br);
            br.SetColor(&white(0.10));
            rt.DrawRoundedRectangle(&rrect(rc(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), radius - 0.5), br, 1.0, None);
        }
    }

    /// Tombol bulat dengan ikon. `on` = warna isi saat aktif
    fn circle_btn(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, cx: f32, cy: f32, rad: f32, glyph: &str, on: Option<D2D1_COLOR_F>, hit: PHit, hits: &mut Vec<(PHit, D2D_RECT_F)>) -> Result<()> {
        unsafe {
            let p = self.panel.as_ref().unwrap();
            let hov = self.hovered(hit);
            match on {
                Some(c) => br.SetColor(&D2D1_COLOR_F { a: if hov { 0.85 } else { 1.0 }, ..c }),
                None => br.SetColor(&white(if hov { 0.28 } else { 0.17 })),
            }
            rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad, radiusY: rad }, br);
            let r = rc(cx - rad, cy - rad, cx + rad, cy + rad);
            let f = if rad >= 26.0 { &p.f_glyph_big } else { &p.f_glyph };
            self.ptext(rt, br, glyph, f, r, DWRITE_TEXT_ALIGNMENT_CENTER, white(1.0))?;
            hits.push((hit, r));
            Ok(())
        }
    }

    // ================= Control Center (ala iOS) =================
    fn draw_control(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, hits: &mut Vec<(PHit, D2D_RECT_F)>) -> Result<()> {
        unsafe {
            let p = self.panel.as_ref().unwrap();

            // --- modul konektivitas: 4 tombol bulat ---
            let m1 = rc(16.0, 16.0, 152.0, 152.0);
            self.module_bg(rt, br, m1, 26.0, false);
            let wifi_on = matches!(self.net, Net::Wifi(_));
            let blue = hex(BLUE);
            self.circle_btn(rt, br, 50.0, 50.0, 25.0, "\u{E701}", if wifi_on { Some(blue) } else { None }, PHit::Conn(0), hits)?;
            self.circle_btn(rt, br, 118.0, 50.0, 25.0, "\u{E702}", None, PHit::Conn(1), hits)?;
            self.circle_btn(rt, br, 50.0, 118.0, 25.0, "\u{E709}", None, PHit::Conn(2), hits)?;
            self.circle_btn(rt, br, 118.0, 118.0, 25.0, "\u{E774}", if self.net == Net::Ethernet { Some(blue) } else { None }, PHit::Conn(3), hits)?;

            // --- modul baterai: cincin besar ---
            let m2 = rc(164.0, 16.0, 300.0, 152.0);
            self.module_bg(rt, br, m2, 26.0, self.hovered(PHit::BatteryModule));
            hits.push((PHit::BatteryModule, m2));
            let b = self.bat;
            let (cx, cy, rr) = (232.0, 72.0, 34.0);
            let color = if !b.present { white(1.0) } else if b.plugged { hex(GREEN) } else if b.saver { hex(YELLOW) } else if b.pct <= 20 { hex(RED) } else { white(1.0) };
            let track = self.arc_path(cx, cy, rr, 225.0, 270.0)?;
            br.SetColor(&white(0.18));
            rt.DrawGeometry(&track, br, 6.0, &self.soft);
            let level = if b.present { b.pct as f32 / 100.0 } else { 1.0 };
            if level > 0.005 {
                let fill = self.arc_path(cx, cy, rr, 225.0, 270.0 * level)?;
                br.SetColor(&color);
                rt.DrawGeometry(&fill, br, 6.0, &self.soft);
            }
            let pct_text = if b.present { format!("{}%", b.pct) } else { "AC".into() };
            self.ptext(rt, br, &pct_text, &p.f_big, rc(cx - 34.0, cy - 14.0, cx + 34.0, cy + 14.0), DWRITE_TEXT_ALIGNMENT_CENTER, white(1.0))?;
            let status = if !b.present { "Plugged in" } else if b.plugged { "Charging" } else if b.saver { "Battery saver" } else { "On battery" };
            self.ptext(rt, br, status, &p.f_small, rc(m2.left, 118.0, m2.right, 140.0), DWRITE_TEXT_ALIGNMENT_CENTER, white(0.75))?;

            // --- dua tile kiri: Night light & Battery saver ---
            for (i, (title, sub, active)) in [("Night light", "Settings", false), ("Battery saver", if b.saver { "On" } else { "Off" }, b.saver)].iter().enumerate() {
                let y = 164.0 + i as f32 * 74.0;
                let r = rc(16.0, y, 152.0, y + 62.0);
                let hit = PHit::Tile(i as u8);
                self.module_bg(rt, br, r, 20.0, self.hovered(hit));
                hits.push((hit, r));
                let (icx, icy) = (16.0 + 31.0, y + 31.0);
                br.SetColor(&if *active { hex(YELLOW) } else { white(0.17) });
                rt.FillEllipse(&D2D1_ELLIPSE { point: pt(icx, icy), radiusX: 18.0, radiusY: 18.0 }, br);
                let fg = if *active { col(0.1, 0.08, 0.0, 1.0) } else { white(1.0) };
                if i == 0 {
                    self.ptext(rt, br, "\u{E708}", &p.f_glyph, rc(icx - 18.0, icy - 18.0, icx + 18.0, icy + 18.0), DWRITE_TEXT_ALIGNMENT_CENTER, fg)?;
                } else {
                    br.SetColor(&fg);
                    rt.DrawRoundedRectangle(&rrect(rc(icx - 8.0, icy - 5.0, icx + 6.0, icy + 5.0), 2.5), br, 1.4, None);
                    rt.FillRectangle(&rc(icx + 7.0, icy - 2.0, icx + 8.6, icy + 2.0), br);
                    rt.FillRectangle(&rc(icx - 5.5, icy - 2.5, icx - 1.0, icy + 2.5), br);
                }
                self.ptext(rt, br, title, &p.f_text, rc(72.0, y + 13.0, 148.0, y + 31.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(1.0))?;
                self.ptext(rt, br, sub, &p.f_small, rc(72.0, y + 31.0, 148.0, y + 48.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(0.65))?;
            }

            // --- slider vertikal tebal: kecerahan & volume ---
            let bright_ok = self.bright_val.is_some();
            self.vslider(rt, br, cc_bright_rect(), p.brightness, "\u{E706}", bright_ok, PHit::VBright, hits)?;
            let vol_glyph = if p.volume < 0.005 { "\u{E74F}" } else { "\u{E767}" };
            self.vslider(rt, br, cc_vol_rect(), p.volume, vol_glyph, p.audio.as_ref().map(|a| a.get().is_some()).unwrap_or(false), PHit::VVol, hits)?;

            // --- baris tombol bulat ---
            for (i, g) in ["\u{E713}", "\u{E722}", "\u{E72E}", "\u{E7E8}"].iter().enumerate() {
                let cx = 16.0 + 31.0 + i as f32 * 74.0;
                self.circle_btn(rt, br, cx, 343.0, 31.0, g, None, PHit::Btn(i as u8), hits)?;
            }
            Ok(())
        }
    }

    /// Slider vertikal ala iOS: kotak tebal, terisi dari bawah, ikon di bawah
    fn vslider(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, r: D2D_RECT_F, value: f32, glyph: &str, enabled: bool, hit: PHit, hits: &mut Vec<(PHit, D2D_RECT_F)>) -> Result<()> {
        unsafe {
            let p = self.panel.as_ref().unwrap();
            let rad = 20.0;
            let shape = rrect(r, rad);
            br.SetColor(&white(0.13));
            rt.FillRoundedRectangle(&shape, br);
            let v = if enabled { value.clamp(0.0, 1.0) } else { 0.0 };
            let top = r.bottom - (r.bottom - r.top) * v;
            if v > 0.0 {
                rt.PushAxisAlignedClip(&rc(r.left, top, r.right, r.bottom), D2D1_ANTIALIAS_MODE_ALIASED);
                br.SetColor(&white(0.95));
                rt.FillRoundedRectangle(&shape, br);
                rt.PopAxisAlignedClip();
            }
            br.SetColor(&white(0.10));
            rt.DrawRoundedRectangle(&rrect(rc(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), rad - 0.5), br, 1.0, None);
            let gr = rc(r.left, r.bottom - 42.0, r.right, r.bottom - 10.0);
            let on_fill = top < gr.top + 12.0;
            let gc = if !enabled { white(0.3) } else if on_fill { col(0.12, 0.12, 0.14, 1.0) } else { white(0.95) };
            self.ptext(rt, br, glyph, &p.f_glyph_big, gr, DWRITE_TEXT_ALIGNMENT_CENTER, gc)?;
            if enabled { hits.push((hit, r)); }
            Ok(())
        }
    }

    // ================= Calendar =================
    fn draw_calendar(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, w: f32, hits: &mut Vec<(PHit, D2D_RECT_F)>) -> Result<()> {
        unsafe {
            let p = self.panel.as_ref().unwrap();
            let pad = 20.0;
            let (ty, tm, td) = p.today;
            self.ptext(rt, br, DAY_NAMES[weekday(ty, tm, td) as usize], &p.f_small, rc(pad, 16.0, w - pad, 32.0), DWRITE_TEXT_ALIGNMENT_LEADING, hex(RED))?;
            self.ptext(rt, br, &format!("{} {}", MONTH_NAMES[(tm - 1) as usize], td), &p.f_title, rc(pad, 32.0, w - pad, 54.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(1.0))?;

            let (y, m) = p.month;
            self.ptext(rt, br, &format!("{} {}", MONTH_NAMES[(m - 1) as usize], y), &p.f_text, rc(pad, 64.0, w - 100.0, 90.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(0.9))?;
            self.circle_btn(rt, br, w - pad - 46.0, 77.0, 13.0, "\u{E76B}", None, PHit::Prev, hits)?;
            self.circle_btn(rt, br, w - pad - 13.0, 77.0, 13.0, "\u{E76C}", None, PHit::Next, hits)?;

            let cw = 37.0;
            let ch = 34.0;
            let gx = (w - cw * 7.0) / 2.0;
            for (i, d) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
                let x = gx + i as f32 * cw;
                self.ptext(rt, br, d, &p.f_small, rc(x, 98.0, x + cw, 116.0), DWRITE_TEXT_ALIGNMENT_CENTER, white(0.5))?;
            }
            let first = weekday(y, m, 1) as i32;
            let dim = days_in_month(y, m) as i32;
            let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
            let pdim = days_in_month(py, pm) as i32;
            for cell in 0..42 {
                let day = cell - first + 1;
                let (num, inside_month) = if day < 1 { (pdim + day, false) } else if day > dim { (day - dim, false) } else { (day, true) };
                let x = gx + (cell % 7) as f32 * cw;
                let yy = 118.0 + (cell / 7) as f32 * ch;
                let r = rc(x, yy, x + cw, yy + ch);
                let is_today = inside_month && (y, m, day as u32) == (ty, tm, td);
                if is_today {
                    br.SetColor(&hex(RED));
                    rt.FillEllipse(&D2D1_ELLIPSE { point: pt(x + cw / 2.0, yy + ch / 2.0), radiusX: 14.5, radiusY: 14.5 }, br);
                }
                let weekend = cell % 7 == 0 || cell % 7 == 6;
                let c = if is_today { white(1.0) } else if !inside_month { white(0.22) } else if weekend { white(0.6) } else { white(0.95) };
                self.ptext(rt, br, &num.to_string(), &p.f_text, r, DWRITE_TEXT_ALIGNMENT_CENTER, c)?;
            }
            Ok(())
        }
    }

    // ================= Start menu =================
    /// Ikon aplikasi yang terlihat: ambil dari cache, atau minta thread latar belakang memuatnya
    fn start_load_icons(&mut self, rt: &ID2D1RenderTarget) {
        let px = (40.0 * scale(self.hwnd)).round() as i32;
        if self.icon_loader.is_none() {
            self.icon_loader = Some(apps::spawn_icon_loader(self.hwnd.0 as isize, WM_APP_ICON));
        }
        let Some(p) = self.panel.as_mut() else { return };
        let wanted: Vec<String> = p.filtered.iter().skip(p.scroll * 6).take(48).map(|&i| p.apps[i].1.clone()).collect();
        for target in wanted {
            if p.icons.contains_key(&target) { continue; }
            match self.start_icons.get(&target) {
                Some(Some((w, h, data))) => unsafe {
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: *w, height: *h }, Some(data.as_ptr() as *const c_void), w * 4, &props) {
                        p.icons.insert(target, b);
                    }
                },
                Some(None) => {}
                None => {
                    if self.icon_requested.insert(target.clone()) {
                        if let Some((tx, _)) = &self.icon_loader { let _ = tx.send((target, px)); }
                    }
                }
            }
        }
    }

    /// Terima ikon yang sudah selesai dimuat thread latar belakang
    pub fn receive_icons(&mut self) {
        let mut got = false;
        if let Some((_, rx)) = &self.icon_loader {
            while let Ok((target, px)) = rx.try_recv() {
                self.start_icons.insert(target, px);
                got = true;
            }
        }
        let start_open = self.panel.as_ref().map(|p| p.kind == PanelKind::Start).unwrap_or(false);
        if got && start_open { self.panel_invalidate(); }
    }

    fn draw_start_menu(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, w: f32, h: f32, hits: &mut Vec<(PHit, D2D_RECT_F)>) -> Result<()> {
        unsafe {
            let p = self.panel.as_ref().unwrap();
            let pad = 20.0;
            // kotak pencarian
            let sb = rc(pad, 20.0, w - pad, 58.0);
            br.SetColor(&white(0.13));
            rt.FillRoundedRectangle(&rrect(sb, 19.0), br);
            br.SetColor(&white(0.22));
            rt.DrawRoundedRectangle(&rrect(rc(sb.left + 0.5, sb.top + 0.5, sb.right - 0.5, sb.bottom - 0.5), 18.5), br, 1.0, None);
            self.ptext(rt, br, "\u{E721}", &p.f_glyph, rc(sb.left + 8.0, sb.top, sb.left + 40.0, sb.bottom), DWRITE_TEXT_ALIGNMENT_CENTER, white(0.7))?;
            let tr = rc(sb.left + 44.0, sb.top, sb.right - 14.0, sb.bottom);
            if p.search.is_empty() {
                self.ptext(rt, br, "Search apps", &p.f_text, tr, DWRITE_TEXT_ALIGNMENT_LEADING, white(0.45))?;
            } else {
                let tw = self.ptext(rt, br, &p.search, &p.f_text, tr, DWRITE_TEXT_ALIGNMENT_LEADING, white(1.0))?;
                br.SetColor(&white(0.85));
                rt.FillRectangle(&rc(tr.left + tw + 1.0, sb.top + 11.0, tr.left + tw + 2.5, sb.bottom - 11.0), br);
            }

            let label = if p.search.is_empty() { "All apps".to_string() } else { format!("Results  \u{00B7}  {}", p.filtered.len()) };
            self.ptext(rt, br, &label, &p.f_text, rc(pad + 4.0, 68.0, w - pad, 92.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(0.75))?;

            // grid aplikasi
            let cols = 6usize;
            let cw = (w - pad * 2.0) / cols as f32;
            let chh = 88.0;
            let gy = 98.0;
            let rows = ((h - 62.0 - gy) / chh).floor() as usize;
            for (slot, &i) in p.filtered.iter().skip(p.scroll * cols).take(rows * cols).enumerate() {
                let x = pad + (slot % cols) as f32 * cw;
                let y = gy + (slot / cols) as f32 * chh;
                let r = rc(x + 2.0, y, x + cw - 2.0, y + chh - 4.0);
                if self.hovered(PHit::App(i)) {
                    br.SetColor(&white(0.15));
                    rt.FillRoundedRectangle(&rrect(r, 14.0), br);
                }
                let (name, target) = &p.apps[i];
                let ir = rc(x + cw / 2.0 - 20.0, y + 8.0, x + cw / 2.0 + 20.0, y + 48.0);
                if let Some(b) = p.icons.get(target) {
                    rt.DrawBitmap(b, Some(&ir), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                } else {
                    br.SetColor(&white(0.10));
                    rt.FillRoundedRectangle(&rrect(ir, 11.0), br);
                }
                self.ptext(rt, br, name, &p.f_name, rc(x + 6.0, y + 52.0, x + cw - 6.0, y + 82.0), DWRITE_TEXT_ALIGNMENT_CENTER, white(0.95))?;
                hits.push((PHit::App(i), r));
            }
            if p.filtered.is_empty() {
                self.ptext(rt, br, "No apps found", &p.f_text, rc(pad, gy + 40.0, w - pad, gy + 70.0), DWRITE_TEXT_ALIGNMENT_CENTER, white(0.6))?;
            }
            // indikator gulir
            let total_rows = (p.filtered.len() + cols - 1) / cols;
            if total_rows > rows {
                let track_h = rows as f32 * chh;
                let th = (track_h * rows as f32 / total_rows as f32).max(24.0);
                let ty = gy + (track_h - th) * (p.scroll as f32 / (total_rows - rows) as f32);
                br.SetColor(&white(0.3));
                rt.FillRoundedRectangle(&rrect(rc(w - 10.0, ty, w - 7.0, ty + th), 1.5), br);
            }

            // baris bawah: nama pengguna + tombol power
            let by = h - 54.0;
            br.SetColor(&white(0.12));
            rt.FillRectangle(&rc(pad, by, w - pad, by + 1.0), br);
            self.ptext(rt, br, "\u{E77B}", &p.f_glyph, rc(pad, by + 11.0, pad + 32.0, by + 43.0), DWRITE_TEXT_ALIGNMENT_CENTER, white(0.9))?;
            let user = std::env::var("USERNAME").unwrap_or_default();
            self.ptext(rt, br, &user, &p.f_text, rc(pad + 38.0, by + 11.0, w - 100.0, by + 43.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(0.9))?;
            self.circle_btn(rt, br, w - pad - 17.0, by + 27.0, 17.0, "\u{E7E8}", None, PHit::Power, hits)?;
            Ok(())
        }
    }

    fn start_rows(&self) -> usize { 4 }

    fn apply_search(&mut self) {
        self.apply_search_keep_scroll();
        if let Some(p) = self.panel.as_mut() { p.scroll = 0; }
    }

    fn apply_search_keep_scroll(&mut self) {
        let Some(p) = self.panel.as_mut() else { return };
        let q = p.search.to_lowercase();
        let mut starts: Vec<usize> = Vec::new();
        let mut contains: Vec<usize> = Vec::new();
        for (i, (name, _)) in p.apps.iter().enumerate() {
            let n = name.to_lowercase();
            if q.is_empty() || n.starts_with(&q) || n.split(' ').any(|wd| wd.starts_with(&q)) { starts.push(i); } else if n.contains(&q) { contains.push(i); }
        }
        starts.extend(contains);
        p.filtered = starts;
        let total_rows = (p.filtered.len() + 5) / 6;
        p.scroll = p.scroll.min(total_rows.saturating_sub(4));
    }

    // ================= input =================
    fn panel_active(&self) -> bool { self.panel.as_ref().map(|p| !p.closing).unwrap_or(false) }

    fn panel_hit(&self, x: f32, y: f32) -> Option<PHit> {
        let p = self.panel.as_ref()?;
        p.hits.iter().find(|(_, r)| inside(r, x, y)).map(|(h, _)| *h)
    }

    fn vslider_value(r: D2D_RECT_F, y: f32) -> f32 { ((r.bottom - y) / (r.bottom - r.top)).clamp(0.0, 1.0) }

    fn apply_slider(&mut self, which: Slider, v: f32) {
        let Some(p) = self.panel.as_mut() else { return };
        match which {
            Slider::Volume => {
                p.volume = v;
                if let Some(a) = &p.audio {
                    a.set(v);
                    a.set_mute(v < 0.005);
                }
            }
            Slider::Bright => {
                p.brightness = v;
                self.bright_val = Some(v);
                // dikirim ke thread WMI: tidak membuat panel tersendat
                if let Some(tx) = &self.bright_tx { let _ = tx.send(v); }
            }
        }
    }

    pub fn panel_mouse_move(&mut self, x: f32, y: f32) {
        if !self.panel_active() { return; }
        if let Some(d) = self.panel.as_ref().and_then(|p| p.drag) {
            let r = if d == Slider::Volume { cc_vol_rect() } else { cc_bright_rect() };
            self.apply_slider(d, Self::vslider_value(r, y));
            self.panel_invalidate();
            return;
        }
        let h = self.panel_hit(x, y);
        if let Some(p) = self.panel.as_mut() {
            if p.hover != h { p.hover = h; self.panel_invalidate(); }
        }
    }

    pub fn panel_down(&mut self, x: f32, y: f32) {
        if !self.panel_active() { return; }
        let (which, r) = match self.panel_hit(x, y) {
            Some(PHit::VVol) => (Slider::Volume, cc_vol_rect()),
            Some(PHit::VBright) => (Slider::Bright, cc_bright_rect()),
            _ => return,
        };
        if let Some(p) = self.panel.as_mut() {
            p.drag = Some(which);
            unsafe { SetCapture(p.hwnd); }
        }
        self.apply_slider(which, Self::vslider_value(r, y));
        self.panel_invalidate();
    }

    pub fn panel_up(&mut self, x: f32, y: f32) {
        if !self.panel_active() { return; }
        if let Some(d) = self.panel.as_mut().and_then(|p| p.drag.take()) {
            unsafe { let _ = ReleaseCapture(); }
            let r = if d == Slider::Volume { cc_vol_rect() } else { cc_bright_rect() };
            self.apply_slider(d, Self::vslider_value(r, y));
            self.panel_invalidate();
            return;
        }
        let Some(hit) = self.panel_hit(x, y) else { return };
        let open = |page: &str| apps::launch(page);
        match hit {
            PHit::Conn(i) => { open(["ms-settings:network-wifi", "ms-settings:bluetooth", "ms-settings:network-airplanemode", "ms-settings:network"][i as usize]); self.close_panel(); }
            PHit::BatteryModule | PHit::Tile(1) => { open("ms-settings:batterysaver"); self.close_panel(); }
            PHit::Tile(_) => { open("ms-settings:nightlight"); self.close_panel(); }
            PHit::Btn(0) => { open("ms-settings:"); self.close_panel(); }
            PHit::Btn(1) => { self.destroy_panel(); open("ms-screenclip:"); } // tutup dulu agar tidak ikut terfoto
            PHit::Btn(2) => { self.destroy_panel(); unsafe { let _ = LockWorkStation(); } }
            PHit::Btn(_) | PHit::Power => self.power_menu(),
            PHit::Prev | PHit::Next => {
                if let Some(p) = self.panel.as_mut() {
                    let (y, m) = p.month;
                    p.month = if hit == PHit::Prev {
                        if m == 1 { (y - 1, 12) } else { (y, m - 1) }
                    } else if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                }
                self.panel_invalidate();
            }
            PHit::App(i) => {
                let target = self.panel.as_ref().map(|p| p.apps[i].1.clone());
                if let Some(t) = target { apps::launch(&t); }
                self.close_panel();
            }
            _ => {}
        }
    }

    pub fn panel_wheel(&mut self, delta: i32) {
        if !self.panel_active() { return; }
        let rows = self.start_rows();
        let Some(p) = self.panel.as_mut() else { return };
        if p.kind == PanelKind::Calendar {
            let (y, m) = p.month;
            p.month = if delta > 0 { if m == 1 { (y - 1, 12) } else { (y, m - 1) } } else if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        } else if p.kind == PanelKind::Start {
            let total_rows = (p.filtered.len() + 5) / 6;
            let max = total_rows.saturating_sub(rows);
            p.scroll = if delta > 0 { p.scroll.saturating_sub(1) } else { (p.scroll + 1).min(max) };
        } else {
            return;
        }
        self.panel_invalidate();
    }

    pub fn panel_char(&mut self, c: u32) {
        if !self.panel_active() { return; }
        let Some(p) = self.panel.as_mut() else { return };
        if p.kind != PanelKind::Start { return; }
        match c {
            8 => { p.search.pop(); }
            13 | 27 => return,
            c if c >= 32 => { if let Some(ch) = char::from_u32(c) { p.search.push(ch); } }
            _ => return,
        }
        self.apply_search();
        self.panel_invalidate();
    }

    pub fn panel_key(&mut self, vk: u32) {
        if vk == VK_ESCAPE.0 as u32 { self.close_panel(); return; }
        if vk == VK_RETURN.0 as u32 {
            let first = self.panel.as_ref().and_then(|p| if p.kind == PanelKind::Start { p.filtered.first().map(|&i| p.apps[i].1.clone()) } else { None });
            if let Some(t) = first { apps::launch(&t); self.close_panel(); }
        }
    }

    fn power_menu(&mut self) {
        let Some(hwnd) = self.panel.as_ref().map(|p| p.hwnd) else { return };
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, 1, w!("Sleep"));
            let _ = AppendMenuW(menu, MF_STRING, 2, w!("Restart"));
            let _ = AppendMenuW(menu, MF_STRING, 3, w!("Shut down"));
            let mut pnt = POINT::default();
            let _ = GetCursorPos(&mut pnt);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pnt.x, pnt.y, 0, hwnd, None);
            let _ = DestroyMenu(menu);
            match cmd.0 {
                1 => { self.destroy_panel(); let _ = SetSuspendState(false, false, false); }
                2 => { self.destroy_panel(); apps::run("shutdown.exe", "/r /t 0"); }
                3 => { self.destroy_panel(); apps::run("shutdown.exe", "/s /t 0"); }
                _ => {}
            }
        }
    }
}

pub extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let is_ours = |a: &App| a.panel.as_ref().map(|p| p.hwnd == hwnd).unwrap_or(false);
        let pos = || {
            let s = scale(hwnd);
            ((lp.0 & 0xffff) as u16 as i16 as f32 / s, ((lp.0 >> 16) & 0xffff) as u16 as i16 as f32 / s)
        };
        match msg {
            WM_APP_PAINT => { with_app(|a| if is_ours(a) { a.panel_render() }); LRESULT(0) }
            WM_TIMER if wp.0 == ANIM_TIMER => { with_app(|a| if is_ours(a) { a.panel_anim_step() }); LRESULT(0) }
            WM_ACTIVATE => {
                if (wp.0 & 0xffff) as u32 == WA_INACTIVE { let _ = PostMessageW(hwnd, WM_APP_CLOSE, WPARAM(0), LPARAM(0)); }
                LRESULT(0)
            }
            WM_APP_CLOSE => { with_app(|a| if is_ours(a) { a.close_panel() }); LRESULT(0) }
            WM_MOUSEMOVE => { let (x, y) = pos(); with_app(|a| if is_ours(a) { a.panel_mouse_move(x, y) }); LRESULT(0) }
            WM_LBUTTONDOWN => { let (x, y) = pos(); with_app(|a| if is_ours(a) { a.panel_down(x, y) }); LRESULT(0) }
            WM_LBUTTONUP => { let (x, y) = pos(); with_app(|a| if is_ours(a) { a.panel_up(x, y) }); LRESULT(0) }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16 as i32;
                with_app(|a| if is_ours(a) { a.panel_wheel(delta) });
                LRESULT(0)
            }
            WM_CHAR => { with_app(|a| if is_ours(a) { a.panel_char(wp.0 as u32) }); LRESULT(0) }
            WM_KEYDOWN => { with_app(|a| if is_ours(a) { a.panel_key(wp.0 as u32) }); LRESULT(0) }
            WM_PAINT => { let _ = ValidateRect(hwnd, None); LRESULT(0) }
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
