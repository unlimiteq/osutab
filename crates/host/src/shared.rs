//! State shared between the input thread, the report thread and the settings window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::adb::DeviceState;
use crate::mapping::{ActiveArea, Rect, Rotation};
use crate::otd::Relay;
use crate::settings::{self, Output, Settings};
use crate::stats::Stats;

/// What the input thread applies to each pen report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveConfig {
    pub output: Output,
    /// The screen the pen area maps onto, and the whole desktop around it.
    pub target: Rect,
    pub desktop: Rect,
    pub rotation: Rotation,
    pub area: ActiveArea,
    pub tip_click: bool,
    pub button_scan: Option<u16>,
}

/// Command-line choices that win over saved settings for this run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Overrides {
    pub rotation: Option<Rotation>,
    pub tip_click: Option<bool>,
    pub button_scan: Option<u16>,
}

pub struct Shared {
    pub stats: Stats,
    pub relay: Relay,
    /// The OpenTabletDriver daemon this program started, to stop when it quits.
    pub otd_daemon: Mutex<Option<std::process::Child>>,
    pub clock: Instant,
    pub connected: AtomicBool,
    pub app_connected: AtomicBool,
    pub test_mode: AtomicBool,
    /// The engine waits on this while the setup screen is showing.
    pub setup_done: AtomicBool,
    /// The settings window's handle, once it exists.
    pub window: AtomicIsize,
    /// The tray thread's id while its icon exists. Closing the window hides it only then.
    pub tray_thread: AtomicU32,
    /// True while the pen is coming over direct USB rather than through adb.exe.
    pub direct_active: AtomicBool,
    /// Whether the direct USB link is running at USB 3 speed.
    pub usb3_link: AtomicBool,
    /// True once this program has pinned the tablet app.
    pub pinned: AtomicBool,
    /// Something to tell the person, such as a settings file that had problems.
    pub notice: Mutex<Option<String>>,
    /// Why direct USB isn't in use, when it was asked for.
    pub direct_problem: Mutex<Option<String>>,
    device_state: AtomicU8,
    pen_u: AtomicU32,
    pen_v: AtomicU32,
    pen_flags: AtomicU8,
    generation: AtomicU64,
    live: Mutex<LiveConfig>,
    settings: Mutex<Settings>,
    screen_rotation: Mutex<Rotation>,
    display: Mutex<(Rect, Rect)>,
    overrides: Overrides,
    settings_path: PathBuf,
    // Keeps two saves from writing at once. Taken before the settings, so a slow disk can't hold
    // up whoever's changing them.
    save_lock: Mutex<()>,
}

fn build_live(settings: &Settings, overrides: &Overrides, screen_rotation: Rotation, display: (Rect, Rect)) -> LiveConfig {
    let rotation = overrides.rotation.or(settings.rotation).unwrap_or(screen_rotation);
    LiveConfig {
        output: settings.output,
        target: display.0,
        desktop: display.1,
        rotation,
        area: settings.area().resolve(rotation),
        tip_click: overrides.tip_click.unwrap_or(settings.tip_click),
        button_scan: overrides.button_scan.or_else(|| crate::keys::code_for_name(&settings.button_key)),
    }
}

impl Shared {
    pub fn new(
        settings: Settings,
        settings_path: PathBuf,
        overrides: Overrides,
        target: Rect,
        setup_done: bool,
    ) -> Self {
        let screen_rotation = Rotation::R90;
        let display = (target, target);
        let live = build_live(&settings, &overrides, screen_rotation, display);
        let clock = Instant::now();
        Shared {
            stats: Stats::new(clock),
            relay: Relay::default(),
            otd_daemon: Mutex::new(None),
            clock,
            connected: AtomicBool::new(false),
            app_connected: AtomicBool::new(false),
            test_mode: AtomicBool::new(false),
            setup_done: AtomicBool::new(setup_done),
            window: AtomicIsize::new(0),
            tray_thread: AtomicU32::new(0),
            direct_active: AtomicBool::new(false),
            usb3_link: AtomicBool::new(false),
            pinned: AtomicBool::new(false),
            notice: Mutex::new(None),
            direct_problem: Mutex::new(None),
            device_state: AtomicU8::new(DeviceState::NoDevice as u8),
            pen_u: AtomicU32::new(0),
            pen_v: AtomicU32::new(0),
            pen_flags: AtomicU8::new(0),
            generation: AtomicU64::new(0),
            live: Mutex::new(live),
            settings: Mutex::new(settings),
            screen_rotation: Mutex::new(screen_rotation),
            display: Mutex::new(display),
            overrides,
            settings_path,
            save_lock: Mutex::new(()),
        }
    }

    pub fn device_state(&self) -> DeviceState {
        match self.device_state.load(Ordering::Relaxed) {
            x if x == DeviceState::AdbMissing as u8 => DeviceState::AdbMissing,
            x if x == DeviceState::Unauthorized as u8 => DeviceState::Unauthorized,
            x if x == DeviceState::Offline as u8 => DeviceState::Offline,
            x if x == DeviceState::Ready as u8 => DeviceState::Ready,
            _ => DeviceState::NoDevice,
        }
    }

