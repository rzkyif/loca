//! Monitor enumeration, full-monitor capture, and the region-capture overlay flow.

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Position, Size, State};

#[derive(serde::Serialize)]
pub struct CaptureMonitorInfo {
    pub id: String,
    pub name: String,
    #[serde(rename = "isPrimary")]
    pub is_primary: bool,
    /// Physical-pixel top-left x of the monitor in the global virtual desktop.
    /// Lets the JS side match this xcap monitor against Tauri's
    /// `currentMonitor()` position when launching the region-capture overlay,
    /// without having to rely on monitor names lining up across the two APIs.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// List attached monitors for the screen-capture picker.
#[tauri::command]
pub async fn list_capture_monitors() -> AppResult<Vec<CaptureMonitorInfo>> {
    let monitors = xcap::Monitor::all().map_err(|e| AppError::external(e.to_string()))?;
    let mut out = Vec::new();
    for (idx, m) in monitors.iter().enumerate() {
        let id = m
            .id()
            .map(|v| v.to_string())
            .unwrap_or_else(|_| idx.to_string());
        let name = m.name().unwrap_or_else(|_| format!("Monitor {}", idx + 1));
        let is_primary = m.is_primary().unwrap_or(false);
        let x = m.x().unwrap_or(0);
        let y = m.y().unwrap_or(0);
        let width = m.width().unwrap_or(0);
        let height = m.height().unwrap_or(0);
        out.push(CaptureMonitorInfo {
            id,
            name,
            is_primary,
            x,
            y,
            width,
            height,
        });
    }
    Ok(out)
}

/// Capture the named monitor and return a base64-encoded PNG.
#[tauri::command]
pub async fn capture_monitor(monitor_id: String) -> AppResult<String> {
    use base64::Engine;
    let monitors = xcap::Monitor::all().map_err(|e| AppError::external(e.to_string()))?;
    let monitor = monitors
        .into_iter()
        .enumerate()
        .find(|(idx, m)| {
            m.id()
                .ok()
                .map(|v| v.to_string() == monitor_id)
                .unwrap_or(false)
                || idx.to_string() == monitor_id
        })
        .map(|(_, m)| m)
        .ok_or_else(|| AppError::not_found("Monitor not found"))?;

    let image = monitor
        .capture_image()
        .map_err(|e| AppError::external(e.to_string()))?;

    let mut buf: Vec<u8> = Vec::new();
    let dyn_img = image::DynamicImage::ImageRgba8(image);
    dyn_img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;

    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Position and size the `region-capture` overlay window over the same
/// monitor the main window is currently on, then show it. Returns the xcap
/// monitor id that matches that monitor (so the JS helper can stash it for
/// `capture_monitor_region` to use).
///
/// All the geometry math lives here rather than in JS because Tauri exposes
/// monitor bounds in physical pixels with a separate scale factor; getting
/// the logical-coordinate conversion right across macOS retina + multi-
/// monitor setups in JS is error-prone.
#[tauri::command]
pub fn show_region_capture_overlay(app: AppHandle) -> AppResult<String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| AppError::not_found("Main window missing"))?;
    let target_monitor = main
        .current_monitor()?
        .or(app.primary_monitor()?)
        .ok_or_else(|| AppError::not_found("No monitor available"))?;

    let scale = target_monitor.scale_factor();
    let pos = target_monitor.position();
    let sz = target_monitor.size();
    let logical_x = pos.x as f64 / scale;
    let logical_y = pos.y as f64 / scale;
    let logical_w = sz.width as f64 / scale;
    let logical_h = sz.height as f64 / scale;

    let overlay = app
        .get_webview_window("region-capture")
        .ok_or_else(|| AppError::not_found("region-capture window missing"))?;

    overlay.set_size(Size::Logical(LogicalSize::new(logical_w, logical_h)))?;
    overlay.set_position(Position::Logical(LogicalPosition::new(
        logical_x, logical_y,
    )))?;
    overlay.show()?;
    overlay.set_focus()?;

