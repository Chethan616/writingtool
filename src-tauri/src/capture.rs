//! Screenshot capture for "ask Gemini about what's on screen".
//!
//! Two flows:
//!  - [`capture_primary_monitor`] — instant full-screen of the primary monitor,
//!    returns base64 PNG.
//!  - Region: [`start_region_capture`] spawns a fullscreen transparent overlay
//!    window with the cached screenshot as background; the JS layer draws a
//!    selection rectangle and calls [`finish_region_capture`] with the coords,
//!    which crops + returns the PNG. [`cancel_region_capture`] tears the
//!    overlay down on Esc.
//!
//! White-flash fix: the capture window is created *hidden*; the frontend
//! fetches the cached payload via `get_pending_screenshot`, paints the
//! background, then calls `show_capture_window` so the first frame the user
//! sees is already the screen image (not a white WebView2 default).

use std::sync::Mutex;

use anyhow::{anyhow, Result};
use base64::Engine;
use image::{codecs::png::PngEncoder, ColorType, GenericImageView, ImageEncoder, RgbaImage};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Default)]
pub struct CaptureState {
    /// Captured RGBA pixels — kept while the region selector is open so we can
    /// crop in place without re-encoding.
    image: Mutex<Option<RgbaImage>>,
    /// Pre-encoded PNG payload — read once by the capture window frontend to
    /// paint the background before showing.
    payload: Mutex<Option<ScreenshotPayload>>,
}

impl CaptureState {
    pub fn new() -> Self {
        Self::default()
    }
    fn clear(&self) {
        *self.image.lock().unwrap() = None;
        *self.payload.lock().unwrap() = None;
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct ScreenshotPayload {
    pub mime_type: String,
    pub data_base64: String,
}

pub fn capture_primary_monitor() -> Result<ScreenshotPayload> {
    let img = grab_primary()?;
    encode_png(&img)
}

/// Pre-create the hidden capture window at app startup so the first region
/// screenshot doesn't pay for WebView2 cold-init. Reused across captures.
pub fn precreate_capture_window(app: &AppHandle) -> Result<()> {
    if app.get_webview_window("capture").is_some() {
        return Ok(());
    }

    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("no primary monitor"))?;
    let scale = primary.scale_factor();
    let size = primary.size();
    let pos = primary.position();
    let logical_w = (size.width as f64) / scale;
    let logical_h = (size.height as f64) / scale;
    let logical_x = (pos.x as f64) / scale;
    let logical_y = (pos.y as f64) / scale;

    let win = WebviewWindowBuilder::new(app, "capture", WebviewUrl::App("capture.html".into()))
        .title("")
        .inner_size(logical_w, logical_h)
        .position(logical_x, logical_y)
        .transparent(false)
        .background_color(tauri::webview::Color(0, 0, 0, 255))
        .always_on_top(true)
        .decorations(false)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .shadow(false)
        .build()
        .map_err(|e| anyhow!("pre-create capture window: {e}"))?;

    #[cfg(windows)]
    if let Ok(hwnd) = win.hwnd() {
        let _ = crate::win32::make_stealth(hwnd.0 as isize);
    }
    Ok(())
}

pub async fn start_region_capture(app: AppHandle) -> Result<()> {
    // Capture + encode on a blocking thread (xcap is sync).
    let (img, payload) = tauri::async_runtime::spawn_blocking(|| -> Result<(RgbaImage, ScreenshotPayload)> {
        let img = grab_primary()?;
        let payload = encode_png(&img)?;
        Ok((img, payload))
    })
    .await
    .map_err(|e| anyhow!("blocking task: {e}"))??;

    // Stage both for the selector window.
    let state = app.state::<CaptureState>();
    *state.image.lock().unwrap() = Some(img);
    *state.payload.lock().unwrap() = Some(payload);

    // If the pre-created window exists, signal it to repaint and show.
    if app.get_webview_window("capture").is_some() {
        app.emit("capture://prepare", ()).ok();
        return Ok(());
    }

    // Fallback: create on-demand (cold path).
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("no primary monitor"))?;
    let scale = primary.scale_factor();
    let size = primary.size();
    let pos = primary.position();
    let logical_w = (size.width as f64) / scale;
    let logical_h = (size.height as f64) / scale;
    let logical_x = (pos.x as f64) / scale;
    let logical_y = (pos.y as f64) / scale;

    let win = WebviewWindowBuilder::new(&app, "capture", WebviewUrl::App("capture.html".into()))
        .title("")
        .inner_size(logical_w, logical_h)
        .position(logical_x, logical_y)
        .transparent(false)
        .background_color(tauri::webview::Color(0, 0, 0, 255))
        .always_on_top(true)
        .decorations(false)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .shadow(false)
        .build()
        .map_err(|e| anyhow!("create capture window: {e}"))?;

    #[cfg(windows)]
    if let Ok(hwnd) = win.hwnd() {
        let _ = crate::win32::make_stealth(hwnd.0 as isize);
    }

    Ok(())
}

/// Called from the capture window once it has the payload + painted bg.
pub fn get_pending_screenshot(app: &AppHandle) -> Option<ScreenshotPayload> {
    app.state::<CaptureState>().payload.lock().unwrap().clone()
}

/// Called from JS after the bg is painted so the user never sees a white frame.
pub fn show_capture_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("capture") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn finish_region_capture(
    app: AppHandle,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    device_pixel_ratio: f64,
) -> Result<ScreenshotPayload> {
    let state = app.state::<CaptureState>();
    let img = state
        .image
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| anyhow!("no cached screenshot — call start_region_capture first"))?;
    state.payload.lock().unwrap().take();

    if width == 0 || height == 0 {
        return Err(anyhow!("zero-size selection"));
    }

    let dpr = if device_pixel_ratio > 0.0 { device_pixel_ratio } else { 1.0 };
    let px = ((x as f64) * dpr).round() as u32;
    let py = ((y as f64) * dpr).round() as u32;
    let pw = ((width as f64) * dpr).round() as u32;
    let ph = ((height as f64) * dpr).round() as u32;

    let img_w = img.width();
    let img_h = img.height();
    let cx = px.min(img_w.saturating_sub(1));
    let cy = py.min(img_h.saturating_sub(1));
    let cw = pw.min(img_w - cx);
    let ch = ph.min(img_h - cy);

    let cropped = img.view(cx, cy, cw, ch).to_image();
    let payload = encode_png(&cropped)?;

    // Hide (not destroy) so the next capture is instant.
    if let Some(w) = app.get_webview_window("capture") {
        let _ = w.hide();
    }
    Ok(payload)
}

pub fn cancel_region_capture(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("capture") {
        let _ = w.hide();
    }
    app.state::<CaptureState>().clear();
}

fn grab_primary() -> Result<RgbaImage> {
    let monitors = xcap::Monitor::all().map_err(|e| anyhow!("xcap::Monitor::all: {e}"))?;
    let monitor = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| xcap::Monitor::all().ok().and_then(|m| m.into_iter().next()))
        .ok_or_else(|| anyhow!("no monitor found"))?;
    monitor
        .capture_image()
        .map_err(|e| anyhow!("monitor.capture_image: {e}"))
}

fn encode_png(img: &RgbaImage) -> Result<ScreenshotPayload> {
    let mut buf = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(img.as_raw(), img.width(), img.height(), ColorType::Rgba8.into())
        .map_err(|e| anyhow!("PNG encode: {e}"))?;
    Ok(ScreenshotPayload {
        mime_type: "image/png".into(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&buf),
    })
}
