//! OpenTabletDriver output: pen reports go to OpenTabletDriver's daemon, through the bridge
//! plugin, so its filters, output modes and other plugins all apply to the S Pen.

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use osutab_core::DOWN_FRAME_LEN;
use serde_json::{json, Value};

use crate::assets;
use crate::mapping::{to_unit, Rotation};

pub const RELAY_PORT: u16 = 27193;
const TOOL_PATH: &str = "OsutabBridge.OsutabBridgeTool";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FLUSH_INTERVAL: Duration = Duration::from_millis(2);
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

/// Adds the bridge tool to OpenTabletDriver's settings, keeping everything else in them. Returns
/// the new text, or `None` if it's already there. A tool the user turned off stays off.
pub fn settings_with_tool(existing: Option<&str>) -> Result<Option<String>, String> {
    let mut settings: Value = match existing {
        Some(text) => serde_json::from_str(text).map_err(|e| format!("can't read settings: {e}"))?,
        None => json!({ "Profiles": [], "LockUsableAreaDisplay": true, "LockUsableAreaTablet": true }),
    };
    let root = settings.as_object_mut().ok_or("settings aren't an object")?;
    let tools = root.entry("Tools").or_insert_with(|| json!([]));
    let list = tools.as_array_mut().ok_or("Tools isn't a list")?;
    if list.iter().any(|t| t.get("Path").and_then(Value::as_str) == Some(TOOL_PATH)) {
        return Ok(None);
    }
    list.push(json!({ "Path": TOOL_PATH, "Settings": [], "Enable": true }));
    serde_json::to_string_pretty(&settings).map(Some).map_err(|e| e.to_string())
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
    write(data.join("Configurations").join("osutab S Pen.json"), config).map_err(|e| e.to_string())?;

    let settings_path = data.join("settings.json");
    let existing = fs::read_to_string(&settings_path).ok();
    if let Some(updated) = settings_with_tool(existing.as_deref())? {
        if existing.is_some() {
            let _ = fs::copy(&settings_path, data.join("settings.before-osutab.json"));
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

/// Runs the daemon that does the work. It exits by itself if another copy is already running.
pub fn start_daemon(dir: &Path) -> io::Result<Child> {
    Command::new(dir.join("OpenTabletDriver.Daemon.exe"))
        .current_dir(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Opens OpenTabletDriver's own settings window.
pub fn open_settings_window(dir: &Path) -> io::Result<()> {
    Command::new(dir.join("OpenTabletDriver.UX.Wpf.exe")).current_dir(dir).spawn().map(|_| ())
}

/// Which output mode OpenTabletDriver's settings use, to warn about Windows Ink.
pub fn uses_windows_ink(data: &Path) -> bool {
    fs::read_to_string(data.join("settings.json")).is_ok_and(|text| {
        let Ok(settings) = serde_json::from_str::<Value>(&text) else { return false };
        let Some(profiles) = settings.get("Profiles").and_then(Value::as_array) else { return false };
        profiles.iter().any(|p| {
            let matches_tablet = p.get("Tablet").and_then(Value::as_str) == Some("osutab S Pen");
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
    fn broken_settings_are_left_alone() {
        assert!(settings_with_tool(Some("not json")).is_err());
        assert!(settings_with_tool(Some("[1,2]")).is_err());
    }

    #[test]
    fn windows_ink_is_noticed_only_for_our_tablet() {
        let dir = std::env::temp_dir().join(format!("osutab-test-otd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ink = r#"{"Profiles":[{"Tablet":"osutab S Pen","OutputMode":{"Path":"VoiDPlugins.OutputMode.WinInkAbsoluteMode"}}]}"#;
        fs::write(dir.join("settings.json"), ink).unwrap();
        assert!(uses_windows_ink(&dir));
        let other = r#"{"Profiles":[{"Tablet":"Wacom","OutputMode":{"Path":"VoiDPlugins.OutputMode.WinInkAbsoluteMode"}},{"Tablet":"osutab S Pen","OutputMode":{"Path":"OpenTabletDriver.Desktop.Output.AbsoluteMode"}}]}"#;
        fs::write(dir.join("settings.json"), other).unwrap();
        assert!(!uses_windows_ink(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
