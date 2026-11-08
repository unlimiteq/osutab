//! Thin wrapper around the adb executable.

use std::io::{self, BufRead, BufReader, ErrorKind};
use std::net::{Ipv4Addr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use sha1::{Digest, Sha1};

use crate::mapping::Rotation;

pub const REMOTE_HELPER: &str = "/data/local/tmp/osutab-helper";
// What's on the tablet, so a later start can check that without adb.exe.
const HELPER_STAMP: &str = "/data/local/tmp/osutab-helper.stamp";
const APP_STAMP: &str = "/data/local/tmp/osutab-app.stamp";
const APP_PACKAGE: &str = "dev.osutab.app";
// Android's WINDOWING_MODE_FULLSCREEN.
const FULLSCREEN_WINDOWING_MODE: u32 = 1;
// Pinning needs the app already showing, which takes a moment after starting it.
const PIN_SETTLE_SECONDS: &str = "0.8";
// Keeps each adb call from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What adb can see of the tablet right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    AdbMissing,
    NoDevice,
    Unauthorized,
    Offline,
    Ready,
}

/// Identifies this exact helper.
pub fn helper_stamp(helper: &Path) -> io::Result<String> {
    let digest = Sha1::digest(std::fs::read(helper)?);
    Ok(format!("helper-{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// Identifies this exact tablet app.
pub fn app_stamp(apk: &Path) -> io::Result<String> {
    let meta = std::fs::metadata(apk)?;
    let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    Ok(format!("{}-{modified}", meta.len()))
}

/// The tablet command that lists what's installed, to compare against the stamps above.
pub fn read_stamps_command() -> String {
    format!("cat {HELPER_STAMP} {APP_STAMP} 2>/dev/null")
}

/// True if every stamp wanted is in what the tablet said.
pub fn stamps_match(installed: &str, wanted: &[String]) -> bool {
    wanted.iter().all(|stamp| installed.lines().any(|line| line.trim() == stamp))
}

/// The tablet command that brings up the app. If the app has gone, it forgets the app's stamp, so
/// the next start installs it again.
pub fn open_app_command() -> String {
    format!("{} 2>&1 | grep -q Error && rm -f {APP_STAMP}", start_app_command())
}

/// Starts the app full screen. A tablet in its desktop mode would otherwise open it in a floating
/// window, where the area it draws doesn't line up with the pen.
fn start_app_command() -> String {
    format!("am start --windowingMode {FULLSCREEN_WINDOWING_MODE} -n {APP_PACKAGE}/.MainActivity")
}

/// The tablet command that pins the app once it's showing, for the same job as `Adb::pin_app`.
pub fn pin_app_command() -> String {
    format!(
        "sleep {}; task=$(dumpsys activity activities | grep -m1 'Task{{.*{APP_PACKAGE}' | sed -E 's/^[^#]*#([0-9]+).*/\\1/'); [ -n \"$task\" ] && cmd activity task lock $task",
        PIN_SETTLE_SECONDS
    )
}

pub const UNPIN_COMMAND: &str = "cmd activity task lock stop";

/// Whether an adb server is listening on its standard port. This takes up to 200 ms when there
/// isn't one, so it's only for when adb.exe is about to be needed anyway.
pub fn server_running() -> bool {
    let address = (Ipv4Addr::LOCALHOST, 5037).into();
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

/// Reads the result of `adb get-state`.
fn parse_state(stdout: &str, stderr: &str) -> DeviceState {
    if stdout.trim() == "device" {
        DeviceState::Ready
    } else if stderr.contains("unauthorized") || stdout.contains("unauthorized") {
        DeviceState::Unauthorized
    } else if stderr.contains("offline") || stdout.contains("offline") {
        DeviceState::Offline
    } else {
        DeviceState::NoDevice
    }
}

#[derive(Clone)]
pub struct Adb {
    path: PathBuf,
}

impl Adb {
    pub fn new(path: PathBuf) -> Self {
        Adb { path }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }

    fn output(&self, args: &[&str]) -> io::Result<String> {
        let out = self.command().args(args).stdin(Stdio::null()).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "adb {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn start_server(&self) -> io::Result<()> {
        self.output(&["start-server"]).map(|_| ())
    }

    /// Lets go of the USB interface, so this program can open it directly.
    pub fn kill_server(&self) -> io::Result<()> {
        self.output(&["kill-server"]).map(|_| ())
    }

    pub fn device_state(&self) -> DeviceState {
        match self.command().arg("get-state").stdin(Stdio::null()).output() {
            Ok(out) => parse_state(&String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr)),
            Err(e) if e.kind() == ErrorKind::NotFound => DeviceState::AdbMissing,
            Err(_) => DeviceState::NoDevice,
        }
    }

    pub fn push_helper(&self, local: &Path) -> io::Result<()> {
        let stamp = helper_stamp(local)?;
        let local = local.to_string_lossy();
        self.output(&["push", &local, REMOTE_HELPER])?;
        self.output(&["shell", &format!("chmod 755 {REMOTE_HELPER}; echo {stamp} > {HELPER_STAMP}")])?;
        Ok(())
    }

    pub fn forward(&self, port: u16, socket: &str) -> io::Result<()> {
        let local = format!("tcp:{port}");
        let remote = format!("localabstract:{socket}");
        self.output(&["forward", &local, &remote]).map(|_| ())
    }

    pub fn reverse(&self, port: u16) -> io::Result<()> {
        let spec = format!("tcp:{port}");
        self.output(&["reverse", &spec, &spec]).map(|_| ())
    }

    /// Installs the tablet app if it isn't there or this APK differs from the installed one.
    pub fn ensure_app(&self, apk: &Path) -> io::Result<()> {
        let wanted = app_stamp(apk)?;
        let installed = self.output(&["shell", &format!("pm path {APP_PACKAGE}; cat {APP_STAMP} 2>/dev/null")]).unwrap_or_default();
        if installed.contains("package:") && installed.contains(&wanted) {
            return Ok(());
        }
        let apk = apk.to_string_lossy();
        self.output(&["install", "-r", &apk])?;
        self.output(&["shell", &format!("echo {wanted} > {APP_STAMP}")]).map(|_| ())
    }

    pub fn start_app(&self) -> io::Result<()> {
        self.output(&["shell", &start_app_command()]).map(|_| ())
    }

    /// Pins the tablet app, which blocks Home and Back. Android allows this from adb's shell.
    pub fn pin_app(&self) -> io::Result<()> {
        let dump = self.output(&["shell", "dumpsys activity activities"])?;
        let task = parse_app_task(&dump).ok_or_else(|| io::Error::other("the tablet app isn't running"))?;
        self.output(&["shell", &format!("cmd activity task lock {task}")]).map(|_| ())
    }

    /// On this tablet, this also sends it to its lock screen.
    pub fn unpin_app(&self) -> io::Result<()> {
        self.output(&["shell", UNPIN_COMMAND]).map(|_| ())
    }

    pub fn spawn_helper(&self, args: &str) -> io::Result<Child> {
        let command = format!("pkill -x osutab-helper; exec {REMOTE_HELPER} {args}");
        let mut child = self
            .command()
            .args(["shell", &command])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // The helper's messages only matter when someone is watching a console.
        for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn io::Read + Send>)].into_iter().flatten() {
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    println!("{line}");
                }
            });
        }
        Ok(child)
    }

    pub fn display_rotation(&self) -> Option<Rotation> {
        let dump = self.output(&["shell", "dumpsys input"]).ok()?;
        parse_rotation(&dump)
    }
}

