//! The settings window: setup questions, pre-play checks, area editor, latency HUD and precision
//! test. It runs on its own thread and only reaches the input path through `Shared`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, RichText, Stroke, Vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::adb::DeviceState;
use crate::lazer::{self, LazerDisplay, LazerInput};
use crate::mapping::{Rotation, TABLET_PX_PER_MM};
use crate::otd;
use crate::preflight;
use crate::settings::{Output, MIN_AREA_MM};
use crate::shared::Shared;
use crate::stats::Dist;
use crate::tray;

const WINDOW_TITLE: &str = "osutab";
const SAVE_DELAY: Duration = Duration::from_millis(500);
const CHECK_EVERY: Duration = Duration::from_secs(1);
const PROCESS_CHECK_EVERY: Duration = Duration::from_secs(3);
// Long enough to check accuracy, short enough that it can't quietly slow a whole play session.
const PRECISION_TEST_TIME: Duration = Duration::from_secs(120);
const QUICK_WIDTHS_MM: [f64; 4] = [80.0, 100.0, 120.0, 150.0];

const WARNING: Color32 = Color32::from_rgb(255, 170, 80);
const GOOD: Color32 = Color32::from_rgb(120, 220, 120);
const BAD: Color32 = Color32::from_rgb(255, 120, 120);
const ACCENT: Color32 = Color32::from_rgb(90, 170, 255);
const MUTED: Color32 = Color32::from_gray(150);

const ORIENTATIONS: [(Option<Rotation>, &str); 5] = [
    (Some(Rotation::R90), "Landscape, USB port on the right"),
    (Some(Rotation::R270), "Landscape, USB port on the left"),
    (Some(Rotation::R0), "Portrait, USB port at the bottom"),
    (Some(Rotation::R180), "Portrait, USB port at the top"),
    (None, "Follow the tablet's screen rotation"),
];

// Lets the tray thread wake the window after showing it.
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Play,
    Area,
    Latency,
    Precision,
}

