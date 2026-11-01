//! The tray icon. It runs on its own thread with its own message loop, so it keeps working while
//! the settings window is hidden and not being drawn.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::gui;
use crate::shared::Shared;

pub use crate::icon::ring_rgba as icon_rgba;

/// Starts the tray icon. If it can't be created, closing the window quits instead of hiding.
pub fn start(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        const SIZE: u32 = 32;
        let show = MenuItem::new("Show osutab", true, None);
        let quit = MenuItem::new("Quit osutab", true, None);
        let menu = Menu::new();
        let built = menu.append(&show).and_then(|()| menu.append(&quit));
        let icon = Icon::from_rgba(icon_rgba(SIZE), SIZE, SIZE);
        let tray = match (built, icon) {
            (Ok(()), Ok(icon)) => TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("osutab")
                .with_icon(icon)
                .build(),
            (Err(e), _) => {
                eprintln!("tray menu failed: {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("tray icon failed: {e}");
                return;
            }
        };
        let tray = match tray {
            Ok(t) => t,
            Err(e) => {
                eprintln!("tray icon failed: {e}");
                return;
            }
        };

        let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
        let for_menu = shared.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == show_id {
                gui::show_window(&for_menu);
            } else if event.id == quit_id {
                for_menu.stats.stop.store(true, Ordering::SeqCst);
            }
        }));
        let for_click = shared.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                gui::show_window(&for_click);
            }
        }));

        shared.tray_thread.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        // Dropping it here removes the icon, instead of leaving a dead one until it's hovered.
        drop(tray);
        shared.tray_thread.store(0, Ordering::SeqCst);
    });
}

/// Removes the tray icon before the program exits.
pub fn stop(shared: &Shared) {
    let thread = shared.tray_thread.load(Ordering::SeqCst);
    if thread == 0 {
        return;
    }
    unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) };
    for _ in 0..20 {
        if shared.tray_thread.load(Ordering::SeqCst) == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_ring_of_the_right_size() {
        let pixels = icon_rgba(32);
        assert_eq!(pixels.len(), 32 * 32 * 4);
        let alpha = |x: usize, y: usize| pixels[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corners are clear");
        assert_eq!(alpha(16, 16), 0, "the middle is clear");
        assert_eq!(alpha(16, 4), 255, "the ring is solid");
    }
}
