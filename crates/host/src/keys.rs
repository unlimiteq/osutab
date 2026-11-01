//! Keyboard keys for the pen's side button: their names, how each one is sent, and catching the
//! next one pressed.
//!
//! A key is held as a code. `0x00XX` is a scan code, `0xE0XX` an extended scan code such as the
//! arrows or Right Ctrl, and `0xE1XX` a virtual key, for the few keys a scan code can't send.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP};

const EXTENDED: u16 = 0xE000;
const VIRTUAL: u16 = 0xE100;

/// Every key with a name of its own, the name first and then other spellings. Letters and digits
/// aren't here, because where they sit depends on the keyboard layout. Scan codes are positions,
/// so `` ` `` here means the key to the left of 1, whatever a layout prints on it.
const NAMED: &[(u16, &[&str])] = &[
    (0x01, &["Esc", "escape"]),
    (0x0C, &["-", "minus"]),
    (0x0D, &["=", "equals"]),
    (0x0E, &["Backspace"]),
    (0x0F, &["Tab"]),
    (0x1A, &["[", "leftbracket"]),
    (0x1B, &["]", "rightbracket"]),
    (0x1C, &["Enter", "return"]),
    (0x1D, &["Left Ctrl", "ctrl", "lctrl"]),
    (0x27, &[";", "semicolon"]),
    (0x28, &["'", "quote"]),
    (0x29, &["`", "grave", "backtick", "tilde"]),
    (0x2A, &["Left Shift", "shift", "lshift"]),
    (0x2B, &["\\", "backslash"]),
    (0x33, &[",", "comma"]),
    (0x34, &[".", "period"]),
    (0x35, &["/", "slash"]),
    (0x36, &["Right Shift", "rshift"]),
    (0x37, &["Numpad *", "multiply"]),
    (0x38, &["Left Alt", "alt", "lalt"]),
    (0x39, &["Space", "spacebar"]),
    (0x3A, &["Caps Lock", "caps"]),
    (0x3B, &["F1"]),
    (0x3C, &["F2"]),
    (0x3D, &["F3"]),
    (0x3E, &["F4"]),
    (0x3F, &["F5"]),
    (0x40, &["F6"]),
    (0x41, &["F7"]),
    (0x42, &["F8"]),
    (0x43, &["F9"]),
    (0x44, &["F10"]),
    (0x45, &["Num Lock"]),
    (0x46, &["Scroll Lock"]),
    (0x47, &["Numpad 7"]),
    (0x48, &["Numpad 8"]),
    (0x49, &["Numpad 9"]),
    (0x4A, &["Numpad -"]),
    (0x4B, &["Numpad 4"]),
    (0x4C, &["Numpad 5"]),
    (0x4D, &["Numpad 6"]),
    (0x4E, &["Numpad +"]),
    (0x4F, &["Numpad 1"]),
    (0x50, &["Numpad 2"]),
    (0x51, &["Numpad 3"]),
    (0x52, &["Numpad 0"]),
    (0x53, &["Numpad ."]),
    (0x56, &["Extra \\ key", "oem102"]),
    (0x57, &["F11"]),
    (0x58, &["F12"]),
    (0x64, &["F13"]),
    (0x65, &["F14"]),
    (0x66, &["F15"]),
    (0x67, &["F16"]),
    (0x68, &["F17"]),
    (0x69, &["F18"]),
    (0x6A, &["F19"]),
    (0x6B, &["F20"]),
    (0x6C, &["F21"]),
    (0x6D, &["F22"]),
    (0x6E, &["F23"]),
    (0x76, &["F24"]),
    (EXTENDED | 0x10, &["Previous Track"]),
    (EXTENDED | 0x19, &["Next Track"]),
    (EXTENDED | 0x1C, &["Numpad Enter"]),
    (EXTENDED | 0x1D, &["Right Ctrl", "rctrl"]),
    (EXTENDED | 0x20, &["Mute"]),
    (EXTENDED | 0x22, &["Play/Pause"]),
    (EXTENDED | 0x24, &["Stop"]),
    (EXTENDED | 0x2E, &["Volume Down"]),
    (EXTENDED | 0x30, &["Volume Up"]),
    (EXTENDED | 0x35, &["Numpad /", "divide"]),
    (EXTENDED | 0x37, &["Print Screen", "prtsc", "printscr"]),
    (EXTENDED | 0x38, &["Right Alt", "ralt", "altgr"]),
    (EXTENDED | 0x47, &["Home"]),
    (EXTENDED | 0x48, &["Up", "uparrow"]),
    (EXTENDED | 0x49, &["Page Up", "pgup"]),
    (EXTENDED | 0x4B, &["Left", "leftarrow"]),
    (EXTENDED | 0x4D, &["Right", "rightarrow"]),
    (EXTENDED | 0x4F, &["End"]),
    (EXTENDED | 0x50, &["Down", "downarrow"]),
    (EXTENDED | 0x51, &["Page Down", "pgdn"]),
    (EXTENDED | 0x52, &["Insert", "ins"]),
    (EXTENDED | 0x53, &["Delete", "del"]),
    (EXTENDED | 0x5B, &["Left Win", "win", "lwin"]),
    (EXTENDED | 0x5C, &["Right Win", "rwin"]),
    (EXTENDED | 0x5D, &["Menu", "apps"]),
    (VIRTUAL | VK_PAUSE, &["Pause", "break"]),
];