impl Tab {
    const ALL: [(Tab, &'static str); 4] = [
        (Tab::Play, "Play"),
        (Tab::Area, "Pen area"),
        (Tab::Latency, "Latency"),
        (Tab::Precision, "Precision test"),
    ];

    pub fn from_name(name: &str) -> Option<Tab> {
        match name {
            "play" => Some(Tab::Play),
            "area" => Some(Tab::Area),
            "latency" => Some(Tab::Latency),
            "precision" => Some(Tab::Precision),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CheckLevel {
    Good,
    Warn,
    Bad,
}

struct Check {
    level: CheckLevel,
    title: String,
    detail: Option<String>,
}

fn good(title: impl Into<String>) -> Check {
    Check { level: CheckLevel::Good, title: title.into(), detail: None }
}

fn warn(title: impl Into<String>, detail: impl Into<String>) -> Check {
    Check { level: CheckLevel::Warn, title: title.into(), detail: Some(detail.into()) }
}

fn bad(title: impl Into<String>, detail: impl Into<String>) -> Check {
    Check { level: CheckLevel::Bad, title: title.into(), detail: Some(detail.into()) }
}

fn tablet_check(state: DeviceState, connected: bool) -> Check {
    if connected {
        return good("Tablet connected");
    }
    match state {
        DeviceState::AdbMissing => bad(
            "adb not found",
            "Put Google's platform-tools folder next to osutab.exe. The README explains this.",
        ),
        DeviceState::NoDevice => bad(
            "Tablet not found",
            "Connect the USB cable, and check USB debugging is on in Developer options.",
        ),
        DeviceState::Unauthorized => bad(
            "Waiting for your OK on the tablet",
            "Tap Allow on the \"Allow USB debugging?\" prompt, and tick Always allow. No prompt? Replug the cable with the tablet unlocked.",
        ),
        DeviceState::Offline => bad("Tablet not responding", "Unplug the USB cable and plug it back in."),
        DeviceState::Ready => warn("Connecting to the tablet", "This usually takes under a second."),
    }
}

fn lazer_check(input: Option<LazerInput>) -> Option<Check> {
    match input? {
        LazerInput { high_precision: true } => Some(bad(
            "osu!lazer: High precision mouse is on",
            "The cursor won't stay under the pen. Turn it off in lazer: Settings > Input > Mouse.",
        )),
        LazerInput { high_precision: false } => Some(good("osu!lazer: High precision mouse is off")),
    }
}

/// Display settings that hold input back in lazer. No value means nothing to say.
fn display_checks(display: &LazerDisplay) -> Vec<Check> {
    let mut checks = Vec::new();
    match display.frame_sync.as_deref() {
        Some("VSync") => checks.push(warn(
            "osu!lazer: Frame limiter is VSync",
            "It holds each frame back for your display, which delays the cursor. Unlimited has the least delay: Settings > Graphics.",
        )),
        Some(_) | None => {}
    }
    match display.window_mode.as_deref() {
        Some(mode) if mode != "Fullscreen" => checks.push(warn(
            format!("osu!lazer: Screen mode is {mode}"),
            "Outside fullscreen, Windows composites each frame, which can add a frame of delay. Fullscreen has the least: Settings > Graphics.",
        )),
        _ => {}
    }
    checks
}

fn otd_checks(installed: bool, relay_connected: bool, windows_ink: bool) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.push(if !installed {
        bad("OpenTabletDriver isn't included in this build", "Run package.ps1, or switch Output back to Direct.")
    } else if relay_connected {
        good("OpenTabletDriver is receiving the pen")
    } else {
        bad(
            "Waiting for OpenTabletDriver",
            "It's starting up. If this stays, open OpenTabletDriver and check \"osutab bridge\" is on in its Tools tab.",
        )
    });
    if windows_ink {
        checks.push(warn(
            "OpenTabletDriver is using Windows Ink",
            "That adds Windows Ink to the path, and lazer may treat a pen differently. Absolute Mode avoids it.",
        ));
    }
    checks
}

fn apps_check(apps: &[&str]) -> Check {
    if apps.is_empty() {
        return good("No apps linked with stutters are running");
    }
    warn(
        format!("May cause stutters: {}", apps.join(", ")),
        "Closing these removed the biggest delays during play in testing.",
    )
}

/// How far apart the two directions' speeds are, as a percentage.
fn stretch_percent(px_per_mm: (f64, f64)) -> f64 {
    (px_per_mm.0.max(px_per_mm.1) / px_per_mm.0.min(px_per_mm.1) - 1.0) * 100.0
}

fn shape_check(px_per_mm: (f64, f64)) -> Check {
    let percent = stretch_percent(px_per_mm);
    if percent <= 1.0 {
        return good(format!("Pen area matches your screen's shape ({:.1} px per mm)", px_per_mm.0));
    }
    warn(
        format!("Pen area is stretched by {percent:.0}%"),
        "The same pen movement goes further one way than the other. The Pen area tab can fix this.",
    )
}

/// The worst result decides the headline.
fn summary(checks: &[Check]) -> (CheckLevel, &'static str) {
    if checks.iter().any(|c| c.level == CheckLevel::Bad) {
        (CheckLevel::Bad, "Not ready yet")
    } else if checks.iter().any(|c| c.level == CheckLevel::Warn) {
        (CheckLevel::Warn, "Ready, with warnings")
    } else {
        (CheckLevel::Good, "Ready to play")
    }
}

fn level_color(level: CheckLevel) -> Color32 {
    match level {
        CheckLevel::Good => GOOD,
        CheckLevel::Warn => WARNING,
        CheckLevel::Bad => BAD,
    }
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

struct App {
    shared: Arc<Shared>,
    tab: Tab,
    preset_name: String,
    message: String,
    dirty_since: Option<Instant>,
    lazer: Option<LazerInput>,
    lazer_checked: Option<Instant>,
    display: LazerDisplay,
    exe_dir: PathBuf,
    otd_windows_ink: bool,
    apps: Vec<&'static str>,
    apps_checked: Option<Instant>,
    precision_ends: Option<Instant>,
}

impl App {
    fn changed(&mut self) {
        self.dirty_since = Some(Instant::now());
    }

    fn save_now(&mut self) {
        self.dirty_since = None;
        if let Err(e) = self.shared.save_settings() {
            self.message = format!("Could not save settings: {e}");
        }
    }

    fn save_if_due(&mut self) {
        if self.dirty_since.is_some_and(|t| t.elapsed() >= SAVE_DELAY) {
            self.save_now();
        }
    }

    fn refresh_checks(&mut self) {
        if self.lazer_checked.is_none_or(|t| t.elapsed() >= CHECK_EVERY) {
            self.lazer = lazer::read();
            self.display = lazer::read_display().unwrap_or_default();
            self.otd_windows_ink = otd::uses_windows_ink(&otd::data_dir());
            self.lazer_checked = Some(Instant::now());
        }
        if self.apps_checked.is_none_or(|t| t.elapsed() >= PROCESS_CHECK_EVERY) {
            let names = preflight::process_names();
            self.apps = preflight::interfering(names.iter().map(String::as_str));
            self.apps_checked = Some(Instant::now());
        }
    }

    fn checks(&self) -> Vec<Check> {
        let live = self.shared.live_config();
        let connected = self.shared.connected.load(Ordering::SeqCst);
        let mut checks = vec![tablet_check(self.shared.device_state(), connected)];
        checks.extend(lazer_check(self.lazer));
        checks.extend(display_checks(&self.display));
        if live.output == Output::Direct {
            checks.push(shape_check(live.area.px_per_mm(&self.shared.target)));
        } else {
            let installed = otd::program_dir(&self.exe_dir).is_some();
            let relay = self.shared.relay.connected.load(Ordering::SeqCst);
            checks.extend(otd_checks(installed, relay, self.otd_windows_ink));
        }
        checks.push(apps_check(&self.apps));
        if self.shared.test_mode.load(Ordering::SeqCst) {
            checks.push(warn(
                "Precision test is on",
                "It adds work to every pen report. Turn it off before playing.",
            ));
        }
        checks
    }

    /// Starts OpenTabletDriver when its mode is chosen, or at startup if it was left on.
    fn ensure_otd_running(&mut self) {
        let Some(dir) = otd::program_dir(&self.exe_dir) else { return };
        if let Err(e) = otd::install(&otd::data_dir()) {
            self.message = format!("Could not set up OpenTabletDriver: {e}");
            return;
        }
        let mut daemon = self.shared.otd_daemon.lock().unwrap();
        if daemon.is_none() {
            match otd::start_daemon(&dir) {
                Ok(child) => *daemon = Some(child),
                Err(e) => self.message = format!("Could not start OpenTabletDriver: {e}"),
            }
        }
    }

    fn output_picker(&mut self, ui: &mut egui::Ui) {
        let mut output = self.shared.settings().output;
        ui.horizontal(|ui| {
            ui.label("Output");
            let mut changed = ui
                .radio_value(&mut output, Output::Direct, "Direct")
                .on_hover_text("Straight to the cursor. The fastest path, with no Windows Ink, filters or other programs.")
                .changed();
            changed |= ui
                .radio_value(&mut output, Output::OpenTabletDriver, "OpenTabletDriver")
                .on_hover_text("Sends the pen through OpenTabletDriver, so its filters and plugins work. It sets the area itself.")
                .changed();
            if changed {
                self.shared.update_settings(|s| s.output = output);
                self.changed();
                if output == Output::OpenTabletDriver {
                    self.ensure_otd_running();
                }
            }
            if output == Output::OpenTabletDriver {
                if let Some(dir) = otd::program_dir(&self.exe_dir) {
                    if ui.button("Open OpenTabletDriver").clicked() {
                        if let Err(e) = otd::open_settings_window(&dir) {
                            self.message = format!("Could not open OpenTabletDriver: {e}");
                        }
                    }
                }
            }
        });
    }

    fn stop_precision_test(&mut self) {
        self.shared.test_mode.store(false, Ordering::SeqCst);
        self.precision_ends = None;
    }

    fn enforce_precision_limit(&mut self) {
        if self.precision_ends.is_some_and(|end| Instant::now() >= end) {
            self.stop_precision_test();
            self.message = "The precision test turned itself off after 2 minutes.".to_string();
        }
    }

    fn orientation_picker(&mut self, ui: &mut egui::Ui) {
        let current = self.shared.settings().rotation;
        let label = ORIENTATIONS.iter().find(|o| o.0 == current).map_or("", |o| o.1);
        egui::ComboBox::from_id_salt("orientation").selected_text(label).width(280.0).show_ui(ui, |ui| {
            for (rotation, name) in ORIENTATIONS {
                if ui.selectable_label(current == rotation, name).clicked() {
                    self.shared.update_settings(|s| s.rotation = rotation);
                    self.changed();
                }
            }
        });
    }

    fn set_size(&mut self, width: f64, height: f64) {
        self.shared.update_settings(|s| {
            s.area_mut().width = width;
            s.area_mut().height = height;
        });
        self.changed();
    }

    /// One-click common sizes, at the screen's shape.
    fn quick_sizes(&mut self, ui: &mut egui::Ui) {
        let live = self.shared.live_config();
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        let aspect = self.shared.target.width / self.shared.target.height;
        ui.horizontal(|ui| {
            ui.label("Quick size");
            for width in QUICK_WIDTHS_MM {
                let height = (width / aspect).clamp(MIN_AREA_MM, max_h);
                if ui.button(format!("{width:.0} mm")).clicked() {
                    self.set_size(width.min(max_w), height);
                }
            }
            if ui.button("Full").clicked() {
                let height = (max_w / aspect).min(max_h);
                self.set_size((height * aspect).min(max_w), height);
            }
        });
    }

    fn size_fields(&mut self, ui: &mut egui::Ui) {
        let settings = self.shared.settings();
        let live = self.shared.live_config();
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        let screen_aspect = self.shared.target.width / self.shared.target.height;
        let (mut width, mut height) = (live.area.width_mm, live.area.height_mm);
        let mut lock = settings.aspect_lock;

        ui.horizontal(|ui| {
            ui.label("Width");
            let w_changed = ui
                .add(egui::DragValue::new(&mut width).range(MIN_AREA_MM..=max_w).speed(0.5).max_decimals(1).suffix(" mm"))
                .changed();
            ui.label("Height");
            let h_changed = ui
                .add(egui::DragValue::new(&mut height).range(MIN_AREA_MM..=max_h).speed(0.5).max_decimals(1).suffix(" mm"))
                .changed();
            if ui.checkbox(&mut lock, "Lock to screen shape").changed() {
                self.shared.update_settings(|s| s.aspect_lock = lock);
                if lock {
                    height = (width / screen_aspect).clamp(MIN_AREA_MM, max_h);
                    width = (height * screen_aspect).clamp(MIN_AREA_MM, max_w);
                    self.set_size(width, height);
                }
                self.changed();
            }
            if w_changed || h_changed {
                if lock {
                    if w_changed {
                        height = (width / screen_aspect).clamp(MIN_AREA_MM, max_h);
                        width = (height * screen_aspect).clamp(MIN_AREA_MM, max_w);
                    } else {
                        width = (height * screen_aspect).clamp(MIN_AREA_MM, max_w);
                        height = (width / screen_aspect).clamp(MIN_AREA_MM, max_h);
                    }
                }
                self.set_size(width, height);
            }
        });
    }

    fn sensitivity_line(&self, ui: &mut egui::Ui) {
        let live = self.shared.live_config();
        let (x, y) = live.area.px_per_mm(&self.shared.target);
        if stretch_percent((x, y)) <= 1.0 {
            ui.label(format!("1 mm of pen movement = {x:.1} px on screen"));
        } else {
            ui.label(format!("1 mm of pen movement = {x:.1} px across, {y:.1} px down"));
        }
    }

    fn setup_screen(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.heading("Welcome to osutab");
        ui.label("Two quick questions, and you can change both later.");
        ui.add_space(14.0);
        ui.strong("How will the tablet sit while you play?");
        self.orientation_picker(ui);
        ui.add_space(14.0);
        ui.strong("How big should the pen area be?");
        ui.label("This part of the tablet covers your whole screen. A smaller area feels faster.");
        self.quick_sizes(ui);
        self.size_fields(ui);
        self.sensitivity_line(ui);
        ui.add_space(4.0);
        self.preview(ui, 150.0);
        ui.add_space(14.0);
        if ui.add(egui::Button::new(RichText::new("Start").size(16.0)).min_size(Vec2::new(120.0, 32.0))).clicked() {
            self.save_now();
            self.shared.setup_done.store(true, Ordering::SeqCst);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let checks = self.checks();
        let (level, headline) = summary(&checks);
        ui.horizontal(|ui| {
            status_dot(ui, level_color(level));
            ui.label(RichText::new(headline).size(18.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quit").on_hover_text("Stops osutab and gives the pen back to Android").clicked() {
                    self.shared.stats.stop.store(true, Ordering::SeqCst);
                }
                let hud = self.shared.stats.hud();
                if hud.recent.total.count > 0 {
                    ui.label(RichText::new(format!("{:.2} ms, {:.0} Hz", hud.recent.total.p50, hud.rate_hz)).color(MUTED));
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (tab, name) in Tab::ALL {
                ui.selectable_value(&mut self.tab, tab, name);
            }
        });
        ui.separator();
    }

    fn play_tab(&mut self, ui: &mut egui::Ui) {
        for check in self.checks() {
            ui.horizontal(|ui| {
                status_dot(ui, level_color(check.level));
                ui.label(check.title);
            });
            if let Some(detail) = check.detail {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(20.0);
                    ui.label(RichText::new(detail).color(MUTED).small());
                });
            }
            ui.add_space(2.0);
        }
        let app = self.shared.app_connected.load(Ordering::SeqCst);
        if self.shared.connected.load(Ordering::SeqCst) && !app {
            ui.horizontal(|ui| {
                status_dot(ui, WARNING);
                ui.label("Tablet app not showing");
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(20.0);
                ui.label(RichText::new("The pen still works. Without the app, your palm can tap whatever's on the tablet. It may be waiting for you to approve the install.").color(MUTED).small());
            });
        }
        ui.add_space(10.0);
        self.output_picker(ui);
        if self.shared.settings().output == Output::Direct {
            self.preset_picker(ui);
            self.sensitivity_line(ui);
            ui.add_space(4.0);
            self.preview(ui, 170.0);
        } else {
            ui.label("OpenTabletDriver sets the area, filters and buttons in this mode.");
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Closing this window keeps osutab running in the tray. Quit gives the pen back to Android.").color(MUTED).small());
    }

    fn preset_picker(&mut self, ui: &mut egui::Ui) {
        let settings = self.shared.settings();
        ui.horizontal(|ui| {
            ui.label("Preset");
            egui::ComboBox::from_id_salt(ui.next_auto_id())
                .selected_text(settings.presets[settings.active].name.clone())
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (index, preset) in settings.presets.iter().enumerate() {
                        if ui.selectable_label(index == settings.active, &preset.name).clicked() {
                            self.shared.update_settings(|s| s.active = index);
                            self.changed();
                        }
                    }
                });
        });
    }

    fn area_tab(&mut self, ui: &mut egui::Ui) {
        if self.shared.settings().output == Output::OpenTabletDriver {
            ui.label("In OpenTabletDriver mode, the area, filters and buttons are set in OpenTabletDriver.");
            ui.label(RichText::new("It sees a landscape tablet, 313.8 x 195.9 mm. If you hold the tablet in portrait, set the area's Rotation there to 90 or 270.").color(MUTED).small());
            ui.add_space(6.0);
            self.output_picker(ui);
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Tablet");
            self.orientation_picker(ui);
        });
        self.preset_row(ui);
        ui.add_space(6.0);
        self.quick_sizes(ui);
        self.size_fields(ui);

        let live = self.shared.live_config();
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        // Shows where the area really is, after fitting it on the tablet.
        let mut offset_x = live.area.center_x - live.area.tablet_w / 2.0;
        let mut offset_y = live.area.center_y - live.area.tablet_h / 2.0;
        let mut rotation = self.shared.settings().area().rotation_deg;
        ui.horizontal(|ui| {
            let mut changed = false;
            ui.label("X offset");
            changed |= ui.add(egui::DragValue::new(&mut offset_x).range(-max_w / 2.0..=max_w / 2.0).speed(0.5).max_decimals(1).suffix(" mm")).changed();
            ui.label("Y offset");
            changed |= ui.add(egui::DragValue::new(&mut offset_y).range(-max_h / 2.0..=max_h / 2.0).speed(0.5).max_decimals(1).suffix(" mm")).changed();
            ui.label("Rotation");
            changed |= ui.add(egui::DragValue::new(&mut rotation).range(-180.0..=180.0).speed(0.25).max_decimals(1).suffix(" deg")).changed();
            if ui.button("Centre").clicked() {
                (offset_x, offset_y) = (0.0, 0.0);
                changed = true;
            }
            if changed {
                self.shared.update_settings(|s| {
                    let a = s.area_mut();
                    (a.offset_x, a.offset_y, a.rotation_deg) = (offset_x, offset_y, rotation);
                });
                self.changed();
            }
        });

        ui.label(
            RichText::new(format!(
                "On the tablet's screen: {:.0} x {:.0} px",
                live.area.width_mm * TABLET_PX_PER_MM,
                live.area.height_mm * TABLET_PX_PER_MM
            ))
            .color(MUTED),
        );
        self.sensitivity_line(ui);
        self.stretch_fix(ui);
        ui.add_space(4.0);
        self.preview(ui, 170.0);
        ui.add_space(6.0);

        let settings = self.shared.settings();
        let mut outline = settings.show_outline;
        let (mut tip, mut key) = (settings.tip_click, settings.button_key.clone());
        ui.horizontal(|ui| {
            if ui.checkbox(&mut outline, "Show the area on the tablet").changed() {
                self.shared.update_settings(|s| s.show_outline = outline);
                self.changed();
            }
            if ui.checkbox(&mut tip, "Pen tip clicks").changed() {
                self.shared.update_settings(|s| s.tip_click = tip);
                self.changed();
            }
            ui.label("Side button key");
            let response = ui
                .add(egui::TextEdit::singleline(&mut key).desired_width(28.0).char_limit(1))
                .on_hover_text("A letter or digit, such as z. Leave empty for none.");
            if response.changed() && (key.is_empty() || crate::inject::scan_code_for_key(&key).is_some()) {
                let key = key.to_lowercase();
                self.shared.update_settings(|s| s.button_key = key);
                self.changed();
            }
        });
    }

    /// Offers the two ways to fix a stretched area.
    fn stretch_fix(&mut self, ui: &mut egui::Ui) {
        let live = self.shared.live_config();
        let px_per_mm = live.area.px_per_mm(&self.shared.target);
        if stretch_percent(px_per_mm) <= 1.0 {
            return;
        }
        let aspect = self.shared.target.width / self.shared.target.height;
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        let (w, h) = (live.area.width_mm, live.area.height_mm);
        let keep_width = (w, (w / aspect).clamp(MIN_AREA_MM, max_h));
        let keep_height = ((h * aspect).clamp(MIN_AREA_MM, max_w), h);
        ui.colored_label(WARNING, format!("This area is stretched by {:.0}%. Fix it:", stretch_percent(px_per_mm)));
        ui.horizontal(|ui| {
            for size in [keep_width, keep_height] {
                if ui.button(format!("Make it {:.1} x {:.1} mm", size.0, size.1)).clicked() {
                    self.set_size(size.0, size.1);
                }
            }
        });
    }

    fn preset_row(&mut self, ui: &mut egui::Ui) {
        let settings = self.shared.settings();
        ui.horizontal(|ui| {
            self.preset_picker(ui);
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).desired_width(120.0).hint_text("New name"));
            if ui.button("Save as").clicked() {
                let mut outcome = Ok(());
                let name = self.preset_name.clone();
                self.shared.update_settings(|s| outcome = s.save_preset(&name));
                match outcome {
                    Ok(()) => {
                        self.preset_name.clear();
                        self.message.clear();
                        self.changed();
                    }
                    Err(e) => self.message = e,
                }
            }
            if ui.add_enabled(settings.presets.len() > 1, egui::Button::new("Delete")).clicked() {
                self.shared.update_settings(|s| {
                    s.delete_active_preset();
                });
                self.changed();
            }
        });
    }

    /// The tablet, the pen area on it, and where the pen is.
    fn preview(&self, ui: &mut egui::Ui, max_height: f32) {
        let live = self.shared.live_config();
        let (tablet_w, tablet_h) = (live.area.tablet_w as f32, live.area.tablet_h as f32);
        let scale = (ui.available_width().min(420.0) / tablet_w).min(max_height / tablet_h);
        let (response, painter) = ui.allocate_painter(Vec2::new(tablet_w * scale, tablet_h * scale), egui::Sense::hover());
        let origin = response.rect.min;
        let at = |x: f32, y: f32| origin + Vec2::new(x * scale, y * scale);

        painter.rect_filled(response.rect, 4.0, Color32::from_gray(35));
        let corners: Vec<Pos2> = live.area.corners().iter().map(|c| at(c.0 as f32, c.1 as f32)).collect();
        painter.add(egui::Shape::convex_polygon(
            corners,
            Color32::from_rgba_unmultiplied(90, 170, 255, 60),
            Stroke::new(2.0_f32, ACCENT),
        ));

        let (u, v, flags) = self.shared.pen();
        if flags & osutab_core::FLAG_IN_RANGE != 0 {
            let color = if flags & osutab_core::FLAG_TOUCH != 0 { Color32::from_rgb(255, 120, 90) } else { Color32::WHITE };
            painter.circle_filled(at(u * tablet_w, v * tablet_h), 4.0, color);
        }
    }

    fn latency_tab(&mut self, ui: &mut egui::Ui) {
        let hud = self.shared.stats.hud();
        if hud.recent.total.count == 0 {
            ui.label("Move the pen to see numbers.");
        } else {
            let total = hud.recent.total;
            let color = if total.p50 <= 1.0 { GOOD } else { WARNING };
            ui.colored_label(color, RichText::new(format!("Added latency: {:.2} ms median", total.p50)).size(18.0));
            ui.label(format!(
                "99th percentile {:.2} ms, worst {:.2} ms, jitter {:.2} ms",
                total.p99, total.max, hud.jitter_ms
            ));
            ui.label(format!("Report rate {:.0} Hz, dropped {}, overruns {}", hud.rate_hz, hud.dropped, hud.overruns));
            ui.add_space(6.0);
            egui::Grid::new("stages").striped(true).show(ui, |ui| {
                ui.strong("Stage");
                ui.strong("Median");
                ui.strong("p99");
                ui.strong("Worst");
                ui.end_row();
                let mut row = |name: &str, d: Dist| {
                    ui.label(name);
                    if d.count == 0 {
                        ui.label("-");
                        ui.label("-");
                        ui.label("-");
                    } else {
                        ui.label(format!("{:.3}", d.p50));
                        ui.label(format!("{:.3}", d.p99));
                        ui.label(format!("{:.3}", d.max));
                    }
                    ui.end_row();
                };
                row("Pen event to helper sending", hud.recent.helper);
                row("USB and adb to the PC", hud.recent.link);
                row("PC to SendInput finished", hud.recent.host);
                row("SendInput to cursor moved", hud.recent.cursor);
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "Times in ms over the last 10 s of pen use. The USB stage relies on clock alignment, so it's accurate to a fraction of a ms (best round trip {:.2} ms). The last stage only shows during the precision test.",
                    hud.best_rtt_ms
                ))
                .color(MUTED)
                .small(),
            );
        }
        ui.add_space(10.0);
        self.input_path(ui);
        ui.add_space(10.0);
        if ui.button("Open the logs folder").clicked() {
            if let Some(dir) = self.shared.settings_path().parent() {
                let _ = Command::new("explorer").arg(dir).spawn();
            }
        }
    }

