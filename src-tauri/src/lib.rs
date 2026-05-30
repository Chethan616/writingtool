//! Writing Agent — Tauri backend.
//!
//! Wires up two windows (Settings + overlay), the system tray, global
//! shortcuts, the Gemini client and the Win32 stealth / typing layer.

mod capture;
mod gemini;
#[cfg(windows)]
mod win32;

use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewWindow, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[cfg(windows)]
type Typing = Arc<win32::TypingState>;

#[cfg(not(windows))]
type Typing = Arc<()>;

struct AppState {
    typing: Typing,
}

#[tauri::command]
fn apply_stealth(window: WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        let raw = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        win32::make_stealth(raw).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(())
    }
}

#[tauri::command]
async fn ask_gemini(
    app: AppHandle,
    request_id: String,
    api_key: String,
    model: String,
    history: Vec<gemini::ChatMessage>,
    prompt: String,
    images: Vec<gemini::ImageAttachment>,
    system_prompt: String,
) -> Result<(), String> {
    let app2 = app.clone();
    let rid = request_id.clone();
    tauri::async_runtime::spawn(async move {
        let err = match gemini::stream(app2.clone(), rid.clone(), api_key, model, history, prompt, images, system_prompt).await {
            Ok(()) => None,
            Err(e) => Some(e.to_string()),
        };
        gemini::emit_done(&app2, rid, err);
    });
    Ok(())
}

#[tauri::command]
fn type_text(
    state: State<'_, AppState>,
    app: AppHandle,
    text: String,
    delay_ms: u32,
    jitter_ms: u32,
    human: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let st = state.typing.clone();
        let total_emit = text.chars().count();
        let app_start = app.clone();
        app_start
            .emit(
                "typing://progress",
                serde_json::json!({"current": 0, "total": total_emit, "paused": false, "done": false}),
            )
            .ok();
        std::thread::spawn(move || {
            let app_cb = app.clone();
            let result = st.type_text(&text, delay_ms, jitter_ms, human, move |cur, total, paused| {
                app_cb
                    .emit(
                        "typing://progress",
                        serde_json::json!({
                            "current": cur,
                            "total": total,
                            "paused": paused,
                            "done": false,
                        }),
                    )
                    .ok();
            });
            let (cur, total) = st.progress();
            app.emit(
                "typing://progress",
                serde_json::json!({
                    "current": cur,
                    "total": total,
                    "paused": false,
                    "done": true,
                    "error": result.err().map(|e| e.to_string()),
                }),
            )
            .ok();
        });
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (state, app, text, delay_ms, jitter_ms, human);
        Err("typing only supported on Windows".into())
    }
}

#[tauri::command]
fn cancel_typing(state: State<'_, AppState>) {
    #[cfg(windows)]
    { state.typing.cancel(); }
    #[cfg(not(windows))]
    { let _ = state; }
}

#[tauri::command]
fn pause_typing(state: State<'_, AppState>) {
    #[cfg(windows)]
    { state.typing.pause(); }
    #[cfg(not(windows))]
    { let _ = state; }
}

#[tauri::command]
fn resume_typing(state: State<'_, AppState>) {
    #[cfg(windows)]
    { state.typing.resume(); }
    #[cfg(not(windows))]
    { let _ = state; }
}

#[tauri::command]
fn toggle_overlay(app: AppHandle) -> Result<(), String> {
    do_toggle(&app).map_err(|e| e.to_string())
}