/// Finds the tablet app's task id in `dumpsys activity activities`.
fn parse_app_task(dump: &str) -> Option<u32> {
    let line = dump.lines().find(|l| l.contains("Task{") && l.contains(APP_PACKAGE))?;
    let rest = &line[line.find('#')? + 1..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn parse_rotation(dump: &str) -> Option<Rotation> {
    let line = dump.lines().find(|l| l.contains("Viewport INTERNAL"))?;
    let rest = &line[line.find("orientation=")? + "orientation=".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Rotation::from_android(digits.parse().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_command_finds_the_app_the_same_way() {
        let command = pin_app_command();
        assert!(command.contains("Task{.*dev.osutab.app"), "{command}");
        assert!(command.contains(r"s/^[^#]*#([0-9]+).*/\1/"), "{command}");
        assert!(command.ends_with("cmd activity task lock $task"), "{command}");
    }

    #[test]
    fn stamps_must_all_be_there_on_their_own_line() {
        let wanted = ["helper-abc".to_string(), "100-200".to_string()];
        assert!(stamps_match("helper-abc\r\n100-200\r\n", &wanted));
        assert!(!stamps_match("helper-abc\n", &wanted));
        assert!(!stamps_match("helper-abcd\n100-200\n", &wanted));
        assert!(!stamps_match("", &wanted));
        assert!(stamps_match("", &[]));
    }

    #[test]
    fn a_stamp_changes_with_the_file() {
        let dir = std::env::temp_dir().join(format!("osutab-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("helper");
        std::fs::write(&path, b"one").unwrap();
        let first = helper_stamp(&path).unwrap();
        assert_eq!(first, helper_stamp(&path).unwrap());
        std::fs::write(&path, b"two").unwrap();
        assert_ne!(first, helper_stamp(&path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_real_viewport_line() {
        let dump = "      Viewports:\n        Viewport INTERNAL: displayId=0, uniqueId=local:46, port=130, orientation=1, logicalFrame=[0, 0, 2960, 1848], isActive=[1]\n";
        assert_eq!(parse_rotation(dump), Some(Rotation::R90));
    }

    #[test]
    fn finds_the_apps_task() {
        let dump = "    * Task{4c1 #12 type=standard A=2000:com.other}\n    * Task{9f2 #57 type=standard A=2000:dev.osutab.app}\n";
        assert_eq!(parse_app_task(dump), Some(57));
        assert_eq!(parse_app_task("nothing useful"), None);
    }

    #[test]
    fn missing_viewport_gives_none() {
        assert_eq!(parse_rotation("nothing useful"), None);
    }

    #[test]
    fn device_states_from_real_adb_output() {
        assert_eq!(parse_state("device\n", ""), DeviceState::Ready);
        assert_eq!(parse_state("", "error: device unauthorized.\nThis adb server's $ADB_VENDOR_KEYS is not set"), DeviceState::Unauthorized);
        assert_eq!(parse_state("", "error: device offline"), DeviceState::Offline);
        assert_eq!(parse_state("", "error: no devices/emulators found"), DeviceState::NoDevice);
    }
}