/// Ignores case and spacing, so `page up`, `Page_Up` and `pageup` are the same. A lone symbol stays
/// as it is, because `-` is a key.
fn simplified(name: &str) -> String {
    if name.chars().count() == 1 {
        return name.to_string();
    }
    name.chars().filter(|c| !matches!(c, ' ' | '_' | '-')).flat_map(char::to_lowercase).collect()
}

/// The code for a key's name, such as `z`, `esc`, `f5` or `` ` ``.
pub fn code_for_name(name: &str) -> Option<u16> {
    let name = name.trim();
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return match unsafe { MapVirtualKeyW(c.to_ascii_uppercase() as u32, MAPVK_VK_TO_VSC) } {
                0 => None,
                scan => Some(scan as u16),
            };
        }
    }
    let wanted = simplified(name);
    let named = NAMED.iter().find(|(_, names)| names.iter().any(|n| simplified(n) == wanted));
    if let Some((code, _)) = named {
        return Some(*code);
    }
    // The spelling used for a key with no name of its own, such as `Scan E06C`.
    let code = |hex: &str, base: u16| u8::from_str_radix(hex, 16).ok().filter(|&low| low != 0).map(|low| base | low as u16);
    if let Some(hex) = wanted.strip_prefix("scane0") {
        code(hex, EXTENDED)
    } else if let Some(hex) = wanted.strip_prefix("scan") {
        code(hex, 0)
    } else {
        wanted.strip_prefix("vk").and_then(|hex| code(hex, VIRTUAL))
    }
}

fn name_for_code(code: u16) -> String {
    match NAMED.iter().find(|(c, _)| *c == code) {
        Some((_, names)) => names[0].to_string(),
        None if code & 0xFF00 == VIRTUAL => format!("VK {:02X}", code & 0xFF),
        None if code & 0xFF00 == EXTENDED => format!("Scan E0{:02X}", code & 0xFF),
        None => format!("Scan {code:02X}"),
    }
}

/// What to show for a saved key.
pub fn label(name: &str) -> String {
    let name = name.trim();
    if name.chars().count() == 1 && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return name.to_uppercase();
    }
    code_for_name(name).map_or("None".to_string(), name_for_code)
}

/// The virtual key, scan code and flags that SendInput needs for a key.
pub fn key_fields(code: u16, up: bool) -> (u16, u16, u32) {
    let release = if up { KEYEVENTF_KEYUP } else { 0 };
    let low = code & 0xFF;
    match code & 0xFF00 {
        VIRTUAL => (low, 0, release),
        EXTENDED => (0, low, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | release),
        _ => (0, low, KEYEVENTF_SCANCODE | release),
    }
}

/// The code for a key as a window message reports it. Num Lock is marked as extended although it
/// isn't, and Pause has the same scan code as Num Lock.
fn code_from_message(vk: u16, scan: u32, extended: bool) -> u16 {
    match vk {
        VK_NUMLOCK => 0x45,
        VK_PAUSE => VIRTUAL | VK_PAUSE,
        _ if scan == 0 || scan > 0xFF => VIRTUAL | vk,
        _ if extended => EXTENDED | scan as u16,
        _ => scan as u16,
    }
}

