//! OpenTabletDriver output: pen reports go to OpenTabletDriver's daemon, through the bridge
//! plugin, so its filters, output modes and other plugins all apply to the S Pen.

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use osutab_core::DOWN_FRAME_LEN;
use serde_json::{json, Value};

use crate::assets;
use crate::elevation::{self, Launched};
use crate::mapping::{to_unit, AreaMm, Rotation, TABLET_LONG_MM, TABLET_SHORT_MM};

pub const RELAY_PORT: u16 = 27193;
const TOOL_PATH: &str = "OsutabBridge.OsutabBridgeTool";
/// The tablet's name in OpenTabletDriver, and the name it had before the program was osu!tab.
pub const TABLET_NAME: &str = "osu!tab S Pen";
const OLD_TABLET_NAME: &str = "osutab S Pen";
const FLUSH_INTERVAL: Duration = Duration::from_millis(2);
// How often to look at OpenTabletDriver's settings for a changed area.
const AREA_CHECK_EVERY: Duration = Duration::from_millis(500);
// The tablet configuration in otd-plugin: landscape, at 0.01 mm per unit.
const OTD_MAX_X: f64 = 31376.0;
const OTD_MAX_Y: f64 = 19589.0;

/// Pen coordinates for OpenTabletDriver, which sees a landscape tablet whichever way it's held.
/// In portrait, it's the same picture turned, and OpenTabletDriver's Rotation setting turns it
/// back, because a tablet's size can't change in its configuration.
pub fn landscape_coordinates(x: u16, y: u16, max_x: u16, max_y: u16, rotation: Rotation) -> (u16, u16) {
    let landscape = if rotation == Rotation::R270 { Rotation::R270 } else { Rotation::R90 };
    let (u, v) = to_unit(x, y, max_x, max_y, landscape);
    ((u * OTD_MAX_X).round() as u16, (v * OTD_MAX_Y).round() as u16)
}

/// Frames waiting for a socket that's full. A frame that has started must finish, or the stream
/// would be cut mid-frame. After it only the newest frame waits, because older pen positions are
/// out of date, and the newest one carries the pen's current buttons.
#[derive(Default)]
struct Backlog {
    started: Vec<u8>,
    newest: Option<[u8; DOWN_FRAME_LEN]>,
}

impl Backlog {
    fn is_empty(&self) -> bool {
        self.started.is_empty() && self.newest.is_none()
    }

    /// Writes as much as the socket takes without waiting.
    fn flush(&mut self, out: &mut impl Write) -> io::Result<()> {
        loop {
            let written = match (self.started.is_empty(), &self.newest) {
                (false, _) => out.write(&self.started),
                (true, Some(frame)) => out.write(frame),
                (true, None) => return Ok(()),
            };
            match written {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) if self.started.is_empty() => {
                    if let Some(frame) = self.newest.take() {
                        self.started.extend_from_slice(&frame[n..]);
                    }
                }
                Ok(n) => {
                    self.started.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

struct Client {
    stream: TcpStream,
    backlog: Backlog,
}

/// Hands pen reports to the bridge plugin. Sending costs one socket write on the input thread, and
/// never waits: a slow plugin misses out-of-date reports instead of holding up the pen or the link.
#[derive(Default)]
pub struct Relay {
    client: Mutex<Option<Client>>,
    backed_up: AtomicBool,
    pub connected: AtomicBool,
}

impl Relay {
    pub fn send(&self, frame: &[u8; DOWN_FRAME_LEN]) {
        let mut client = self.client.lock().unwrap();
        if let Some(c) = client.as_mut() {
            c.backlog.newest = Some(*frame);
            self.flush(&mut client);
        }
    }

    fn flush(&self, client: &mut Option<Client>) {
        let Some(c) = client.as_mut() else { return };
        if c.backlog.flush(&mut c.stream).is_err() {
            *client = None;
            self.connected.store(false, Ordering::SeqCst);
            self.backed_up.store(false, Ordering::SeqCst);
        } else {
            self.backed_up.store(!c.backlog.is_empty(), Ordering::SeqCst);
        }
    }

    /// The last report before the pen goes quiet can have nothing after it to push it out.
    fn flush_loop(&self) {
        loop {
            std::thread::sleep(FLUSH_INTERVAL);
            if self.backed_up.load(Ordering::SeqCst) {
                self.flush(&mut self.client.lock().unwrap());
            }
        }
    }

    /// Accepts the bridge plugin for the whole run. A new connection replaces the old one.
    pub fn serve(&self) {
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, RELAY_PORT)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("OpenTabletDriver link unavailable: {e}");
                return;
            }
        };
        std::thread::scope(|scope| {
            scope.spawn(|| self.flush_loop());
            for stream in listener.incoming().flatten() {
                if stream.set_nodelay(true).is_ok() && stream.set_nonblocking(true).is_ok() {
                    *self.client.lock().unwrap() = Some(Client { stream, backlog: Backlog::default() });
                    self.backed_up.store(false, Ordering::SeqCst);
                    self.connected.store(true, Ordering::SeqCst);
                }
            }
        });
    }
}

/// OpenTabletDriver's data folder, which its own program and plugin manager also use.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("OpenTabletDriver")
}

