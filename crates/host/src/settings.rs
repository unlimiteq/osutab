//! Saved settings and the first-run questions about orientation and pen area.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::mapping::{AreaMm, Rotation, TABLET_LONG_MM, TABLET_SHORT_MM};
use crate::theme::{self, Rgb, Theme, ThemeOverrides, DEFAULT_THEME, MAX_THICKNESS, MIN_THICKNESS};

pub const MIN_AREA_MM: f64 = 10.0;
const DEFAULT_PRESET: &str = "Default";
const FIRST_RUN_WIDTH_MM: f64 = 120.0;
const FIRST_RUN_HEIGHT_MM: f64 = 75.0;
pub const HOLD_SECONDS: std::ops::RangeInclusive<f64> = 0.3..=5.0;
pub const EDIT_TIMEOUT_SECONDS: std::ops::RangeInclusive<f64> = 5.0..=120.0;
pub const DEFAULT_HOLD_SECONDS: f64 = 1.0;
pub const DEFAULT_EDIT_TIMEOUT_SECONDS: f64 = 20.0;

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

/// How the pen's reports reach the PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbPath {
    /// Through adb.exe, which everything else on the PC can share.
    Adb,
    /// Straight to the USB interface, which stops adb.exe while osutab runs.
    Direct,
}

impl UsbPath {
    fn name(self) -> &'static str {
        match self {
            UsbPath::Adb => "adb",
            UsbPath::Direct => "direct",
        }
    }

    pub fn parse(name: &str) -> Option<UsbPath> {
        match name {
            "adb" => Some(UsbPath::Adb),
            "direct" => Some(UsbPath::Direct),
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
    /// The name of a built-in or saved theme.
    pub theme: String,
    pub theme_overrides: ThemeOverrides,
    pub saved_themes: Vec<Theme>,
    pub animations: bool,
    /// Shows the size and sensitivity inside the area on the tablet.
    pub show_area_label: bool,
    pub hold_to_edit_seconds: f64,
    pub edit_timeout_seconds: f64,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    /// Brings the tablet app to the front when the tablet connects.
    pub open_tablet_app: bool,
    pub screen_pinning: bool,
    pub usb_path: UsbPath,
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
            theme: DEFAULT_THEME.to_string(),
            theme_overrides: ThemeOverrides::default(),
            saved_themes: Vec::new(),
            animations: true,
            show_area_label: true,
            hold_to_edit_seconds: DEFAULT_HOLD_SECONDS,
            edit_timeout_seconds: DEFAULT_EDIT_TIMEOUT_SECONDS,
            close_to_tray: true,
            start_minimized: false,
            open_tablet_app: true,
            screen_pinning: false,
            usb_path: UsbPath::Direct,
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

    /// The theme chosen, without any changes made on top of it.
    pub fn base_theme(&self) -> Theme {
        self.saved_themes
            .iter()
            .find(|t| t.name == self.theme)
            .cloned()
            .or_else(|| theme::built_in().into_iter().find(|t| t.name == self.theme))
            .unwrap_or_else(|| theme::built_in().remove(0))
    }

    /// The colors to draw with.
    pub fn active_theme(&self) -> Theme {
        self.theme_overrides.apply(&self.base_theme())
    }

    pub fn choose_theme(&mut self, name: &str) {
        self.theme = name.to_string();
        self.theme_overrides = ThemeOverrides::default();
    }

    /// Saves the colors in use under `name`, replacing a saved theme of that name, and selects it.
    pub fn save_theme(&mut self, name: &str) -> Result<(), String> {
        let name = theme::clean_theme_name(name)?;
        let saved = Theme { name: name.clone(), ..self.active_theme() };
        match self.saved_themes.iter_mut().find(|t| t.name == name) {
            Some(existing) => *existing = saved,
            None => self.saved_themes.push(saved),
        }
        self.choose_theme(&name);
        Ok(())
    }

    /// Removes the selected theme if it's a saved one, and goes back to the default.
    pub fn delete_saved_theme(&mut self) -> bool {
        let Some(index) = self.saved_themes.iter().position(|t| t.name == self.theme) else {
            return false;
        };
        self.saved_themes.remove(index);
        self.choose_theme(DEFAULT_THEME);
        true
    }
}

pub(crate) fn clean_name(name: &str, what: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("a {what} needs a name"));
    }
    if name.contains(['[', ']', '\n', '\r']) {
        return Err(format!("a {what} name can't contain [ or ]"));
    }
    Ok(name.to_string())
}

