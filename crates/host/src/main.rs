//! Windows host: starts the tablet helper over adb, turns pen samples into input and reports
//! measured latency. Stays running across unplug and replug.
//!
//! It's a windowed program, so starting it opens no console. When started from a terminal it
//! attaches to that terminal, so its messages still show there.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod adb;
mod assets;
mod control;
mod engine;
mod gui;
mod icon;
mod inject;
mod lazer;
mod mapping;
mod otd;
mod preflight;
mod selftest;
mod settings;
mod shared;
mod stats;
mod theme;
mod trace;
mod tray;
mod tuning;
mod usbadb;

use std::fs::{self, File};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Console::{AttachConsole, SetConsoleCtrlHandler, ATTACH_PARENT_PROCESS};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

use adb::Adb;
use engine::EngineOptions;
use mapping::{Rect, Rotation};
use settings::{Settings, UsbPath};
use shared::{Overrides, Shared};

// Lets the console handler reach the stop flag.
static SHARED: AtomicPtr<Shared> = AtomicPtr::new(std::ptr::null_mut());
const KEPT_LOGS: usize = 30;
const ADB_SERVER_PORT: u16 = 5037;
const DISPLAY_CHECK: Duration = Duration::from_secs(1);
// How long quitting waits for the direct USB link to say goodbye, so the pen comes straight back.
const DIRECT_CLOSE_WAIT: Duration = Duration::from_millis(400);

struct Config {
    adb: Option<PathBuf>,
    helper: Option<PathBuf>,
    overrides: Overrides,
    duration: Option<Duration>,
    grab: bool,
    dry_run: bool,
    setup: bool,
    no_gui: bool,
    self_test: bool,
    test_mode: bool,
    busy_poll: bool,
    helper_busy: bool,
    ping_bench: bool,
    no_app: bool,
    stall_trace: Option<PathBuf>,
    ping_ms: Option<u64>,
    burst: Option<(u64, u64)>,
    sendinput_bench: Option<u64>,
    otd_bench: Option<u64>,
    usb_bench: Option<u64>,
    usb_path: Option<UsbPath>,
    no_tuning: bool,
    cpu_burn: Option<u64>,
    tab: gui::Tab,
}

const USAGE: &str = "osutab [--setup] [--no-gui] [--no-app] [--self-test] [--test-mode] [--busy-poll] [--helper-busy] [--ping-bench]
       [--stall-trace DIR] [--ping-ms MS] [--burst ON_MS OFF_MS] [--sendinput-bench SECONDS] [--tab NAME]
       [--rotation 0|90|180|270] [--duration SECONDS] [--no-tip-click] [--button-key KEY]
       [--no-grab] [--dry-run] [--adb PATH] [--helper PATH] [--usb-path adb|direct] [--no-tuning] [--cpu-burn SECONDS]";

