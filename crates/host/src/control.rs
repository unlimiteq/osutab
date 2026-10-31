//! The link to the tablet app, carried over USB by `adb reverse`. It's separate from the pen
//! stream and runs on its own thread, so it can't delay pen input.

use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mapping::{AreaMm, Rotation, TABLET_LONG_MM};
use crate::settings::{Output, Settings, MIN_AREA_MM};
use crate::shared::{LiveConfig, Shared};

pub const APP_PORT: u16 = 27192;
const POLL: Duration = Duration::from_millis(30);
const SAVE_DELAY: Duration = Duration::from_millis(500);

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

fn serve(stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(POLL))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let screen_aspect = shared.target.width / shared.target.height;
    let mut sent_state = String::new();
    let mut sent_outline = None;
    let mut unsaved_since: Option<Instant> = None;

    while !shared.stats.stop.load(Ordering::SeqCst) {
        let settings = shared.settings();
        let state = state_line(&settings, &shared.live_config(), shared.target.width, screen_aspect);
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
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        shared.app_connected.store(true, Ordering::SeqCst);
        let _ = serve(stream, &shared);
        shared.app_connected.store(false, Ordering::SeqCst);
        let _ = shared.save_settings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{Rotation, TABLET_SHORT_MM};

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
        let live = LiveConfig {
            output: Output::Direct,
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