fn is_our_tablet(name: Option<&str>) -> bool {
    matches!(name, Some(TABLET_NAME | OLD_TABLET_NAME))
}

/// Adds the bridge tool to OpenTabletDriver's settings, keeping everything else in them, and moves
/// a profile from the tablet's old name to its new one, so its area and filters carry over. Returns
/// the new text, or `None` if nothing needed changing. A tool the user turned off stays off.
pub fn settings_with_tool(existing: Option<&str>) -> Result<Option<String>, String> {
    let mut settings: Value = match existing {
        Some(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("can't read settings: {e}"))?,
        None => json!({ "Profiles": [], "LockUsableAreaDisplay": true, "LockUsableAreaTablet": true }),
    };
    let root = settings.as_object_mut().ok_or("settings aren't an object")?;
    let mut changed = false;
    if let Some(profiles) = root.get_mut("Profiles").and_then(Value::as_array_mut) {
        let has_new = profiles.iter().any(|p| p.get("Tablet").and_then(Value::as_str) == Some(TABLET_NAME));
        for profile in profiles.iter_mut() {
            if !has_new && profile.get("Tablet").and_then(Value::as_str) == Some(OLD_TABLET_NAME) {
                profile["Tablet"] = json!(TABLET_NAME);
                changed = true;
            }
        }
    }
    let tools = root.entry("Tools").or_insert_with(|| json!([]));
    let list = tools.as_array_mut().ok_or("Tools isn't a list")?;
    if !list.iter().any(|t| t.get("Path").and_then(Value::as_str) == Some(TOOL_PATH)) {
        list.push(json!({ "Path": TOOL_PATH, "Settings": [], "Enable": true }));
        changed = true;
    }
    if !changed {
        return Ok(None);
    }
    serde_json::to_string_pretty(&settings).map(Some).map_err(|e| e.to_string())
}

/// OpenTabletDriver's pen area for the S Pen, in mm on its landscape picture of the tablet, measured
/// to the area's middle, with the size of the screen area it maps to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OtdArea {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
    pub display_width: f64,
    pub display_height: f64,
}