    // Match the same monitor on the xcap side so the cropping command
    // operates on the same physical pixels we just covered with the overlay.
    // Match by physical position; fall back to the primary monitor.
    let xcap_monitors = xcap::Monitor::all().map_err(|e| AppError::external(e.to_string()))?;
    let mut chosen_id: Option<String> = None;
    for (idx, m) in xcap_monitors.iter().enumerate() {
        let mx = m.x().unwrap_or(0);
        let my = m.y().unwrap_or(0);
        if mx == pos.x && my == pos.y {
            chosen_id = Some(
                m.id()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| idx.to_string()),
            );
            break;
        }
    }
    if chosen_id.is_none() {
        for (idx, m) in xcap_monitors.iter().enumerate() {
            if m.is_primary().unwrap_or(false) {
                chosen_id = Some(
                    m.id()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|_| idx.to_string()),
                );
                break;
            }
        }
    }
    Ok(chosen_id.unwrap_or_else(|| "primary".to_string()))
}

/// Hide the `region-capture` overlay window. Called from the JS helper after
/// the user finishes / cancels selection.
#[tauri::command]
pub fn hide_region_capture_overlay(app: AppHandle) -> AppResult<()> {
    if let Some(overlay) = app.get_webview_window("region-capture") {
        overlay.hide()?;
    }
    Ok(())
}

/// Stash the monitor id the next `capture_monitor_region` should crop
/// against. The region-capture overlay page reads this on mount via
/// `get_region_capture_target` so the overlay knows which xcap monitor to
/// pass back. Set by the JS helper before showing the overlay window.
#[tauri::command]
pub fn set_region_capture_target(state: State<AppState>, monitor_id: String) -> AppResult<()> {
    let mut t =
        state.0.region_capture_target.lock().map_err(|e| {
            AppError::external(format!("region_capture_target mutex poisoned: {e}"))
        })?;
    *t = monitor_id;
    Ok(())
}

/// Read the monitor id stashed by `set_region_capture_target`. Returns
/// "primary" when nothing has been set yet.
#[tauri::command]
pub fn get_region_capture_target(state: State<AppState>) -> AppResult<String> {
    let t =
        state.0.region_capture_target.lock().map_err(|e| {
            AppError::external(format!("region_capture_target mutex poisoned: {e}"))
        })?;
    Ok(t.clone())
}

/// Capture a rectangular region of the named monitor and return a
/// base64-encoded PNG. Coordinates are physical pixels relative to the
/// monitor's top-left. Out-of-bounds rectangles are clamped to the monitor.
#[tauri::command]
pub async fn capture_monitor_region(
    monitor_id: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> AppResult<String> {
    use base64::Engine;
    let monitors = xcap::Monitor::all().map_err(|e| AppError::external(e.to_string()))?;
    let monitor = monitors
        .into_iter()
        .enumerate()
        .find(|(idx, m)| {
            m.id()
                .ok()
                .map(|v| v.to_string() == monitor_id)
                .unwrap_or(false)
                || idx.to_string() == monitor_id
        })
        .map(|(_, m)| m)
        .ok_or_else(|| AppError::not_found("Monitor not found"))?;

    let image = monitor
        .capture_image()
        .map_err(|e| AppError::external(e.to_string()))?;

    let img_w = image.width();
    let img_h = image.height();
    // Clamp the rect to the monitor's physical bounds.
    let crop_x = x.max(0) as u32;
    let crop_y = y.max(0) as u32;
    let crop_x = crop_x.min(img_w);
    let crop_y = crop_y.min(img_h);
    let crop_w = width.min(img_w.saturating_sub(crop_x));
    let crop_h = height.min(img_h.saturating_sub(crop_y));
    if crop_w == 0 || crop_h == 0 {
        return Err(AppError::external("Region has zero area".to_string()));
    }

    let dyn_img = image::DynamicImage::ImageRgba8(image).crop_imm(crop_x, crop_y, crop_w, crop_h);
    let mut buf: Vec<u8> = Vec::new();
    dyn_img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;

    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}