    /// Everything between the pen and the game, so nothing is hidden.
    fn input_path(&self, ui: &mut egui::Ui) {
        let direct = self.shared.settings().output == Output::Direct;
        ui.strong("What's between the pen and the game");
        egui::Grid::new("path").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            let mut row = |name: &str, state: &str, color: Color32| {
                ui.label(name);
                ui.colored_label(color, state);
                ui.end_row();
            };
            row("Android's input system", "Bypassed: read from the kernel", GOOD);
            row("Smoothing, prediction, acceleration", if direct { "None" } else { "Whatever OpenTabletDriver has on" }, if direct { GOOD } else { WARNING });
            row("Windows Ink", if direct { "Not used" } else if self.otd_windows_ink { "In use" } else { "Not used" }, if direct || !self.otd_windows_ink { GOOD } else { WARNING });
            row("adbd, USB and adb.exe", "In the path: most of the added time", WARNING);
            if !direct {
                row("OpenTabletDriver", "In the path", WARNING);
            }
            row("Windows SendInput", "In the path: under 0.1 ms", GOOD);
        });
    }

    fn precision_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Compares where the pen says the cursor should be with where Windows put it.");
        ui.label(RichText::new("It checks the Windows cursor. osu! draws its own, which is why High precision mouse must be off in lazer.").color(MUTED).small());
        ui.add_space(6.0);
        let mut on = self.shared.test_mode.load(Ordering::SeqCst);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut on, "Compare the pen with the cursor").changed() {
                self.shared.stats.reset_precision();
                self.shared.test_mode.store(on, Ordering::SeqCst);
                self.precision_ends = on.then(|| Instant::now() + PRECISION_TEST_TIME);
                self.message.clear();
            }
            if on && ui.button("Reset").clicked() {
                self.shared.stats.reset_precision();
            }
        });
        if !on {
            ui.label(RichText::new("It adds a little work to every pen report, so it turns itself off after 2 minutes, and when this window closes.").color(MUTED).small());
            return;
        }
        if let Some(end) = self.precision_ends {
            ui.colored_label(WARNING, format!("Turns off in {} s. Don't leave it on while playing.", end.saturating_duration_since(Instant::now()).as_secs()));
        }
        let p = self.shared.stats.precision();
        if p.samples == 0 {
            ui.label("Hover the pen and make big, fast jumps.");
            return;
        }
        ui.add_space(6.0);
        ui.label(format!(
            "Pen {:.2}, {:.2} px, cursor {}, {}, error {:.2} px",
            p.latest.exact.0, p.latest.exact.1, p.latest.cursor.0, p.latest.cursor.1, p.latest.error_px
        ));
        let color = if p.worst_px <= 1.0 { GOOD } else { BAD };
        ui.colored_label(color, RichText::new(format!("Worst error {:.2} px over {} samples", p.worst_px, p.samples)).size(16.0));
        ui.label(format!("Over 1 px: {}. Big jumps: {}, worst {:.2} px.", p.over_1px, p.jumps, p.worst_jump_px));
    }

    /// Hides to the tray, and stops anything that shouldn't carry on unattended.
    fn hide_window(&mut self) {
        self.stop_precision_test();
        self.save_now();
        let window = self.shared.window.load(Ordering::SeqCst);
        unsafe { ShowWindow(window as _, SW_HIDE) };
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.shared.tray_thread.load(Ordering::SeqCst) != 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_window();
            } else {
                self.shared.stats.stop.store(true, Ordering::SeqCst);
            }
        }
        // Enough for the numbers, without spending CPU and GPU the game could use.
        ctx.request_repaint_after(Duration::from_millis(100));
        self.save_if_due();
        self.enforce_precision_limit();
        self.refresh_checks();
        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.shared.setup_done.load(Ordering::SeqCst) {
                self.setup_screen(ui);
            } else {
                self.header(ui);
                egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                    Tab::Play => self.play_tab(ui),
                    Tab::Area => self.area_tab(ui),
                    Tab::Latency => self.latency_tab(ui),
                    Tab::Precision => self.precision_tab(ui),
                });
            }
            if !self.message.is_empty() {
                ui.add_space(4.0);
                ui.colored_label(WARNING, &self.message);
            }
        });
    }
}

