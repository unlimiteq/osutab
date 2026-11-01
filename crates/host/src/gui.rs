//! The settings window: setup questions, pre-play checks, area editor, latency HUD, precision test,
//! appearance and settings. It runs on its own thread and only reaches the input path through
//! `Shared`, so nothing drawn here, animations included, can hold up a pen report.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontFamily, Pos2, RichText, Stroke, Vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::adb::DeviceState;
use crate::lazer::{self, LazerDisplay, LazerInput};
use crate::mapping::{Rotation, TABLET_PX_PER_MM};
use crate::otd;
use crate::preflight;
use crate::settings::{self, Output, Settings, UsbPath, MIN_AREA_MM};
use crate::shared::Shared;
use crate::theme::{self, Rgb, Theme, ThemeOverrides, MAX_THICKNESS, MIN_THICKNESS};
use crate::tray;

const WINDOW_TITLE: &str = "osutab";
const SAVE_DELAY: Duration = Duration::from_millis(500);
const CHECK_EVERY: Duration = Duration::from_secs(1);
const PROCESS_CHECK_EVERY: Duration = Duration::from_secs(3);
// Long enough to check accuracy, short enough that it can't quietly slow a whole play session.
const PRECISION_TEST_TIME: Duration = Duration::from_secs(120);
const QUICK_WIDTHS_MM: [f64; 4] = [80.0, 100.0, 120.0, 150.0];

const TAB_FADE: f32 = 0.12;
const NAV_FADE: f32 = 0.1;
const STATUS_FADE: f32 = 0.2;
const GLIDE: f32 = 0.15;
const GLIDE_WINDOW: Duration = Duration::from_millis(200);
const NEXT_CONNECT: &str = "Applies the next time the tablet connects.";

const SEMIBOLD: &str = "semibold";
const SIDEBAR_WIDTH: f32 = 196.0;
const PAGE_WIDTH: f32 = 820.0;
const SETUP_WIDTH: f32 = 560.0;
const CARD_RADIUS: u8 = 14;
const WIDGET_RADIUS: u8 = 8;
const CARD_GAP: f32 = 14.0;
const ITEM_SPACING: Vec2 = Vec2::new(8.0, 8.0);
const THEME_TILE: Vec2 = Vec2::new(116.0, 96.0);

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
    Appearance,
    Settings,
}

impl Tab {
    const ALL: [(Tab, &'static str); 6] = [
        (Tab::Play, "Play"),
        (Tab::Area, "Pen area"),
        (Tab::Latency, "Latency"),
        (Tab::Precision, "Precision test"),
        (Tab::Appearance, "Appearance"),
        (Tab::Settings, "Settings"),
    ];

    pub fn from_name(name: &str) -> Option<Tab> {
        match name {
            "play" => Some(Tab::Play),
            "area" => Some(Tab::Area),
            "latency" => Some(Tab::Latency),
            "precision" => Some(Tab::Precision),
            "appearance" => Some(Tab::Appearance),
            "settings" => Some(Tab::Settings),
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

fn screen_aspect_of(shared: &Shared) -> f64 {
    let target = shared.target();
    target.width / target.height
}

fn to_color(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Whether panels are too close to the background to stand out without an outline.
fn needs_edges(t: &Theme) -> bool {
    let gap = |a: u8, b: u8| a.abs_diff(b) as u32;
    gap(t.background.0, t.panel.0) + gap(t.background.1, t.panel.1) + gap(t.background.2, t.panel.2) < 12
}

/// The colors the window draws with, worked out from the theme.
#[derive(Debug, Clone, Copy)]
struct Palette {
    panel: Color32,
    line: Color32,
    soft: Color32,
    raised: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    on_accent: Color32,
    area: Color32,
    outside: Color32,
    outline: Color32,
    thickness: f32,
    good: Color32,
    warning: Color32,
    bad: Color32,
    edges: bool,
    light: bool,
}

impl Palette {
    fn of(t: &Theme) -> Palette {
        let (good, warning, bad) = if t.background.is_light() {
            (Rgb(29, 138, 76), Rgb(176, 96, 8), Rgb(200, 40, 40))
        } else {
            (Rgb(95, 211, 141), Rgb(255, 170, 80), Rgb(255, 120, 120))
        };
        Palette {
            panel: to_color(t.panel),
            line: to_color(t.panel.blend(t.text, 0.12)),
            soft: to_color(t.panel.blend(t.text, 0.07)),
            raised: to_color(t.panel.blend(t.text, 0.22)),
            text: to_color(t.text),
            muted: to_color(t.muted),
            accent: to_color(t.accent),
            on_accent: if t.accent.is_light() { Color32::from_rgb(20, 20, 24) } else { Color32::WHITE },
            area: to_color(t.background),
            outside: to_color(t.outside()),
            outline: to_color(t.outline),
            thickness: t.outline_thickness,
            good: to_color(good),
            warning: to_color(warning),
            bad: to_color(bad),
            edges: needs_edges(t),
            light: t.background.is_light(),
        }
    }

    fn level(&self, level: CheckLevel) -> Color32 {
        match level {
            CheckLevel::Good => self.good,
            CheckLevel::Warn => self.warning,
            CheckLevel::Bad => self.bad,
        }
    }
}

fn visuals_for(t: &Theme) -> egui::Visuals {
    let mut v = if t.background.is_light() { egui::Visuals::light() } else { egui::Visuals::dark() };
    let radius = egui::CornerRadius::same(WIDGET_RADIUS);
    let (panel, text) = (t.panel, t.text);
    let line = to_color(panel.blend(text, 0.12));
    // Shade alone separates things, unless a theme's shades are too close to tell apart.
    let edge = if needs_edges(t) { Stroke::new(1.0_f32, line) } else { Stroke::NONE };
    v.panel_fill = to_color(t.background);
    v.window_fill = to_color(panel);
    v.extreme_bg_color = to_color(t.background);
    v.faint_bg_color = to_color(panel.blend(text, 0.04));
    v.override_text_color = Some(to_color(text));
    v.hyperlink_color = to_color(t.accent);
    v.selection.bg_fill = to_color(t.background.blend(t.accent, 0.55));
    v.selection.stroke = Stroke::new(1.0_f32, to_color(text));
    v.slider_trailing_fill = true;
    v.window_stroke = Stroke::new(1.0_f32, line);
    v.window_corner_radius = egui::CornerRadius::same(12);
    v.menu_corner_radius = egui::CornerRadius::same(12);
    let widgets = [
        (&mut v.widgets.noninteractive, panel, Stroke::new(1.0_f32, line)),
        (&mut v.widgets.inactive, panel.blend(text, 0.08), edge),
        (&mut v.widgets.hovered, panel.blend(text, 0.14), edge),
        (&mut v.widgets.active, panel.blend(text, 0.2), edge),
        (&mut v.widgets.open, panel.blend(text, 0.14), edge),
    ];
    for (widget, fill, border) in widgets {
        widget.bg_fill = to_color(fill);
        widget.weak_bg_fill = to_color(fill);
        widget.bg_stroke = border;
        widget.fg_stroke = Stroke::new(1.0_f32, to_color(text));
        widget.corner_radius = radius;
        widget.expansion = 0.0;
    }
    v
}

/// Geist, built into the program so it looks the same on every PC. egui's own fonts stay behind it
/// for any symbol Geist doesn't have.
fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let mut add = |name: &str, data: &'static [u8]| -> Vec<String> {
        fonts.font_data.insert(name.to_string(), Arc::new(egui::FontData::from_static(data)));
        let mut family = fallback.clone();
        family.insert(0, name.to_string());
        family
    };
    let regular = add("geist", include_bytes!("../../../assets/fonts/Geist-Regular.ttf"));
    let semibold = add("geist-semibold", include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"));
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    fonts
}

/// Colors the title bar to match, so the window reads as one surface. Windows 10 ignores this.
fn color_title_bar(window: isize, t: &Theme) {
    if window == 0 {
        return;
    }
    let colors = [
        (DWMWA_CAPTION_COLOR, t.background),
        (DWMWA_TEXT_COLOR, t.text),
        (DWMWA_BORDER_COLOR, t.background.blend(t.text, 0.12)),
    ];
    for (attribute, c) in colors {
        let value = c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16;
        unsafe { DwmSetWindowAttribute(window as _, attribute as u32, (&value as *const u32).cast(), 4) };
    }
}

/// Fades to a new color, or changes at once with no time given.
fn faded(ctx: &egui::Context, id: egui::Id, color: Color32, seconds: f32) -> Color32 {
    let channel = |i: u8, value: u8| ctx.animate_value_with_time(id.with(i), value as f32, seconds).round() as u8;
    Color32::from_rgb(channel(0, color.r()), channel(1, color.g()), channel(2, color.b()))
}

fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).family(FontFamily::Name(SEMIBOLD.into()))
}

fn status_dot(ui: &mut egui::Ui, salt: &str, color: Color32, seconds: f32) {
    let color = faded(ui.ctx(), egui::Id::new(("status", salt)), color, seconds);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(strong(text));
    ui.add_space(4.0);
}

fn hint(ui: &mut egui::Ui, text: &str, palette: &Palette) {
    ui.label(RichText::new(text).color(palette.muted).small());
}

fn divider(ui: &mut egui::Ui) {
    ui.add(egui::Separator::default().spacing(10.0));
}

fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str, palette: &Palette) {
    ui.label(RichText::new(title).heading());
    ui.label(RichText::new(subtitle).color(palette.muted));
    ui.add_space(12.0);
}

/// Only there when there's something to reset.
fn reset_button(ui: &mut egui::Ui, changed_from_default: bool, palette: &Palette) -> bool {
    changed_from_default && ui.add(egui::Button::new(RichText::new("Reset").color(palette.muted).small()).frame(false)).clicked()
}

fn toggle(ui: &mut egui::Ui, on: &mut bool, accent: Color32) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(38.0, 22.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, ""));
    let amount = ui.ctx().animate_bool_responsive(response.id, *on);
    let radius = rect.height() / 2.0;
    let track = ui.style().interact(&response).bg_fill.lerp_to_gamma(accent, amount);
    ui.painter().rect_filled(rect, radius, track);
    let knob = egui::lerp((rect.left() + radius)..=(rect.right() - radius), amount);
    ui.painter().circle_filled(Pos2::new(knob, rect.center().y), radius - 3.0, Color32::WHITE);
    response
}

