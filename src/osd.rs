// ============================================================
//  OSD kaca untuk volume: menggantikan indikator bawaan Windows.
//  Tombol volume di keyboard ditangkap keyboard hook (main.rs),
//  volume diubah lewat Core Audio, lalu pil kaca kecil muncul
//  di atas dock dan memudar sendiri.
// ============================================================

use super::*;
use std::time::{Duration, Instant};
use windows::Foundation::Numerics::Matrix3x2;

const OSD_W: f32 = 250.0;
const OSD_H: f32 = 54.0;
const OSD_TIMER: usize = 1;
const SHOW_FOR: Duration = Duration::from_millis(1500);
const STEP: f32 = 0.02; // sama dengan langkah tombol volume Windows (2%)
pub(crate) const OSD_VOL: u8 = 0;
pub(crate) const OSD_BRIGHT: u8 = 1;
pub(crate) const OSD_KB: u8 = 2;

pub struct Osd {
    hwnd: HWND,
    rt: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    surf: Option<Surface>,
    bg: Option<(u32, u32, Vec<u8>)>,
    bg_bmp: Option<ID2D1Bitmap>,
    size_px: (i32, i32),
    pos: (i32, i32),
    anim: f32,      // 0 = tak terlihat, 1 = tampil penuh
    fading: bool,   // sedang memudar keluar
    hide_at: Instant,
    level: f32,
    muted: bool,
    mode: u8,       // OSD_VOL / OSD_BRIGHT / OSD_KB
    f_text: IDWriteTextFormat,
    f_glyph: IDWriteTextFormat,
}

pub fn register_class(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(osd_wndproc),
            hInstance: hinst,
            lpszClassName: w!("GlassOsd"),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

unsafe extern "system" fn osd_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == OSD_TIMER => { with_app(|a| a.osd_step()); LRESULT(0) }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => { crate::paint_noop(hwnd); LRESULT(0) }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn speaker_glyph(level: f32, muted: bool) -> &'static str {
    if muted || level < 0.005 { "\u{E74F}" }      // speaker dicoret
    else if level < 0.34 { "\u{E993}" }            // 1 gelombang
    else if level < 0.67 { "\u{E994}" }            // 2 gelombang
    else { "\u{E995}" }                            // 3 gelombang
}

impl App {
    pub(crate) fn osd_hwnd(&self) -> Option<HWND> { self.osd.as_ref().map(|o| o.hwnd) }

    /// Tombol volume (VK_VOLUME_MUTE/DOWN/UP) dari keyboard hook
    pub(crate) fn on_volume_key(&mut self, vk: u32) {
        // perangkat audio bisa berganti (headset dicolok) -> buka ulang bila gagal dibaca
        let mut cur = self.osd_audio.as_ref().and_then(|a| a.get());
        if cur.is_none() {
            self.osd_audio = Some(sys::Audio::open());
            cur = self.osd_audio.as_ref().and_then(|a| a.get());
        }
        let Some((v, m)) = cur else { return };
        let Some(audio) = self.osd_audio.as_ref() else { return };
        let (mut level, mut muted) = (v, m);
        match vk {
            0xAD => { muted = !m; audio.set_mute(muted); }
            0xAE => {
                level = ((v / STEP).round() - 1.0).max(0.0) * STEP;
                audio.set(level);
            }
            _ => {
                level = (((v / STEP).round() + 1.0) * STEP).min(1.0);
                audio.set(level);
                if m { muted = false; audio.set_mute(false); } // naikkan volume = bunyikan lagi
            }
        }
        // Control Center yang sedang terbuka ikut diperbarui
        self.sync_panel_volume(if muted { 0.0 } else { level });
        self.show_osd(level, muted, OSD_VOL);
    }

    /// Kecerahan layar berubah (Fn+F5/F6, slider Windows, dll.): notifikasi daya GUID_VIDEO_CURRENT_MONITOR_BRIGHTNESS
    pub(crate) fn on_brightness_change(&mut self, pct: u32) {
        let level = (pct.min(100) as f32) / 100.0;
        // nilai pertama dikirim Windows saat didaftarkan dan kadang tidak sesuai kecerahan asli -> abaikan
        let first = self.osd_bright_last.is_none();
        let same = self.osd_bright_last == Some(pct);
        self.osd_bright_last = Some(pct);
        if first || same { return; }
        self.bright_val = Some(level);
        self.sync_panel_brightness(level);
        // slider Control Center sedang dipakai: cukup sinkronkan, tanpa OSD
        let cc_open = self.panel.as_ref().map(|p| p.kind == panels::PanelKind::Control).unwrap_or(false);
        if cc_open { return; }
        self.show_osd(level, false, OSD_BRIGHT);
    }