fn clean_preset_name(name: &str) -> Result<String, String> {
    clean_name(name, "preset")
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

fn optional_color(color: Option<Rgb>) -> String {
    color.map_or(String::new(), Rgb::to_hex)
}

fn optional_number<T: ToString>(value: Option<T>) -> String {
    value.map_or(String::new(), |v| v.to_string())
}

pub fn to_text(s: &Settings) -> String {
    let o = &s.theme_overrides;
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
         # The key for the pen's side button, such as z, esc, f5 or `. Leave empty for none.\n\
         button_key = {}\n\
         \n\
         # Appearance. A built-in theme, or one saved below. Colors are #rrggbb.\n\
         theme = {}\n\
         animations = {}\n\
         # Changes on top of the theme. Leave one empty to use the theme's value.\n\
         color_background = {}\n\
         color_panel = {}\n\
         color_text = {}\n\
         color_muted = {}\n\
         color_accent = {}\n\
         outline_color = {}\n\
         # From {MIN_THICKNESS} to {MAX_THICKNESS}.\n\
         outline_thickness = {}\n\
         # How dark the tablet is outside the pen area, from 0 to 100.\n\
         outside_dim = {}\n\
         \n\
         # The tablet app.\n\
         show_area_label = {}\n\
         hold_to_edit_seconds = {}\n\
         edit_timeout_seconds = {}\n\
         open_tablet_app = {}\n\
         # Blocks Home and Back on the tablet. Leaving it sends the tablet to its lock screen.\n\
         screen_pinning = {}\n\
         \n\
         # This program.\n\
         close_to_tray = {}\n\
         start_minimized = {}\n\
         # usb_path: adb, direct. Direct stops adb.exe while osutab runs.\n\
         usb_path = {}\n\
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
        s.theme,
        s.animations,
        optional_color(o.background),
        optional_color(o.panel),
        optional_color(o.text),
        optional_color(o.muted),
        optional_color(o.accent),
        optional_color(o.outline),
        optional_number(o.outline_thickness),
        optional_number(o.outside_dim),
        s.show_area_label,
        s.hold_to_edit_seconds,
        s.edit_timeout_seconds,
        s.open_tablet_app,
        s.screen_pinning,
        s.close_to_tray,
        s.start_minimized,
        s.usb_path.name(),
    );
    for preset in &s.presets {
        let a = &preset.area;
        text.push_str(&format!(
            "\n[preset {}]\nwidth_mm = {}\nheight_mm = {}\noffset_x_mm = {}\noffset_y_mm = {}\nrotation_deg = {}\n",
            preset.name, a.width, a.height, a.offset_x, a.offset_y, a.rotation_deg,
        ));
    }
    for t in &s.saved_themes {
        text.push_str(&format!(
            "\n[theme {}]\nbackground = {}\npanel = {}\ntext = {}\nmuted = {}\naccent = {}\noutline = {}\noutline_thickness = {}\noutside_dim = {}\n",
            t.name,
            t.background.to_hex(),
            t.panel.to_hex(),
            t.text.to_hex(),
            t.muted.to_hex(),
            t.accent.to_hex(),
            t.outline.to_hex(),
            t.outline_thickness,
            t.outside_dim,
        ));
    }
    text
}

/// What a settings file held, and everything in it that couldn't be used.
#[derive(Debug)]
pub struct Parsed {
    pub settings: Settings,
    pub problems: Vec<String>,
}

fn number(value: &str) -> Result<f64, String> {
    value.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or(format!("{value} isn't a number"))
}

fn boolean(value: &str) -> Result<bool, String> {
    value.parse::<bool>().map_err(|_| format!("expected true or false, not {value}"))
}

fn color(value: &str) -> Result<Rgb, String> {
    Rgb::parse(value).ok_or(format!("{value} isn't a #rrggbb color"))
}

fn optional_color_value(value: &str) -> Result<Option<Rgb>, String> {
    if value.is_empty() { Ok(None) } else { color(value).map(Some) }
}

fn thickness(value: &str) -> Result<f32, String> {
    let v = number(value)? as f32;
    (MIN_THICKNESS..=MAX_THICKNESS).contains(&v).then_some(v).ok_or(format!("must be from {MIN_THICKNESS} to {MAX_THICKNESS}"))
}

fn dim(value: &str) -> Result<u8, String> {
    value.parse::<u8>().ok().filter(|v| *v <= 100).ok_or("must be from 0 to 100".to_string())
}

fn in_range(value: &str, range: &std::ops::RangeInclusive<f64>) -> Result<f64, String> {
    let v = number(value)?;
    range.contains(&v).then_some(v).ok_or(format!("must be from {} to {}", range.start(), range.end()))
}

