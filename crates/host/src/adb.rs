//! Thin wrapper around the adb executable.

use std::io::{self, BufRead, BufReader, ErrorKind};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::mapping::Rotation;

pub const REMOTE_HELPER: &str = "/data/local/tmp/osutab-helper";
const APP_PACKAGE: &str = "dev.osutab.app";
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

    pub fn device_state(&self) -> DeviceState {
        match self.command().arg("get-state").stdin(Stdio::null()).output() {
            Ok(out) => parse_state(&String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr)),
            Err(e) if e.kind() == ErrorKind::NotFound => DeviceState::AdbMissing,
            Err(_) => DeviceState::NoDevice,
        }
    }

    pub fn push_helper(&self, local: &Path) -> io::Result<()> {
        let local = local.to_string_lossy();
        self.output(&["push", &local, REMOTE_HELPER])?;
        self.output(&["shell", &format!("chmod 755 {REMOTE_HELPER}")])?;
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
        const STAMP: &str = "/data/local/tmp/osutab-app.stamp";
        let meta = std::fs::metadata(apk)?;
        let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let wanted = format!("{}-{modified}", meta.len());
        let installed = self.output(&["shell", &format!("pm path {APP_PACKAGE}; cat {STAMP} 2>/dev/null")]).unwrap_or_default();
        if installed.contains("package:") && installed.contains(&wanted) {
            return Ok(());
        }
        let apk = apk.to_string_lossy();
        self.output(&["install", "-r", &apk])?;
        self.output(&["shell", &format!("echo {wanted} > {STAMP}")]).map(|_| ())
    }

    pub fn start_app(&self) -> io::Result<()> {
        self.output(&["shell", &format!("am start -n {APP_PACKAGE}/.MainActivity")]).map(|_| ())
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
    fn parses_real_viewport_line() {
        let dump = "      Viewports:\n        Viewport INTERNAL: displayId=0, uniqueId=local:46, port=130, orientation=1, logicalFrame=[0, 0, 2960, 1848], isActive=[1]\n";
        assert_eq!(parse_rotation(dump), Some(Rotation::R90));
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