/// A slider that fits the room left in its row, with a slimmer knob than the theme's controls.
fn row_slider(ui: &mut egui::Ui, slider: egui::Slider, palette: &Palette) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 100.0).clamp(60.0, 130.0);
        ui.spacing_mut().interact_size.y = 22.0;
        ui.spacing_mut().button_padding.y = 2.0;
        let widgets = &mut ui.visuals_mut().widgets;
        for widget in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active] {
            widget.fg_stroke = Stroke::new(1.5_f32, palette.accent);
        }
        ui.add(slider)
    })
    .inner
}

/// A row of options with one chosen. Says whether the choice changed.
fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)], palette: &Palette) -> bool {
    let mut changed = false;
    egui::Frame::new().fill(palette.soft).corner_radius(WIDGET_RADIUS).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().interact_size.y = 24.0;
        ui.spacing_mut().button_padding = Vec2::new(10.0, 3.0);
        ui.horizontal(|ui| {
            let reversed = ui.layout().prefer_right_to_left();
            let ordered: Vec<_> = if reversed { options.iter().rev().collect() } else { options.iter().collect() };
            for (option, name) in ordered {
                let chosen = *value == *option;
                let text = if chosen { strong(*name) } else { RichText::new(*name).color(palette.muted) };
                let fill = if chosen { palette.raised } else { Color32::TRANSPARENT };
                if ui.add(egui::Button::new(text).fill(fill).corner_radius(WIDGET_RADIUS - 2)).clicked() && !chosen {
                    *value = *option;
                    changed = true;
                }
            }
        });
    });
    changed
}

fn nav_icon(painter: &egui::Painter, tab: Tab, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.6_f32, color);
    let at = |x: f32, y: f32| c + Vec2::new(x, y);
    match tab {
        Tab::Play => {
            painter.add(egui::Shape::convex_polygon(vec![at(-4.0, -6.0), at(6.5, 0.0), at(-4.0, 6.0)], color, Stroke::new(1.0_f32, color)));
        }
        Tab::Area => {
            let rect = egui::Rect::from_center_size(c, Vec2::new(16.0, 11.0));
            painter.rect_stroke(rect, 2.5, stroke, egui::StrokeKind::Middle);
            painter.circle_filled(c, 1.8, color);
        }
        Tab::Latency => {
            let bolt = vec![at(1.5, -7.0), at(-4.5, 1.0), at(0.0, 1.0), at(-1.5, 7.0), at(4.5, -1.0), at(0.0, -1.0), at(1.5, -7.0)];
            painter.add(egui::Shape::line(bolt, stroke));
        }
        Tab::Precision => {
            painter.circle_stroke(c, 5.0, stroke);
            for (x, y) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                painter.line_segment([at(x * 2.5, y * 2.5), at(x * 7.5, y * 7.5)], stroke);
            }
        }
        Tab::Appearance => {
            painter.circle_stroke(c, 6.5, stroke);
            let half = (0..=12).map(|i| {
                let angle = std::f32::consts::PI * (i as f32 / 12.0 - 0.5);
                at(6.5 * angle.cos(), 6.5 * angle.sin())
            });
            painter.add(egui::Shape::convex_polygon(half.collect(), color, Stroke::NONE));
        }
        Tab::Settings => {
            for (y, knob) in [(-3.5, 3.0), (3.5, -3.0)] {
                painter.line_segment([at(-7.0, y), at(7.0, y)], stroke);
                painter.circle_filled(at(knob, y), 2.6, color);
            }
        }
    }
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
    palette: Palette,
    applied: Option<(Theme, bool)>,
    animations: bool,
    tab_changed: Instant,
    glide_until: Instant,
    last_preset: usize,
    theme_name: String,
    setup_step: u8,
    hide_at_start: bool,
    show_passed: bool,
}

impl App {
    fn seconds(&self, wanted: f32) -> f32 {
        if self.animations { wanted } else { 0.0 }
    }