    pub fn set_device_state(&self, state: DeviceState) {
        self.device_state.store(state as u8, Ordering::Relaxed);
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn settings_path(&self) -> &PathBuf {
        &self.settings_path
    }

    /// Only tells the input thread about changes it cares about, so dragging a color slider costs
    /// the pen nothing.
    fn rebuild(&self, settings: &Settings) {
        let screen_rotation = *self.screen_rotation.lock().unwrap();
        let display = *self.display.lock().unwrap();
        let wanted = build_live(settings, &self.overrides, screen_rotation, display);
        let mut live = self.live.lock().unwrap();
        if *live != wanted {
            *live = wanted;
            self.generation.fetch_add(1, Ordering::Release);
        }
    }

    /// True when the pen's orientation comes from the tablet's screen.
    pub fn follows_screen(&self) -> bool {
        self.overrides.rotation.is_none() && self.settings.lock().unwrap().rotation.is_none()
    }

    /// The screen the pen area maps onto.
    pub fn target(&self) -> Rect {
        self.live.lock().unwrap().target
    }

    pub fn set_display(&self, target: Rect, desktop: Rect) {
        let mut display = self.display.lock().unwrap();
        if *display != (target, desktop) {
            *display = (target, desktop);
            drop(display);
            self.rebuild(&self.settings.lock().unwrap());
        }
    }

    /// Changes settings. The input thread picks the change up on its next pen report.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) {
        let mut settings = self.settings.lock().unwrap();
        change(&mut settings);
        self.rebuild(&settings);
    }

    pub fn set_screen_rotation(&self, rotation: Rotation) {
        *self.screen_rotation.lock().unwrap() = rotation;
        self.rebuild(&self.settings.lock().unwrap());
    }

    pub fn save_settings(&self) -> std::io::Result<()> {
        let _saving = self.save_lock.lock().unwrap();
        let settings = self.settings();
        settings::save(&self.settings_path, &settings)
    }

    pub fn live_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn live_config(&self) -> LiveConfig {
        *self.live.lock().unwrap()
    }

    pub fn store_pen(&self, u: f64, v: f64, flags: u8) {
        self.pen_u.store((u as f32).to_bits(), Ordering::Relaxed);
        self.pen_v.store((v as f32).to_bits(), Ordering::Relaxed);
        self.pen_flags.store(flags, Ordering::Relaxed);
    }

    /// The pen's latest position on the whole tablet, in 0..1, and its flags.
    pub fn pen(&self) -> (f32, f32, u8) {
        (
            f32::from_bits(self.pen_u.load(Ordering::Relaxed)),
            f32::from_bits(self.pen_v.load(Ordering::Relaxed)),
            self.pen_flags.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::TABLET_LONG_MM;

    fn shared(settings: Settings, overrides: Overrides) -> Shared {
        let target = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        Shared::new(settings, PathBuf::new(), overrides, target, true)
    }

    #[test]
    fn area_changes_reach_the_input_thread_with_a_new_generation() {
        let s = shared(Settings { rotation: Some(Rotation::R90), ..Settings::default() }, Overrides::default());
        let before = s.live_generation();
        assert_eq!(s.live_config().area.width_mm, TABLET_LONG_MM);
        s.update_settings(|st| st.area_mut().width = 100.0);
        assert_ne!(s.live_generation(), before);
        assert_eq!(s.live_config().area.width_mm, 100.0);
    }

    #[test]
    fn changes_the_input_thread_ignores_dont_disturb_it() {
        let s = shared(Settings::default(), Overrides::default());
        let before = s.live_generation();
        s.update_settings(|st| {
            st.choose_theme("Daylight");
            st.animations = false;
        });
        assert_eq!(s.live_generation(), before);
    }

    #[test]
    fn a_new_display_reaches_the_input_thread() {
        let s = shared(Settings::default(), Overrides::default());
        let before = s.live_generation();
        let new = Rect { left: 0.0, top: 0.0, width: 1920.0, height: 1080.0 };
        let desktop = Rect { left: -1920.0, top: 0.0, width: 3840.0, height: 1080.0 };
        s.set_display(new, desktop);
        assert_ne!(s.live_generation(), before);
        assert_eq!(s.target(), new);
        assert_eq!(s.live_config().desktop, desktop);
        let after = s.live_generation();
        s.set_display(new, desktop);
        assert_eq!(s.live_generation(), after, "the same display changes nothing");
    }

    #[test]
    fn switching_preset_applies_its_area() {
        let s = shared(Settings { rotation: Some(Rotation::R90), ..Settings::default() }, Overrides::default());
        s.update_settings(|st| {
            st.area_mut().width = 100.0;
            st.save_preset("Small").unwrap();
            st.active = 0;
            st.area_mut().width = 200.0;
        });
        assert_eq!(s.live_config().area.width_mm, 200.0);
        s.update_settings(|st| st.active = 1);
        assert_eq!(s.live_config().area.width_mm, 100.0);
    }

    #[test]
    fn overrides_win_and_auto_follows_the_screen() {
        let s = shared(Settings::default(), Overrides::default());
        s.set_screen_rotation(Rotation::R270);
        assert_eq!(s.live_config().rotation, Rotation::R270);

        let forced = shared(
            Settings { rotation: Some(Rotation::R90), ..Settings::default() },
            Overrides { rotation: Some(Rotation::R0), tip_click: Some(false), button_scan: Some(0x2c) },
        );
        let live = forced.live_config();
        assert_eq!((live.rotation, live.tip_click, live.button_scan), (Rotation::R0, false, Some(0x2c)));
    }

    #[test]
    fn pen_position_round_trips() {
        let s = shared(Settings::default(), Overrides::default());
        s.store_pen(0.25, 0.75, 3);
        assert_eq!(s.pen(), (0.25, 0.75, 3));
    }

    #[test]
    fn device_state_round_trips() {
        let s = shared(Settings::default(), Overrides::default());
        for state in [DeviceState::AdbMissing, DeviceState::NoDevice, DeviceState::Unauthorized, DeviceState::Offline, DeviceState::Ready] {
            s.set_device_state(state);
            assert_eq!(s.device_state(), state);
        }
    }
}
