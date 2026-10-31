//! Saved settings and the first-run questions about orientation and pen area.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::mapping::{AreaMm, Rotation, TABLET_LONG_MM, TABLET_SHORT_MM};

pub const MIN_AREA_MM: f64 = 10.0;
const DEFAULT_PRESET: &str = "Default";
const FIRST_RUN_WIDTH_MM: f64 = 120.0;
const FIRST_RUN_HEIGHT_MM: f64 = 75.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub area: AreaMm,
}

/// Where pen reports go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Straight to the cursor. The fastest path.
    Direct,
    /// Through OpenTabletDriver, so its filters and plugins apply.
    OpenTabletDriver,
}

impl Output {
    fn name(self) -> &'static str {
        match self {
            Output::Direct => "direct",
            Output::OpenTabletDriver => "opentabletdriver",
        }
    }

    fn parse(name: &str) -> Option<Output> {
        match name {
            "direct" => Some(Output::Direct),
            "opentabletdriver" => Some(Output::OpenTabletDriver),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub output: Output,
    /// `None` follows the tablet's screen rotation.
    pub rotation: Option<Rotation>,
    pub presets: Vec<Preset>,
    /// Index into `presets`, always valid.
    pub active: usize,
    /// Keeps the area's shape the same as the screen's while resizing.
    pub aspect_lock: bool,
    pub tip_click: bool,
    /// A single letter or digit, or empty for no key.
    pub button_key: String,
    /// Draws the pen area on the tablet's screen.
    pub show_outline: bool,
}

fn full_area() -> AreaMm {
    AreaMm { width: TABLET_LONG_MM, height: TABLET_SHORT_MM, offset_x: 0.0, offset_y: 0.0, rotation_deg: 0.0 }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            output: Output::Direct,
            rotation: None,
            presets: vec![Preset { name: DEFAULT_PRESET.to_string(), area: full_area() }],
            active: 0,
            aspect_lock: true,
            tip_click: true,
            button_key: String::new(),
            show_outline: true,
        }
    }
}

impl Settings {
    /// What the setup screen starts from: landscape, and a size that suits osu! rather than the
    /// whole tablet, which feels slow.
    pub fn first_run() -> Self {
        let mut settings = Settings { rotation: Some(Rotation::R90), ..Settings::default() };
        settings.presets[0].area.width = FIRST_RUN_WIDTH_MM;
        settings.presets[0].area.height = FIRST_RUN_HEIGHT_MM;
        settings
    }

    pub fn area(&self) -> AreaMm {
        self.presets[self.active].area
    }

    pub fn area_mut(&mut self) -> &mut AreaMm {
        &mut self.presets[self.active].area
    }

    /// Saves the current area under `name`, replacing a preset of that name, and selects it.
    pub fn save_preset(&mut self, name: &str) -> Result<(), String> {
        let name = clean_preset_name(name)?;
        let area = self.area();
        match self.presets.iter().position(|p| p.name == name) {
            Some(index) => {
                self.presets[index].area = area;
                self.active = index;
            }
            None => {
                self.presets.push(Preset { name, area });
                self.active = self.presets.len() - 1;
            }
        }
        Ok(())
    }

    /// Removes the active preset. The last one can't be removed.
    pub fn delete_active_preset(&mut self) -> bool {
        if self.presets.len() <= 1 {
            return false;
        }
        self.presets.remove(self.active);
        self.active = self.active.min(self.presets.len() - 1);
        true
    }
}

fn clean_preset_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a preset needs a name".to_string());
    }
    if name.contains(['[', ']', '\n', '\r']) {
        return Err("a preset name can't contain [ or ]".to_string());
    }
    Ok(name.to_string())
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("osutab").join("config.toml")
}

pub fn orientation_name(rotation: Option<Rotation>) -> &'static str {
    match rotation {
        None => "auto",
        Some(Rotation::R0) => "portrait-usb-bottom",
        Some(Rotation::R90) => "landscape-usb-right",
        Some(Rotation::R180) => "portrait-usb-top",
        Some(Rotation::R270) => "landscape-usb-left",
    }
}

fn parse_orientation(name: &str) -> Option<Option<Rotation>> {
    match name {
        "auto" => Some(None),
        "portrait-usb-bottom" => Some(Some(Rotation::R0)),
        "landscape-usb-right" => Some(Some(Rotation::R90)),
        "portrait-usb-top" => Some(Some(Rotation::R180)),
        "landscape-usb-left" => Some(Some(Rotation::R270)),
        _ => None,
    }
}

