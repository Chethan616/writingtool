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
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rand::Rng;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_RETURN, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, SetWindowDisplayAffinity, SetWindowLongPtrW,
    GWL_EXSTYLE, WDA_EXCLUDEFROMCAPTURE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
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
