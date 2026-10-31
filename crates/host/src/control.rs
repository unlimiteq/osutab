//! The link to the tablet app, carried over USB by `adb reverse`. It's separate from the pen
//! stream and runs on its own thread, so it can't delay pen input.

use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mapping::{AreaMm, Rotation, TABLET_LONG_MM};
use crate::settings::{Output, Settings, MIN_AREA_MM};
use crate::theme::Rgb;
use crate::shared::{LiveConfig, Shared};

pub const APP_PORT: u16 = 27192;
const POLL: Duration = Duration::from_millis(30);
const SAVE_DELAY: Duration = Duration::from_millis(500);
// Long enough for a busy tablet, and short enough that a dead connection gets noticed.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

fn rotation_degrees(rotation: Rotation) -> u32 {
    match rotation {
        Rotation::R0 => 0,
        Rotation::R90 => 90,
        Rotation::R180 => 180,
        Rotation::R270 => 270,
    }
}

/// What the tablet app is told. The sizes are the fitted ones, so it draws what's really in use.
pub fn state_line(settings: &Settings, live: &LiveConfig, screen_width: f64, screen_aspect: f64) -> String {
    let a = &live.area;
    format!(
        "state {} {} {} {} {} {} {} {} {} {} {} {}\n",
        a.width_mm,
        a.height_mm,
        a.center_x - a.tablet_w / 2.0,
        a.center_y - a.tablet_h / 2.0,
        settings.area().rotation_deg,
        a.tablet_w,
        a.tablet_h,
        if settings.aspect_lock { 1 } else { 0 },
        screen_aspect,
        rotation_degrees(live.rotation),
        screen_width,
        settings.presets[settings.active].name,
    )
}

/// Colors without the `#`, then outline thickness, outside darkness and whether to animate.
pub fn theme_line(settings: &Settings) -> String {
    let t = settings.active_theme();
    let hex = |c: Rgb| c.to_hex()[1..].to_string();
    format!(
        "theme {} {} {} {} {} {} {} {} {}\n",
        hex(t.background),
        hex(t.panel),
        hex(t.text),
        hex(t.muted),
        hex(t.accent),
        hex(t.outline),
        t.outline_thickness,
        t.outside_dim,
        u8::from(settings.animations),
    )
}

/// The app's other settings. Following the screen means the app mustn't lock its orientation.
pub fn prefs_line(settings: &Settings, following: bool) -> String {
    format!(
        "prefs {} {} {} {}\n",
        u8::from(settings.show_area_label),
        (settings.hold_to_edit_seconds * 1000.0) as u32,
        (settings.edit_timeout_seconds * 1000.0) as u32,
        u8::from(following),
    )
}

/// Reads a `rotation` line from the tablet app, sent when its screen turns.
pub fn parse_rotation_line(line: &str) -> Option<Rotation> {
    let degrees = line.trim().strip_prefix("rotation ")?;
    Rotation::from_android(degrees.parse::<u32>().ok().filter(|d| d % 90 == 0)? / 90)
}

/// Reads a `set` line from the tablet app. Anything malformed or out of range is rejected.
pub fn parse_set(line: &str) -> Option<AreaMm> {
    let mut parts = line.trim().split(' ');
    if parts.next()? != "set" {
        return None;
    }
    let mut number = || parts.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
    let area = AreaMm {
        width: number()?,
        height: number()?,
        offset_x: number()?,
        offset_y: number()?,
        rotation_deg: number()?,
    };
    let in_range = area.width >= MIN_AREA_MM
        && area.height >= MIN_AREA_MM
        && area.width <= TABLET_LONG_MM + 1.0
        && area.height <= TABLET_LONG_MM + 1.0
        && area.rotation_deg.abs() <= 360.0;
    (in_range && parts.next().is_none()).then_some(area)
}

/// Serves one connection until it ends, or until a newer one replaces it.
fn serve(stream: TcpStream, shared: &Shared, id: u64, newest: &AtomicU64) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut sent_state = String::new();
    let mut sent_outline = None;
    let mut sent_theme = String::new();
    let mut sent_prefs = String::new();
    let mut unsaved_since: Option<Instant> = None;

    while !shared.stats.stop.load(Ordering::SeqCst) && newest.load(Ordering::SeqCst) == id {
        let settings = shared.settings();
        let theme = theme_line(&settings);
        if theme != sent_theme {
            writer.write_all(theme.as_bytes())?;
            sent_theme = theme;
        }
        // Before the state line, so the app knows whether to obey the orientation in it.
        let prefs = prefs_line(&settings, shared.follows_screen());
        if prefs != sent_prefs {
            writer.write_all(prefs.as_bytes())?;
            sent_prefs = prefs;
        }
        let live = shared.live_config();
        let state = state_line(&settings, &live, live.target.width, live.target.width / live.target.height);
        if state != sent_state {
            writer.write_all(state.as_bytes())?;
            sent_state = state;
        }
        // In OpenTabletDriver mode the area is OpenTabletDriver's, so ours would mislead.
        let outline = settings.show_outline && settings.output == Output::Direct;
        if sent_outline != Some(outline) {
            writer.write_all(if outline { b"outline 1\n" } else { b"outline 0\n" })?;
            sent_outline = Some(outline);
        }

        match reader.read_line(&mut line) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(_) => {
                if let Some(area) = parse_set(&line) {
                    shared.update_settings(|s| *s.area_mut() = area);
                    unsaved_since = Some(Instant::now());
                } else if let Some(rotation) = parse_rotation_line(&line) {
                    shared.set_screen_rotation(rotation);
                }
                line.clear();
            }
            // A partial line stays in the buffer, and the next read finishes it.
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }

        if unsaved_since.is_some_and(|t| t.elapsed() >= SAVE_DELAY) {
            unsaved_since = None;
            if let Err(e) = shared.save_settings() {
                eprintln!("could not save settings: {e}");
            }
        }
    }
    Ok(())
}