    /// Draws a card, and says how tall its content wanted to be.
    fn sized_card(&mut self, ui: &mut egui::Ui, min_height: f32, add: impl FnOnce(&mut Self, &mut egui::Ui)) -> f32 {
        let palette = self.palette;
        let mut frame = egui::Frame::new().fill(palette.panel).corner_radius(CARD_RADIUS).inner_margin(egui::Margin::same(18));
        if palette.edges {
            frame = frame.stroke(Stroke::new(1.0_f32, palette.line));
        }
        if palette.light {
            frame = frame.shadow(egui::Shadow { offset: [0, 1], blur: 10, spread: 0, color: Color32::from_black_alpha(18) });
        }
        let mut height = 0.0;
        frame.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(min_height);
            ui.spacing_mut().item_spacing = ITEM_SPACING;
            height = ui.vertical(|ui| add(self, ui)).response.rect.height();
        });
        height
    }

    fn card(&mut self, ui: &mut egui::Ui, add: impl FnOnce(&mut Self, &mut egui::Ui)) {
        self.sized_card(ui, 0.0, add);
        ui.add_space(CARD_GAP - ui.spacing().item_spacing.y);
    }

    /// Two cards side by side, kept the same height using what each measured last frame.
    fn card_pair(&mut self, ui: &mut egui::Ui, id: &str, left: impl FnOnce(&mut Self, &mut egui::Ui), right: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let key = egui::Id::new(("card_pair", id));
        let height = ui.ctx().data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
        let mut tallest = 0.0_f32;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            ui.columns(2, |columns| {
                tallest = tallest.max(self.sized_card(&mut columns[0], height, left));
                tallest = tallest.max(self.sized_card(&mut columns[1], height, right));
            });
        });
        ui.ctx().data_mut(|d| d.insert_temp(key, tallest));
        ui.add_space(CARD_GAP - ui.spacing().item_spacing.y);
    }

    /// A title and optional explanation on the left, with its control on the right.
    fn setting_row(&mut self, ui: &mut egui::Ui, title: &str, explanation: &str, add: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let palette = self.palette;
        let left = if explanation.is_empty() {
            let font = egui::TextStyle::Body.resolve(ui.style());
            ui.fonts(|f| f.layout_no_wrap(title.to_string(), font, palette.text)).size().x.ceil()
        } else {
            (ui.available_width() * 0.62).floor()
        };
        ui.horizontal_top(|ui| {
            let text = ui.allocate_ui_with_layout(Vec2::new(left, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(left);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add_space(4.0);
                ui.label(title);
                if !explanation.is_empty() {
                    hint(ui, explanation, &palette);
                }
            });
            // A set height, because a page that scrolls would otherwise centre this in endless room.
            let room = Vec2::new(ui.available_width(), text.response.rect.height().max(ui.spacing().interact_size.y));
            ui.allocate_ui_with_layout(room, egui::Layout::right_to_left(egui::Align::Center), |ui| add(self, ui));
        });
    }

    fn toggle_row(&mut self, ui: &mut egui::Ui, title: &str, explanation: &str, current: bool, default: bool, set: fn(&mut Settings, bool)) {
        self.setting_row(ui, title, explanation, |app, ui| {
            let accent = app.palette.accent;
            let mut on = current;
            if toggle(ui, &mut on, accent).changed() {
                app.shared.update_settings(|s| set(s, on));
                app.changed();
            }
            if reset_button(ui, current != default, &app.palette) {
                app.shared.update_settings(|s| set(s, default));
                app.changed();
            }
        });
    }

    fn stat_tile(&mut self, ui: &mut egui::Ui, name: &str, value: &str, unit: &str, color: Color32) {
        self.sized_card(ui, 0.0, |app, ui| {
            hint(ui, name, &app.palette);
            stat_value(ui, value, unit, color, app.palette.muted);
        });
    }

    /// Follows the settings, so a new theme or the animation switch shows at once.
    fn apply_appearance(&mut self, ctx: &egui::Context, settings: &Settings) {
        let wanted = (settings.active_theme(), settings.animations);
        if self.applied.as_ref() == Some(&wanted) {
            return;
        }
        self.palette = Palette::of(&wanted.0);
        self.animations = wanted.1;
        color_title_bar(self.shared.window.load(Ordering::SeqCst) as isize, &wanted.0);
        let visuals = visuals_for(&wanted.0);
        let animation_time = if wanted.1 { 0.1 } else { 0.0 };
        ctx.style_mut(|style| {
            use egui::{FontId, TextStyle};
            style.visuals = visuals;
            style.animation_time = animation_time;
            style.text_styles.insert(TextStyle::Heading, FontId::new(26.0, FontFamily::Name(SEMIBOLD.into())));
            style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Small, FontId::proportional(12.5));
            style.spacing.item_spacing = ITEM_SPACING;
            style.spacing.button_padding = Vec2::new(12.0, 5.0);
            style.spacing.interact_size.y = 28.0;
            style.spacing.slider_width = 130.0;
            style.spacing.slider_rail_height = 4.0;
        });
        self.applied = Some(wanted);
    }

    fn select_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.tab_changed = Instant::now();
        }
    }

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
            checks.push(shape_check(live.area.px_per_mm(&live.target)));
        } else {
            let installed = otd::program_dir(&self.exe_dir).is_some();
            let relay = self.shared.relay.connected.load(Ordering::SeqCst);
            checks.extend(otd_checks(installed, relay, self.otd_windows_ink));
        }
        if let Some(problem) = self.shared.direct_problem.lock().unwrap().clone() {
            checks.push(warn(
                "Direct USB isn't working, so osutab is using adb.exe",
                format!("It tries again the next time the tablet connects. The error was: {problem}"),
            ));
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
        let options = [(Output::Direct, "Direct"), (Output::OpenTabletDriver, "OpenTabletDriver")];
        if segmented(ui, &mut output, &options, &self.palette) {
            self.shared.update_settings(|s| s.output = output);
            self.changed();
            if output == Output::OpenTabletDriver {
                self.ensure_otd_running();
            }
        }
        if output == Output::Direct {
            hint(ui, "Straight to the cursor. The fastest path, with no Windows Ink, filters or other programs.", &self.palette);
        } else {
            hint(ui, "Sends the pen through OpenTabletDriver, so its filters and plugins work. It sets the area itself.", &self.palette);
            if let Some(dir) = otd::program_dir(&self.exe_dir) {
                if ui.button("Open OpenTabletDriver").clicked() {
                    if let Err(e) = otd::open_settings_window(&dir) {
                        self.message = format!("Could not open OpenTabletDriver: {e}");
                    }
                }
            }
        }
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
        egui::ComboBox::from_id_salt("orientation").selected_text(label).width(ui.available_width().min(300.0)).show_ui(ui, |ui| {
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
        let aspect = screen_aspect_of(&self.shared);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.spacing_mut().button_padding.x = 8.0;
            for width in QUICK_WIDTHS_MM {
                let height = (width / aspect).clamp(MIN_AREA_MM, max_h);
                if ui.button(format!("{width:.0}")).on_hover_text(format!("{width:.0} mm wide")).clicked() {
                    self.set_size(width.min(max_w), height);
                }
            }
            if ui.button("Full").clicked() {
                let height = (max_w / aspect).min(max_h);
                self.set_size((height * aspect).min(max_w), height);
            }
            ui.add_space(4.0);
            hint(ui, "mm wide", &self.palette);
        });
    }

    fn size_fields(&mut self, ui: &mut egui::Ui) {
        let settings = self.shared.settings();
        let live = self.shared.live_config();
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        let screen_aspect = screen_aspect_of(&self.shared);
        let (mut width, mut height) = (live.area.width_mm, live.area.height_mm);
        let lock = settings.aspect_lock;

        let (mut w_changed, mut h_changed) = (false, false);
        self.setting_row(ui, "Width", "", |_, ui| {
            w_changed = ui.add(egui::DragValue::new(&mut width).range(MIN_AREA_MM..=max_w).speed(0.5).max_decimals(1).suffix(" mm")).changed();
        });
        self.setting_row(ui, "Height", "", |_, ui| {
            h_changed = ui.add(egui::DragValue::new(&mut height).range(MIN_AREA_MM..=max_h).speed(0.5).max_decimals(1).suffix(" mm")).changed();
        });
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
        self.setting_row(ui, "Lock to screen shape", "", |app, ui| {
            let mut on = lock;
            if toggle(ui, &mut on, app.palette.accent).changed() {
                app.shared.update_settings(|s| s.aspect_lock = on);
                if on {
                    let new_height = (width / screen_aspect).clamp(MIN_AREA_MM, max_h);
                    let new_width = (new_height * screen_aspect).clamp(MIN_AREA_MM, max_w);
                    app.set_size(new_width, new_height);
                }
                app.changed();
            }
        });
    }

    fn sensitivity_text(&self) -> String {
        let live = self.shared.live_config();
        let (x, y) = live.area.px_per_mm(&live.target);
        if stretch_percent((x, y)) <= 1.0 {
            format!("1 mm of pen movement = {x:.1} px on screen")
        } else {
            format!("1 mm of pen movement = {x:.1} px across, {y:.1} px down")
        }
    }

    /// One question at a time, with the pen area showing throughout.
    fn setup_screen(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let palette = self.palette;
        page_header(ui, "Welcome to osutab", &format!("Question {} of 2. You can change both later.", self.setup_step + 1), &palette);
        self.card(ui, |app, ui| {
            if app.setup_step == 0 {
                section_title(ui, "How will the tablet sit while you play?");
                app.orientation_picker(ui);
            } else {
                section_title(ui, "How big should the pen area be?");
                hint(ui, "This part of the tablet covers your whole screen. A smaller area feels faster.", &palette);
                ui.add_space(4.0);
                app.quick_sizes(ui);
                app.size_fields(ui);
                hint(ui, &app.sensitivity_text(), &palette);
            }
        });
        self.card(ui, |app, ui| {
            ui.vertical_centered(|ui| app.preview(ui, 200.0));
        });
        ui.horizontal(|ui| {
            if self.setup_step == 1 && ui.button("Back").clicked() {
                self.setup_step = 0;
            }
            let label = if self.setup_step == 0 { "Next" } else { "Start" };
            let button = egui::Button::new(strong(label).color(palette.on_accent)).fill(palette.accent).min_size(Vec2::new(110.0, 32.0));
            if ui.add(button).clicked() {
                if self.setup_step == 0 {
                    self.setup_step = 1;
                } else {
                    self.save_now();
                    self.shared.setup_done.store(true, Ordering::SeqCst);
                }
            }
        });
    }

    fn nav_item(&mut self, ui: &mut egui::Ui, tab: Tab, name: &str) {
        let palette = self.palette;
        let chosen = self.tab == tab;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), egui::Sense::click());
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, chosen, name));
        let lit = ui.ctx().animate_bool_with_time(response.id, chosen || response.hovered(), self.seconds(NAV_FADE));
        let painter = ui.painter();
        if lit > 0.0 {
            painter.rect_filled(rect, 10.0, palette.panel.gamma_multiply(lit));
        }
        nav_icon(painter, tab, Pos2::new(rect.left() + 21.0, rect.center().y), if chosen { palette.accent } else { palette.muted });
        let (family, color) = if chosen { (FontFamily::Name(SEMIBOLD.into()), palette.text) } else { (FontFamily::Proportional, palette.muted) };
        painter.text(Pos2::new(rect.left() + 40.0, rect.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::new(14.0, family), color);
        if response.clicked() {
            self.select_tab(tab);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        for (tab, name) in Tab::ALL {
            self.nav_item(ui, tab, name);
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| self.status_block(ui));
    }

    /// Sits at the foot of the sidebar, so it's on every page.
    fn status_block(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        let quit = egui::Button::new(RichText::new("Quit").color(palette.muted)).min_size(Vec2::new(ui.available_width(), 30.0));
        if ui.add(quit).on_hover_text("Stops osutab and gives the pen back to Android").clicked() {
            self.shared.stats.stop.store(true, Ordering::SeqCst);
        }
        ui.add_space(8.0);
        let hud = self.shared.stats.hud();
        if hud.recent.total.count > 0 {
            hint(ui, &format!("{:.2} ms, {:.0} Hz", hud.recent.total.p50, hud.rate_hz), &palette);
        }
        let checks = self.checks();
        let (level, headline) = summary(&checks);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            status_dot(ui, "summary", palette.level(level), self.seconds(STATUS_FADE));
            ui.label(RichText::new(headline).small());
        });
    }

    fn check_row(&self, ui: &mut egui::Ui, check: &Check) {
        ui.horizontal(|ui| {
            status_dot(ui, &check.title, self.palette.level(check.level), self.seconds(STATUS_FADE));
            ui.label(&check.title);
        });
        if let Some(detail) = &check.detail {
            ui.horizontal_top(|ui| {
                ui.add_space(20.0);
                ui.vertical(|ui| hint(ui, detail, &self.palette));
            });
        }
    }

    /// Problems come first, and whatever's fine folds down to one line.
    fn play_tab(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        let mut checks = self.checks();
        let (level, headline) = summary(&checks);
        if self.shared.connected.load(Ordering::SeqCst) && !self.shared.app_connected.load(Ordering::SeqCst) {
            checks.push(warn(
                "Tablet app not showing",
                "The pen still works. Without the app, your palm can tap whatever's on the tablet. It may be waiting for you to approve the install.",
            ));
        }
        let color = if level == CheckLevel::Good { palette.text } else { palette.level(level) };
        ui.label(RichText::new(headline).heading().color(color));
        ui.label(RichText::new(match level {
            CheckLevel::Good => "Everything's in place.",
            CheckLevel::Warn => "You can play, but these could get in the way.",
            CheckLevel::Bad => "Fix the items below to start.",
        }).color(palette.muted));
        ui.add_space(12.0);

        let (passed, problems): (Vec<&Check>, Vec<&Check>) = checks.iter().partition(|c| c.level == CheckLevel::Good);
        if !problems.is_empty() {
            self.card(ui, |app, ui| {
                for (i, check) in problems.iter().enumerate() {
                    if i > 0 {
                        divider(ui);
                    }
                    app.check_row(ui, check);
                }
            });
        }
        if !passed.is_empty() {
            self.card(ui, |app, ui| {
                let label = if passed.len() == 1 { "1 check passed".to_string() } else { format!("{} checks passed", passed.len()) };
                ui.horizontal(|ui| {
                    status_dot(ui, "passed", palette.good, app.seconds(STATUS_FADE));
                    ui.label(label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = if app.show_passed { "Hide" } else { "Show" };
                        if ui.add(egui::Button::new(RichText::new(text).color(palette.muted).small()).frame(false)).clicked() {
                            app.show_passed = !app.show_passed;
                        }
                    });
                });
                if app.show_passed {
                    ui.add_space(4.0);
                    for check in &passed {
                        ui.horizontal_top(|ui| {
                            ui.add_space(20.0);
                            ui.vertical(|ui| hint(ui, &check.title, &palette));
                        });
                    }
                }
            });
        }

        let direct = self.shared.settings().output == Output::Direct;
        self.card_pair(ui, "play", |app, ui| {
            section_title(ui, "Pen area");
            if direct {
                ui.vertical_centered(|ui| app.preview(ui, 170.0));
            } else {
                hint(ui, "Set in OpenTabletDriver.", &palette);
            }
        }, |app, ui| {
            section_title(ui, "Output");
            app.output_picker(ui);
            if direct {
                ui.add_space(6.0);
                app.preset_picker(ui);
                hint(ui, &app.sensitivity_text(), &palette);
            }
        });
        let closing = if self.shared.settings().close_to_tray {
            "Closing this window keeps osutab running in the tray. Quit gives the pen back to Android."
        } else {
            "Closing this window quits osutab and gives the pen back to Android."
        };
        hint(ui, closing, &palette);
    }

    fn preset_picker(&mut self, ui: &mut egui::Ui) {
        let settings = self.shared.settings();
        self.setting_row(ui, "Preset", "", |app, ui| {
            egui::ComboBox::from_id_salt(ui.next_auto_id())
                .selected_text(settings.presets[settings.active].name.clone())
                .width(ui.available_width().min(140.0))
                .show_ui(ui, |ui| {
                    for (index, preset) in settings.presets.iter().enumerate() {
                        if ui.selectable_label(index == settings.active, &preset.name).clicked() {
                            app.shared.update_settings(|s| s.active = index);
                            app.changed();
                        }
                    }
                });
        });
    }

    fn area_tab(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        page_header(ui, "Pen area", "The part of the tablet that covers your whole screen.", &palette);
        if self.shared.settings().output == Output::OpenTabletDriver {
            self.card(ui, |app, ui| {
                ui.label("In OpenTabletDriver mode, the area, filters and buttons are set in OpenTabletDriver.");
                hint(ui, "It sees a landscape tablet, 313.8 x 195.9 mm. If you hold the tablet in portrait, set the area's Rotation there to 90 or 270.", &palette);
                ui.add_space(6.0);
                app.output_picker(ui);
            });
            return;
        }
        self.card(ui, |app, ui| {
            let live = app.shared.live_config();
            ui.horizontal(|ui| {
                ui.label(strong(format!("{:.1} x {:.1} mm", live.area.width_mm, live.area.height_mm)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| hint(ui, &app.sensitivity_text(), &palette));
            });
            ui.add_space(4.0);
            ui.vertical_centered(|ui| app.preview(ui, 210.0));
            ui.add_space(4.0);
            app.stretch_fix(ui);
        });
        self.card_pair(ui, "size", |app, ui| {
            section_title(ui, "Size");
            app.quick_sizes(ui);
            app.size_fields(ui);
        }, |app, ui| {
            section_title(ui, "Position");
            app.position_fields(ui);
        });
        self.card_pair(ui, "presets", |app, ui| {
            section_title(ui, "Presets");
            app.preset_row(ui);
        }, |app, ui| {
            section_title(ui, "Tablet and pen");
            app.pen_options(ui);
        });
    }

    fn position_fields(&mut self, ui: &mut egui::Ui) {
        let live = self.shared.live_config();
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        // Shows where the area really is, after fitting it on the tablet.
        let mut offset_x = live.area.center_x - live.area.tablet_w / 2.0;
        let mut offset_y = live.area.center_y - live.area.tablet_h / 2.0;
        let mut rotation = self.shared.settings().area().rotation_deg;
        let mut changed = false;
        self.setting_row(ui, "X offset", "", |_, ui| {
            changed |= ui.add(egui::DragValue::new(&mut offset_x).range(-max_w / 2.0..=max_w / 2.0).speed(0.5).max_decimals(1).suffix(" mm")).changed();
        });
        self.setting_row(ui, "Y offset", "", |_, ui| {
            changed |= ui.add(egui::DragValue::new(&mut offset_y).range(-max_h / 2.0..=max_h / 2.0).speed(0.5).max_decimals(1).suffix(" mm")).changed();
        });
        self.setting_row(ui, "Rotation", "", |_, ui| {
            changed |= ui.add(egui::DragValue::new(&mut rotation).range(-180.0..=180.0).speed(0.25).max_decimals(1).suffix(" deg")).changed();
        });
        if ui.button("Center").clicked() {
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
        hint(
            ui,
            &format!(
                "On the tablet's screen: {:.0} x {:.0} px",
                live.area.width_mm * TABLET_PX_PER_MM,
                live.area.height_mm * TABLET_PX_PER_MM
            ),
            &self.palette,
        );
    }

    fn pen_options(&mut self, ui: &mut egui::Ui) {
        self.orientation_picker(ui);
        let settings = self.shared.settings();
        let defaults = Settings::default();
        let mut key = settings.button_key.clone();
        self.toggle_row(ui, "Show the area on the tablet", "", settings.show_outline, defaults.show_outline, |s, on| s.show_outline = on);
        self.toggle_row(ui, "Pen tip clicks", "", settings.tip_click, defaults.tip_click, |s, on| s.tip_click = on);
        self.setting_row(ui, "Side button key", "", |app, ui| {
            let response = ui
                .add(egui::TextEdit::singleline(&mut key).hint_text("None").desired_width(44.0).char_limit(1))
                .on_hover_text("A letter or digit, such as z. Leave empty for none.");
            if response.changed() && (key.is_empty() || crate::inject::scan_code_for_key(&key).is_some()) {
                let key = key.to_lowercase();
                app.shared.update_settings(|s| s.button_key = key);
                app.changed();
            }
        });
    }

    /// Offers the two ways to fix a stretched area.
    fn stretch_fix(&mut self, ui: &mut egui::Ui) {
        let live = self.shared.live_config();
        let px_per_mm = live.area.px_per_mm(&live.target);
        if stretch_percent(px_per_mm) <= 1.0 {
            return;
        }
        let aspect = screen_aspect_of(&self.shared);
        let (max_w, max_h) = live.rotation.tablet_size_mm();
        let (w, h) = (live.area.width_mm, live.area.height_mm);
        let keep_width = (w, (w / aspect).clamp(MIN_AREA_MM, max_h));
        let keep_height = ((h * aspect).clamp(MIN_AREA_MM, max_w), h);
        ui.colored_label(self.palette.warning, format!("This area is stretched by {:.0}%. Fix it:", stretch_percent(px_per_mm)));
        ui.horizontal_wrapped(|ui| {
            for size in [keep_width, keep_height] {
                if ui.button(format!("Make it {:.1} x {:.1} mm", size.0, size.1)).clicked() {
                    self.set_size(size.0, size.1);
                }
            }
        });
    }

    fn preset_row(&mut self, ui: &mut egui::Ui) {
        let settings = self.shared.settings();
        self.preset_picker(ui);
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).desired_width(120.0).hint_text("New name").margin(Vec2::new(8.0, 6.0)));
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
            if settings.presets.len() > 1 && ui.button("Delete").clicked() {
                self.shared.update_settings(|s| {
                    s.delete_active_preset();
                });
                self.changed();
            }
        });
    }

    /// The tablet, the pen area on it, and where the pen is, drawn the way the tablet shows them.
    fn preview(&self, ui: &mut egui::Ui, max_height: f32) {
        let live = self.shared.live_config();
        let (tablet_w, tablet_h) = (live.area.tablet_w as f32, live.area.tablet_h as f32);
        let scale = (ui.available_width() / tablet_w).min(max_height / tablet_h);
        let (response, painter) = ui.allocate_painter(Vec2::new(tablet_w * scale, tablet_h * scale), egui::Sense::hover());
        let origin = response.rect.min;
        let at = |x: f32, y: f32| origin + Vec2::new(x * scale, y * scale);

        // It only glides when switching presets. While editing, it follows the numbers exactly.
        let seconds = if Instant::now() < self.glide_until { self.seconds(GLIDE) } else { 0.0 };
        let ctx = ui.ctx();
        let corners: Vec<Pos2> = live
            .area
            .corners()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let id = egui::Id::new(("area_corner", i));
                at(
                    ctx.animate_value_with_time(id.with(0), c.0 as f32, seconds),
                    ctx.animate_value_with_time(id.with(1), c.1 as f32, seconds),
                )
            })
            .collect();

        painter.rect_filled(response.rect, 8.0, self.palette.outside);
        painter.add(egui::Shape::convex_polygon(corners, self.palette.area, Stroke::new(self.palette.thickness, self.palette.outline)));

        let (u, v, flags) = self.shared.pen();
        if flags & osutab_core::FLAG_IN_RANGE != 0 {
            let color = if flags & osutab_core::FLAG_TOUCH != 0 { self.palette.accent } else { self.palette.text };
            painter.circle_filled(at(u * tablet_w, v * tablet_h), 4.0, color);
        }
    }

    fn latency_tab(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        page_header(ui, "Latency", "How long each pen report takes to become cursor movement.", &palette);
        let hud = self.shared.stats.hud();
        if hud.recent.total.count == 0 {
            self.card(ui, |_, ui| {
                ui.label("Move the pen to see numbers.");
            });
        } else {
            let total = hud.recent.total;
            let median_color = if total.p50 <= 1.0 { palette.good } else { palette.warning };
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = CARD_GAP;
                ui.columns(3, |columns| {
                    let stats = [("Median", total.p50, median_color), ("99th percentile", total.p99, palette.text), ("Worst", total.max, palette.text)];
                    for (column, (name, value, color)) in columns.iter_mut().zip(stats) {
                        self.stat_tile(column, name, &format!("{value:.2}"), "ms", color);
                    }
                });
            });
            ui.add_space(CARD_GAP - ui.spacing().item_spacing.y);
            self.card(ui, |app, ui| {
                section_title(ui, "Where the time goes");
                let direct = app.shared.direct_active.load(Ordering::SeqCst);
                let row = |ui: &mut egui::Ui, name: &str, cells: [String; 3], color: Color32| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).color(color));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for cell in cells.iter().rev() {
                                ui.allocate_ui_with_layout(Vec2::new(58.0, 18.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.set_min_width(58.0);
                                    ui.label(RichText::new(cell).color(color));
                                });
                            }
                        });
                    });
                };
                row(ui, "Stage, in ms", ["Median".into(), "p99".into(), "Worst".into()], palette.muted);
                let stages = [
                    ("Pen event to helper sending", hud.recent.helper),
                    (if direct { "USB to the PC" } else { "USB and adb to the PC" }, hud.recent.link),
                    ("PC to SendInput finished", hud.recent.host),
                    ("SendInput to cursor moved", hud.recent.cursor),
                ];
                for (name, d) in stages {
                    let cells: [String; 3] = if d.count == 0 {
                        ["-".into(), "-".into(), "-".into()]
                    } else {
                        [format!("{:.3}", d.p50), format!("{:.3}", d.p99), format!("{:.3}", d.max)]
                    };
                    row(ui, name, cells, palette.text);
                }
                ui.add_space(4.0);
                hint(
                    ui,
                    &format!(
                        "{:.0} Hz, jitter {:.2} ms, dropped {}, overruns {}. Over the last 10 s of pen use. The USB stage relies on clock alignment, so it's accurate to a fraction of a ms (best round trip {:.2} ms). The last stage only shows during the precision test.",
                        hud.rate_hz, hud.jitter_ms, hud.dropped, hud.overruns, hud.best_rtt_ms
                    ),
                    &palette,
                );
            });
        }
        self.card(ui, |app, ui| app.input_path(ui));
        if ui.button("Open the logs folder").clicked() {
            if let Some(dir) = self.shared.settings_path().parent() {
                let _ = Command::new("explorer").arg(dir).spawn();
            }
        }
    }

    /// Everything between the pen and the game, so nothing is hidden.
    fn input_path(&self, ui: &mut egui::Ui) {
        let direct = self.shared.settings().output == Output::Direct;
        let direct_usb = self.shared.direct_active.load(Ordering::SeqCst);
        let (good, warning) = (self.palette.good, self.palette.warning);
        section_title(ui, "What's between the pen and the game");
        let row = |ui: &mut egui::Ui, name: &str, state: &str, color: Color32| {
            ui.horizontal(|ui| {
                ui.label(name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.colored_label(color, state));
            });
        };
        row(ui, "Android's input system", "Bypassed: read from the kernel", good);
        row(ui, "Smoothing, prediction, acceleration", if direct { "None" } else { "Whatever OpenTabletDriver has on" }, if direct { good } else { warning });
        row(ui, "Windows Ink", if direct { "Not used" } else if self.otd_windows_ink { "In use" } else { "Not used" }, if direct || !self.otd_windows_ink { good } else { warning });
        if direct_usb {
            row(ui, "adbd and USB", "In the path: most of the added time", warning);
            row(ui, "adb.exe", "Bypassed: direct USB", good);
        } else {
            row(ui, "adbd, USB and adb.exe", "In the path: most of the added time", warning);
        }
        if !direct {
            row(ui, "OpenTabletDriver", "In the path", warning);
        }
        row(ui, "Windows SendInput", "In the path: under 0.1 ms", good);
    }

    fn precision_tab(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        page_header(ui, "Precision test", "Compares where the pen says the cursor should be with where Windows put it.", &palette);
        let mut on = self.shared.test_mode.load(Ordering::SeqCst);
        self.card(ui, |app, ui| {
            app.setting_row(ui, "Compare the pen with the cursor", "It adds a little work to every pen report, so it turns itself off after 2 minutes, and when this window closes.", |app, ui| {
                if toggle(ui, &mut on, palette.accent).changed() {
                    app.shared.stats.reset_precision();
                    app.shared.test_mode.store(on, Ordering::SeqCst);
                    app.precision_ends = on.then(|| Instant::now() + PRECISION_TEST_TIME);
                    app.message.clear();
                }
                if on && ui.add(egui::Button::new(RichText::new("Reset").color(palette.muted).small()).frame(false)).clicked() {
                    app.shared.stats.reset_precision();
                }
            });
            if let Some(end) = app.precision_ends {
                ui.colored_label(palette.warning, format!("Turns off in {} s. Don't leave it on while playing.", end.saturating_duration_since(Instant::now()).as_secs()));
            }
            divider(ui);
            hint(ui, "It checks the Windows cursor. osu! draws its own, which is why High precision mouse must be off in lazer.", &palette);
        });
        if !on {
            return;
        }
        let p = self.shared.stats.precision();
        if p.samples == 0 {
            self.card(ui, |_, ui| {
                ui.label("Hover the pen and make big, fast jumps.");
            });
            return;
        }
        let color = if p.worst_px <= 1.0 { palette.good } else { palette.bad };
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            ui.columns(3, |columns| {
                self.stat_tile(&mut columns[0], "Worst error", &format!("{:.2}", p.worst_px), "px", color);
                self.stat_tile(&mut columns[1], "Samples over 1 px", &p.over_1px.to_string(), &format!("of {}", p.samples), palette.text);
                self.stat_tile(&mut columns[2], "Worst of the big jumps", &format!("{:.2}", p.worst_jump_px), &format!("px, {} jumps", p.jumps), palette.text);
            });
        });
        ui.add_space(CARD_GAP - ui.spacing().item_spacing.y);
        hint(
            ui,
            &format!(
                "Pen {:.2}, {:.2} px, cursor {}, {}, error {:.2} px",
                p.latest.exact.0, p.latest.exact.1, p.latest.cursor.0, p.latest.cursor.1, p.latest.error_px
            ),
            &palette,
        );
    }

    fn color_row(&mut self, ui: &mut egui::Ui, name: &str, field: fn(&mut ThemeOverrides) -> &mut Option<Rgb>, current: Rgb) {
        self.setting_row(ui, name, "", |app, ui| {
            let mut color = [current.0, current.1, current.2];
            if ui.color_edit_button_srgb(&mut color).changed() {
                app.shared.update_settings(|s| *field(&mut s.theme_overrides) = Some(Rgb(color[0], color[1], color[2])));
                app.changed();
            }
            let mut overrides = app.shared.settings().theme_overrides;
            if reset_button(ui, field(&mut overrides).is_some(), &app.palette) {
                app.shared.update_settings(|s| *field(&mut s.theme_overrides) = None);
                app.changed();
            }
        });
    }

    /// A small picture of the theme, which is easier to choose from than its name.
    fn theme_tile(&mut self, ui: &mut egui::Ui, t: &Theme, chosen: bool) {
        let palette = self.palette;
        let (rect, response) = ui.allocate_exact_size(THEME_TILE, egui::Sense::click());
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, chosen, &t.name));
        let picture = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), rect.height() - 24.0));
        let painter = ui.painter();
        painter.rect_filled(picture, 10.0, to_color(t.background));
        let card = picture.shrink(10.0);
        painter.rect_filled(card, 6.0, to_color(t.panel));
        let bar = |y: f32, width: f32, color: Rgb| {
            let bar = egui::Rect::from_min_size(card.min + Vec2::new(8.0, y), Vec2::new(width, 4.0));
            painter.rect_filled(bar, 2.0, to_color(color));
        };
        bar(9.0, card.width() * 0.5, t.text);
        bar(19.0, card.width() * 0.32, t.muted);
        painter.circle_filled(card.max - Vec2::new(11.0, 11.0), 5.0, to_color(t.accent));
        let edge = if chosen {
            Stroke::new(2.0_f32, palette.accent)
        } else if response.hovered() {
            Stroke::new(1.0_f32, palette.muted)
        } else {
            Stroke::new(1.0_f32, palette.line)
        };
        painter.rect_stroke(picture, 10.0, edge, egui::StrokeKind::Outside);
        let name: String = t.name.chars().take(18).collect();
        let family = if chosen { FontFamily::Name(SEMIBOLD.into()) } else { FontFamily::Proportional };
        painter.text(
            Pos2::new(rect.center().x, rect.bottom() - 9.0),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::new(12.5, family),
            if chosen { palette.text } else { palette.muted },
        );
        if response.clicked() {
            self.shared.update_settings(|s| s.choose_theme(&t.name));
            self.changed();
        }
    }

    fn appearance_tab(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        page_header(ui, "Appearance", "Changes show on the tablet straight away.", &palette);
        let settings = self.shared.settings();
        let active = settings.active_theme();
        let mut themes = theme::built_in();
        themes.extend(settings.saved_themes.iter().cloned());

        self.card(ui, |app, ui| {
            section_title(ui, "Theme");
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                for t in &themes {
                    app.theme_tile(ui, t, settings.theme == t.name);
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.theme_name).desired_width(140.0).hint_text("My theme").margin(Vec2::new(8.0, 6.0)));
                if ui.button("Save as my theme").clicked() {
                    let mut outcome = Ok(());
                    let name = app.theme_name.clone();
                    app.shared.update_settings(|s| outcome = s.save_theme(&name));
                    match outcome {
                        Ok(()) => {
                            app.theme_name.clear();
                            app.message.clear();
                            app.changed();
                        }
                        Err(e) => app.message = e,
                    }
                }
                if settings.saved_themes.iter().any(|t| t.name == settings.theme) && ui.button("Delete").clicked() {
                    app.shared.update_settings(|s| {
                        s.delete_saved_theme();
                    });
                    app.changed();
                }
            });
        });

        self.card_pair(ui, "appearance", |app, ui| {
            section_title(ui, "Colors");
            app.color_row(ui, "Background", |o| &mut o.background, active.background);
            app.color_row(ui, "Panels", |o| &mut o.panel, active.panel);
            app.color_row(ui, "Text", |o| &mut o.text, active.text);
            app.color_row(ui, "Quiet text", |o| &mut o.muted, active.muted);
            app.color_row(ui, "Accent", |o| &mut o.accent, active.accent);
        }, |app, ui| {
            section_title(ui, "Pen area on the tablet");
            app.color_row(ui, "Outline color", |o| &mut o.outline, active.outline);
            app.setting_row(ui, "Outline thickness", "", |app, ui| {
                let mut thickness = active.outline_thickness;
                if row_slider(ui, egui::Slider::new(&mut thickness, MIN_THICKNESS..=MAX_THICKNESS).step_by(0.5), &palette).changed() {
                    app.shared.update_settings(|s| s.theme_overrides.outline_thickness = Some(thickness));
                    app.changed();
                }
                if reset_button(ui, settings.theme_overrides.outline_thickness.is_some(), &palette) {
                    app.shared.update_settings(|s| s.theme_overrides.outline_thickness = None);
                    app.changed();
                }
            });
            app.setting_row(ui, "Darkness outside it", "", |app, ui| {
                let mut dim = active.outside_dim;
                if row_slider(ui, egui::Slider::new(&mut dim, 0..=100).suffix("%"), &palette).changed() {
                    app.shared.update_settings(|s| s.theme_overrides.outside_dim = Some(dim));
                    app.changed();
                }
                if reset_button(ui, settings.theme_overrides.outside_dim.is_some(), &palette) {
                    app.shared.update_settings(|s| s.theme_overrides.outside_dim = None);
                    app.changed();
                }
            });
            ui.add_space(4.0);
            ui.vertical_centered(|ui| app.preview(ui, 100.0));
        });

        self.card(ui, |app, ui| {
            app.toggle_row(ui, "Animations", "On both the PC and the tablet. They never touch the pen's path.", settings.animations, true, |s, on| s.animations = on);
            let defaults = settings.theme == theme::DEFAULT_THEME && settings.theme_overrides == ThemeOverrides::default() && settings.animations;
            if !defaults {
                divider(ui);
                if ui.button("Reset appearance").clicked() {
                    app.shared.update_settings(|s| {
                        s.choose_theme(theme::DEFAULT_THEME);
                        s.animations = true;
                    });
                    app.changed();
                }
                hint(ui, "Saved themes stay.", &palette);
            }
        });
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        page_header(ui, "Settings", "Saved as you change them.", &palette);
        let s = self.shared.settings();
        let d = Settings::default();

        self.card(ui, |app, ui| {
            section_title(ui, "Tablet app");
            app.toggle_row(ui, "Size text in the area", "", s.show_area_label, d.show_area_label, |st, on| st.show_area_label = on);
            divider(ui);
            app.setting_row(ui, "Hold to edit", "", |app, ui| {
                let mut hold = s.hold_to_edit_seconds;
                if row_slider(ui, egui::Slider::new(&mut hold, settings::HOLD_SECONDS).step_by(0.1).suffix(" s"), &palette).changed() {
                    app.shared.update_settings(|st| st.hold_to_edit_seconds = hold);
                    app.changed();
                }
                if reset_button(ui, s.hold_to_edit_seconds != d.hold_to_edit_seconds, &palette) {
                    app.shared.update_settings(|st| st.hold_to_edit_seconds = d.hold_to_edit_seconds);
                    app.changed();
                }
            });
            divider(ui);
            app.setting_row(ui, "Edit mode ends after", "", |app, ui| {
                let mut timeout = s.edit_timeout_seconds;
                if row_slider(ui, egui::Slider::new(&mut timeout, settings::EDIT_TIMEOUT_SECONDS).step_by(1.0).suffix(" s"), &palette).changed() {
                    app.shared.update_settings(|st| st.edit_timeout_seconds = timeout);
                    app.changed();
                }
                if reset_button(ui, s.edit_timeout_seconds != d.edit_timeout_seconds, &palette) {
                    app.shared.update_settings(|st| st.edit_timeout_seconds = d.edit_timeout_seconds);
                    app.changed();
                }
            });
            divider(ui);
            app.toggle_row(ui, "Open the app on connect", NEXT_CONNECT, s.open_tablet_app, d.open_tablet_app, |st, on| st.open_tablet_app = on);
            divider(ui);
            app.toggle_row(ui, "Pin the app", "Blocks Home and Back. Applies the next time the tablet connects.", s.screen_pinning, d.screen_pinning, |st, on| st.screen_pinning = on);
            if s.screen_pinning {
                ui.colored_label(palette.warning, "When osutab quits, the tablet goes to its lock screen, so you'll need your PIN. If the cable is pulled while pinned, the app stays pinned until you hold Back and Recents.");
            }
        });

        self.card(ui, |app, ui| {
            section_title(ui, "This program");
            app.toggle_row(ui, "Keep running in the tray when closed", "", s.close_to_tray, d.close_to_tray, |st, on| st.close_to_tray = on);
            divider(ui);
            app.toggle_row(ui, "Start in the tray", "Applies the next time osutab starts.", s.start_minimized, d.start_minimized, |st, on| st.start_minimized = on);
        });

        self.card(ui, |app, ui| {
            let active = app.shared.direct_active.load(Ordering::SeqCst);
            let now = match (active, app.shared.usb3_link.load(Ordering::SeqCst)) {
                (true, true) => "Now: direct USB, on a USB 3 link",
                (true, false) => "Now: direct USB, on a USB 2 link",
                (false, _) => "Now: adb.exe",
            };
            app.setting_row(ui, "USB path", now, |app, ui| {
                let mut path = s.usb_path;
                if segmented(ui, &mut path, &[(UsbPath::Adb, "adb.exe"), (UsbPath::Direct, "Direct USB")], &palette) {
                    app.shared.update_settings(|st| st.usb_path = path);
                    app.changed();
                }
                if reset_button(ui, s.usb_path != d.usb_path, &palette) {
                    app.shared.update_settings(|st| st.usb_path = d.usb_path);
                    app.changed();
                }
            });
            divider(ui);
            hint(ui, "Direct USB skips adb.exe. In testing it was slightly faster than adb.exe, and steadier when the CPU was busy. It stops adb.exe while osutab runs, so anything else using adb on this PC, such as Android Studio, loses the tablet. Applies the next time the tablet connects.", &palette);
        });
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
        let settings = self.shared.settings();
        self.apply_appearance(ctx, &settings);
        let has_tray = self.shared.tray_thread.load(Ordering::SeqCst) != 0;
        if ctx.input(|i| i.viewport().close_requested()) {
            if has_tray && settings.close_to_tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_window();
            } else {
                self.shared.stats.stop.store(true, Ordering::SeqCst);
            }
        }
        // Waits for the tray icon, or the window could vanish with no way back to it.
        let setup_done = self.shared.setup_done.load(Ordering::SeqCst);
        if self.hide_at_start && has_tray && setup_done {
            self.hide_at_start = false;
            self.hide_window();
        }
        if settings.active != self.last_preset {
            self.last_preset = settings.active;
            self.glide_until = Instant::now() + GLIDE_WINDOW;
        }
        // Enough for the numbers, without spending CPU and GPU the game could use.
        ctx.request_repaint_after(Duration::from_millis(100));
        self.save_if_due();
        self.enforce_precision_limit();
        self.refresh_checks();

        if !self.message.is_empty() {
            let bar = egui::Frame::new().fill(self.palette.panel).inner_margin(egui::Margin::symmetric(16, 6));
            egui::TopBottomPanel::bottom("message").show_separator_line(false).frame(bar).show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(self.palette.warning, &self.message);
                    if ui.small_button("Dismiss").clicked() {
                        self.message.clear();
                    }
                });
                ui.add_space(4.0);
            });
        }
        if !setup_done {
            let surface = egui::Frame::new().fill(self.palette.area).inner_margin(egui::Margin::symmetric(0, 16));
            egui::CentralPanel::default().frame(surface).show(ctx, |ui| {
                page_scroll(ui, SETUP_WIDTH, |ui| self.setup_screen(ui));
            });
            return;
        }
        let surface = egui::Frame::new().fill(self.palette.area);
        egui::SidePanel::left("navigation")
            .exact_width(SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(surface.inner_margin(egui::Margin::symmetric(14, 16)))
            .show(ctx, |ui| self.sidebar(ui));
        let fade_time = self.seconds(TAB_FADE);
        let opacity = if fade_time > 0.0 { (self.tab_changed.elapsed().as_secs_f32() / fade_time).min(1.0) } else { 1.0 };
        if opacity < 1.0 {
            ctx.request_repaint();
        }
        egui::CentralPanel::default().frame(surface.inner_margin(egui::Margin::symmetric(0, 16))).show(ctx, |ui| {
            ui.multiply_opacity(opacity);
            page_scroll(ui, PAGE_WIDTH, |ui| match self.tab {
                Tab::Play => self.play_tab(ui),
                Tab::Area => self.area_tab(ui),
                Tab::Latency => self.latency_tab(ui),
                Tab::Precision => self.precision_tab(ui),
                Tab::Appearance => self.appearance_tab(ui),
                Tab::Settings => self.settings_tab(ui),
            });
        });
    }
}