    /// Lampu keyboard berubah (Fn+F4 di laptop ASUS): level 0..3 dari registry driver ASUS
    pub(crate) fn on_kb_light(&mut self, level: u32) {
        self.show_osd(level.min(3) as f32 / 3.0, false, OSD_KB);
    }

    fn show_osd(&mut self, level: f32, muted: bool, mode: u8) {
        unsafe {
            let s = scale(self.hwnd);
            let (pw, ph) = ((OSD_W * s).round() as i32, (OSD_H * s).round() as i32);
            // di atas dock bila dock tampil; kalau tidak, dekat tepi bawah layar
            let (bx, by, bw, _) = self.body();
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let gap = (14.0 * s).round() as i32;
            let left = if self.shown > 0.5 { bx + bw / 2 - pw / 2 } else { sw / 2 - pw / 2 };
            let top = if self.shown > 0.5 { by - ph - gap } else { sh - ph - (28.0 * s).round() as i32 };
            // pemberitahuan update di posisi yang sama: taruh OSD di atasnya
            let top = match self.toast_top_px() { Some(t) => t - ph - (10.0 * s).round() as i32, None => top };

            let fresh = self.osd.as_ref().map(|o| o.anim <= 0.0 || o.pos != (left, top)).unwrap_or(true);
            if self.osd.is_none() {
                let hinst = match GetModuleHandleW(None) { Ok(h) => h, Err(_) => return };
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                    w!("GlassOsd"), w!(""), WS_POPUP,
                    left, top, pw, ph, None, None, HINSTANCE(hinst.0), None,
                ) else { return };
                let text = |weight, size| text_format(&self.dw, FONT, weight, size);
                let (Ok(f_text), Ok(f_glyph)) = (text(DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0), text_format(&self.dw, w!("Segoe MDL2 Assets"), DWRITE_FONT_WEIGHT_NORMAL, 17.0)) else { return };
                self.osd = Some(Osd {
                    hwnd, rt: None, brush: None, surf: None, bg: None, bg_bmp: None,
                    size_px: (pw, ph), pos: (left, top), anim: 0.0, fading: false,
                    hide_at: Instant::now(), level, muted, mode, f_text, f_glyph,
                });
            }
            // latar blur diambil sekali saat muncul (sebelum OSD terlihat), tanpa jendela GlassDock
            let new_bg = if fresh { self.capture_behind(left, top, pw, ph) } else { None };
            let Some(o) = self.osd.as_mut() else { return };
            if fresh {
                o.pos = (left, top);
                o.size_px = (pw, ph);
                o.bg = new_bg;
                o.bg_bmp = None;
                if o.surf.as_ref().map(|s| (s.w, s.h) != (pw, ph)).unwrap_or(false) { o.surf = None; }
            }
            o.level = level;
            o.muted = muted;
            o.mode = mode;
            o.fading = false;
            o.hide_at = Instant::now() + SHOW_FOR;
            let hwnd = o.hwnd;
            if let Err(e) = self.osd_render() { log_line(&format!("osd render failed: {e}")); return; }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(hwnd, OSD_TIMER, 15, None);
        }
    }

    pub(crate) fn osd_step(&mut self) {
        let Some(o) = self.osd.as_mut() else { return };
        if !o.fading && Instant::now() >= o.hide_at { o.fading = true; }
        if o.fading {
            o.anim -= 0.08; // ~190 ms
            if o.anim <= 0.0 {
                o.anim = 0.0;
                unsafe {
                    let _ = KillTimer(o.hwnd, OSD_TIMER);
                    let _ = ShowWindow(o.hwnd, SW_HIDE);
                }
                o.bg = None;
                o.bg_bmp = None;
                return;
            }
        } else if o.anim < 1.0 {
            o.anim = (o.anim + 0.14).min(1.0); // ~110 ms
        } else {
            return; // tampil penuh, tunggu waktunya memudar
        }
        self.osd_present();
    }

    fn osd_present(&self) {
        let Some(o) = self.osd.as_ref() else { return };
        let Some(surf) = o.surf.as_ref() else { return };
        unsafe {
            let s = scale(self.hwnd);
            let e = 1.0 - (1.0 - o.anim.clamp(0.0, 1.0)).powi(3);
            let offset = ((1.0 - e) * 10.0 * s).round() as i32;
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: (e * 255.0) as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
            let size = SIZE { cx: o.size_px.0, cy: o.size_px.1 };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT { x: o.pos.0, y: o.pos.1 + offset };
            let _ = UpdateLayeredWindow(o.hwnd, None, Some(&dst), Some(&size), surf.dc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }

    fn osd_render(&mut self) -> Result<()> {
        unsafe {
            let s = scale(self.hwnd);
            let mut look = self.style.look();
            look.base = 0.0;
            let d2d = self.d2d.clone();
            let Some(o) = self.osd.as_mut() else { return Ok(()) };
            let (pw, ph) = o.size_px;
            if o.rt.is_none() {
                let dpi = 96.0 * s;
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: dpi,
                    dpiY: dpi,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = d2d.CreateDCRenderTarget(&props)?;
                o.brush = Some(rt.CreateSolidColorBrush(&white(1.0), None)?);
                o.rt = Some(rt);
            }
            if o.surf.is_none() { o.surf = Surface::new(pw, ph); }
            let Some(surf) = o.surf.as_ref() else { return Ok(()) };
            let dc_rt = o.rt.clone().unwrap();
            dc_rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: pw, bottom: ph })?;
            let rt: ID2D1RenderTarget = dc_rt.cast()?;
            if o.bg_bmp.is_none() {
                if let Some((bw, bh, data)) = &o.bg {
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    o.bg_bmp = rt.CreateBitmap(D2D_SIZE_U { width: *bw, height: *bh }, Some(data.as_ptr() as *const c_void), bw * 4, &props).ok();
                }
            }
            let br = o.brush.clone().unwrap();
            let (w, h) = (OSD_W, OSD_H);
            let r = h / 2.0;
            let body = rrect(rc(0.0, 0.0, w, h), r);

            rt.BeginDraw();
            rt.Clear(Some(&col(0.0, 0.0, 0.0, 0.0)));
            match (&o.bg_bmp, &o.bg) {
                (Some(bmp), Some((bw, bh, _))) => {
                    let bb = rt.CreateBitmapBrush(
                        bmp,
                        Some(&D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR }),
                        None,
                    )?;
                    bb.SetTransform(&Matrix3x2 { M11: w / *bw as f32, M12: 0.0, M21: 0.0, M22: h / *bh as f32, M31: 0.0, M32: 0.0 });
                    rt.FillRoundedRectangle(&body, &bb);
                }
                _ => {
                    br.SetColor(&col(0.1, 0.1, 0.12, 0.92));
                    rt.FillRoundedRectangle(&body, &br);
                }
            }
            br.SetColor(&col(0.05, 0.05, 0.07, 0.42));
            rt.FillRoundedRectangle(&body, &br);
            draw_glass(&rt, &br, w, h, r, &look, 0.0)?;

            // ikon speaker
            let glyph: Vec<u16> = (match o.mode { OSD_BRIGHT => "\u{E706}", OSD_KB => "\u{E765}", _ => speaker_glyph(o.level, o.muted) }).encode_utf16().collect();
            br.SetColor(&white(1.0));
            rt.DrawText(&glyph, &o.f_glyph, &rc(14.0, 0.0, 50.0, h), &br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);

            // bar level: jalur redup + isi putih (abu-abu saat bisu)
            let (x0, x1, cy, th) = (54.0, w - 62.0, h / 2.0, 6.0);
            br.SetColor(&white(0.20));
            rt.FillRoundedRectangle(&rrect(rc(x0, cy - th / 2.0, x1, cy + th / 2.0), th / 2.0), &br);
            let lv = if o.muted { 0.0 } else { o.level.clamp(0.0, 1.0) };
            if lv > 0.0 {
                let fx = x0 + (x1 - x0) * lv;
                br.SetColor(&white(1.0));
                rt.FillRoundedRectangle(&rrect(rc(x0, cy - th / 2.0, fx.max(x0 + th), cy + th / 2.0), th / 2.0), &br);
            }

            // persen / "Muted"
            let label = if o.mode == OSD_KB {
                ["Off", "Low", "Med", "High"][((o.level * 3.0).round() as usize).min(3)].to_string()
            } else if o.muted { "Muted".to_string() } else { format!("{}%", (o.level * 100.0).round() as i32) };
            let t: Vec<u16> = label.encode_utf16().collect();
            br.SetColor(&white(if o.muted { 0.75 } else { 1.0 }));
            rt.DrawText(&t, &o.f_text, &rc(w - 62.0, 0.0, w - 10.0, h), &br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);

            let _ = rt.EndDraw(None, None);
        }
        self.osd_present();
        Ok(())
    }
}
