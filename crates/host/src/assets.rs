//! The tablet helper and tablet app, which adb needs as files on disk. In a finished build they
//! are embedded in this program and unpacked to its data folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(embedded_assets)]
const HELPER: Option<&[u8]> = Some(include_bytes!(env!("OSUTAB_HELPER_PATH")));
#[cfg(embedded_assets)]
const APP: Option<&[u8]> = Some(include_bytes!(env!("OSUTAB_APK_PATH")));
#[cfg(not(embedded_assets))]
const HELPER: Option<&[u8]> = None;
#[cfg(not(embedded_assets))]
const APP: Option<&[u8]> = None;

#[cfg(embedded_bridge)]
const BRIDGE: Option<&[u8]> = Some(include_bytes!(env!("OSUTAB_BRIDGE_PATH")));
#[cfg(not(embedded_bridge))]
const BRIDGE: Option<&[u8]> = None;
const BRIDGE_CONFIG: &[u8] = include_bytes!("../../../otd-plugin/osu!tab S Pen.json");

pub fn bridge_plugin() -> Option<&'static [u8]> {
    BRIDGE
}

pub fn bridge_config() -> &'static [u8] {
    BRIDGE_CONFIG
}

/// Writes the file only if it differs. The app's install check uses the file's time, so an
/// unchanged file must keep it.
fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, bytes)
}

fn unpack(dir: &Path, name: &str, bytes: Option<&[u8]>) -> Option<PathBuf> {
    let path = dir.join(name);
    match write_if_changed(&path, bytes?) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!("could not unpack {name}: {e}");
            None
        }
    }
}

pub fn helper(dir: &Path) -> Option<PathBuf> {
    unpack(dir, "osutab-helper", HELPER)
}

pub fn app(dir: &Path) -> Option<PathBuf> {
    unpack(dir, "osutab.apk", APP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("osutab-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn unchanged_file_is_not_rewritten() {
        let dir = temp_dir("same");
        let path = dir.join("a");
        write_if_changed(&path, b"one").unwrap();
        let first = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        write_if_changed(&path, b"one").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), first);
        write_if_changed(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let _ = fs::remove_dir_all(&dir);
    }
}
