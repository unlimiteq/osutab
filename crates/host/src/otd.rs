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
// The IDs the bridge plugin gives the S Pen, which its tablet configuration names.
const BRIDGE_VENDOR: u64 = 0x1209;
const BRIDGE_PRODUCT: u64 = 0x0001;
const DAEMON: &str = "OpenTabletDriver.Daemon.exe";
const FLUSH_INTERVAL: Duration = Duration::from_millis(2);
const DAEMON_STOP_WAIT_MS: u32 = 3000;
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

    /// Accepts the bridge plugin for the whole run.
    pub fn serve(&self) {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, RELAY_PORT)) {
            Ok(listener) => self.serve_on(listener),
            Err(e) => eprintln!("OpenTabletDriver link unavailable: {e}"),
        }
    }

    /// A new connection takes over only once the one before has closed. OpenTabletDriver makes a
    /// tablet from each configuration that names the S Pen, and each one connects. If each could
    /// take over, two would swap every time the other reconnected, and the cursor would jump
    /// between their two areas.
    fn serve_on(&self, listener: TcpListener) {
        std::thread::scope(|scope| {
            scope.spawn(|| self.flush_loop());
            for stream in listener.incoming().flatten() {
                let mut client = self.client.lock().unwrap();
                if client.as_ref().is_some_and(|c| still_open(&c.stream)) {
                    continue;
                }
                if stream.set_nodelay(true).is_ok() && stream.set_nonblocking(true).is_ok() {
                    *client = Some(Client { stream, backlog: Backlog::default() });
                    self.backed_up.store(false, Ordering::SeqCst);
                    self.connected.store(true, Ordering::SeqCst);
                }
            }
        });
    }
}

/// The plugin never sends anything, so a socket with nothing to read is still open.
fn still_open(stream: &TcpStream) -> bool {
    match stream.peek(&mut [0u8; 1]) {
        Ok(0) => false,
        Ok(_) => true,
        Err(e) => e.kind() == ErrorKind::WouldBlock,
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

fn plugin_path(data: &Path) -> PathBuf {
    data.join("Plugins").join("OsutabBridge").join("OsutabBridge.dll")
}

fn config_path(data: &Path) -> PathBuf {
    data.join("Configurations").join(format!("{TABLET_NAME}.json"))
}

/// True if a tablet configuration names the S Pen's IDs.
fn names_the_s_pen(text: &str) -> bool {
    let Ok(config) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else { return false };
    let ids = config.get("DigitizerIdentifiers").and_then(Value::as_array);
    ids.is_some_and(|ids| {
        ids.iter().any(|id| {
            id.get("VendorID").and_then(Value::as_u64) == Some(BRIDGE_VENDOR) && id.get("ProductID").and_then(Value::as_u64) == Some(BRIDGE_PRODUCT)
        })
    })
}

/// Tablet configurations besides ours that name the S Pen, such as the one an older osu!tab left
/// under its old name. OpenTabletDriver makes a second tablet from each, with its own area.
pub fn duplicate_configurations(data: &Path) -> Vec<PathBuf> {
    let ours = format!("{TABLET_NAME}.json");
    let Ok(entries) = fs::read_dir(data.join("Configurations")) else { return Vec::new() };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .filter(|path| path.file_name().is_some_and(|name| name != ours.as_str()))
        .filter(|path| fs::read_to_string(path).is_ok_and(|text| names_the_s_pen(&text)))
        .collect()
}

/// True if setting up would change what a running daemon has loaded: the plugin, the S Pen's
/// configuration, or a second configuration for it. A daemon only reads those as it starts, and
/// it holds the plugin's file open.
pub fn setup_needs_restart(data: &Path) -> bool {
    let differs = |path: PathBuf, bytes: &[u8]| fs::read(path).map_or(true, |existing| existing != bytes);
    let plugin = assets::bridge_plugin().is_some_and(|plugin| differs(plugin_path(data), plugin));
    plugin || differs(config_path(data), assets::bridge_config()) || !duplicate_configurations(data).is_empty()
}

pub fn daemon_running() -> bool {
    !crate::tuning::process_ids(DAEMON).is_empty()
}

/// Stops every OpenTabletDriver daemon, and waits for each to go. False if one couldn't be
/// stopped, which happens when it has administrator rights and this program doesn't.
pub fn stop_daemons() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
    let mut all = true;
    for id in crate::tuning::process_ids(DAEMON) {
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, id);
            if process.is_null() {
                all = false;
                continue;
            }
            // Stopping one that's already on its way out fails, so what counts is that it's gone.
            TerminateProcess(process, 1);
            if WaitForSingleObject(process, DAEMON_STOP_WAIT_MS) != 0 {
                all = false;
            }
            CloseHandle(process);
        }
    }
    all
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
    write(plugin_path(data), plugin).map_err(|e| e.to_string())?;
    write(config_path(data), config).map_err(|e| e.to_string())?;
    remove_duplicate_configurations(data)?;

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