/// The name to save for a key. Letters and digits go by what the layout says.
fn name_from_message(vk: u16, scan: u32, extended: bool) -> String {
    match char::from_u32(vk as u32) {
        Some(c) if c.is_ascii_uppercase() || c.is_ascii_digit() => c.to_ascii_lowercase().to_string(),
        _ => name_for_code(code_from_message(vk, scan, extended)),
    }
}

/// The key a message is about, if it's a press. Windows only reports Print Screen when it's let go.
fn pressed_key(message: u32, vk: u16, details: isize) -> Option<(u32, bool)> {
    let held_already = details & (1 << 30) != 0;
    let press = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => !held_already,
        WM_KEYUP | WM_SYSKEYUP => vk == VK_SNAPSHOT,
        _ => false,
    };
    press.then_some((((details >> 16) & 0xFF) as u32, details & (1 << 24) != 0))
}

static LISTENING: AtomicBool = AtomicBool::new(false);
static CAUGHT: Mutex<Option<String>> = Mutex::new(None);

/// Sees each message for the window before anything else does. It only does anything while a
/// `KeyCapture` exists, and runs on the window's thread, so it's never in the pen's path. True stops
/// the message, so choosing Esc or Tab does nothing else in this window.
pub fn on_window_message(message: *const c_void) -> bool {
    if !LISTENING.load(Ordering::SeqCst) {
        return false;
    }
    let message = unsafe { &*(message as *const MSG) };
    let vk = message.wParam as u16;
    let Some((scan, extended)) = pressed_key(message.message, vk, message.lParam) else { return false };
    *CAUGHT.lock().unwrap_or_else(|e| e.into_inner()) = Some(name_from_message(vk, scan, extended));
    LISTENING.store(false, Ordering::SeqCst);
    true
}

/// Waits for the next key pressed while the window is in front.
pub struct KeyCapture;

impl KeyCapture {
    pub fn start() -> Self {
        *CAUGHT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        LISTENING.store(true, Ordering::SeqCst);
        KeyCapture
    }