pub fn to_text(s: &Settings) -> String {
    let mut text = format!(
        "# osutab settings. Delete this file to be asked again on the next start.\n\
         # output: direct, opentabletdriver\n\
         output = {}\n\
         # orientation: landscape-usb-right, landscape-usb-left, portrait-usb-bottom, portrait-usb-top, auto\n\
         orientation = {}\n\
         active_preset = {}\n\
         aspect_lock = {}\n\
         tip_click = {}\n\
         show_outline = {}\n\
         # A single letter or digit for the pen's side button. Leave empty for none.\n\
         button_key = {}\n\
         \n\
         # Pen areas in millimetres, as you see the tablet. Offsets move an area from the\n\
         # centre of the tablet, and rotation turns it clockwise.\n",
        s.output.name(),
        orientation_name(s.rotation),
        s.presets[s.active].name,
        s.aspect_lock,
        s.tip_click,
        s.show_outline,
        s.button_key,
    );
    for preset in &s.presets {
        let a = &preset.area;
        text.push_str(&format!(
            "\n[preset {}]\nwidth_mm = {}\nheight_mm = {}\noffset_x_mm = {}\noffset_y_mm = {}\nrotation_deg = {}\n",
            preset.name, a.width, a.height, a.offset_x, a.offset_y, a.rotation_deg,
        ));
    }
    text
}

pub fn from_text(text: &str) -> Result<Settings, String> {
    let mut s = Settings { presets: Vec::new(), ..Settings::default() };
    let mut active_name: Option<String> = None;
    // Files from before presets existed keep their one area at the top level.
    let mut legacy = full_area();
    let mut has_legacy = false;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[preset ") {
            let name = rest.strip_suffix(']').ok_or(format!("line {}: expected [preset NAME]", index + 1))?;
            s.presets.push(Preset { name: clean_preset_name(name)?, area: full_area() });
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(format!("line {}: expected key = value", index + 1))?;
        let (key, value) = (key.trim(), value.trim());
        let number = || value.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or(format!("{key}: invalid number {value}"));
        let boolean = || value.parse::<bool>().map_err(|_| format!("{key}: expected true or false, not {value}"));
        if let Some(preset) = s.presets.last_mut() {
            match key {
                "width_mm" => preset.area.width = number()?,
                "height_mm" => preset.area.height = number()?,
                "offset_x_mm" => preset.area.offset_x = number()?,
                "offset_y_mm" => preset.area.offset_y = number()?,
                "rotation_deg" => preset.area.rotation_deg = number()?,
                other => return Err(format!("unknown preset setting {other}")),
            }
            continue;
        }
        match key {
            "output" => s.output = Output::parse(value).ok_or(format!("unknown output {value}"))?,
            "orientation" => s.rotation = parse_orientation(value).ok_or(format!("unknown orientation {value}"))?,
            "active_preset" => active_name = Some(value.to_string()),
            "aspect_lock" => s.aspect_lock = boolean()?,
            "tip_click" => s.tip_click = boolean()?,
            "show_outline" => s.show_outline = boolean()?,
            "button_key" => s.button_key = value.to_string(),
            "area_width_mm" => (legacy.width, has_legacy) = (number()?, true),
            "area_height_mm" => (legacy.height, has_legacy) = (number()?, true),
            "area_offset_x_mm" => (legacy.offset_x, has_legacy) = (number()?, true),
            "area_offset_y_mm" => (legacy.offset_y, has_legacy) = (number()?, true),
            other => return Err(format!("unknown setting {other}")),
        }
    }

    if s.presets.is_empty() || has_legacy {
        s.presets.insert(0, Preset { name: DEFAULT_PRESET.to_string(), area: legacy });
    }
    for preset in &s.presets {
        if preset.area.width < MIN_AREA_MM || preset.area.height < MIN_AREA_MM {
            return Err(format!("preset {}: the area must be at least {MIN_AREA_MM} mm on each side", preset.name));
        }
    }
    if let Some(name) = active_name {
        s.active = s.presets.iter().position(|p| p.name == name).ok_or(format!("no preset named {name}"))?;
    }
    Ok(s)
}