/// Reads the S Pen's area from OpenTabletDriver's settings. `None` if there isn't one, or if the
/// output mode is relative, where an area doesn't apply.
pub fn area_from_settings(text: &str) -> Option<OtdArea> {
    let settings: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let profile = settings.get("Profiles")?.as_array()?.iter().find(|p| is_our_tablet(p.get("Tablet").and_then(Value::as_str)))?;
    let mode = profile.get("OutputMode").and_then(|o| o.get("Path")).and_then(Value::as_str).unwrap_or("");
    if mode.to_lowercase().contains("relative") {
        return None;
    }
    let absolute = profile.get("AbsoluteModeSettings")?;
    let (tablet, display) = (absolute.get("Tablet")?, absolute.get("Display")?);
    let number = |object: &Value, key: &str| object.get(key)?.as_f64().filter(|v| v.is_finite());
    let area = OtdArea {
        width: number(tablet, "Width")?,
        height: number(tablet, "Height")?,
        x: number(tablet, "X")?,
        y: number(tablet, "Y")?,
        rotation: number(tablet, "Rotation").unwrap_or(0.0),
        display_width: number(display, "Width")?,
        display_height: number(display, "Height")?,
    };
    (area.width > 0.0 && area.height > 0.0 && area.display_width > 0.0 && area.display_height > 0.0).then_some(area)
}

/// OpenTabletDriver's area as the user sees the tablet. OpenTabletDriver always sees it landscape,
/// so held upright, the picture is turned a quarter: clockwise for the USB port at the bottom.
pub fn area_in_view(area: &OtdArea, rotation: Rotation) -> AreaMm {
    let (long, short) = (TABLET_LONG_MM, TABLET_SHORT_MM);
    let (offset_x, offset_y, turn) = match rotation {
        Rotation::R90 | Rotation::R270 => (area.x - long / 2.0, area.y - short / 2.0, 0.0),
        Rotation::R0 => (short / 2.0 - area.y, area.x - long / 2.0, 90.0),
        Rotation::R180 => (area.y - short / 2.0, long / 2.0 - area.x, -90.0),
    };
    let mut rotation_deg = (area.rotation + turn) % 360.0;
    if rotation_deg > 180.0 {
        rotation_deg -= 360.0;
    } else if rotation_deg < -180.0 {
        rotation_deg += 360.0;
    }
    AreaMm { width: area.width, height: area.height, offset_x, offset_y, rotation_deg }
}

/// Keeps up with OpenTabletDriver's area without reading its settings on every call.
#[derive(Default)]
pub struct AreaWatch {
    checked: Option<Instant>,
    modified: Option<SystemTime>,
    area: Option<OtdArea>,
}

impl AreaWatch {
    pub fn area(&mut self, data: &Path) -> Option<OtdArea> {
        if self.checked.is_some_and(|t| t.elapsed() < AREA_CHECK_EVERY) {
            return self.area;
        }
        self.checked = Some(Instant::now());
        let path = data.join("settings.json");
        let modified = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if modified != self.modified || modified.is_none() {
            self.modified = modified;
            self.area = fs::read_to_string(&path).ok().and_then(|text| area_from_settings(&text));
        }
        self.area
    }
}

