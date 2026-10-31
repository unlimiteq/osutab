//! Reads osu!lazer's saved input settings, so the settings window can warn about ones that stop
//! the cursor following the pen. It only reads; changing them is left to the user, in lazer.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LazerInput {
    /// Lazer's "High precision mouse". When on, lazer moves its cursor by scaled movement
    /// instead of using the absolute position.
    pub high_precision: bool,
}

pub fn parse(json: &str) -> Option<LazerInput> {
    let start = json.find("\"UseRelativeMode\"")? + "\"UseRelativeMode\"".len();
    let value = json[start..].trim_start().strip_prefix(':')?.trim_start();
    if value.starts_with("true") {
        Some(LazerInput { high_precision: true })
    } else if value.starts_with("false") {
        Some(LazerInput { high_precision: false })
    } else {
        None
    }
}

fn data_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("osu"))
}

/// `None` when lazer isn't installed or its settings can't be read.
pub fn read() -> Option<LazerInput> {
    parse(&std::fs::read_to_string(data_dir()?.join("input.json")).ok()?)
}

/// Lazer's display settings that can hold input back before it reaches the screen.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LazerDisplay {
    pub frame_sync: Option<String>,
    pub window_mode: Option<String>,
}

pub fn parse_display(ini: &str) -> LazerDisplay {
    let value = |key: &str| {
        ini.lines().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    };
    LazerDisplay { frame_sync: value("FrameSync"), window_mode: value("WindowMode") }
}

pub fn read_display() -> Option<LazerDisplay> {
    Some(parse_display(&std::fs::read_to_string(data_dir()?.join("framework.ini")).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The relevant part of a real input.json from this project's test machine.
    const REAL: &str = r#"{"InputHandlers":[{"$type":"osu.Framework.Input.Handlers.Pen.PenHandler, osu.Framework","Sensitivity":1.75,"IsActive":true},{"$type":"osu.Framework.Platform.Windows.WindowsMouseHandler, osu.Framework","IsActive":true,"UseRelativeMode":true,"Sensitivity":2.5,"Description":"Mouse","Enabled":true}]}"#;

    #[test]
    fn reads_the_real_file_format() {
        assert_eq!(parse(REAL), Some(LazerInput { high_precision: true }));
        let off = REAL.replace("\"UseRelativeMode\":true", "\"UseRelativeMode\": false");
        assert_eq!(parse(&off), Some(LazerInput { high_precision: false }));
    }

    #[test]
    fn reads_display_settings_from_a_real_ini() {
        let ini = "ConfineMouseMode = Fullscreen\nFrameSync = Unlimited\nWindowMode = Fullscreen\nCursorSensitivity = 1.0\n";
        let d = parse_display(ini);
        assert_eq!(d.frame_sync.as_deref(), Some("Unlimited"));
        assert_eq!(d.window_mode.as_deref(), Some("Fullscreen"));
        assert_eq!(parse_display("nothing"), LazerDisplay::default());
    }

    #[test]
    fn unknown_contents_give_none() {
        assert_eq!(parse("{}"), None);
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"UseRelativeMode":"maybe"}"#), None);
    }
}
