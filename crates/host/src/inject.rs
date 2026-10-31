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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ButtonEdges {
    /// Press or release the left mouse button.
    left: Option<bool>,
    /// Press or release this scan code.
    key: Option<(u16, bool)>,
}

/// What to send when the pen's state changes. Settings decide what a press does, but a release
/// always follows what's actually held. Otherwise, changing a setting during a press would mean
/// the release never comes, and the click or key stays stuck down.
fn button_edges(
    flags: u8,
    changed: u8,
    tip_click: bool,
    button_scan: Option<u16>,
    left_held: bool,
    held_scan: u16,
) -> ButtonEdges {
    let mut edges = ButtonEdges::default();
    if changed & FLAG_TOUCH != 0 {
        if flags & FLAG_TOUCH != 0 {
            if tip_click {
                edges.left = Some(true);
            }
        } else if left_held {
            edges.left = Some(false);
        }
    }
    if changed & FLAG_BUTTON != 0 {
        if flags & FLAG_BUTTON != 0 {
            edges.key = button_scan.map(|scan| (scan, true));
        } else if held_scan != 0 {
            edges.key = Some((held_scan, false));
        }
    }
    edges
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
        let held_scan = KEY_HELD_SCAN.load(Ordering::SeqCst);
        let edges = button_edges(flags, changed, tip_click, button_scan, LEFT_HELD.load(Ordering::SeqCst), held_scan);
        if let Some(down) = edges.left {
            LEFT_HELD.store(down, Ordering::SeqCst);
            inputs[count] = mouse_input(0, 0, if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP });
            count += 1;
        }
        if let Some((scan, down)) = edges.key {
            KEY_HELD_SCAN.store(if down { scan } else { 0 }, Ordering::SeqCst);
            inputs[count] = key_input(scan, !down);
            count += 1;
        }
        if count > 0 {
            send(&inputs[..count]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z: u16 = 0x2c;
    const X: u16 = 0x2d;

    #[test]
    fn normal_press_and_release() {
        let press = button_edges(FLAG_TOUCH | FLAG_BUTTON, FLAG_TOUCH | FLAG_BUTTON, true, Some(Z), false, 0);
        assert_eq!(press, ButtonEdges { left: Some(true), key: Some((Z, true)) });
        let release = button_edges(0, FLAG_TOUCH | FLAG_BUTTON, true, Some(Z), true, Z);
        assert_eq!(release, ButtonEdges { left: Some(false), key: Some((Z, false)) });
    }

    #[test]
    fn nothing_changed_sends_nothing() {
        assert_eq!(button_edges(FLAG_TOUCH, 0, true, Some(Z), true, 0), ButtonEdges::default());
    }

    #[test]
    fn turning_tip_click_off_during_a_press_still_releases() {
        assert_eq!(button_edges(0, FLAG_TOUCH, false, None, true, 0).left, Some(false));
    }

    #[test]
    fn turning_tip_click_off_stops_new_presses() {
        assert_eq!(button_edges(FLAG_TOUCH, FLAG_TOUCH, false, None, false, 0).left, None);
    }

    #[test]
    fn no_release_is_sent_for_a_click_that_never_happened() {
        assert_eq!(button_edges(0, FLAG_TOUCH, true, None, false, 0).left, None);
    }

    #[test]
    fn changing_the_key_during_a_press_releases_the_one_held() {
        assert_eq!(button_edges(0, FLAG_BUTTON, true, Some(X), false, Z).key, Some((Z, false)));
    }

    #[test]
    fn clearing_the_key_during_a_press_still_releases() {
        assert_eq!(button_edges(0, FLAG_BUTTON, true, None, false, Z).key, Some((Z, false)));
    }
}
