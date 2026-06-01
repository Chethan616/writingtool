//! Win32 stealth + keystroke synthesis.
//!
//! - [`make_stealth`] hides a window from screen capture
//!   ([`WDA_EXCLUDEFROMCAPTURE`]) and from Alt-Tab / taskbar
//!   ([`WS_EX_TOOLWINDOW`]), and stops it stealing focus on click
//!   ([`WS_EX_NOACTIVATE`]).
//! - [`TypingState`] drives `SendInput`-based typing with pause / resume /
//!   cancel, exposing progress via a user-supplied callback. Optional human
//!   mode adds neighbor-key typos with backspace corrections, variable
//!   cadence around punctuation, and rare thinking pauses.
//!
//! All entry points are no-ops on non-Windows targets via `#![cfg(windows)]`.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rand::Rng;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, SendInput, SetFocus, ToUnicodeEx,
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_MENU, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
    IsWindowVisible, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
    SetWindowsHookExW, ShowWindow,
    GWL_EXSTYLE, HC_ACTION, HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
    WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

pub fn make_stealth(hwnd_raw: isize) -> Result<()> {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
            .map_err(|e| anyhow!("SetWindowDisplayAffinity failed: {e}"))?;
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new = cur | (WS_EX_TOOLWINDOW.0 as isize) | (WS_EX_NOACTIVATE.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
    }
    Ok(())
}

