// ============================================================
//  Pembaruan otomatis (notifikasi + update 1 klik)
//  - Sekali sehari mengecek GitHub Releases (rilis terbaru, bukan pre-release).
//  - Ada versi baru -> pemberitahuan kaca di atas dock: [Update] / [x].
//  - Update: unduh GlassDock-Setup-x.y.z.exe ke %TEMP%, jalankan /SILENT;
//    installer menutup dock, memasang versi baru, lalu menyalakan dock lagi.
// ============================================================

use super::*;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Networking::WinHttp::*;

const LATEST_URL: &str = "https://api.github.com/repos/K4ZED/GlassDock/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/K4ZED/GlassDock/releases/download/";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const CHECK_EVERY_SECS: u64 = 20 * 3600; // sekali sehari (dengan kelonggaran)

#[derive(Clone, Debug, PartialEq)]
pub struct Release { pub version: String, pub url: String }

pub enum Req { Check { manual: bool }, Download(Release) }
pub enum Resp {
    Available(Release, bool),
    UpToDate(bool),
    CheckFailed(bool),
    Progress(u32),
    Downloaded(Release, PathBuf),
    DownloadFailed,
}

// ---------- versi ----------
fn parse_ver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut it = s.split(|c: char| c == '.' || c == '-' || c == '+').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_ver(candidate), parse_ver(current)) { (Some(a), Some(b)) => a > b, _ => false }
}

// ---------- JSON kecil: cukup ambil nilai string tertentu ----------
fn json_strings<'a>(json: &'a str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(&pat) {
        rest = &rest[i + pat.len()..];
        let t = rest.trim_start();
        let Some(t) = t.strip_prefix(':') else { continue };
        let t = t.trim_start();
        let Some(t) = t.strip_prefix('"') else { continue };
        let mut val = String::new();
        let mut chars = t.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => { if let Some(n) = chars.next() { val.push(match n { 'n' => '\n', 't' => '\t', other => other }); } }
                _ => val.push(c),
            }
        }
        out.push(val);
    }
    out
}

fn parse_release(json: &str, allow_local: bool) -> Option<Release> {
    let tag = json_strings(json, "tag_name").into_iter().next()?;
    let url = json_strings(json, "browser_download_url").into_iter().find(|u| {
        let name = u.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase();
        let ok_name = name.starts_with("glassdock-setup") && name.ends_with(".exe");
        ok_name && (u.starts_with(DOWNLOAD_PREFIX) || allow_local)
    })?;
    Some(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), url })
}

// ---------- HTTP (WinHTTP bawaan Windows) ----------
fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

fn http_get(url: &str, accept: &str, mut progress: impl FnMut(u64, Option<u64>)) -> std::result::Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("https only")?;
    let (host, path) = match rest.find('/') { Some(i) => (&rest[..i], &rest[i..]), None => (rest, "/") };
    unsafe {
        let agent = wide(&format!("GlassDock-Updater/{CURRENT}"));
        let session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() { return Err("open".into()); }
        let close = |h: *mut c_void| { let _ = WinHttpCloseHandle(h); };
        let _ = WinHttpSetTimeouts(session, 10_000, 10_000, 15_000, 30_000);
        let h_host = wide(host);
        let conn = WinHttpConnect(session, PCWSTR(h_host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0);
        if conn.is_null() { close(session); return Err("connect".into()); }
        let h_path = wide(path);
        let verb = wide("GET");
        let req = WinHttpOpenRequest(conn, PCWSTR(verb.as_ptr()), PCWSTR(h_path.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
        if req.is_null() { close(conn); close(session); return Err("request".into()); }
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        let result = (|| -> std::result::Result<Vec<u8>, String> {
            WinHttpSendRequest(req, Some(&headers), None, 0, 0, 0).map_err(|e| format!("send {e}"))?;
            WinHttpReceiveResponse(req, std::ptr::null_mut()).map_err(|e| format!("receive {e}"))?;
            let mut status = 0u32;
            let mut len = 4u32;
            let _ = WinHttpQueryHeaders(req, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut());
            if status != 200 { return Err(format!("HTTP {status}")); }
            let mut total = 0u32;
            let mut len = 4u32;
            let total = if WinHttpQueryHeaders(req, WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut total as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut()).is_ok() { Some(total as u64) } else { None };
            let mut body = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut read = 0u32;
                WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read).map_err(|e| format!("read {e}"))?;
                if read == 0 { break; }
                body.extend_from_slice(&buf[..read as usize]);
                progress(body.len() as u64, total);
                if body.len() > 64 * 1024 * 1024 { return Err("too big".into()); }
            }
            Ok(body)
        })();
        close(req);
        close(conn);
        close(session);
        result
    }
}