/// Two tablets on the one pen would take turns with it.
fn remove_duplicate_configurations(data: &Path) -> Result<(), String> {
    for duplicate in duplicate_configurations(data) {
        fs::remove_file(&duplicate).map_err(|e| format!("can't remove {}: {e}", duplicate.display()))?;
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

    #[test]
    fn a_second_configuration_for_the_s_pen_is_found_and_removed() {
        let dir = std::env::temp_dir().join(format!("osutab-test-otd-dupes-{}", std::process::id()));
        let configs = dir.join("Configurations");
        fs::create_dir_all(&configs).unwrap();
        let ours = std::str::from_utf8(assets::bridge_config()).unwrap();
        assert!(names_the_s_pen(ours), "our own configuration should name the S Pen");
        fs::write(config_path(&dir), ours).unwrap();
        // What the old build installed: the same IDs under the old name.
        fs::write(configs.join("osutab S Pen.json"), ours.replace(TABLET_NAME, OLD_TABLET_NAME)).unwrap();
        fs::write(configs.join("copy.JSON"), format!("\u{feff}{ours}")).unwrap();
        let wacom = r#"{"Name":"Wacom CTL-472","DigitizerIdentifiers":[{"VendorID":1386,"ProductID":890}]}"#;
        fs::write(configs.join("Wacom CTL-472.json"), wacom).unwrap();
        fs::write(configs.join("broken.json"), "{").unwrap();

        let mut found: Vec<String> = duplicate_configurations(&dir).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        found.sort();
        assert_eq!(found, ["copy.JSON", "osutab S Pen.json"]);
        remove_duplicate_configurations(&dir).unwrap();
        assert!(duplicate_configurations(&dir).is_empty());
        assert!(config_path(&dir).exists() && configs.join("Wacom CTL-472.json").exists() && configs.join("broken.json").exists());
        assert!(!configs.join("osutab S Pen.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_configuration_or_a_duplicate_needs_a_restart() {
        let dir = std::env::temp_dir().join(format!("osutab-test-otd-restart-{}", std::process::id()));
        let configs = dir.join("Configurations");
        fs::create_dir_all(&configs).unwrap();
        assert!(setup_needs_restart(&dir), "a daemon without our configuration doesn't know the S Pen");
        fs::write(config_path(&dir), assets::bridge_config()).unwrap();
        if let Some(plugin) = assets::bridge_plugin() {
            fs::create_dir_all(plugin_path(&dir).parent().unwrap()).unwrap();
            fs::write(plugin_path(&dir), plugin).unwrap();
        }
        assert!(!setup_needs_restart(&dir));
        fs::write(configs.join("osutab S Pen.json"), assets::bridge_config()).unwrap();
        assert!(setup_needs_restart(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_second_tablet_cannot_take_the_pen_from_one_that_is_still_connected() {
        use std::io::Read;
        let relay: &'static Relay = Box::leak(Box::default());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || relay.serve_on(listener));
        let read_frame = |mut stream: &TcpStream| -> io::Result<[u8; DOWN_FRAME_LEN]> {
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut frame = [0u8; DOWN_FRAME_LEN];
            stream.read_exact(&mut frame).map(|_| frame)
        };

        let first = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        wait_for("the first connection", || relay.connected.load(Ordering::SeqCst));
        let mut second = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        second.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        match second.read(&mut [0u8; 1]) {
            Ok(0) => {}
            Err(e) if !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            other => panic!("the second connection should have been closed, got {other:?}"),
        }
        relay.send(&[7; DOWN_FRAME_LEN]);
        assert_eq!(read_frame(&first).unwrap()[0], 7, "the first should still get the pen");

        // Once the first has gone, the next one gets the pen.
        drop(first);
        let mut taken = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            let third = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            relay.send(&[9; DOWN_FRAME_LEN]);
            if read_frame(&third).is_ok_and(|frame| frame[0] == 9) {
                taken = Some(third);
                break;
            }
        }
        assert!(taken.is_some(), "a new connection should take over once the old one closed");
    }
}