/// Show the overlay over a fullscreen foreground app without triggering a
/// foreground change. The naive path (`ShowWindow` + `SetForegroundWindow`)
/// kicks the previously-foreground app out of fullscreen and pops the Windows
/// taskbar. Instead:
///   1. `ShowWindow(SW_SHOWNOACTIVATE)` — visible, not activated.
///   2. `SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE)` — topmost without activation.
///   3. AttachThreadInput trick — attach our input queue to the foreground
///      thread, then `SetFocus(hwnd)` (NOT `SetForegroundWindow`), then detach.
///      Windows sees no foreground change, so no taskbar pop, no fullscreen
///      exit; but the overlay's input field is focused and ready to type.
/// Raw Win32 visibility check — bypasses Tauri's caching path. Because we
/// show the overlay via [`show_overlay_no_steal`] using `ShowWindow` directly,
/// the Tauri-side `is_visible()` accessor can occasionally desync; reading
/// the OS state directly via `IsWindowVisible` is the safe source of truth.
#[allow(dead_code)]
pub fn is_overlay_visible(hwnd_raw: isize) -> bool {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Raw Win32 hide. Pair with [`show_overlay_no_steal`] so the show/hide cycle
/// is symmetric and the overlay reliably toggles via `Ctrl+Shift+Space` and
/// the X button.
#[allow(dead_code)]
pub fn hide_overlay_raw(hwnd_raw: isize) {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
}

/// Toggle the WS_EX_TRANSPARENT flag so mouse clicks pass through when the
/// overlay is CSS-hidden. Avoids the OS-level show/hide that would otherwise
/// pop the Windows taskbar over a fullscreen foreground app. When `clickable`
/// is true, the overlay receives clicks normally; when false, clicks fall
/// through to whatever sits behind it.
pub fn set_overlay_clickable(hwnd_raw: isize, clickable: bool) {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let transparent = WS_EX_TRANSPARENT.0 as isize;
        let new = if clickable { cur & !transparent } else { cur | transparent };
        if new != cur {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Low-level keyboard hook (WH_KEYBOARD_LL).
//
// We never grab OS keyboard focus for the overlay — doing so would cause
// fullscreen apps like Chrome/VSCode F11 to detect WM_KILLFOCUS and exit
// fullscreen, which un-hides the Windows taskbar. Instead a global LL hook
// observes every keystroke and, when `CAPTURE_ACTIVE` is set, forwards it
// to the overlay (via a worker thread + Tauri event) while consuming it so
// the foreground app doesn't double-receive the key. The hook itself is
// transparent (CallNextHookEx) the rest of the time.
// ──────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct KbdEvent {
    pub vk: u32,
    pub scan: u32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub text: Option<String>,
}

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static KBD_TX: Mutex<Option<Sender<KbdEvent>>> = Mutex::new(None);

pub fn install_kbd_hook(app: AppHandle) -> Result<()> {
    // Set the app handle exactly once. If we've already installed, bail.
    if APP_HANDLE.set(app).is_err() {
        return Ok(());
    }

    let (tx, rx) = mpsc::channel::<KbdEvent>();
    *KBD_TX.lock() = Some(tx);

    // Worker thread: drains the channel and emits Tauri events. This keeps
    // the hook proc itself dirt-cheap (just a send) so we never trip
    // Windows' LowLevelHooksTimeout (~300ms) and get silently disabled.
    thread::spawn(move || {
        while let Ok(ev) = rx.recv() {
            if let Some(handle) = APP_HANDLE.get() {
                let _ = handle.emit_to("overlay", "kbd-capture://key", ev);
            }
        }
    });

    unsafe {
        let hmod = GetModuleHandleW(None)
            .map_err(|e| anyhow!("GetModuleHandleW failed: {e}"))?;
        let hinst = HINSTANCE(hmod.0);
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(kbd_hook_proc), hinst, 0)
            .map_err(|e| anyhow!("SetWindowsHookExW failed: {e}"))?;
    }
    Ok(())
}

pub fn set_capture_active(active: bool) {
    CAPTURE_ACTIVE.store(active, Ordering::SeqCst);
}

unsafe extern "system" fn kbd_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 || !CAPTURE_ACTIVE.load(Ordering::SeqCst) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    // Only act on key-down — releases are noise for input emulation.
    let msg = wparam.0 as u32;
    let is_keydown = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    if !is_keydown {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let vk = info.vkCode;
    let scan = info.scanCode;

    let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
    let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
    let alt = (info.flags.0 & LLKHF_ALTDOWN.0) != 0;

    // Passthrough policy — let the user task-switch / close apps as usual.
    // Alt+Tab, Alt+F4, the Win key, and pure modifier presses all skip.
    const VK_TAB_U: u32 = 0x09;
    const VK_F4_U: u32 = 0x73;
    const VK_LWIN_U: u32 = 0x5B;
    const VK_RWIN_U: u32 = 0x5C;
    if alt && (vk == VK_TAB_U || vk == VK_F4_U) {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    if vk == VK_LWIN_U || vk == VK_RWIN_U {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let vk_shift = VK_SHIFT.0 as u32;
    let vk_ctrl = VK_CONTROL.0 as u32;
    let vk_menu = VK_MENU.0 as u32;
    if vk == vk_shift || vk == vk_ctrl || vk == vk_menu {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    // Translate vk + scan + current modifier state → unicode text via
    // ToUnicodeEx. This is the canonical way to honor the user's keyboard
    // layout / dead keys / IME composition. Non-printable keys (Enter,
    // Backspace, arrows) yield control chars which we filter out — the
    // frontend handler dispatches them by `vk` instead.
    let mut text: Option<String> = None;
    let mut state = [0u8; 256];
    if GetKeyboardState(&mut state).is_ok() {
        let mut buf = [0u16; 8];
        let layout = GetKeyboardLayout(0);
        let n = ToUnicodeEx(vk, scan, &state, &mut buf, 0, layout);
        if n > 0 {
            let s = String::from_utf16_lossy(&buf[..n as usize]);
            if !s.chars().all(|c| c.is_control()) {
                text = Some(s);
            }
        }
    }

    let ev = KbdEvent { vk, scan, shift, ctrl, alt, text };

    // Push to worker — never block in the hook proc.
    if let Some(tx) = KBD_TX.lock().as_ref() {
        let _ = tx.send(ev);
    }

    // Consume the keystroke so the foreground app doesn't also receive it.
    LRESULT(1)
}

/// Grab keyboard focus for the overlay without changing the OS foreground.
/// Uses the AttachThreadInput trick: temporarily merge our thread's input
/// queue with the current foreground app's so cross-thread `SetFocus`
/// transfers keyboard focus without triggering a foreground change. Skips
/// `ShowWindow` / `SetWindowPos` entirely — the overlay is kept always-on
/// at the OS level, so we never need to toggle its z-order.
pub fn focus_overlay_no_steal(hwnd_raw: isize) {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() || fg.0 == hwnd.0 {
            return;
        }
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let our_thread = GetCurrentThreadId();
        if fg_thread == 0 || fg_thread == our_thread {
            return;
        }
        let _ = AttachThreadInput(fg_thread, our_thread, BOOL(1));
        let _ = SetFocus(hwnd);
        let _ = AttachThreadInput(fg_thread, our_thread, BOOL(0));
    }
}

#[allow(dead_code)]
pub fn show_overlay_no_steal(hwnd_raw: isize) -> Result<()> {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        let fg = GetForegroundWindow();
        if !fg.0.is_null() && fg.0 != hwnd.0 {
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let our_thread = GetCurrentThreadId();
            if fg_thread != 0 && fg_thread != our_thread {
                let _ = AttachThreadInput(fg_thread, our_thread, BOOL(1));
                let _ = SetFocus(hwnd);
                let _ = AttachThreadInput(fg_thread, our_thread, BOOL(0));
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

#[derive(Default)]
pub struct TypingState {
    cancel: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    active: Mutex<bool>,
    progress: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
}

impl TypingState {
    pub fn new() -> Self { Self::default() }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.pause.store(false, Ordering::SeqCst);
    }
    pub fn pause(&self)  { self.pause.store(true,  Ordering::SeqCst); }
    pub fn resume(&self) { self.pause.store(false, Ordering::SeqCst); }
    #[allow(dead_code)]
    pub fn is_paused(&self) -> bool { self.pause.load(Ordering::SeqCst) }
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool { *self.active.lock() }
    pub fn progress(&self) -> (usize, usize) {
        (self.progress.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }

    /// Type `text`. Blocks the calling thread.
    /// * `human` enables typo/backspace/cadence simulation.
    /// * `on_progress(current_char_index, total, paused)` fires after each char + pause-state changes.
    pub fn type_text<F: Fn(usize, usize, bool)>(
        &self,
        text: &str,
        base_delay_ms: u32,
        jitter_ms: u32,
        human: bool,
        on_progress: F,
    ) -> Result<()> {
        {
            let mut a = self.active.lock();
            if *a { return Err(anyhow!("typing already in progress")); }
            *a = true;
        }
        self.cancel.store(false, Ordering::SeqCst);
        self.pause.store(false, Ordering::SeqCst);

        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();
        self.total.store(total, Ordering::SeqCst);
        self.progress.store(0, Ordering::SeqCst);

        let mut rng = rand::thread_rng();
        let mut last_paused = false;

        for (i, ch) in chars.iter().enumerate() {
            if self.cancel.load(Ordering::SeqCst) { break; }

            // pause loop
            while self.pause.load(Ordering::SeqCst) {
                if !last_paused { on_progress(i, total, true); last_paused = true; }
                thread::sleep(Duration::from_millis(80));
                if self.cancel.load(Ordering::SeqCst) {
                    *self.active.lock() = false;
                    return Ok(());
                }
            }
            if last_paused { on_progress(i, total, false); last_paused = false; }

            // ─── HUMAN: occasional typo + correction ───
            if human && ch.is_ascii_alphabetic() && rng.gen_bool(0.018) {
                if let Some(wrong) = neighbor_key(*ch) {
                    send_char(wrong)?;
                    thread::sleep(Duration::from_millis(rng.gen_range(30..=90)));
                    if !self.cancel.load(Ordering::SeqCst) {
                        send_vk(VK_BACK)?;
                        thread::sleep(Duration::from_millis(rng.gen_range(50..=120)));
                    }
                }
            }

            send_char(*ch)?;
            self.progress.store(i + 1, Ordering::SeqCst);
            on_progress(i + 1, total, false);

            // ─── delay between chars ───
            let delay_ms = if human {
                human_delay(*ch, base_delay_ms, &mut rng)
            } else {
                let extra = if jitter_ms > 0 { rng.gen_range(0..=jitter_ms) } else { 0 };
                base_delay_ms + extra
            };
            thread::sleep(Duration::from_millis(delay_ms as u64));

            // ─── HUMAN: rare thinking pause ───
            if human && rng.gen_bool(0.006) {
                let pause = rng.gen_range(220..=750);
                let mut slept = 0;
                while slept < pause {
                    if self.cancel.load(Ordering::SeqCst) { break; }
                    thread::sleep(Duration::from_millis(50));
                    slept += 50;
                }
            }
        }

        *self.active.lock() = false;
        Ok(())
    }
}

fn human_delay(ch: char, base: u32, rng: &mut impl Rng) -> u32 {
    // Add 30-50% variance around base, longer pauses around punctuation/spaces.
    let (lo_mult, hi_mult) = match ch {
        ' '  => (1.6, 3.2),    // word boundary
        '\n' => (3.0, 5.5),    // newline = end of thought
        '.' | '!' | '?'  => (3.0, 5.0),  // sentence end
        ',' | ';' | ':'  => (1.8, 3.0),  // soft pause
        '('|')'|'{'|'}'|'['|']' => (1.2, 2.0),
        _ => (0.7, 1.4),
    };
    let lo = ((base as f32) * lo_mult) as u32;
    let hi = ((base as f32) * hi_mult).max(lo as f32 + 1.0) as u32;
    rng.gen_range(lo..=hi)
}

fn neighbor_key(c: char) -> Option<char> {
    let lower = c.to_ascii_lowercase();
    let row: &[char] = match lower {
        'q' => &['w', 'a'],
        'w' => &['q', 'e', 's', 'a'],
        'e' => &['w', 'r', 'd', 's'],
        'r' => &['e', 't', 'f', 'd'],
        't' => &['r', 'y', 'g', 'f'],
        'y' => &['t', 'u', 'h', 'g'],
        'u' => &['y', 'i', 'j', 'h'],
        'i' => &['u', 'o', 'k', 'j'],
        'o' => &['i', 'p', 'l', 'k'],
        'p' => &['o', 'l'],
        'a' => &['q', 'w', 's', 'z'],
        's' => &['a', 'w', 'e', 'd', 'x', 'z'],
        'd' => &['s', 'e', 'r', 'f', 'c', 'x'],
        'f' => &['d', 'r', 't', 'g', 'v', 'c'],
        'g' => &['f', 't', 'y', 'h', 'b', 'v'],
        'h' => &['g', 'y', 'u', 'j', 'n', 'b'],
        'j' => &['h', 'u', 'i', 'k', 'm', 'n'],
        'k' => &['j', 'i', 'o', 'l', 'm'],
        'l' => &['k', 'o', 'p'],
        'z' => &['a', 's', 'x'],
        'x' => &['z', 's', 'd', 'c'],
        'c' => &['x', 'd', 'f', 'v'],
        'v' => &['c', 'f', 'g', 'b'],
        'b' => &['v', 'g', 'h', 'n'],
        'n' => &['b', 'h', 'j', 'm'],
        'm' => &['n', 'j', 'k'],
        _ => return None,
    };
    let n = row[rand::thread_rng().gen_range(0..row.len())];
    Some(if c.is_ascii_uppercase() { n.to_ascii_uppercase() } else { n })
}

fn send_char(ch: char) -> Result<()> {
    match ch {
        '\n' | '\r' => send_vk(VK_RETURN),
        '\t' => send_vk(VK_TAB),
        c => send_unicode(c),
    }
}

fn send_vk(vk: VIRTUAL_KEY) -> Result<()> {
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let mut up = down;
    up.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
    let inputs = [down, up];
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n != 2 { return Err(anyhow!("SendInput returned {n}, expected 2")); }
    Ok(())
}

fn send_unicode(c: char) -> Result<()> {
    let mut buf = [0u16; 2];
    let units = c.encode_utf16(&mut buf);
    let mut inputs: Vec<INPUT> = Vec::with_capacity(units.len() * 2);
    for unit in units.iter() {
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: *unit,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let mut up = down;
        up.Anonymous.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        inputs.push(down);
        inputs.push(up);
    }
    let expected = inputs.len() as u32;
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n != expected { return Err(anyhow!("SendInput returned {n}, expected {expected}")); }
    Ok(())
}