fn preset_field(preset: &mut Preset, key: &str, value: &str) -> Option<Result<(), String>> {
    let target = match key {
        "width_mm" => &mut preset.area.width,
        "height_mm" => &mut preset.area.height,
        "offset_x_mm" => &mut preset.area.offset_x,
        "offset_y_mm" => &mut preset.area.offset_y,
        "rotation_deg" => &mut preset.area.rotation_deg,
        _ => return None,
    };
    Some(number(value).map(|v| *target = v))
}

fn theme_field(t: &mut Theme, key: &str, value: &str) -> Option<Result<(), String>> {
    let target = match key {
        "background" => &mut t.background,
        "panel" => &mut t.panel,
        "text" => &mut t.text,
        "muted" => &mut t.muted,
        "accent" => &mut t.accent,
        "outline" => &mut t.outline,
        "outline_thickness" => return Some(thickness(value).map(|v| t.outline_thickness = v)),
        "outside_dim" => return Some(dim(value).map(|v| t.outside_dim = v)),
        _ => return None,
    };
    Some(color(value).map(|c| *target = c))
}

/// Files from before presets existed keep their one area at the top level.
struct Legacy {
    area: AreaMm,
    found: bool,
}

fn top_level_field(s: &mut Settings, active: &mut Option<String>, legacy: &mut Legacy, key: &str, value: &str) -> Result<(), String> {
    let o = &mut s.theme_overrides;
    match key {
        "output" => s.output = Output::parse(value).ok_or(format!("unknown output {value}"))?,
        "orientation" => s.rotation = parse_orientation(value).ok_or(format!("unknown orientation {value}"))?,
        "active_preset" => *active = Some(value.to_string()),
        "aspect_lock" => s.aspect_lock = boolean(value)?,
        "tip_click" => s.tip_click = boolean(value)?,
        "show_outline" => s.show_outline = boolean(value)?,
        "button_key" => s.button_key = value.to_string(),
        "theme" => s.theme = value.to_string(),
        "animations" => s.animations = boolean(value)?,
        "color_background" => o.background = optional_color_value(value)?,
        "color_panel" => o.panel = optional_color_value(value)?,
        "color_text" => o.text = optional_color_value(value)?,
        "color_muted" => o.muted = optional_color_value(value)?,
        "color_accent" => o.accent = optional_color_value(value)?,
        "outline_color" => o.outline = optional_color_value(value)?,
        "outline_thickness" => o.outline_thickness = if value.is_empty() { None } else { Some(thickness(value)?) },
        "outside_dim" => o.outside_dim = if value.is_empty() { None } else { Some(dim(value)?) },
        "show_area_label" => s.show_area_label = boolean(value)?,
        "hold_to_edit_seconds" => s.hold_to_edit_seconds = in_range(value, &HOLD_SECONDS)?,
        "edit_timeout_seconds" => s.edit_timeout_seconds = in_range(value, &EDIT_TIMEOUT_SECONDS)?,
        "close_to_tray" => s.close_to_tray = boolean(value)?,
        "start_minimized" => s.start_minimized = boolean(value)?,
        "open_tablet_app" => s.open_tablet_app = boolean(value)?,
        "screen_pinning" => s.screen_pinning = boolean(value)?,
        "usb_path" => s.usb_path = UsbPath::parse(value).ok_or(format!("unknown path {value}"))?,
        "area_width_mm" => (legacy.area.width, legacy.found) = (number(value)?, true),
        "area_height_mm" => (legacy.area.height, legacy.found) = (number(value)?, true),
        "area_offset_x_mm" => (legacy.area.offset_x, legacy.found) = (number(value)?, true),
        "area_offset_y_mm" => (legacy.area.offset_y, legacy.found) = (number(value)?, true),
        _ => return Err("unknown setting".to_string()),
    }
    Ok(())
}

enum Section {
    Top,
    Preset,
    Theme,
    /// A section with an unusable name, whose values have nowhere to go.
    Skipped,
}