/// Listens for the tablet app for the whole run. It reconnects by itself after a replug.
pub fn run(shared: Arc<Shared>) {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, APP_PORT)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tablet app link unavailable: {e}");
            return;
        }
    };
    // The newest connection wins. A half-open old one can't block it, because the old one's thread
    // sees it's been replaced within one poll.
    let newest = Arc::new(AtomicU64::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let id = newest.fetch_add(1, Ordering::SeqCst) + 1;
        let (shared, newest) = (shared.clone(), newest.clone());
        std::thread::spawn(move || {
            shared.app_connected.store(true, Ordering::SeqCst);
            let _ = serve(stream, &shared, id, &newest);
            if newest.load(Ordering::SeqCst) == id {
                shared.app_connected.store(false, Ordering::SeqCst);
            }
            let _ = shared.save_settings();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{Rotation, TABLET_SHORT_MM};

    #[test]
    fn theme_line_matches_what_the_app_parses() {
        let mut settings = Settings::default();
        settings.choose_theme("Daylight");
        settings.theme_overrides.outline = Some(Rgb(1, 2, 255));
        settings.animations = false;
        // The exact form the Kotlin test checks on the other side.
        assert_eq!(theme_line(&settings), "theme f4f4f6 ffffff 1c1e23 62666f d62f7c 0102ff 2 35 0\n");
    }

    #[test]
    fn prefs_line_matches_what_the_app_parses() {
        let settings = Settings { hold_to_edit_seconds: 1.5, edit_timeout_seconds: 30.0, show_area_label: false, ..Settings::default() };
        assert_eq!(prefs_line(&settings, true), "prefs 0 1500 30000 1\n");
    }

    #[test]
    fn rotation_lines_from_the_app() {
        assert_eq!(parse_rotation_line("rotation 90\n"), Some(Rotation::R90));
        assert_eq!(parse_rotation_line("rotation 270"), Some(Rotation::R270));
        for bad in ["rotation 45", "rotation x", "rotation 360", "rotation", "set 1 2 3 4 5"] {
            assert_eq!(parse_rotation_line(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_new_connection_replaces_a_half_open_one() {
        use crate::mapping::Rect;
        use crate::shared::Overrides;
        use std::net::TcpListener;
        let target = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        let shared = Shared::new(Settings::default(), std::path::PathBuf::new(), Overrides::default(), target, true);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        // Connected and never read from or written to, as a dead tablet app would be.
        let _silent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let newest = AtomicU64::new(1);
        std::thread::scope(|scope| {
            let ending = scope.spawn(|| serve(stream, &shared, 1, &newest));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!ending.is_finished());
            newest.store(2, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            assert!(ending.is_finished());
        });
    }

    #[test]
    fn set_line_round_trip_from_the_app() {
        // The exact form the Kotlin test checks on the other side.
        let area = parse_set("set 100.0 62.5 0.0 0.0 0.0\n").unwrap();
        assert_eq!(area, AreaMm { width: 100.0, height: 62.5, offset_x: 0.0, offset_y: 0.0, rotation_deg: 0.0 });
        // Kotlin writes small numbers in scientific notation.
        assert_eq!(parse_set("set 100.0 62.5 1.0E-4 -2.5 0.0").unwrap().offset_x, 1.0e-4);
    }

    #[test]
    fn bad_set_lines_are_rejected() {
        for bad in [
            "",
            "hello",
            "set 100 62.5",
            "set 100 62.5 0 0 0 extra",
            "set wide 62.5 0 0 0",
            "set NaN 62.5 0 0 0",
            "set inf 62.5 0 0 0",
            "set 1 62.5 0 0 0",
            "set 100 62.5 0 0 9000",
            "set 9000 62.5 0 0 0",
        ] {
            assert_eq!(parse_set(bad), None, "{bad}");
        }
    }

    #[test]
    fn state_line_matches_what_the_app_parses() {
        let mut settings = Settings { rotation: Some(Rotation::R90), ..Settings::default() };
        settings.save_preset("My preset name").unwrap();
        *settings.area_mut() = AreaMm { width: 120.0, height: 75.0, offset_x: 10.0, offset_y: -5.0, rotation_deg: 15.0 };
        let screen = crate::mapping::Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        let live = LiveConfig {
            output: Output::Direct,
            target: screen,
            desktop: screen,
            rotation: Rotation::R90,
            area: settings.area().resolve(Rotation::R90),
            tip_click: true,
            button_scan: None,
        };
        let line = state_line(&settings, &live, 2560.0, 1.6);
        assert!(line.ends_with('\n'));
        let parts: Vec<&str> = line.trim().splitn(13, ' ').collect();
        assert_eq!(parts.len(), 13);
        assert_eq!(parts[0], "state");
        assert_eq!(parts[1].parse::<f64>().unwrap(), 120.0);
        assert!((parts[3].parse::<f64>().unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(parts[5], "15");
        assert_eq!(parts[6].parse::<f64>().unwrap(), TABLET_LONG_MM);
        assert_eq!(parts[7].parse::<f64>().unwrap(), TABLET_SHORT_MM);
        assert_eq!(parts[8], "1");
        assert_eq!(parts[10], "90");
        assert_eq!(parts[12], "My preset name");
    }
}
