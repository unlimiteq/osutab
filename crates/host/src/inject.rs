//! Windows input output: absolute mouse, tip click and pen button, all through SendInput.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use osutab_core::{Sample, FLAG_BUTTON, FLAG_IN_RANGE, FLAG_TOUCH};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::mapping::{to_absolute, Rect};

// Global so the console handler can release anything held when the program is stopped.
static LEFT_HELD: AtomicBool = AtomicBool::new(false);
static KEY_HELD_SCAN: AtomicU16 = AtomicU16::new(0);

pub fn primary_display() -> Rect {
    unsafe {
        Rect {
            left: 0.0,
            top: 0.0,
            width: GetSystemMetrics(SM_CXSCREEN) as f64,
            height: GetSystemMetrics(SM_CYSCREEN) as f64,
        }
    }
}

pub fn virtual_desktop() -> Rect {
    unsafe {
        Rect {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN) as f64,
            top: GetSystemMetrics(SM_YVIRTUALSCREEN) as f64,
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as f64,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as f64,
        }
    }
}

pub fn cursor_position() -> (i32, i32) {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

pub fn set_cursor_position(x: i32, y: i32) {
    unsafe { SetCursorPos(x, y) };
}

/// Accepts a single letter or digit, such as `z`.
pub fn scan_code_for_key(name: &str) -> Option<u16> {
    let mut chars = name.chars();
    let c = chars.next()?.to_ascii_uppercase();
    if chars.next().is_some() || !c.is_ascii_alphanumeric() {
        return None;
    }
    match unsafe { MapVirtualKeyW(c as u32, MAPVK_VK_TO_VSC) } {
        0 => None,
        scan => Some(scan as u16),
    }
}

fn mouse_input(dx: i32, dy: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key_input(scan: u16, up: bool) -> INPUT {
    let flags = KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { 0 };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: 0, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
}

/// Sends a mouse move of zero distance. It goes through the same path as a real move, including
/// any global mouse hooks, without moving the cursor.
pub fn send_zero_move() {
    send(&[mouse_input(0, 0, MOUSEEVENTF_MOVE)]);
}

/// Releases any held mouse button or key. Safe to call from any thread.
pub fn release_all() {
    if LEFT_HELD.swap(false, Ordering::SeqCst) {
        send(&[mouse_input(0, 0, MOUSEEVENTF_LEFTUP)]);
    }
    let scan = KEY_HELD_SCAN.swap(0, Ordering::SeqCst);
    if scan != 0 {
        send(&[key_input(scan, true)]);
    }
}

pub struct Injector {
    pub target: Rect,
    pub desktop: Rect,
    pub dry_run: bool,
    prev_flags: u8,
}

impl Injector {
    pub fn new(target: Rect, desktop: Rect, dry_run: bool) -> Self {
        Injector { target, desktop, dry_run, prev_flags: 0 }
    }

    pub fn reset(&mut self) {
        release_all();
        self.prev_flags = 0;
    }

    /// `u` and `v` are the pen position on the target in 0..1.
    pub fn apply(&mut self, sample: &Sample, u: f64, v: f64, tip_click: bool, button_scan: Option<u16>) {
        let flags = sample.flags;
        let changed = flags ^ self.prev_flags;
        self.prev_flags = flags;
        if self.dry_run {
            return;
        }

        let mut inputs: [INPUT; 3] = unsafe { std::mem::zeroed() };
        let mut count = 0;
        if flags & FLAG_IN_RANGE != 0 {
            // Position goes first so a press lands where the pen is.
            let (dx, dy) = to_absolute(u, v, &self.target, &self.desktop);
            inputs[count] = mouse_input(dx, dy, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
            count += 1;
        }
        if tip_click && changed & FLAG_TOUCH != 0 {
            let down = flags & FLAG_TOUCH != 0;
            LEFT_HELD.store(down, Ordering::SeqCst);
            inputs[count] = mouse_input(0, 0, if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP });
            count += 1;
        }
        if let Some(scan) = button_scan {
            if changed & FLAG_BUTTON != 0 {
                let down = flags & FLAG_BUTTON != 0;
                KEY_HELD_SCAN.store(if down { scan } else { 0 }, Ordering::SeqCst);
                inputs[count] = key_input(scan, !down);
                count += 1;
            }
        }
        if count > 0 {
            send(&inputs[..count]);
        }
    }
}