/// Reads a settings file. Anything missing gets its default, so older files keep working, and one
/// bad line only costs that line. The caller keeps a copy of a file with problems before it's saved.
pub fn parse(text: &str) -> Parsed {
    let mut s = Settings { presets: Vec::new(), ..Settings::default() };
    let mut problems = Vec::new();
    let mut active_name: Option<String> = None;
    let mut legacy = Legacy { area: full_area(), found: false };
    let mut section = Section::Top;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let number = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[preset ") {
            match rest.strip_suffix(']').ok_or("expected [preset NAME]".to_string()).and_then(clean_preset_name) {
                Ok(name) if s.presets.iter().any(|p| p.name == name) => {
                    problems.push(format!("line {number}: there's already a preset named {name}"));
                    section = Section::Skipped;
                }
                Ok(name) => {
                    s.presets.push(Preset { name, area: full_area() });
                    section = Section::Preset;
                }
                Err(e) => {
                    problems.push(format!("line {number}: {e}"));
                    section = Section::Skipped;
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("[theme ") {
            match rest.strip_suffix(']').ok_or("expected [theme NAME]".to_string()).and_then(theme::clean_theme_name) {
                Ok(name) if s.saved_themes.iter().any(|t| t.name == name) => {
                    problems.push(format!("line {number}: there's already a theme named {name}"));
                    section = Section::Skipped;
                }
                Ok(name) => {
                    s.saved_themes.push(Theme { name, ..theme::built_in().remove(0) });
                    section = Section::Theme;
                }
                Err(e) => {
                    problems.push(format!("line {number}: {e}"));
                    section = Section::Skipped;
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            problems.push(format!("line {number}: expected key = value"));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        // A section's own keys go to it. Anything else is a top-level setting that came after a
        // section, which is easy to do when editing by hand.
        let handled = match section {
            Section::Preset => s.presets.last_mut().and_then(|p| preset_field(p, key, value)),
            Section::Theme => s.saved_themes.last_mut().and_then(|t| theme_field(t, key, value)),
            Section::Skipped => Some(Ok(())),
            Section::Top => None,
        };
        let outcome = handled.unwrap_or_else(|| top_level_field(&mut s, &mut active_name, &mut legacy, key, value));
        if let Err(e) = outcome {
            problems.push(format!("line {number}: {key}: {e}"));
        }
    }

    if s.presets.is_empty() || legacy.found {
        s.presets.insert(0, Preset { name: DEFAULT_PRESET.to_string(), area: legacy.area });
    }
    for preset in &mut s.presets {
        if preset.area.width < MIN_AREA_MM || preset.area.height < MIN_AREA_MM {
            problems.push(format!("preset {}: the area must be at least {MIN_AREA_MM} mm on each side", preset.name));
            preset.area.width = preset.area.width.max(MIN_AREA_MM);
            preset.area.height = preset.area.height.max(MIN_AREA_MM);
        }
    }
    if let Some(name) = active_name {
        match s.presets.iter().position(|p| p.name == name) {
            Some(index) => s.active = index,
            None => problems.push(format!("active_preset: no preset named {name}")),
        }
    }
    if !theme::is_built_in(&s.theme) && !s.saved_themes.iter().any(|t| t.name == s.theme) {
        problems.push(format!("theme: no theme named {}", s.theme));
        s.theme = DEFAULT_THEME.to_string();
    }
    Parsed { settings: s, problems }
}

pub fn load(path: &Path) -> Result<Option<Parsed>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(parse(&text))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Keeps a copy of a file with problems, because saving replaces it.
pub fn back_up(path: &Path) -> io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let copy = path.with_file_name(format!("config-problem-{stamp}.toml"));
    fs::copy(path, &copy)?;
    Ok(copy)
}

/// Replaces the file in one step, so a crash or power cut can't leave half of it.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, to_text(settings))?;
    fs::rename(&temporary, path)
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
                theme: "Mine".into(),
                theme_overrides: ThemeOverrides {
                    outline: Some(Rgb(1, 2, 3)),
                    outline_thickness: Some(4.5),
                    outside_dim: Some(70),
                    ..Default::default()
                },
                saved_themes: vec![Theme { name: "Mine".into(), outside_dim: 12, ..theme::built_in().remove(1) }],
                animations: false,
                show_area_label: false,
                hold_to_edit_seconds: 2.5,
                edit_timeout_seconds: 60.0,
                close_to_tray: false,
                start_minimized: true,
                open_tablet_app: false,
                screen_pinning: true,
                usb_path: UsbPath::Direct,
            };
            let parsed = parse(&to_text(&s));
            assert_eq!(parsed.problems, Vec::<String>::new());
            assert_eq!(parsed.settings, s);
        }
    }

    #[test]
    fn default_settings_round_trip_with_no_problems() {
        let parsed = parse(&to_text(&Settings::default()));
        assert_eq!(parsed.problems, Vec::<String>::new());
        assert_eq!(parsed.settings, Settings::default());
    }

    #[test]
    fn reads_the_older_single_area_file() {
        let parsed = parse(
            "orientation = landscape-usb-right\narea_width_mm = 120\narea_height_mm = 75\narea_offset_x_mm = 1\narea_offset_y_mm = 2\n",
        );
        assert_eq!(parsed.problems, Vec::<String>::new());
        let s = parsed.settings;
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
    fn files_from_before_round_2_keep_their_settings_and_get_the_new_defaults() {
        let parsed = parse(
            "output = direct\norientation = landscape-usb-right\nactive_preset = Small\naspect_lock = true\ntip_click = false\nshow_outline = true\nbutton_key = z\n\n[preset Default]\nwidth_mm = 200\nheight_mm = 125\noffset_x_mm = 0\noffset_y_mm = 0\nrotation_deg = 0\n\n[preset Small]\nwidth_mm = 88\nheight_mm = 55\noffset_x_mm = 3\noffset_y_mm = 0\nrotation_deg = 0\n",
        );
        assert_eq!(parsed.problems, Vec::<String>::new());
        let s = parsed.settings;
        assert_eq!(s.area().width, 88.0);
        assert!(!s.tip_click);
        assert_eq!(s.button_key, "z");
        assert_eq!(s.theme, DEFAULT_THEME);
        assert!(s.animations);
        // Direct USB has been the default since it stopped needing adb.exe at all.
        assert_eq!(s.usb_path, UsbPath::Direct);
    }

    #[test]
    fn top_level_settings_after_a_preset_still_count() {
        let parsed = parse("[preset A]\nwidth_mm = 100\nheight_mm = 62\ntip_click = false\nactive_preset = A\n");
        assert_eq!(parsed.problems, Vec::<String>::new());
        assert!(!parsed.settings.tip_click);
        assert_eq!(parsed.settings.area().width, 100.0);
    }

    #[test]
    fn one_bad_line_only_costs_that_line() {
        let parsed = parse("tip_click = false\norientation = sideways\nbutton_key = z\n");
        assert_eq!(parsed.problems.len(), 1);
        assert!(parsed.problems[0].starts_with("line 2: orientation"), "{}", parsed.problems[0]);
        assert!(!parsed.settings.tip_click);
        assert_eq!(parsed.settings.button_key, "z");
        assert_eq!(parsed.settings.rotation, None);
    }

    #[test]
    fn bad_values_are_reported_and_replaced() {
        for bad in [
            "orientation = sideways",
            "output = windows-ink",
            "area_width_mm = wide",
            "area_width_mm = NaN",
            "area_width_mm = 1",
            "mystery = 1",
            "no equals sign",
            "active_preset = Missing",
            "[preset Oops]\nwidth_mm = 2",
            "[preset]",
            "theme = Missing",
            "[theme Daylight]",
            "color_accent = pink",
            "outline_thickness = 900",
            "outside_dim = 101",
            "hold_to_edit_seconds = 0",
            "edit_timeout_seconds = 9999",
            "usb_path = wifi",
        ] {
            let parsed = parse(bad);
            assert!(!parsed.problems.is_empty(), "{bad}");
            assert!(parsed.settings.area().width >= MIN_AREA_MM);
            assert!(parsed.settings.active < parsed.settings.presets.len());
            let again = parse(&to_text(&parsed.settings));
            assert_eq!(again.problems, Vec::<String>::new(), "{bad} saved something unreadable");
        }
    }

    #[test]
    fn a_problem_file_is_copied_before_it_can_be_replaced() {
        let dir = std::env::temp_dir().join(format!("osutab-test-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "orientation = sideways\n").unwrap();
        let copy = back_up(&path).unwrap();
        assert_eq!(fs::read_to_string(copy).unwrap(), "orientation = sideways\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_replaces_the_whole_file_and_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("osutab-test-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        for active_width in [100.0, 200.0] {
            let mut s = Settings::default();
            s.area_mut().width = active_width;
            save(&path, &s).unwrap();
            assert_eq!(load(&path).unwrap().unwrap().settings.area().width, active_width);
        }
        let names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["config.toml"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn themes_can_be_saved_chosen_and_deleted() {
        let mut s = Settings::default();
        s.theme_overrides.outline = Some(Rgb(9, 8, 7));
        s.save_theme("Mine").unwrap();
        assert_eq!(s.theme, "Mine");
        assert_eq!(s.theme_overrides, ThemeOverrides::default());
        assert_eq!(s.active_theme().outline, Rgb(9, 8, 7));

        s.choose_theme("Daylight");
        assert_eq!(s.active_theme().name, "Daylight");
        assert!(!s.delete_saved_theme(), "a built-in theme can't be deleted");
        assert!(s.save_theme("Daylight").is_err());

        s.choose_theme("Mine");
        assert!(s.delete_saved_theme());
        assert_eq!(s.theme, DEFAULT_THEME);
        assert!(s.saved_themes.is_empty());
    }
}