pub fn load(path: &Path) -> Result<Option<Settings>, String> {
    match fs::read_to_string(path) {
        Ok(text) => from_text(&text).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, to_text(settings))
}

fn orientation_size(rotation: Option<Rotation>) -> (f64, f64) {
    rotation.unwrap_or(Rotation::R90).tablet_size_mm()
}

fn read_answer(input: &mut impl BufRead, output: &mut impl Write, question: &str) -> io::Result<String> {
    write!(output, "{question}")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no more input"));
    }
    Ok(line.trim().to_string())
}

/// Asks until the answer is empty (returning `default`), a word like "full" (returning `max`)
/// or a number within `min..=max`.
fn ask_mm(
    input: &mut impl BufRead,
    output: &mut impl Write,
    question: &str,
    default: f64,
    min: f64,
    max: f64,
) -> io::Result<f64> {
    loop {
        let answer = read_answer(input, output, question)?;
        if answer.is_empty() {
            return Ok(default);
        }
        let answer = answer.to_lowercase();
        if answer.starts_with("full") || answer == "max" {
            return Ok(max);
        }
        match answer.trim_end_matches("mm").trim().parse::<f64>() {
            Ok(v) if v >= min && v <= max => return Ok(v),
            _ => writeln!(output, "Type a number from {min:.0} to {max:.0}, or just press Enter.")?,
        }
    }
}