/// Brings the window back, from the tray.
pub fn show_window(shared: &Shared) {
    let window = shared.window.load(Ordering::SeqCst);
    if window == 0 {
        return;
    }
    unsafe {
        ShowWindow(window as _, SW_SHOW);
        ShowWindow(window as _, SW_RESTORE);
        SetForegroundWindow(window as _);
    }
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

/// Used by a second copy of the program, to bring up the first one instead of starting.
pub fn show_existing_window() {
    let title: Vec<u16> = WINDOW_TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let window = FindWindowW(std::ptr::null(), title.as_ptr());
        if !window.is_null() {
            ShowWindow(window, SW_SHOW);
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
}

pub fn run(shared: Arc<Shared>, tab: Tab, exe_dir: PathBuf) -> Result<(), eframe::Error> {
    const ICON_SIZE: u32 = 64;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 500.0])
            .with_min_inner_size([560.0, 420.0])
            .with_title(WINDOW_TITLE)
            .with_icon(egui::IconData { rgba: tray::icon_rgba(ICON_SIZE), width: ICON_SIZE, height: ICON_SIZE }),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |cc| {
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                    shared.window.store(win32.hwnd.get(), Ordering::SeqCst);
                }
            }
            let _ = CONTEXT.set(cc.egui_ctx.clone());
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut app = App {
                shared,
                tab,
                preset_name: String::new(),
                message: String::new(),
                dirty_since: None,
                lazer: None,
                lazer_checked: None,
                display: LazerDisplay::default(),
                exe_dir,
                otd_windows_ink: false,
                apps: Vec::new(),
                apps_checked: None,
                // Started with --test-mode, it stays on until turned off by hand.
                precision_ends: None,
            };
            if app.shared.settings().output == Output::OpenTabletDriver {
                app.ensure_otd_running();
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_settings_that_add_delay_are_flagged() {
        let best = LazerDisplay { frame_sync: Some("Unlimited".into()), window_mode: Some("Fullscreen".into()) };
        assert!(display_checks(&best).is_empty());
        let worse = LazerDisplay { frame_sync: Some("VSync".into()), window_mode: Some("Borderless".into()) };
        let checks = display_checks(&worse);
        assert_eq!(checks.len(), 2);
        assert!(checks[0].title.contains("VSync"));
        assert!(checks[1].title.contains("Borderless"));
        assert!(display_checks(&LazerDisplay::default()).is_empty());
    }

    #[test]
    fn otd_states_say_what_to_do() {
        assert_eq!(otd_checks(false, false, false)[0].level, CheckLevel::Bad);
        assert_eq!(otd_checks(true, true, false)[0].level, CheckLevel::Good);
        assert!(otd_checks(true, false, false)[0].detail.as_ref().unwrap().contains("Tools"));
        let ink = otd_checks(true, true, true);
        assert_eq!(ink.len(), 2);
        assert!(ink[1].title.contains("Windows Ink"));
    }

    #[test]
    fn tablet_states_say_what_to_do() {
        assert_eq!(tablet_check(DeviceState::Ready, true).level, CheckLevel::Good);
        let unauthorized = tablet_check(DeviceState::Unauthorized, false);
        assert_eq!(unauthorized.level, CheckLevel::Bad);
        assert!(unauthorized.detail.unwrap().contains("Allow"));
        assert!(tablet_check(DeviceState::NoDevice, false).detail.unwrap().contains("USB"));
        assert!(tablet_check(DeviceState::AdbMissing, false).detail.unwrap().contains("platform-tools"));
    }

    #[test]
    fn high_precision_on_blocks_ready() {
        let on = lazer_check(Some(LazerInput { high_precision: true })).unwrap();
        assert_eq!(on.level, CheckLevel::Bad);
        let off = lazer_check(Some(LazerInput { high_precision: false })).unwrap();
        assert_eq!(off.level, CheckLevel::Good);
        assert!(lazer_check(None).is_none(), "no lazer means nothing to say");
    }

    #[test]
    fn the_stretched_area_from_real_use_is_flagged() {
        // 88 x 75.2 mm on a 2560 x 1600 screen, from a real session.
        let check = shape_check((2560.0 / 88.0, 1600.0 / 75.2));
        assert_eq!(check.level, CheckLevel::Warn);
        assert!(check.title.contains("37%"), "{}", check.title);
        assert_eq!(shape_check((2560.0 / 83.0, 1600.0 / 51.875)).level, CheckLevel::Good);
    }

    #[test]
    fn apps_are_named_in_the_warning() {
        assert_eq!(apps_check(&[]).level, CheckLevel::Good);
        let check = apps_check(&["PowerToys", "Wallpaper Engine"]);
        assert_eq!(check.level, CheckLevel::Warn);
        assert!(check.title.contains("PowerToys, Wallpaper Engine"));
    }

    #[test]
    fn the_worst_check_decides_the_headline() {
        let (level, _) = summary(&[good("a"), good("b")]);
        assert_eq!(level, CheckLevel::Good);
        let (level, _) = summary(&[good("a"), warn("b", "x")]);
        assert_eq!(level, CheckLevel::Warn);
        let (level, text) = summary(&[warn("a", "x"), bad("b", "y")]);
        assert_eq!((level, text), (CheckLevel::Bad, "Not ready yet"));
    }

    #[test]
    fn tab_names_from_the_command_line() {
        assert_eq!(Tab::from_name("area"), Some(Tab::Area));
        assert_eq!(Tab::from_name("nope"), None);
    }
}