/// Puts the bridge plugin, tablet configuration and setting where OpenTabletDriver looks for them.
pub fn install(data: &Path) -> Result<(), String> {
    let plugin = assets::bridge_plugin().ok_or("this build doesn't include the bridge plugin")?;
    let config = assets::bridge_config();
    let write = |path: PathBuf, bytes: &[u8]| -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        if fs::read(&path).is_ok_and(|existing| existing == bytes) {
            return Ok(());
        }
        fs::write(path, bytes)
    };
    write(data.join("Plugins").join("OsutabBridge").join("OsutabBridge.dll"), plugin).map_err(|e| e.to_string())?;
    write(data.join("Configurations").join(format!("{TABLET_NAME}.json")), config).map_err(|e| e.to_string())?;
    // Two configurations for the same device would leave OpenTabletDriver to pick one.
    let _ = fs::remove_file(data.join("Configurations").join(format!("{OLD_TABLET_NAME}.json")));

    let settings_path = data.join("settings.json");
    let existing = fs::read_to_string(&settings_path).ok();
    if let Some(updated) = settings_with_tool(existing.as_deref())? {
        if existing.is_some() {
            let _ = fs::copy(&settings_path, data.join("settings.before-osu!tab.json"));
        }
        fs::write(&settings_path, updated).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Finds OpenTabletDriver's programs: beside this program when packaged, or built from source.
pub fn program_dir(exe_dir: &Path) -> Option<PathBuf> {
    [exe_dir.join("otd"), exe_dir.join("../../target/otd")]
        .into_iter()
        .find(|dir| dir.join("OpenTabletDriver.Daemon.exe").exists())
}

/// Runs the daemon that does the work, without administrator rights even if this program has
/// them. It exits by itself if another copy is already running.
pub fn start_daemon(dir: &Path) -> io::Result<Launched> {
    elevation::spawn_unelevated(&dir.join("OpenTabletDriver.Daemon.exe"), dir, true)
}

/// Opens OpenTabletDriver's own settings window, without administrator rights too.
pub fn open_settings_window(dir: &Path) -> io::Result<()> {
    // Letting go of the handle leaves the window running.
    elevation::spawn_unelevated(&dir.join("OpenTabletDriver.UX.Wpf.exe"), dir, false).map(|_| ())
}

/// True if a running OpenTabletDriver daemon has administrator rights, which stops it working
/// properly. `None` if there isn't one running, or Windows won't say.
pub fn daemon_elevated() -> Option<bool> {
    let ids = crate::tuning::process_ids("OpenTabletDriver.Daemon.exe");
    let mut answer = None;
    for id in ids {
        match elevation::process_is_elevated(id) {
            Some(true) => return Some(true),
            Some(false) => answer = Some(false),
            None => {}
        }
    }
    answer
}

/// Which output mode OpenTabletDriver's settings use, to warn about Windows Ink.
pub fn uses_windows_ink(data: &Path) -> bool {
    fs::read_to_string(data.join("settings.json")).is_ok_and(|text| {
        let Ok(settings) = serde_json::from_str::<Value>(&text) else { return false };
        let Some(profiles) = settings.get("Profiles").and_then(Value::as_array) else { return false };
        profiles.iter().any(|p| {
            let matches_tablet = is_our_tablet(p.get("Tablet").and_then(Value::as_str));
            let path = p.get("OutputMode").and_then(|o| o.get("Path")).and_then(Value::as_str).unwrap_or("");
            matches_tablet && path.to_lowercase().contains("ink")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_X: u16 = 19589;
    const MAX_Y: u16 = 31376;

    /// A socket that takes `room` more bytes, then says it's full.
    #[derive(Default)]
    struct Socket {
        room: usize,
        taken: Vec<u8>,
    }

    impl Write for Socket {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.room == 0 {
                return Err(ErrorKind::WouldBlock.into());
            }
            let n = bytes.len().min(self.room);
            self.room -= n;
            self.taken.extend_from_slice(&bytes[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_full_socket_finishes_the_started_frame_then_sends_only_the_newest() {
        let frame = |n: u8| [n; DOWN_FRAME_LEN];
        let mut socket = Socket { room: DOWN_FRAME_LEN + 1, ..Default::default() };
        let mut backlog = Backlog::default();
        for n in 1..=3 {
            backlog.newest = Some(frame(n));
            backlog.flush(&mut socket).unwrap();
        }
        assert!(!backlog.is_empty());

        socket.room = usize::MAX;
        backlog.flush(&mut socket).unwrap();
        assert!(backlog.is_empty());
        let expected: Vec<u8> = [frame(1), frame(2), frame(3)].concat();
        assert_eq!(socket.taken, expected, "frame 2 had started, and frame 3 is the newest");

        socket.taken.clear();
        socket.room = DOWN_FRAME_LEN;
        for n in 4..=6 {
            backlog.newest = Some(frame(n));
            backlog.flush(&mut socket).unwrap();
        }
        socket.room = usize::MAX;
        backlog.flush(&mut socket).unwrap();
        assert_eq!(socket.taken, [frame(4), frame(6)].concat(), "frame 5 was out of date");
    }

    #[test]
    fn coordinates_match_the_direct_mapping_in_landscape() {
        // Sensor corner (0,0) is the bottom left when landscape with the USB port on the right.
        assert_eq!(landscape_coordinates(0, 0, MAX_X, MAX_Y, Rotation::R90), (0, 19589));
        assert_eq!(landscape_coordinates(MAX_X, MAX_Y, MAX_X, MAX_Y, Rotation::R90), (31376, 0));
        assert_eq!(landscape_coordinates(0, 0, MAX_X, MAX_Y, Rotation::R270), (31376, 0));
        assert_eq!(landscape_coordinates(MAX_X, MAX_Y, MAX_X, MAX_Y, Rotation::R270), (0, 19589));
    }

    #[test]
    fn portrait_uses_the_landscape_picture() {
        for rotation in [Rotation::R0, Rotation::R180] {
            assert_eq!(landscape_coordinates(0, 0, MAX_X, MAX_Y, rotation), (0, 19589));
        }
    }

    #[test]
    fn tool_is_added_to_new_settings() {
        let text = settings_with_tool(None).unwrap().unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["Tools"][0]["Path"], TOOL_PATH);
        assert_eq!(v["Tools"][0]["Enable"], true);
    }

    #[test]
    fn existing_settings_are_kept() {
        let before = r#"{"Profiles":[{"Tablet":"X"}],"Tools":[{"Path":"Other.Tool","Enable":true}],"Custom":5}"#;
        let v: Value = serde_json::from_str(&settings_with_tool(Some(before)).unwrap().unwrap()).unwrap();
        assert_eq!(v["Custom"], 5);
        assert_eq!(v["Profiles"][0]["Tablet"], "X");
        assert_eq!(v["Tools"].as_array().unwrap().len(), 2);
        assert_eq!(v["Tools"][0]["Path"], "Other.Tool");
    }

    #[test]
    fn tool_already_there_changes_nothing_even_if_turned_off() {
        let off = format!(r#"{{"Tools":[{{"Path":"{TOOL_PATH}","Enable":false}}]}}"#);
        assert_eq!(settings_with_tool(Some(&off)).unwrap(), None);
    }

    #[test]
    fn a_profile_under_the_old_name_moves_to_the_new_one() {
        let before = format!(r#"{{"Profiles":[{{"Tablet":"{OLD_TABLET_NAME}","Custom":1}}],"Tools":[{{"Path":"{TOOL_PATH}","Enable":true}}]}}"#);
        let v: Value = serde_json::from_str(&settings_with_tool(Some(&before)).unwrap().unwrap()).unwrap();
        assert_eq!(v["Profiles"][0]["Tablet"], TABLET_NAME);
        assert_eq!(v["Profiles"][0]["Custom"], 1);
        // Already moved: nothing to do.
        let after = serde_json::to_string(&v).unwrap();
        assert_eq!(settings_with_tool(Some(&after)).unwrap(), None);
        // A profile under the new name wins, and the old one is left as it was.
        let both = format!(r#"{{"Profiles":[{{"Tablet":"{OLD_TABLET_NAME}"}},{{"Tablet":"{TABLET_NAME}"}}],"Tools":[{{"Path":"{TOOL_PATH}"}}]}}"#);
        assert_eq!(settings_with_tool(Some(&both)).unwrap(), None);
    }

    /// The settings OpenTabletDriver 0.6 wrote for the S Pen on the test PC.
    const REAL_SETTINGS: &str = r#"{"Revision":"0.6.6.2","Profiles":[{"Tablet":"osutab S Pen","OutputMode":{"Path":"OpenTabletDriver.Desktop.Output.AbsoluteMode","Settings":[],"Enable":true},"AbsoluteModeSettings":{"Display":{"Width":2560.0,"Height":1600.0,"X":1280.0,"Y":800.0,"Rotation":0.0},"Tablet":{"Width":200.0,"Height":125.0,"X":100.0,"Y":133.39001,"Rotation":0.0},"EnableClipping":true,"EnableAreaLimiting":false,"LockAspectRatio":false}}]}"#;

    #[test]
    fn the_area_is_read_from_real_settings() {
        let area = area_from_settings(REAL_SETTINGS).unwrap();
        assert_eq!((area.width, area.height, area.x, area.y), (200.0, 125.0, 100.0, 133.39001));
        assert_eq!((area.display_width, area.display_height), (2560.0, 1600.0));
        let relative = REAL_SETTINGS.replace("Output.AbsoluteMode", "Output.RelativeMode");
        assert_eq!(area_from_settings(&relative), None);
        assert_eq!(area_from_settings(r#"{"Profiles":[{"Tablet":"Wacom"}]}"#), None);
        assert_eq!(area_from_settings("not json"), None);
    }

    #[test]
    fn landscape_areas_map_straight_across() {
        let area = area_from_settings(REAL_SETTINGS).unwrap();
        let view = area_in_view(&area, Rotation::R90);
        assert!((view.offset_x - (100.0 - TABLET_LONG_MM / 2.0)).abs() < 1e-9);
        assert!((view.offset_y - (133.39001 - TABLET_SHORT_MM / 2.0)).abs() < 1e-9);
        assert_eq!((view.width, view.height, view.rotation_deg), (200.0, 125.0, 0.0));
        assert_eq!(area_in_view(&area, Rotation::R270), view);
    }

    /// The outline must sit where the pen really maps: a pen position in the middle of
    /// OpenTabletDriver's area must land in the middle of the drawn one, held any way.
    #[test]
    fn the_drawn_area_matches_what_opentabletdriver_maps() {
        let area = OtdArea { width: 100.0, height: 60.0, x: 120.0, y: 80.0, rotation: 0.0, display_width: 2560.0, display_height: 1600.0 };
        for rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            let view = area_in_view(&area, rotation).resolve(rotation);
            // Find the sensor position OpenTabletDriver sees at the area's middle.
            let (want_x, want_y) = (area.x / TABLET_LONG_MM, area.y / TABLET_SHORT_MM);
            let max = (19589u16, 31376u16);
            let mut best = (f64::MAX, (0.0, 0.0));
            for sx in (0..=max.0).step_by(97) {
                for sy in (0..=max.1).step_by(97) {
                    let (lx, ly) = landscape_coordinates(sx, sy, max.0, max.1, rotation);
                    let d = (lx as f64 / OTD_MAX_X - want_x).abs() + (ly as f64 / OTD_MAX_Y - want_y).abs();
                    if d < best.0 {
                        best = (d, to_unit(sx, sy, max.0, max.1, rotation));
                    }
                }
            }
            let (u, v) = best.1;
            let mapped = view.map(u, v);
            assert!((mapped.0 - 0.5).abs() < 0.02 && (mapped.1 - 0.5).abs() < 0.02, "{rotation:?}: {mapped:?}");
        }
    }

    #[test]
    fn broken_settings_are_left_alone() {
        assert!(settings_with_tool(Some("not json")).is_err());
        assert!(settings_with_tool(Some("[1,2]")).is_err());
    }

    #[test]
    fn windows_ink_is_noticed_only_for_our_tablet() {
        let dir = std::env::temp_dir().join(format!("osutab-test-otd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ink = r#"{"Profiles":[{"Tablet":"osu!tab S Pen","OutputMode":{"Path":"VoiDPlugins.OutputMode.WinInkAbsoluteMode"}}]}"#;
        fs::write(dir.join("settings.json"), ink).unwrap();
        assert!(uses_windows_ink(&dir));
        let other = r#"{"Profiles":[{"Tablet":"Wacom","OutputMode":{"Path":"VoiDPlugins.OutputMode.WinInkAbsoluteMode"}},{"Tablet":"osutab S Pen","OutputMode":{"Path":"OpenTabletDriver.Desktop.Output.AbsoluteMode"}}]}"#;
        fs::write(dir.join("settings.json"), other).unwrap();
        assert!(!uses_windows_ink(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