fn do_toggle(app: &AppHandle) -> tauri::Result<()> {
    if let Some(win) = app.get_webview_window("overlay") {
        if win.is_visible()? {
            win.hide()?;
        } else {
            win.show()?;
            win.set_focus().ok();
            app.emit("overlay://focus-input", ()).ok();
        }
    }
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn show_main(app: AppHandle) -> Result<(), String> {
    show_main_window(&app);
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn screenshot_full(app: AppHandle) -> Result<capture::ScreenshotPayload, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || capture::capture_primary_monitor())
        .await
        .map_err(|e| e.to_string())?
        .map(|p| {
            app2.emit("screenshot://captured", p.clone()).ok();
            p
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn screenshot_region(app: AppHandle) -> Result<(), String> {
    capture::start_region_capture(app).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn screenshot_region_finish(
    app: AppHandle,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    dpr: f64,
) -> Result<capture::ScreenshotPayload, String> {
    let app2 = app.clone();
    let payload = capture::finish_region_capture(app, x, y, width, height, dpr)
        .map_err(|e| e.to_string())?;
    app2.emit("screenshot://captured", payload.clone()).ok();
    Ok(payload)
}

#[tauri::command]
fn screenshot_region_cancel(app: AppHandle) {
    capture::cancel_region_capture(&app);
}

#[tauri::command]
fn get_pending_screenshot(app: AppHandle) -> Option<capture::ScreenshotPayload> {
    capture::get_pending_screenshot(&app)
}

#[tauri::command]
fn show_capture_window(app: AppHandle) {
    capture::show_capture_window(&app);
}

#[tauri::command]
fn move_overlay(app: AppHandle, dx: i32, dy: i32) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("overlay") {
        let pos = win.outer_position().map_err(|e| e.to_string())?;
        let new = tauri::PhysicalPosition::new(pos.x + dx, pos.y + dy);
        win.set_position(tauri::Position::Physical(new)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Resize overlay to fit its content. Width/height are logical (CSS) pixels.
#[tauri::command]
fn resize_overlay(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("overlay") {
        let size = tauri::LogicalSize::new(width.max(320.0), height.max(56.0));
        win.set_size(tauri::Size::Logical(size)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    let typing: Typing = Arc::new(win32::TypingState::new());
    #[cfg(not(windows))]
    let typing: Typing = Arc::new(());

    let state = AppState { typing };
    let capture_state = capture::CaptureState::new();

    let toggle_shortcut =
        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
    let submit_shortcut =
        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Enter);
    let screenshot_full_shortcut =
        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyS);
    let screenshot_region_shortcut =
        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyR);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    if shortcut == &toggle_shortcut {
                        let _ = do_toggle(app);
                    } else if shortcut == &submit_shortcut {
                        app.emit("overlay://submit", ()).ok();
                    } else if shortcut == &screenshot_full_shortcut {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            match capture::capture_primary_monitor() {
                                Ok(p) => { app.emit("screenshot://captured", p).ok(); }
                                Err(e) => { app.emit("screenshot://error", e.to_string()).ok(); }
                            }
                        });
                    } else if shortcut == &screenshot_region_shortcut {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = capture::start_region_capture(app.clone()).await {
                                app.emit("screenshot://error", e.to_string()).ok();
                            }
                        });
                    }
                })
                .build(),
        )
        .manage(state)
        .manage(capture_state)
        .setup(move |app| {
            let handle = app.handle();

            handle.global_shortcut().register(toggle_shortcut).ok();
            handle.global_shortcut().register(submit_shortcut).ok();
            handle.global_shortcut().register(screenshot_full_shortcut).ok();
            handle.global_shortcut().register(screenshot_region_shortcut).ok();

            // ---- System tray ----
            let mi_show =
                MenuItem::with_id(app, "show_main", "Open Settings", true, None::<&str>)?;
            let mi_toggle = MenuItem::with_id(
                app,
                "toggle",
                "Toggle Overlay  (Ctrl+Shift+Space)",
                true,
                None::<&str>,
            )?;
            let sep = PredefinedMenuItem::separator(app)?;
            let mi_quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let tray_menu =
                Menu::with_items(app, &[&mi_show, &mi_toggle, &sep, &mi_quit])?;

            let icon = app
                .default_window_icon()
                .cloned()
                .expect("missing default window icon");

            TrayIconBuilder::with_id("tray")
                .icon(icon)
                .tooltip("Writing Agent")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show_main" => show_main_window(app),
                    "toggle" => {
                        let _ = do_toggle(app);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = do_toggle(tray.app_handle());
                    }
                })
                .build(app)?;

            // ---- Close main window to tray instead of quitting ----
            if let Some(main) = handle.get_webview_window("main") {
                let h = handle.clone();
                main.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Some(win) = h.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    }
                });
            }

            // ---- Stealth on overlay ----
            #[cfg(windows)]
            {
                if let Some(overlay) = handle.get_webview_window("overlay") {
                    if let Ok(hwnd) = overlay.hwnd() {
                        let _ = win32::make_stealth(hwnd.0 as isize);
                    }
                }
            }

            // ---- Pre-create the capture window so the first region screenshot
            //      doesn't pay for WebView2 cold-init. ----
            let _ = capture::precreate_capture_window(handle);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            apply_stealth,
            ask_gemini,
            type_text,
            cancel_typing,
            pause_typing,
            resume_typing,
            toggle_overlay,
            show_main,
            quit_app,
            screenshot_full,
            screenshot_region,
            screenshot_region_finish,
            screenshot_region_cancel,
            get_pending_screenshot,
            show_capture_window,
            move_overlay,
            resize_overlay,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