fn parse_args() -> Result<Config, String> {
    let mut c = Config {
        adb: None,
        helper: None,
        overrides: Overrides::default(),
        duration: None,
        grab: true,
        dry_run: false,
        setup: false,
        no_gui: false,
        self_test: false,
        test_mode: false,
        busy_poll: false,
        helper_busy: false,
        ping_bench: false,
        no_app: false,
        stall_trace: None,
        ping_ms: None,
        burst: None,
        sendinput_bench: None,
        otd_bench: None,
        usb_bench: None,
        usb_path: None,
        no_tuning: false,
        cpu_burn: None,
        tab: gui::Tab::Play,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--adb" => c.adb = Some(value()?.into()),
            "--helper" => c.helper = Some(value()?.into()),
            "--rotation" => {
                c.overrides.rotation = Some(match value()?.as_str() {
                    "0" => Rotation::R0,
                    "90" => Rotation::R90,
                    "180" => Rotation::R180,
                    "270" => Rotation::R270,
                    other => return Err(format!("invalid rotation {other}")),
                })
            }
            "--duration" => {
                let secs: u64 = value()?.parse().map_err(|_| "invalid duration".to_string())?;
                c.duration = Some(Duration::from_secs(secs));
            }
            "--button-key" => {
                let name = value()?;
                c.overrides.button_scan = Some(inject::scan_code_for_key(&name).ok_or(format!("unsupported key {name}"))?);
            }
            "--tab" => {
                let name = value()?;
                c.tab = gui::Tab::from_name(&name).ok_or(format!("unknown tab {name}"))?;
            }
            "--no-tip-click" => c.overrides.tip_click = Some(false),
            "--no-grab" => c.grab = false,
            "--dry-run" => c.dry_run = true,
            "--setup" => c.setup = true,
            "--no-gui" => c.no_gui = true,
            "--self-test" => c.self_test = true,
            "--test-mode" => c.test_mode = true,
            "--busy-poll" => c.busy_poll = true,
            "--helper-busy" => c.helper_busy = true,
            "--ping-bench" => c.ping_bench = true,
            "--no-app" => c.no_app = true,
            "--sendinput-bench" => {
                c.sendinput_bench = Some(value()?.parse().map_err(|_| "invalid --sendinput-bench".to_string())?)
            }
            "--otd-bench" => c.otd_bench = Some(value()?.parse().map_err(|_| "invalid --otd-bench".to_string())?),
            "--usb-bench" => c.usb_bench = Some(value()?.parse().map_err(|_| "invalid --usb-bench".to_string())?),
            "--usb-path" => {
                let name = value()?;
                c.usb_path = Some(UsbPath::parse(&name).ok_or(format!("unknown USB path {name}"))?);
            }
            "--no-tuning" => c.no_tuning = true,
            "--cpu-burn" => c.cpu_burn = Some(value()?.parse().map_err(|_| "invalid --cpu-burn".to_string())?),
            "--stall-trace" => c.stall_trace = Some(value()?.into()),
            "--ping-ms" => c.ping_ms = Some(value()?.parse().map_err(|_| "invalid --ping-ms".to_string())?),
            "--burst" => {
                let on = value()?.parse().map_err(|_| "invalid --burst".to_string())?;
                let off = value()?.parse().map_err(|_| "invalid --burst".to_string())?;
                c.burst = Some((on, off));
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(c)
}

fn find_adb(config: &Config, exe_dir: &Path) -> PathBuf {
    if let Some(p) = &config.adb {
        return p.clone();
    }
    let candidates = [
        exe_dir.join("platform-tools").join("adb.exe"),
        exe_dir.join("../../tools/platform-tools/adb.exe"),
    ];
    candidates.into_iter().find(|p| p.exists()).unwrap_or_else(|| "adb".into())
}

fn find_helper(config: &Config, exe_dir: &Path, data_dir: &Path) -> Option<PathBuf> {
    if let Some(p) = &config.helper {
        return Some(p.clone());
    }
    assets::helper(data_dir).or_else(|| {
        let candidates = [
            exe_dir.join("osutab-helper"),
            exe_dir.join("../aarch64-unknown-linux-musl/release/osutab-helper"),
        ];
        candidates.into_iter().find(|p| p.exists())
    })
}

fn find_app(exe_dir: &Path, data_dir: &Path) -> Option<PathBuf> {
    assets::app(data_dir).or_else(|| {
        let candidates = [
            exe_dir.join("osutab.apk"),
            exe_dir.join("../../android/app/build/outputs/apk/debug/app-debug.apk"),
        ];
        candidates.into_iter().find(|p| p.exists())
    })
}

/// Reads the saved settings. A file with problems is copied first, because saving replaces it, and
/// the returned message says what happened.
fn read_saved_settings(path: &Path) -> (Option<Settings>, Option<String>) {
    let copy_note = || match settings::back_up(path) {
        Ok(copy) => format!("A copy of the old file is at {}.", copy.display()),
        Err(e) => format!("A copy of the old file couldn't be made: {e}."),
    };
    match settings::load(path) {
        Ok(None) => (None, None),
        Ok(Some(parsed)) if parsed.problems.is_empty() => (Some(parsed.settings), None),
        Ok(Some(parsed)) => {
            let note = format!("Some settings couldn't be read, so they went back to their defaults: {}. {}", parsed.problems.join("; "), copy_note());
            (Some(parsed.settings), Some(note))
        }
        Err(e) => (None, Some(format!("The settings file couldn't be read ({e}), so setup is starting again. {}", copy_note()))),
    }
}

/// The saved settings, and whether they are ready to use without asking first.
fn load_settings(config: &Config, target: &Rect, path: &Path, saved: Option<Settings>) -> (Settings, bool) {
    if !config.setup {
        if let Some(saved) = saved {
            println!("using settings from {}", path.display());
            return (saved, true);
        }
    }
    if !config.no_gui {
        return (saved.unwrap_or_else(Settings::first_run), false);
    }
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        println!("no saved settings and nobody to ask: using the full tablet, following its screen rotation");
        return (saved.unwrap_or_default(), true);
    }
    match settings::ask(&mut stdin.lock(), &mut io::stdout(), target.width / target.height) {
        Ok(new) => {
            match settings::save(path, &new) {
                Ok(()) => println!("saved to {}", path.display()),
                Err(e) => eprintln!("could not save {}: {e}", path.display()),
            }
            (new, true)
        }
        Err(e) => {
            eprintln!("setup stopped: {e}");
            std::process::exit(2);
        }
    }
}

unsafe extern "system" fn on_console_signal(_signal: u32) -> windows_sys::core::BOOL {
    let shared = SHARED.load(Ordering::SeqCst);
    if !shared.is_null() {
        (*shared).stats.stop.store(true, Ordering::SeqCst);
    }
    inject::release_all();
    1
}

/// Removes the oldest latency logs beyond `keep`, so they don't pile up forever.
fn prune_logs(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("latency-") && name.ends_with(".csv")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    logs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in logs.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
}

fn open_log(dir: &Path) -> Option<File> {
    fs::create_dir_all(dir).ok()?;
    prune_logs(dir, KEPT_LOGS.saturating_sub(1));
    let name = format!("latency-{}.csv", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs());
    let log_path = dir.join(name);
    match File::create(&log_path) {
        Ok(file) => {
            println!("latency log: {}", log_path.display());
            Some(file)
        }
        Err(e) => {
            eprintln!("could not create latency log: {e}");
            None
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Shows an error someone can see even with no console.
fn fatal(message: &str) -> ! {
    eprintln!("{message}");
    let (text, title) = (wide(message), wide("osutab"));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    std::process::exit(2);
}

/// Whether an adb server is listening on its standard port.
fn adb_server_running() -> bool {
    let address = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, ADB_SERVER_PORT));
    std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

/// True if this is the only running copy. Two copies would fight over the tablet and the pen.
fn is_first_instance() -> bool {
    let name = wide("Local\\osutab-single-instance");
    // The handle stays open for the life of the process on purpose.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    !handle.is_null() && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
}

fn main() {
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    let config = match parse_args() {
        Ok(c) => c,
        Err(e) => fatal(&e),
    };
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let target = inject::primary_display();

    if let Some(seconds) = config.cpu_burn {
        tuning::cpu_burn(seconds);
        return;
    }
    if config.self_test {
        std::process::exit(if selftest::run(target) { 0 } else { 1 });
    }
    if let Some(seconds) = config.sendinput_bench {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
        selftest::sendinput_bench(seconds);
        return;
    }
    if let Some(seconds) = config.otd_bench {
        selftest::otd_bench(seconds, target);
        return;
    }
    if let Some(seconds) = config.usb_bench {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
        if let Err(e) = usbadb::bench(seconds) {
            eprintln!("direct USB failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    if !is_first_instance() {
        gui::show_existing_window();
        println!("osutab is already running");
        return;
    }

    let path = settings::default_path();
    let data_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let adb = Adb::new(find_adb(&config, &exe_dir));
    let adb_at_exit = adb.clone();
    // A server that was already running belongs to something else, so it stays running at exit.
    let adb_server_was_running = adb_server_running();
    let Some(helper) = find_helper(&config, &exe_dir, &data_dir) else {
        fatal("The tablet helper is missing from this build. Run build.ps1 to rebuild osutab.");
    };

    let (saved, settings_note) = read_saved_settings(&path);
    if let Some(note) = &settings_note {
        eprintln!("{note}");
    }
    let (loaded, ready) = load_settings(&config, &target, &path, saved);
    let shared = Arc::new(Shared::new(loaded, path.clone(), config.overrides, target, ready));
    *shared.notice.lock().unwrap() = settings_note;
    shared.set_display(target, inject::virtual_desktop());
    shared.test_mode.store(config.test_mode, Ordering::SeqCst);
    SHARED.store(Arc::into_raw(shared.clone()) as *mut Shared, Ordering::SeqCst);
    unsafe { SetConsoleCtrlHandler(Some(on_console_signal), 1) };

    println!(
        "target {}x{} real pixels | grab {} | dry run {} | test mode {} | busy poll {} | helper busy {}",
        target.width, target.height, config.grab, config.dry_run, config.test_mode, config.busy_poll, config.helper_busy
    );

    let options = EngineOptions {
        adb,
        helper,
        grab: config.grab,
        dry_run: config.dry_run,
        busy_poll: config.busy_poll,
        helper_busy: config.helper_busy,
        ping_bench: config.ping_bench,
        app: if config.no_app { None } else { find_app(&exe_dir, &data_dir) },
        stall_trace: config.stall_trace.clone(),
        ping_ms: config.ping_ms,
        burst: config.burst,
        usb_path: config.usb_path,
        tuning: !config.no_tuning,
    };
    if !config.no_tuning {
        tuning::apply_to_this_program();
    }
    if options.app.is_none() && !config.no_app {
        println!("tablet app not found; run build.ps1 to build it");
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || control::run(shared));
    }
    {
        // A changed resolution or main monitor would otherwise leave the mapping out of date.
        let shared = shared.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(DISPLAY_CHECK);
            shared.set_display(inject::primary_display(), inject::virtual_desktop());
        });
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || shared.relay.serve());
    }
    {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || {
                unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
                engine::run(options, shared);
            })
            .expect("could not start input thread");
    }

    if let Some(duration) = config.duration {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut end = None;
            while !shared.stats.stop.load(Ordering::SeqCst) {
                // The clock starts once setup is finished.
                if shared.setup_done.load(Ordering::SeqCst) {
                    let finish = *end.get_or_insert_with(|| Instant::now() + duration);
                    if Instant::now() >= finish {
                        shared.stats.stop.store(true, Ordering::SeqCst);
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }

    {
        let shared = shared.clone();
        let log = open_log(&data_dir);
        std::thread::spawn(move || {
            shared.stats.report_loop(log);
            inject::release_all();
            let _ = shared.save_settings();
            let closing = Instant::now();
            while shared.direct_active.load(Ordering::SeqCst) && closing.elapsed() < DIRECT_CLOSE_WAIT {
                std::thread::sleep(Duration::from_millis(10));
            }
            if shared.pinned.load(Ordering::SeqCst) {
                let _ = adb_at_exit.unpin_app();
            }
            if !adb_server_was_running {
                let _ = adb_at_exit.kill_server();
            }
            if let Some(mut daemon) = shared.otd_daemon.lock().unwrap().take() {
                let _ = daemon.kill();
            }
            tray::stop(&shared);
            std::process::exit(0);
        });
    }

    if config.no_gui {
        loop {
            std::thread::park();
        }
    }
    tray::start(shared.clone());
    if let Err(e) = gui::run(shared.clone(), config.tab, exe_dir) {
        eprintln!("settings window failed: {e}");
    }
    // Quitting stops everything. The report thread saves, prints the summary and exits.
    shared.stats.stop.store(true, Ordering::SeqCst);
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_only_the_newest_latency_logs() {
        let dir = std::env::temp_dir().join(format!("osutab-test-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            fs::write(dir.join(format!("latency-{i}.csv")), "x").unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::write(dir.join("config.toml"), "keep me").unwrap();
        fs::write(dir.join("other.csv"), "keep me").unwrap();

        prune_logs(&dir, 2);

        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["config.toml", "latency-3.csv", "latency-4.csv", "other.csv"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
