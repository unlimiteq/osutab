//! Windows input output: absolute mouse, tip click and pen button, all through SendInput.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use osutab_core::{Sample, FLAG_BUTTON, FLAG_IN_RANGE, FLAG_TOUCH};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::keys::{key_fields, Combo, MOST_KEYS};
use crate::mapping::{to_absolute, Rect};

// Global so the console handler can release anything held when the program is stopped.
static LEFT_HELD: AtomicBool = AtomicBool::new(false);
static KEYS_HELD: AtomicU64 = AtomicU64::new(0);

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

/// The file name of the program in front, for the log.
pub fn foreground_program() -> String {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return format!("process {pid}");
        }
        let mut name = [0u16; 260];
        let mut len = name.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return format!("process {pid}");
        }
        let path = String::from_utf16_lossy(&name[..len as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }
}

pub fn set_cursor_position(x: i32, y: i32) {
    unsafe { SetCursorPos(x, y) };
}

fn mouse_input(dx: i32, dy: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key_input(code: u16, up: bool) -> INPUT {
    let (vk, scan, flags) = key_fields(code, up);
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
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
    let held = Combo::from_bits(KEYS_HELD.swap(0, Ordering::SeqCst));
    let ups: Vec<INPUT> = key_order(held, false).into_iter().map(|code| key_input(code, true)).collect();
    if !ups.is_empty() {
        send(&ups);
    }
}

/// Keys go down in order, so Shift is held before 2, and come up the other way round.
fn key_order(keys: Combo, down: bool) -> Vec<u16> {
    if down {
        keys.keys().collect()
    } else {
        keys.keys().rev().collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ButtonEdges {
    /// Press or release the left mouse button.
    left: Option<bool>,
    /// Press or release these keys.
    keys: Option<(Combo, bool)>,
}

/// What to send when the pen's state changes. Settings decide what a press does, but a release
/// always follows what's actually held. Otherwise, changing a setting during a press would mean
/// the release never comes, and the click or key stays stuck down.
fn button_edges(
    flags: u8,
    changed: u8,
    tip_click: bool,
    button_keys: Option<Combo>,
    left_held: bool,
    held: Combo,
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
            edges.keys = button_keys.map(|keys| (keys, true));
        } else if !held.is_empty() {
            edges.keys = Some((held, false));
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
    pub fn apply(&mut self, sample: &Sample, u: f64, v: f64, tip_click: bool, button_keys: Option<Combo>) {
        let flags = sample.flags;
        let changed = flags ^ self.prev_flags;
        self.prev_flags = flags;
        if self.dry_run {
            return;
        }

        let mut inputs: [INPUT; 2 + MOST_KEYS] = unsafe { std::mem::zeroed() };
        let mut count = 0;
        if flags & FLAG_IN_RANGE != 0 {
            // Position goes first so a press lands where the pen is.
            let (dx, dy) = to_absolute(u, v, &self.target, &self.desktop);
            inputs[count] = mouse_input(dx, dy, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
            count += 1;
        }
        let held = Combo::from_bits(KEYS_HELD.load(Ordering::SeqCst));
        let edges = button_edges(flags, changed, tip_click, button_keys, LEFT_HELD.load(Ordering::SeqCst), held);
        if let Some(down) = edges.left {
            LEFT_HELD.store(down, Ordering::SeqCst);
            inputs[count] = mouse_input(0, 0, if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP });
            count += 1;
        }
        if let Some((keys, down)) = edges.keys {
            KEYS_HELD.store(if down { keys.to_bits() } else { 0 }, Ordering::SeqCst);
            for code in key_order(keys, down) {
                inputs[count] = key_input(code, !down);
                count += 1;
            }
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
    const SHIFT: u16 = 0x2a;
    const ALT: u16 = 0x38;
    const TWO: u16 = 0x03;
    const NONE: Combo = Combo::EMPTY;

    fn key(code: u16) -> Combo {
        Combo::new(&[code]).unwrap()
    }

    #[test]
    fn normal_press_and_release() {
        let press = button_edges(FLAG_TOUCH | FLAG_BUTTON, FLAG_TOUCH | FLAG_BUTTON, true, Some(key(Z)), false, NONE);
        assert_eq!(press, ButtonEdges { left: Some(true), keys: Some((key(Z), true)) });
        let release = button_edges(0, FLAG_TOUCH | FLAG_BUTTON, true, Some(key(Z)), true, key(Z));
        assert_eq!(release, ButtonEdges { left: Some(false), keys: Some((key(Z), false)) });
    }

    #[test]
    fn nothing_changed_sends_nothing() {
        assert_eq!(button_edges(FLAG_TOUCH, 0, true, Some(key(Z)), true, NONE), ButtonEdges::default());
    }

    #[test]
    fn turning_tip_click_off_during_a_press_still_releases() {
        assert_eq!(button_edges(0, FLAG_TOUCH, false, None, true, NONE).left, Some(false));
    }

    #[test]
    fn turning_tip_click_off_stops_new_presses() {
        assert_eq!(button_edges(FLAG_TOUCH, FLAG_TOUCH, false, None, false, NONE).left, None);
    }

    #[test]
    fn no_release_is_sent_for_a_click_that_never_happened() {
        assert_eq!(button_edges(0, FLAG_TOUCH, true, None, false, NONE).left, None);
    }

    #[test]
    fn changing_the_key_during_a_press_releases_the_one_held() {
        assert_eq!(button_edges(0, FLAG_BUTTON, true, Some(key(X)), false, key(Z)).keys, Some((key(Z), false)));
    }

    #[test]
    fn clearing_the_key_during_a_press_still_releases() {
        assert_eq!(button_edges(0, FLAG_BUTTON, true, None, false, key(Z)).keys, Some((key(Z), false)));
    }

    #[test]
    fn a_combination_goes_down_in_order_and_comes_up_the_other_way_round() {
        let combo = Combo::new(&[SHIFT, ALT, TWO]).unwrap();
        let press = button_edges(FLAG_BUTTON, FLAG_BUTTON, true, Some(combo), false, NONE);
        assert_eq!(press.keys, Some((combo, true)));
        assert_eq!(key_order(combo, true), vec![SHIFT, ALT, TWO]);
        assert_eq!(key_order(combo, false), vec![TWO, ALT, SHIFT]);
        // Changed to one key mid-press, all three still come up.
        let release = button_edges(0, FLAG_BUTTON, true, Some(key(Z)), false, combo);
        assert_eq!(release.keys, Some((combo, false)));
    }
}