/// Asks for orientation and pen area. `screen_aspect` is the monitor's width divided by height.
pub fn ask(input: &mut impl BufRead, output: &mut impl Write, screen_aspect: f64) -> io::Result<Settings> {
    writeln!(output, "\nosutab setup\n")?;
    writeln!(output, "How will the tablet sit while you play?")?;
    writeln!(output, "  1. Landscape, USB port on the right (default)")?;
    writeln!(output, "  2. Landscape, USB port on the left")?;
    writeln!(output, "  3. Portrait, USB port at the bottom")?;
    writeln!(output, "  4. Portrait, USB port at the top")?;
    writeln!(output, "  5. Follow the tablet's screen rotation")?;
    let rotation = loop {
        match read_answer(input, output, "Choose 1-5: ")?.as_str() {
            "" | "1" => break Some(Rotation::R90),
            "2" => break Some(Rotation::R270),
            "3" => break Some(Rotation::R0),
            "4" => break Some(Rotation::R180),
            "5" => break None,
            _ => writeln!(output, "Enter a number from 1 to 5.")?,
        }
    };

    let (max_w, max_h) = orientation_size(rotation);
    writeln!(output, "\nHow big should the pen area be? This part of the tablet covers your whole screen.")?;
    let width = ask_mm(
        input,
        output,
        &format!("Width in mm, up to {max_w:.0} (just press Enter for the full width): "),
        max_w,
        MIN_AREA_MM,
        max_w,
    )?;
    let matching = (width / screen_aspect).clamp(MIN_AREA_MM, max_h);
    let height = ask_mm(
        input,
        output,
        &format!("Height in mm, up to {max_h:.0} (just press Enter for {matching:.1}, which matches your screen's shape): "),
        matching,
        MIN_AREA_MM,
        max_h,
    )?;

    writeln!(output, "\nThe pen area is centred on the tablet.\n")?;
    let mut settings = Settings { rotation, ..Settings::default() };
    settings.presets[0].area = AreaMm { width, height, offset_x: 0.0, offset_y: 0.0, rotation_deg: 0.0 };
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn answers(text: &str) -> Settings {
        ask(&mut Cursor::new(text.as_bytes().to_vec()), &mut Vec::new(), 1.6).unwrap()
    }

    fn size(s: &Settings) -> (f64, f64) {
        (s.area().width, s.area().height)
    }

    #[test]
    fn enter_everywhere_gives_full_landscape() {
        let s = answers("\n\n\n");
        assert_eq!(s.rotation, Some(Rotation::R90));
        // The matching height would be 196.1 mm, slightly more than the tablet has.
        assert_eq!(size(&s), (TABLET_LONG_MM, TABLET_SHORT_MM));
    }

    #[test]
    fn height_defaults_to_screen_shape() {
        assert_eq!(size(&answers("1\n120\n\n")), (120.0, 75.0));
    }

    #[test]
    fn explicit_portrait_area() {
        let s = answers("3\n100\n150mm\n");
        assert_eq!(s.rotation, Some(Rotation::R0));
        assert_eq!(size(&s), (100.0, 150.0));
    }

    #[test]
    fn invalid_answers_ask_again() {
        // 9 is not a choice, 5000 is too wide, 2 is too small, abc is not a number.
        let s = answers("9\n2\n5000\n2\nabc\n120\n70\n");
        assert_eq!(s.rotation, Some(Rotation::R270));
        assert_eq!(size(&s), (120.0, 70.0));
    }

    #[test]
    fn portrait_limits_width_to_short_side() {
        assert_eq!(size(&answers("3\n300\n100\n100\n")), (100.0, 100.0));
    }

    #[test]
    fn typing_full_means_the_largest_size() {
        // A real answer from the first test: the words from the prompt, typed out.
        assert_eq!(size(&answers("1\nfull width\nFull\n")), (TABLET_LONG_MM, TABLET_SHORT_MM));
        assert_eq!(size(&answers("1\n100\nmax\n")), (100.0, TABLET_SHORT_MM));
    }

    #[test]
    fn ended_input_is_an_error_not_a_loop() {
        let mut input = Cursor::new(b"9\n".to_vec());
        assert!(ask(&mut input, &mut Vec::new(), 1.6).is_err());
    }

    #[test]
    fn text_round_trip_with_presets() {
        for rotation in [None, Some(Rotation::R0), Some(Rotation::R90), Some(Rotation::R180), Some(Rotation::R270)] {
            let s = Settings {
                output: Output::OpenTabletDriver,
                rotation,
                presets: vec![
                    Preset { name: "Default".into(), area: full_area() },
                    Preset {
                        name: "Small jumps = 90 mm".into(),
                        area: AreaMm { width: 120.5, height: 75.25, offset_x: -3.5, offset_y: 10.0, rotation_deg: 12.5 },
                    },
                ],
                active: 1,
                aspect_lock: false,
                tip_click: false,
                button_key: "z".into(),
                show_outline: false,
            };
            assert_eq!(from_text(&to_text(&s)).unwrap(), s);
        }
    }

    #[test]
    fn reads_the_older_single_area_file() {
        let s = from_text(
            "orientation = landscape-usb-right\narea_width_mm = 120\narea_height_mm = 75\narea_offset_x_mm = 1\narea_offset_y_mm = 2\n",
        )
        .unwrap();
        assert_eq!(s.rotation, Some(Rotation::R90));
        assert_eq!(s.presets.len(), 1);
        assert_eq!(s.area(), AreaMm { width: 120.0, height: 75.0, offset_x: 1.0, offset_y: 2.0, rotation_deg: 0.0 });
    }

    #[test]
    fn first_run_starts_landscape_at_a_playable_size() {
        let s = Settings::first_run();
        assert_eq!(s.rotation, Some(Rotation::R90));
        assert_eq!(size(&s), (120.0, 75.0));
        assert!(s.aspect_lock);
    }

    #[test]
    fn saving_presets() {
        let mut s = Settings::default();
        s.area_mut().width = 100.0;
        s.save_preset("  Small ").unwrap();
        assert_eq!(s.presets[s.active].name, "Small");
        assert_eq!(s.presets.len(), 2);

        s.area_mut().width = 90.0;
        s.save_preset("Small").unwrap();
        assert_eq!(s.presets.len(), 2, "same name replaces");
        assert_eq!(s.area().width, 90.0);

        assert!(s.save_preset("  ").is_err());
        assert!(s.save_preset("a]b").is_err());
    }

    #[test]
    fn last_preset_cannot_be_deleted() {
        let mut s = Settings::default();
        s.save_preset("Second").unwrap();
        assert!(s.delete_active_preset());
        assert_eq!(s.active, 0);
        assert!(!s.delete_active_preset());
        assert_eq!(s.presets.len(), 1);
    }

    #[test]
    fn bad_files_are_rejected() {
        assert!(from_text("orientation = sideways").is_err());
        assert!(from_text("output = windows-ink").is_err());
        assert!(from_text("area_width_mm = wide").is_err());
        assert!(from_text("area_width_mm = NaN").is_err());
        assert!(from_text("area_width_mm = 1").is_err());
        assert!(from_text("mystery = 1").is_err());
        assert!(from_text("no equals sign").is_err());
        assert!(from_text("active_preset = Missing").is_err());
        assert!(from_text("[preset Oops]\nwidth_mm = 2").is_err());
    }
}