    /// The name of the key, once one has been pressed.
    pub fn poll(&self) -> Option<String> {
        CAUGHT.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl Drop for KeyCapture {
    fn drop(&mut self) {
        LISTENING.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_people_ask_for_have_names() {
        for (name, code) in [
            ("`", 0x29),
            ("grave", 0x29),
            ("esc", 0x01),
            ("Escape", 0x01),
            ("f1", 0x3B),
            ("F11", 0x57),
            ("f12", 0x58),
            ("F24", 0x76),
            ("-", 0x0C),
            ("=", 0x0D),
            ("space", 0x39),
            ("right ctrl", EXTENDED | 0x1D),
            ("Page_Up", EXTENDED | 0x49),
            ("pause", VIRTUAL | VK_PAUSE),
        ] {
            assert_eq!(code_for_name(name), Some(code), "{name}");
        }
    }

    #[test]
    fn letters_and_digits_still_work() {
        assert!(code_for_name("z").is_some());
        assert_eq!(code_for_name("z"), code_for_name("Z"));
        assert!(code_for_name("7").is_some());
    }

    #[test]
    fn nonsense_and_empty_names_are_no_key() {
        for name in ["", "  ", "nonsense", "vk", "scan", "scanzz", "vk00", "é"] {
            assert_eq!(code_for_name(name), None, "{name}");
        }
    }

    #[test]
    fn every_name_is_unique_and_reads_back_as_its_key() {
        for (code, names) in NAMED {
            assert_eq!(name_for_code(*code), names[0]);
            for name in *names {
                assert_eq!(code_for_name(name), Some(*code), "{name}");
            }
        }
    }

    #[test]
    fn a_key_without_a_name_can_still_be_saved() {
        for code in [0x7E, EXTENDED | 0x6C, VIRTUAL | 0xB6] {
            assert_eq!(code_for_name(&name_for_code(code)), Some(code), "{code:x}");
        }
    }

    #[test]
    fn labels_are_tidy() {
        assert_eq!(label("z"), "Z");
        assert_eq!(label("ESC"), "Esc");
        assert_eq!(label("`"), "`");
        assert_eq!(label(""), "None");
        assert_eq!(label("nonsense"), "None");
    }

    #[test]
    fn messages_tell_apart_keys_that_share_a_scan_code() {
        // Numpad 1 and End, Enter and Numpad Enter, Num Lock and Pause, the two of each modifier.
        assert_eq!(name_from_message(VK_END, 0x4F, false), "Numpad 1");
        assert_eq!(name_from_message(VK_END, 0x4F, true), "End");
        assert_eq!(name_from_message(VK_RETURN, 0x1C, false), "Enter");
        assert_eq!(name_from_message(VK_RETURN, 0x1C, true), "Numpad Enter");
        assert_eq!(name_from_message(VK_NUMLOCK, 0x45, true), "Num Lock");
        assert_eq!(name_from_message(VK_PAUSE, 0x45, false), "Pause");
        assert_eq!(name_from_message(VK_CONTROL, 0x1D, false), "Left Ctrl");
        assert_eq!(name_from_message(VK_CONTROL, 0x1D, true), "Right Ctrl");
        assert_eq!(name_from_message(VK_SHIFT, 0x2A, false), "Left Shift");
        assert_eq!(name_from_message(VK_SHIFT, 0x36, false), "Right Shift");
        assert_eq!(name_from_message(VK_MENU, 0x38, false), "Left Alt");
        assert_eq!(name_from_message(VK_MENU, 0x38, true), "Right Alt");
    }

    #[test]
    fn messages_name_letters_and_digits_by_layout_and_odd_keys_by_number() {
        assert_eq!(name_from_message(b'Z' as u16, 0x2C, false), "z");
        assert_eq!(name_from_message(b'7' as u16, 0x08, false), "7");
        assert_eq!(name_from_message(VK_NUMPAD7, 0x47, false), "Numpad 7");
        assert_eq!(name_from_message(VK_F13, 0x64, false), "F13");
        assert_eq!(name_from_message(VK_OEM_3, 0x29, false), "`");
        assert_eq!(name_from_message(VK_ESCAPE, 0x01, false), "Esc");
        assert_eq!(name_from_message(0xB6, 0, true), "VK B6");
    }

    #[test]
    fn only_a_new_press_counts() {
        let details = |scan: isize, extended: bool, repeat: bool| (scan << 16) | if extended { 1 << 24 } else { 0 } | if repeat { 1 << 30 } else { 0 };
        assert_eq!(pressed_key(WM_KEYDOWN, VK_ESCAPE, details(0x01, false, false)), Some((0x01, false)));
        assert_eq!(pressed_key(WM_SYSKEYDOWN, VK_F10, details(0x44, false, false)), Some((0x44, false)));
        assert_eq!(pressed_key(WM_KEYDOWN, VK_UP, details(0x48, true, false)), Some((0x48, true)));
        // Still held from before.
        assert_eq!(pressed_key(WM_KEYDOWN, VK_ESCAPE, details(0x01, false, true)), None);
        // Letting go isn't a press, except for the one key that only reports that.
        assert_eq!(pressed_key(WM_KEYUP, VK_ESCAPE, details(0x01, false, true)), None);
        assert_eq!(pressed_key(WM_KEYUP, VK_SNAPSHOT, details(0x37, true, true)), Some((0x37, true)));
        // Not a key at all.
        assert_eq!(pressed_key(0x0200, 0, 0), None);
    }

    #[test]
    fn listening_catches_one_key_and_then_stops() {
        let message = |vk: u16, scan: isize| MSG { hwnd: std::ptr::null_mut(), message: WM_KEYDOWN, wParam: vk as usize, lParam: scan << 16, time: 0, pt: unsafe { std::mem::zeroed() } };
        let esc = message(VK_ESCAPE, 0x01);
        let esc = &esc as *const MSG as *const c_void;
        assert!(!on_window_message(esc), "nothing should be stopped before listening");
        let capture = KeyCapture::start();
        assert_eq!(capture.poll(), None);
        assert!(on_window_message(esc), "the key should be stopped");
        assert_eq!(capture.poll().as_deref(), Some("Esc"));
        assert!(!on_window_message(esc), "only one key should be taken");
        let capture2 = KeyCapture::start();
        drop(capture2);
        assert!(!on_window_message(esc), "canceling should stop listening");
        drop(capture);
    }

    #[test]
    fn each_kind_of_key_is_sent_its_own_way() {
        assert_eq!(key_fields(0x29, false), (0, 0x29, KEYEVENTF_SCANCODE));
        assert_eq!(key_fields(0x29, true), (0, 0x29, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP));
        assert_eq!(key_fields(EXTENDED | 0x48, false), (0, 0x48, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY));
        assert_eq!(key_fields(VIRTUAL | VK_PAUSE, true), (VK_PAUSE, 0, KEYEVENTF_KEYUP));
    }
}