// ---------- thread pengecek & pengunduh ----------
fn now_secs() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

/// `update-feed.txt` (opsional, untuk pengujian): path/URL ke JSON rilis pengganti
fn feed_override() -> Option<String> {
    cfg_read("update-feed.txt").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn check() -> std::result::Result<Option<Release>, String> {
    let fetched = match feed_override() {
        Some(f) if f.starts_with("https://") => http_get(&f, "application/vnd.github+json", |_, _| {}).map(|j| (j, false)),
        Some(f) => std::fs::read(&f).map(|j| (j, true)).map_err(|e| e.to_string()),
        None => http_get(LATEST_URL, "application/vnd.github+json", |_, _| {}).map(|j| (j, false)),
    };
    // 404 = belum ada rilis yang dipublikasikan -> tidak ada update
    let (json, local) = match fetched { Err(e) if e == "HTTP 404" => return Ok(None), other => other? };
    let json = String::from_utf8_lossy(&json);
    let rel = parse_release(&json, local).ok_or("no installer in release")?;
    Ok(if is_newer(&rel.version, CURRENT) { Some(rel) } else { None })
}

fn download(rel: &Release, send: &dyn Fn(Resp)) -> std::result::Result<PathBuf, String> {
    let dest = std::env::temp_dir().join(format!("GlassDock-Setup-{}.exe", rel.version));
    let data = if rel.url.starts_with("https://") {
        let mut last = 0u32;
        http_get(&rel.url, "application/octet-stream", |got, total| {
            if let Some(t) = total.filter(|t| *t > 0) {
                let p = (got * 100 / t) as u32;
                if p != last { last = p; send(Resp::Progress(p)); }
            }
        })?
    } else if feed_override().is_some() {
        std::fs::read(&rel.url).map_err(|e| e.to_string())? // pengujian lokal
    } else {
        return Err("bad url".into());
    };
    // installer Inno Setup asli: file PE ("MZ") yang ukurannya wajar
    if data.len() < 200 * 1024 || !data.starts_with(b"MZ") { return Err("not an installer".into()); }
    std::fs::write(&dest, &data).map_err(|e| e.to_string())?;
    Ok(dest)
}

pub fn spawn_worker(notify: isize, msg: u32) -> (Sender<Req>, Receiver<Resp>) {
    let (req_tx, req_rx) = channel::<Req>();
    let (resp_tx, resp_rx) = channel::<Resp>();
    std::thread::spawn(move || {
        let send = |r: Resp| {
            let _ = resp_tx.send(r);
            unsafe { let _ = PostMessageW(HWND(notify as *mut c_void), msg, WPARAM(0), LPARAM(0)); }
        };
        // tunggu sebentar setelah login supaya tidak membebani saat Windows baru menyala
        let mut wait = Duration::from_secs(90);
        loop {
            let req = match req_rx.recv_timeout(wait) {
                Ok(r) => Some(r),
                Err(RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            };
            wait = Duration::from_secs(3600);
            match req {
                Some(Req::Download(rel)) => match download(&rel, &send) {
                    Ok(path) => send(Resp::Downloaded(rel, path)),
                    Err(e) => { log_line(&format!("update download failed: {e}")); send(Resp::DownloadFailed); }
                },
                other => {
                    let manual = matches!(other, Some(Req::Check { manual: true }));
                    if !manual {
                        let last: u64 = cfg_read("update-check.txt").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
                        if now_secs().saturating_sub(last) < CHECK_EVERY_SECS { continue; }
                    }
                    match check() {
                        Ok(found) => {
                            cfg_write("update-check.txt", &now_secs().to_string());
                            match found { Some(rel) => send(Resp::Available(rel, manual)), None => send(Resp::UpToDate(manual)) }
                        }
                        Err(e) => { log_line(&format!("update check failed: {e}")); send(Resp::CheckFailed(manual)); }
                    }
                }
            }
        }
    });
    (req_tx, resp_rx)
}

// ================= Pemberitahuan kaca =================
const TOAST_W: f32 = 372.0;
const TOAST_H: f32 = 72.0;
const ANIM: usize = 1;
const HIDE: usize = 2;

#[derive(Clone, PartialEq)]
enum TState { Available(Release), UpToDate, CheckFailed, Downloading(String, u32), Installing(String), Failed }

#[derive(Clone, Copy, PartialEq)]
enum THit { Update, Close }

pub struct Toast {
    hwnd: HWND,
    rt: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    surf: Option<Surface>,
    bg: Option<(u32, u32, Vec<u8>)>,
    bg_bmp: Option<ID2D1Bitmap>,
    size_px: (i32, i32),
    pos: (i32, i32),
    anim: f32,
    fading: bool,
    state: TState,
    hits: Vec<(THit, D2D_RECT_F)>,
    f_title: IDWriteTextFormat,
    f_sub: IDWriteTextFormat,
    f_glyph: IDWriteTextFormat,
}

pub fn register_class(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(toast_wndproc),
            hInstance: hinst,
            lpszClassName: w!("GlassToast"),
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

unsafe extern "system" fn toast_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == ANIM => { with_app(|a| a.toast_step()); LRESULT(0) }
        WM_TIMER if wp.0 == HIDE => { let _ = KillTimer(hwnd, HIDE); with_app(|a| a.toast_hide()); LRESULT(0) }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            let s = scale(hwnd);
            let (x, y) = ((lp.0 & 0xffff) as u16 as i16 as f32 / s, ((lp.0 >> 16) & 0xffff) as u16 as i16 as f32 / s);
            with_app(|a| a.toast_click(x, y));
            LRESULT(0)
        }
        WM_PAINT => { crate::paint_noop(hwnd); LRESULT(0) }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl App {
    pub(crate) fn toast_hwnd(&self) -> Option<HWND> { self.toast.as_ref().map(|t| t.hwnd) }

    /// Tepi atas pemberitahuan (piksel layar) bila sedang terlihat; dipakai OSD agar menumpuk di atasnya
    pub(crate) fn toast_top_px(&self) -> Option<i32> {
        self.toast.as_ref().filter(|t| t.anim > 0.0 && !t.fading).map(|t| t.pos.1)
    }

    pub(crate) fn start_updater(&mut self) {
        // installer hasil update sebelumnya sudah tidak perlu
        if let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(v) = name.strip_prefix("GlassDock-Setup-").and_then(|n| n.strip_suffix(".exe")) {
                    if !is_newer(v, CURRENT) { let _ = std::fs::remove_file(e.path()); }
                }
            }
        }
        let (tx, rx) = spawn_worker(self.hwnd.0 as isize, WM_APP_UPDATE);
        self.upd_tx = Some(tx);
        self.upd_rx = Some(rx);
    }

    /// Menu klik kanan: "Check for updates"
    pub(crate) fn check_updates_now(&mut self) {
        if let Some(tx) = &self.upd_tx { let _ = tx.send(Req::Check { manual: true }); }
    }

    /// Menu klik kanan / tombol Update: unduh & pasang
    pub(crate) fn start_update(&mut self) {
        let Some(rel) = self.update.clone() else { return };
        if let Some(tx) = &self.upd_tx {
            if tx.send(Req::Download(rel.clone())).is_ok() {
                self.toast_show(TState::Downloading(rel.version, 0), None);
            }
        }
    }

    pub(crate) fn on_update_msg(&mut self) {
        let msgs: Vec<Resp> = match &self.upd_rx { Some(rx) => rx.try_iter().collect(), None => return };
        for m in msgs {
            match m {
                Resp::Available(rel, _) => {
                    self.update = Some(rel.clone());
                    self.toast_show(TState::Available(rel), Some(20_000));
                }
                Resp::UpToDate(true) => self.toast_show(TState::UpToDate, Some(3_500)),
                Resp::CheckFailed(true) => self.toast_show(TState::CheckFailed, Some(4_500)),
                Resp::UpToDate(false) | Resp::CheckFailed(false) => {}
                Resp::Progress(p) => {
                    if let Some(v) = self.update.as_ref().map(|r| r.version.clone()) {
                        if let Some(t) = self.toast.as_mut() { t.state = TState::Downloading(v, p); }
                        let _ = self.toast_render();
                    }
                }
                Resp::Downloaded(rel, path) => {
                    self.toast_show(TState::Installing(rel.version.clone()), None);
                    log_line(&format!("installing update {}", rel.version));
                    // /SILENT: kecil, ada progres; installer menutup dock lalu menyalakannya lagi
                    let file = wide(&path.to_string_lossy());
                    let args = wide("/SILENT /SUPPRESSMSGBOXES /NORESTART");
                    let r = unsafe { windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), PCWSTR(file.as_ptr()), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL) };
                    if (r.0 as isize) <= 32 {
                        log_line("update: could not start the installer");
                        self.toast_show(TState::Failed, Some(5_000));
                    }
                }
                Resp::DownloadFailed => self.toast_show(TState::Failed, Some(5_000)),
            }
        }
    }

    fn toast_show(&mut self, state: TState, hide_after_ms: Option<u32>) {
        unsafe {
            let s = scale(self.hwnd);
            let (pw, ph) = ((TOAST_W * s).round() as i32, (TOAST_H * s).round() as i32);
            let (bx, by, bw, _) = self.body();
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let gap = (14.0 * s).round() as i32;
            let left = if self.shown > 0.5 { bx + bw / 2 - pw / 2 } else { sw / 2 - pw / 2 };
            let top = if self.shown > 0.5 { by - ph - gap } else { sh - ph - (28.0 * s).round() as i32 };
            if self.toast.is_none() {
                let Ok(hinst) = GetModuleHandleW(None) else { return };
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    w!("GlassToast"), w!(""), WS_POPUP,
                    left, top, pw, ph, None, None, HINSTANCE(hinst.0), None,
                ) else { return };
                let (Ok(f_title), Ok(f_sub), Ok(f_glyph)) = (
                    text_format(&self.dw, FONT, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.5),
                    text_format(&self.dw, FONT, DWRITE_FONT_WEIGHT_NORMAL, 11.5),
                    text_format(&self.dw, w!("Segoe MDL2 Assets"), DWRITE_FONT_WEIGHT_NORMAL, 17.0),
                ) else { return };
                self.toast = Some(Toast {
                    hwnd, rt: None, brush: None, surf: None, bg: None, bg_bmp: None,
                    size_px: (pw, ph), pos: (left, top), anim: 0.0, fading: false,
                    state: state.clone(), hits: Vec::new(), f_title, f_sub, f_glyph,
                });
            }
            let fresh = self.toast.as_ref().map(|t| t.anim <= 0.0 || t.pos != (left, top)).unwrap_or(true);
            let new_bg = if fresh { self.capture_behind(left, top, pw, ph) } else { None };
            let Some(t) = self.toast.as_mut() else { return };
            if fresh {
                t.pos = (left, top);
                t.size_px = (pw, ph);
                t.bg = new_bg;
                t.bg_bmp = None;
                if t.surf.as_ref().map(|s| (s.w, s.h) != (pw, ph)).unwrap_or(false) { t.surf = None; }
            }
            t.state = state;
            t.fading = false;
            let hwnd = t.hwnd;
            let _ = KillTimer(hwnd, HIDE);
            if let Some(ms) = hide_after_ms { SetTimer(hwnd, HIDE, ms, None); }
            if let Err(e) = self.toast_render() { log_line(&format!("toast render failed: {e}")); return; }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(hwnd, ANIM, 15, None);
        }
    }

    pub(crate) fn toast_hide(&mut self) {
        if let Some(t) = self.toast.as_mut() {
            // jangan sembunyikan saat sedang mengunduh/memasang
            if matches!(t.state, TState::Downloading(..) | TState::Installing(_)) { return; }
            t.fading = true;
            unsafe { SetTimer(t.hwnd, ANIM, 15, None); }
        }
    }

    pub(crate) fn toast_click(&mut self, x: f32, y: f32) {
        let hit = self.toast.as_ref().and_then(|t| t.hits.iter().find(|(_, r)| inside(r, x, y)).map(|(h, _)| *h));
        match hit {
            Some(THit::Update) => self.start_update(),
            Some(THit::Close) => self.toast_hide(),
            None => {
                // klik di badan pemberitahuan "tersedia" = Update
                if matches!(self.toast.as_ref().map(|t| &t.state), Some(TState::Available(_))) { self.start_update(); }
            }
        }
    }

    pub(crate) fn toast_step(&mut self) {
        let Some(t) = self.toast.as_mut() else { return };
        if t.fading {
            t.anim -= 0.08;
            if t.anim <= 0.0 {
                t.anim = 0.0;
                unsafe { let _ = KillTimer(t.hwnd, ANIM); let _ = ShowWindow(t.hwnd, SW_HIDE); }
                t.bg = None;
                t.bg_bmp = None;
                return;
            }
        } else if t.anim < 1.0 {
            t.anim = (t.anim + 0.12).min(1.0);
        } else {
            unsafe { let _ = KillTimer(t.hwnd, ANIM); }
            return;
        }
        self.toast_present();
    }

    fn toast_present(&self) {
        let Some(t) = self.toast.as_ref() else { return };
        let Some(surf) = t.surf.as_ref() else { return };
        unsafe {
            let s = scale(self.hwnd);
            let e = 1.0 - (1.0 - t.anim.clamp(0.0, 1.0)).powi(3);
            let offset = ((1.0 - e) * 12.0 * s).round() as i32;
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: (e * 255.0) as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
            let size = SIZE { cx: t.size_px.0, cy: t.size_px.1 };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT { x: t.pos.0, y: t.pos.1 + offset };
            let _ = UpdateLayeredWindow(t.hwnd, None, Some(&dst), Some(&size), surf.dc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }

    fn toast_text(&self, rt: &ID2D1RenderTarget, br: &ID2D1SolidColorBrush, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, align: DWRITE_TEXT_ALIGNMENT, color: D2D1_COLOR_F) -> Result<()> {
        unsafe {
            let t: Vec<u16> = s.encode_utf16().collect();
            let l = self.dw.CreateTextLayout(&t, f, (r.right - r.left).max(1.0), (r.bottom - r.top).max(1.0))?;
            l.SetTextAlignment(align)?;
            br.SetColor(&color);
            rt.DrawTextLayout(pt(r.left, r.top), &l, br, D2D1_DRAW_TEXT_OPTIONS_CLIP);
            Ok(())
        }
    }

    fn toast_render(&mut self) -> Result<()> {
        unsafe {
            let s = scale(self.hwnd);
            let mut look = self.style.look();
            look.base = 0.0;
            let d2d = self.d2d.clone();
            let Some(t) = self.toast.as_mut() else { return Ok(()) };
            let (pw, ph) = t.size_px;
            if t.rt.is_none() {
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
                t.brush = Some(rt.CreateSolidColorBrush(&white(1.0), None)?);
                t.rt = Some(rt);
            }
            if t.surf.is_none() { t.surf = Surface::new(pw, ph); }
            let Some(surf) = t.surf.as_ref() else { return Ok(()) };
            let dc_rt = t.rt.clone().unwrap();
            dc_rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: pw, bottom: ph })?;
            let rt: ID2D1RenderTarget = dc_rt.cast()?;
            if t.bg_bmp.is_none() {
                if let Some((bw, bh, data)) = &t.bg {
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    t.bg_bmp = rt.CreateBitmap(D2D_SIZE_U { width: *bw, height: *bh }, Some(data.as_ptr() as *const c_void), bw * 4, &props).ok();
                }
            }
            let br = t.brush.clone().unwrap();
            let state = t.state.clone();
            let bg = t.bg_bmp.clone().zip(t.bg.as_ref().map(|(bw, bh, _)| (*bw as f32, *bh as f32)));
            let (f_title, f_sub, f_glyph) = (t.f_title.clone(), t.f_sub.clone(), t.f_glyph.clone());
            let (w, h) = (TOAST_W, TOAST_H);
            let radius = 22.0;
            let body = rrect(rc(0.0, 0.0, w, h), radius);

            rt.BeginDraw();
            rt.Clear(Some(&col(0.0, 0.0, 0.0, 0.0)));
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
                None => { br.SetColor(&col(0.1, 0.1, 0.12, 0.92)); rt.FillRoundedRectangle(&body, &br); }
            }
            br.SetColor(&col(0.05, 0.05, 0.07, 0.5));
            rt.FillRoundedRectangle(&body, &br);
            draw_glass(&rt, &br, w, h, radius, &look, 0.0)?;

            let blue = hex(0x0A84FF);
            let (glyph, icon_col, title, sub) = match &state {
                TState::Available(r) => ("\u{E896}", blue, format!("GlassDock {} is available", r.version), format!("You have {CURRENT}")),
                TState::UpToDate => ("\u{E73E}", hex(0x34C759), "GlassDock is up to date".to_string(), format!("Version {CURRENT}")),
                TState::CheckFailed => ("\u{E783}", hex(0xFF9F0A), "Couldn't check for updates".to_string(), "No connection to GitHub. Try again later.".to_string()),
                TState::Downloading(v, p) => ("\u{E896}", blue, format!("Downloading GlassDock {v}"), format!("{p}%")),
                TState::Installing(v) => ("\u{E895}", blue, format!("Installing GlassDock {v}"), "The dock restarts in a moment.".to_string()),
                TState::Failed => ("\u{E783}", hex(0xFF453A), "Update failed".to_string(), "Try again later (right-click the dock).".to_string()),
            };
            // ikon dalam lingkaran berwarna
            let (icx, icy) = (36.0, h / 2.0);
            br.SetColor(&icon_col);
            rt.FillEllipse(&D2D1_ELLIPSE { point: pt(icx, icy), radiusX: 18.0, radiusY: 18.0 }, &br);
            let g: Vec<u16> = glyph.encode_utf16().collect();
            br.SetColor(&white(1.0));
            rt.DrawText(&g, &f_glyph, &rc(icx - 18.0, icy - 18.0, icx + 18.0, icy + 18.0), &br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);

            let mut hits = Vec::new();
            let has_update_btn = matches!(state, TState::Available(_));
            let busy = matches!(state, TState::Downloading(..) | TState::Installing(_));
            let text_right = if has_update_btn { w - 124.0 } else if busy { w - 18.0 } else { w - 46.0 };
            self.toast_text(&rt, &br, &title, &f_title, rc(66.0, 13.0, text_right, 33.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(1.0))?;
            if let TState::Downloading(_, p) = &state {
                // bilah progres
                let (x0, x1, y) = (66.0, w - 70.0, 46.0);
                br.SetColor(&white(0.2));
                rt.FillRoundedRectangle(&rrect(rc(x0, y - 3.0, x1, y + 3.0), 3.0), &br);
                br.SetColor(&white(1.0));
                let fx = x0 + (x1 - x0) * (*p as f32 / 100.0).clamp(0.0, 1.0);
                rt.FillRoundedRectangle(&rrect(rc(x0, y - 3.0, fx.max(x0 + 6.0), y + 3.0), 3.0), &br);
                self.toast_text(&rt, &br, &sub, &f_sub, rc(w - 62.0, 37.0, w - 16.0, 55.0), DWRITE_TEXT_ALIGNMENT_TRAILING, white(0.8))?;
            } else {
                self.toast_text(&rt, &br, &sub, &f_sub, rc(66.0, 34.0, text_right, 54.0), DWRITE_TEXT_ALIGNMENT_LEADING, white(0.7))?;
            }
            if has_update_btn {
                let b = rc(w - 116.0, h / 2.0 - 15.0, w - 46.0, h / 2.0 + 15.0);
                br.SetColor(&blue);
                rt.FillRoundedRectangle(&rrect(b, 15.0), &br);
                self.toast_text(&rt, &br, "Update", &f_title, rc(b.left, b.top + 5.0, b.right, b.bottom), DWRITE_TEXT_ALIGNMENT_CENTER, white(1.0))?;
                hits.push((THit::Update, b));
            }
            if !busy {
                let (cx, cy) = (w - 24.0, h / 2.0);
                br.SetColor(&white(0.14));
                rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 12.0, radiusY: 12.0 }, &br);
                let x: Vec<u16> = "\u{E711}".encode_utf16().collect();
                let small = text_format(&self.dw, w!("Segoe MDL2 Assets"), DWRITE_FONT_WEIGHT_NORMAL, 10.0)?;
                br.SetColor(&white(0.85));
                rt.DrawText(&x, &small, &rc(cx - 12.0, cy - 12.0, cx + 12.0, cy + 12.0), &br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                hits.push((THit::Close, rc(cx - 16.0, cy - 16.0, cx + 16.0, cy + 16.0)));
            }
            let _ = rt.EndDraw(None, None);
            if let Some(t) = self.toast.as_mut() { t.hits = hits; }
        }
        self.toast_present();
        Ok(())
    }
}