/// A scrolling page in a centred column, with the scroll bar at the window's edge.
/// A big number with its unit. It has a set height, as a page that scrolls would otherwise let it
/// fill the page.
fn stat_value(ui: &mut egui::Ui, value: &str, unit: &str, color: Color32, muted: Color32) {
    let room = Vec2::new(ui.available_width(), 36.0);
    ui.allocate_ui_with_layout(room, egui::Layout::left_to_right(egui::Align::Max), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(strong(value).size(28.0).color(color));
        ui.label(RichText::new(unit).color(muted));
    });
}

fn page_scroll(ui: &mut egui::Ui, max_width: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let side = 24.0;
        let room = ui.available_width() - side * 2.0;
        let width = room.min(max_width);
        ui.horizontal_top(|ui| {
            ui.add_space(side + (room - width) / 2.0);
            ui.vertical(|ui| {
                ui.set_width(width);
                add(ui);
                ui.add_space(8.0);
            });
        });
    });
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
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([760.0, 520.0])
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
            cc.egui_ctx.set_fonts(font_definitions());
            let settings = shared.settings();
            let message = shared.notice.lock().unwrap().take().unwrap_or_default();
            let mut app = App {
                shared,
                tab,
                preset_name: String::new(),
                message,
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
                palette: Palette::of(&settings.active_theme()),
                applied: None,
                animations: settings.animations,
                tab_changed: Instant::now(),
                glide_until: Instant::now(),
                last_preset: settings.active,
                theme_name: String::new(),
                setup_step: 0,
                hide_at_start: settings.start_minimized,
                show_passed: false,
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
    fn every_built_in_theme_has_readable_status_colors() {
        for t in theme::built_in() {
            let palette = Palette::of(&t);
            assert_ne!(palette.good, palette.bad, "{}", t.name);
            let _ = visuals_for(&t);
        }
    }

    /// Runs a real click through egui with no window: draws once to learn where the widget is,
    /// then moves the pointer there, presses and releases.
    fn click_at(position: impl Fn(egui::Rect) -> Pos2, draw: &mut dyn FnMut(&mut egui::Ui) -> egui::Rect) {
        let ctx = egui::Context::default();
        ctx.set_fonts(font_definitions());
        let mut frame = |events: Vec<egui::Event>| -> egui::Rect {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events,
                ..Default::default()
            };
            let mut rect = egui::Rect::NOTHING;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| rect = draw(ui));
            });
            rect
        };
        let pos = position(frame(Vec::new()));
        frame(vec![egui::Event::PointerMoved(pos)]);
        let press = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(vec![press(true)]);
        frame(vec![press(false)]);
    }

    #[test]
    fn a_click_flips_a_toggle_and_reports_it() {
        let (mut on, mut changed) = (false, false);
        click_at(|rect| rect.center(), &mut |ui| {
            let response = toggle(ui, &mut on, Color32::RED);
            changed |= response.changed();
            response.rect
        });
        assert!(on);
        assert!(changed);
    }

    #[test]
    fn a_click_chooses_the_option_under_it_in_either_direction() {
        let palette = Palette::of(&theme::built_in()[0]);
        for layout in [egui::Layout::left_to_right(egui::Align::Min), egui::Layout::right_to_left(egui::Align::Min)] {
            let (mut value, mut changed) = (0, false);
            // The options are read left to right wherever they sit, so the last is on the right.
            click_at(|rect| Pos2::new(rect.right() - 6.0, rect.center().y), &mut |ui| {
                ui.with_layout(layout, |ui| {
                    changed |= segmented(ui, &mut value, &[(0, "First"), (1, "Second")], &palette);
                    ui.min_rect()
                })
                .inner
            });
            assert_eq!(value, 1, "{layout:?}");
            assert!(changed);
        }
    }

    #[test]
    fn reset_only_shows_when_there_is_something_to_reset() {
        let palette = Palette::of(&theme::built_in()[0]);
        let mut clicked = false;
        click_at(|rect| rect.center(), &mut |ui| {
            ui.vertical(|ui| {
                clicked |= reset_button(ui, true, &palette);
                ui.min_rect()
            })
            .inner
        });
        assert!(clicked);

        let mut clicked = false;
        let mut size = Vec2::ZERO;
        click_at(|rect| rect.center(), &mut |ui| {
            ui.vertical(|ui| {
                clicked |= reset_button(ui, false, &palette);
                size = ui.min_rect().size();
                ui.min_rect()
            })
            .inner
        });
        assert!(!clicked);
        assert_eq!(size, Vec2::ZERO);
    }

    #[test]
    fn only_themes_with_matching_shades_get_outlines() {
        for t in theme::built_in() {
            assert_eq!(needs_edges(&t), t.name == "High Contrast", "{}", t.name);
        }
    }

    #[test]
    fn a_big_number_stays_small_on_a_page_that_scrolls() {
        let ctx = egui::Context::default();
        ctx.set_fonts(font_definitions());
        let mut height = 0.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                page_scroll(ui, 700.0, |ui| {
                    height = ui.vertical(|ui| stat_value(ui, "0.55", "ms", Color32::WHITE, Color32::GRAY)).response.rect.height();
                });
            });
        });
        assert!(height > 0.0 && height < 60.0, "height {height}");
    }

    #[test]
    fn text_uses_the_built_in_fonts() {
        let fonts = font_definitions();
        assert_eq!(fonts.families[&FontFamily::Proportional][0], "geist");
        assert_eq!(fonts.families[&FontFamily::Name(SEMIBOLD.into())][0], "geist-semibold");
        // egui stops the program on a file it can't read, so this proves both load.
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Ready to play 0.54 ms");
                ui.label(strong("Ready to play 0.54 ms"));
            });
        });
    }

    #[test]
    fn tab_names_from_the_command_line() {
        assert_eq!(Tab::from_name("area"), Some(Tab::Area));
        assert_eq!(Tab::from_name("appearance"), Some(Tab::Appearance));
        assert_eq!(Tab::from_name("settings"), Some(Tab::Settings));
        assert_eq!(Tab::from_name("nope"), None);
    }
}
