//! The input path: starts the tablet helper over adb, then one read and one SendInput per pen report.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use osutab_core::*;

use crate::adb::{self, Adb, DeviceState};
use crate::adbkey::{self, AdbKey};
use crate::control;
use crate::inject::{self, Injector};
use crate::mapping::{to_unit, Rotation};
use crate::otd;
use crate::settings::{Output, UsbPath};
use crate::shared::{LiveConfig, Shared};
use crate::stats::{ClockSync, Event, PrecisionSample, SampleTiming};
use crate::trace::{Entry, Recorder};
use crate::tuning;
use crate::usbadb::{self, AdbInterface, DirectLink, QuickStart, QuickStartError, QuickStarted};

const FORWARD_PORT: u16 = 27191;
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);
const PING_INTERVAL: Duration = Duration::from_millis(200);
const BENCH_PING_INTERVAL: Duration = Duration::from_micros(2080);
const TRACE_PING_INTERVAL: Duration = Duration::from_millis(10);
// A gap this long counts as the pen pausing.
const PAUSE: Duration = Duration::from_millis(200);

static PC_MAX_GAP_US: AtomicU32 = AtomicU32::new(0);
const DEVICE_POLL: Duration = Duration::from_millis(100);
// A cursor move this far counts as a jump in the precision test.
const JUMP_PX: f64 = 300.0;
// Kept short because this wait holds up the input thread. The cursor normally moves within 0.1 ms.
const CURSOR_WAIT: Duration = Duration::from_micros(500);
const PIN_SETTLE: Duration = Duration::from_millis(800);
// A tablet that was just plugged in, or a link that just dropped, can need a moment before its USB
// connection answers. Starting adb.exe instead takes about 5 s, so this is worth waiting for.
const QUICK_START_PATIENCE: Duration = Duration::from_secs(4);
const QUICK_START_RETRY: Duration = Duration::from_millis(50);
// The last session's threads can hold the USB connection open for a moment after it ends.
const BUSY_PATIENCE: Duration = Duration::from_millis(1500);
// How long the tablet's "Allow USB debugging?" prompt stays up before it's asked again.
const PROMPT_PATIENCE: Duration = Duration::from_secs(60);
// On direct USB the helper stops if the PC goes quiet for this long, which gives the pen back to
// Android even if this program is killed. Windows can hold up the cursor for over 1.5 s, and this
// program can't answer the tablet meanwhile, so 2 s was too short: sessions ended mid-use.
const HELPER_PING_TIMEOUT_MS: u64 = 6000;
// A cursor move this slow is Windows holding it up, and worth a line in the log.
const SLOW_CURSOR_MS: f32 = 200.0;

pub struct EngineOptions {
    pub adb: Adb,
    pub helper: PathBuf,
    pub grab: bool,
    pub dry_run: bool,
    pub busy_poll: bool,
    pub helper_busy: bool,
    /// Pings about 480 times a second and records every round trip, so the USB path can be
    /// measured without using the pen.
    pub ping_bench: bool,
    /// The tablet app to install and start. `None` leaves the tablet's screen alone.
    pub app: Option<PathBuf>,
    /// Records around latency stalls, with scheduling watchdogs on both sides.
    pub stall_trace: Option<PathBuf>,
    /// How often to ping, overriding the normal rate.
    pub ping_ms: Option<u64>,
    /// Fake pen bursts, on and off times in ms. For reproducing stalls without the pen.
    pub burst: Option<(u64, u64)>,
    /// Overrides the saved USB path for this run.
    pub usb_path: Option<UsbPath>,
    /// Turns on the scheduling choices that help under load. Only off to measure them.
    pub tuning: bool,
    /// Leaves the kernel's filter on the pen's position. Only on to measure it.
    pub keep_pen_filter: bool,
    /// Stops the helper on the tablet this often, to test how fast the pen comes back.
    pub test_drops: Option<Duration>,
}

impl EngineOptions {
    fn helper_args(&self) -> String {
        let mut args = Vec::new();
        if !self.grab {
            args.push("--no-grab".to_string());
        }
        if self.helper_busy {
            args.push("--busy".to_string());
        }
        if self.stall_trace.is_some() {
            args.push("--watch".to_string());
        }
        if let Some((on, off)) = self.burst {
            args.push(format!("--burst {on} {off}"));
        }
        if !self.tuning {
            args.push("--any-core".to_string());
        }
        if self.keep_pen_filter {
            args.push("--keep-filter".to_string());
        }
        args.join(" ")
    }
}

/// Measures how late this PC schedules a normal thread, to tell PC stalls from USB stalls.
fn start_pc_watchdog(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let mut last = Instant::now();
        while !shared.stats.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(1));
            let gap_us = last.elapsed().as_micros().min(u32::MAX as u128) as u32;
            PC_MAX_GAP_US.fetch_max(gap_us, Ordering::Relaxed);
            last = Instant::now();
        }
    });
}

/// Installs the tablet app if needed and brings it up, as far as the settings ask. `reverse` is for
/// the adb path, where adb.exe carries the app's link. Pinning needs the app already showing, which
/// takes a moment after starting it.
fn prepare_tablet_app(adb: &Adb, apk: &std::path::Path, shared: &Shared, reverse: bool, open: bool) {
    let settings = shared.settings();
    let result = (|| -> io::Result<()> {
        if reverse {
            adb.reverse(control::APP_PORT)?;
        }
        adb.ensure_app(apk)?;
        if open && settings.open_tablet_app {
            adb.start_app()?;
        }
        if settings.screen_pinning && !shared.pinned.load(Ordering::SeqCst) {
            std::thread::sleep(PIN_SETTLE);
            adb.pin_app()?;
            shared.pinned.store(true, Ordering::SeqCst);
        } else if !settings.screen_pinning && shared.pinned.swap(false, Ordering::SeqCst) {
            adb.unpin_app()?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("could not start the tablet app: {e}");
    }
}

/// Connects through the forward and waits for the helper's first frame. A forward accepts the
/// connection even when the helper isn't listening yet, so this retries until a frame arrives.
fn connect(deadline: Instant) -> io::Result<(TcpStream, Hello)> {
    loop {
        let attempt = || -> io::Result<(TcpStream, Hello)> {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, FORWARD_PORT))?;
            stream.set_nodelay(true)?;
            stream.set_read_timeout(Some(Duration::from_millis(500)))?;
            let mut buf = [0u8; DOWN_FRAME_LEN];
            stream.read_exact(&mut buf)?;
            stream.set_read_timeout(None)?;
            match DownFrame::decode(&buf) {
                Some(DownFrame::Hello(h)) if h.version == PROTOCOL_VERSION => Ok((stream, h)),
                _ => Err(io::Error::other("unexpected first frame from helper")),
            }
        };
        match attempt() {
            Ok(ok) => return Ok(ok),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(30)),
        }
    }
}

/// Where pen frames come from. Generic rather than dynamic, so the input thread's path has no
/// extra indirect call.
trait PenLink {
    fn next_frame(&mut self, buf: &mut [u8; DOWN_FRAME_LEN], shared: &Shared) -> io::Result<()>;
}

struct TcpLink {
    stream: TcpStream,
    busy: bool,
}

impl PenLink for TcpLink {
    #[inline]
    fn next_frame(&mut self, buf: &mut [u8; DOWN_FRAME_LEN], shared: &Shared) -> io::Result<()> {
        if !self.busy {
            return self.stream.read_exact(buf);
        }
        let mut filled = 0;
        while filled < buf.len() {
            match self.stream.read(&mut buf[filled..]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if shared.stats.stop.load(Ordering::Relaxed) {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    std::hint::spin_loop();
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl PenLink for DirectLink {
    #[inline]
    fn next_frame(&mut self, buf: &mut [u8; DOWN_FRAME_LEN], _shared: &Shared) -> io::Result<()> {
        DirectLink::next_frame(self, buf)
    }
}

fn ping_interval(options: &EngineOptions) -> Duration {
    match options.ping_ms {
        Some(ms) => Duration::from_millis(ms),
        None if options.ping_bench => BENCH_PING_INTERVAL,
        None if options.stall_trace.is_some() => TRACE_PING_INTERVAL,
        None => PING_INTERVAL,
    }
}

/// Pings until `send` fails.
fn start_pinger(interval: Duration, clock: Instant, mut send: impl FnMut(u64) -> io::Result<()> + Send + 'static) {
    std::thread::spawn(move || {
        tuning::raise_this_thread();
        loop {
            if send(clock.elapsed().as_nanos() as u64).is_err() {
                break;
            }
            if interval < Duration::from_millis(5) {
                // Sleep can't reliably wait this short, and the benchmark needs a steady rate.
                let t0 = Instant::now();
                while t0.elapsed() < interval {
                    std::hint::spin_loop();
                }
            } else {
                std::thread::sleep(interval);
            }
        }
    });
}

/// Common to both USB paths, once the helper has said hello.
fn session_started(adb: Option<&Adb>, shared: &Arc<Shared>, disconnected_at: &mut Option<Instant>, hello: &Hello) {
    shared.stats.event(Event::Connected { recovery: disconnected_at.take().map(|t| t.elapsed()) });
    shared.connected.store(true, Ordering::SeqCst);
    // Only needed when following the screen, because reading it takes a while.
    if let Some(adb) = adb {
        if shared.settings().rotation.is_none() {
            shared.set_screen_rotation(adb.display_rotation().unwrap_or(Rotation::R90));
        }
    }
    let live = shared.live_config();
    println!(
        "connected: pen range {}x{}, pressure {}, rotation {:?}, pen area {:.1} x {:.1} mm",
        hello.max_x, hello.max_y, hello.max_pressure, live.rotation, live.area.width_mm, live.area.height_mm
    );
}

/// In test mode, waits for the cursor to reach the new position and checks where it landed.
fn measure_cursor(shared: &Shared, target: &crate::mapping::Rect, before: (i32, i32), exact: (f64, f64), previous: &mut Option<(f64, f64)>) -> Option<f32> {
    let expected = target.pixel_index(exact);
    let t0 = Instant::now();
    let mut cursor = inject::cursor_position();
    let should_move = expected != before;
    let mut timed_out = false;
    while should_move && cursor == before {
        if t0.elapsed() > CURSOR_WAIT {
            timed_out = true;
            break;
        }
        std::hint::spin_loop();
        cursor = inject::cursor_position();
    }
    let waited_ms = t0.elapsed().as_secs_f32() * 1e3;
    let dx = exact.0 - (cursor.0 as f64 + 0.5);
    let dy = exact.1 - (cursor.1 as f64 + 0.5);
    let jump = previous.is_some_and(|p| (p.0 - exact.0).hypot(p.1 - exact.1) >= JUMP_PX);
    *previous = Some(exact);
    shared.stats.record_precision(PrecisionSample {
        exact,
        cursor,
        error_px: dx.hypot(dy) as f32,
        jump,
        timed_out,
    });
    (should_move && !timed_out).then_some(waited_ms)
}

/// The hot path: one blocking read, one SendInput, per pen report.
fn pump<L: PenLink>(link: &mut L, hello: &Hello, injector: &mut Injector, shared: &Shared, options: &EngineOptions, recorder: &mut Option<Recorder>) -> io::Result<()> {
    let ping_bench = options.ping_bench;
    let mut last_sample_at: Option<Duration> = None;
    let clock = shared.clock;
    let stats = &shared.stats;
    let mut sync = ClockSync::default();
    let mut buf = [0u8; DOWN_FRAME_LEN];
    let mut prev_seq: Option<u32> = None;
    let mut prev_flags = 0u8;
    let mut generation = shared.live_generation();
    let mut live: LiveConfig = shared.live_config();
    let mut previous_exact = None;
    loop {
        link.next_frame(&mut buf, shared)?;
        let now = clock.elapsed();
        if stats.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match DownFrame::decode(&buf) {
            Some(DownFrame::Sample(s)) => {
                let latest = shared.live_generation();
                if latest != generation {
                    generation = latest;
                    let previous_output = live.output;
                    live = shared.live_config();
                    // Only a change of output needs this. Doing it on every area edit released
                    // a held click, and the next report pressed it again.
                    if live.output != previous_output {
                        injector.reset();
                    }
                    injector.target = live.target;
                    injector.desktop = live.desktop;
                }
                let transport_ms = sync.offset_ns().map(|offset| {
                    let on_host = s.timestamp_ns as i64 - offset;
                    (now.as_nanos() as i64 - on_host) as f32 / 1e6
                });
                let (tablet_u, tablet_v) = to_unit(s.x, s.y, hello.max_x, hello.max_y, live.rotation);
                let (u, v) = live.area.map(tablet_u, tablet_v);
                let direct = live.output == Output::Direct;
                let testing = direct && shared.test_mode.load(Ordering::Relaxed) && !injector.dry_run;
                let before = if testing { inject::cursor_position() } else { (0, 0) };

                if direct {
                    injector.apply(&s, u, v, live.tip_click, live.button_scan);
                } else {
                    let (x, y) = otd::landscape_coordinates(s.x, s.y, hello.max_x, hello.max_y, live.rotation);
                    shared.relay.send(&DownFrame::Sample(Sample { x, y, ..s }).encode());
                }
                let host_ms = (clock.elapsed() - now).as_secs_f32() * 1e3;
                if host_ms >= SLOW_CURSOR_MS {
                    stats.event(Event::Note(format!("Windows held up the cursor for {host_ms:.0} ms, with {} in front", inject::foreground_program())));
                }

                let cursor_ms = if testing && s.flags & FLAG_IN_RANGE != 0 {
                    let target = injector.target;
                    measure_cursor(shared, &target, before, target.pixel(u, v), &mut previous_exact)
                } else {
                    None
                };

                shared.store_pen(tablet_u, tablet_v, s.flags);
                let helper_ms = s.helper_delay_ns as f32 / 1e6;
                if last_sample_at.is_some_and(|t| now.saturating_sub(t) >= PAUSE) {
                    if let Some(t) = transport_ms {
                        stats.record_after_pause(t);
                    }
                }
                last_sample_at = Some(now);
                if let Some(r) = recorder.as_mut() {
                    r.push(Entry {
                        at_ms: now.as_secs_f64() * 1e3,
                        seq: s.seq,
                        transport_ms: transport_ms.unwrap_or(0.0),
                        helper_ms,
                        host_ms,
                        ..Entry::default()
                    });
                }
                let timing = SampleTiming {
                    helper_ms,
                    link_ms: transport_ms.map(|t| t - helper_ms),
                    host_ms,
                    cursor_ms,
                };
                let gaps = prev_seq.map_or(0, |p| s.seq.wrapping_sub(p).wrapping_sub(1));
                prev_seq = Some(s.seq);
                let changed = s.flags ^ prev_flags;
                prev_flags = s.flags;
                let in_contact = s.flags & FLAG_TOUCH != 0;
                if changed & FLAG_TOUCH != 0 {
                    stats.event(if in_contact { Event::TipDown } else { Event::TipUp });
                }
                if changed & FLAG_BUTTON != 0 {
                    stats.event(if s.flags & FLAG_BUTTON != 0 {
                        Event::ButtonDown { in_contact }
                    } else {
                        Event::ButtonUp { in_contact }
                    });
                }
                let rtt = sync.best_rtt_ns().map(|r| r as f32 / 1e6);
                stats.record_sample(timing, gaps, s.syn_dropped, s.flags, (u, v), rtt);
            }
            Some(DownFrame::Pong(p)) => {
                let received = now.as_nanos() as u64;
                sync.update(p.echo_ns, received, p.device_ns);
                let rtt_ms = received.saturating_sub(p.echo_ns) as f32 / 1e6;
                if ping_bench {
                    stats.record_round_trip(rtt_ms);
                }
                if let Some(r) = recorder.as_mut() {
                    let on_host = p.device_ns as i64 - sync.offset_ns().unwrap_or(0);
                    r.push(Entry {
                        at_ms: now.as_secs_f64() * 1e3,
                        is_ping: true,
                        rtt_ms,
                        up_ms: (on_host - p.echo_ns as i64) as f32 / 1e6,
                        down_ms: (received as i64 - on_host) as f32 / 1e6,
                        tablet_gap_ms: p.tablet_gap_us as f32 / 1e3,
                        pc_gap_ms: PC_MAX_GAP_US.swap(0, Ordering::Relaxed) as f32 / 1e3,
                        ..Entry::default()
                    });
                }
            }
            Some(DownFrame::Hello(_)) | None => return Err(io::Error::other("bad frame from helper")),
        }
    }
}

/// The path through adb.exe. `had_hello` says whether the helper ever answered.
fn run_session(options: &EngineOptions, shared: &Arc<Shared>, injector: &mut Injector, disconnected_at: &mut Option<Instant>, recorder: &mut Option<Recorder>, open_app: bool, had_hello: &mut bool) -> io::Result<()> {
    let adb = &options.adb;
    adb.forward(FORWARD_PORT, SOCKET_NAME)?;
    let mut child = adb.spawn_helper(&options.helper_args())?;
    if let Some(apk) = &options.app {
        // On another thread, so a slow install can't delay the pen.
        let (adb, apk, shared) = (adb.clone(), apk.clone(), shared.clone());
        std::thread::spawn(move || prepare_tablet_app(&adb, &apk, &shared, true, open_app));
    }
    let result = (|| {
        let (stream, hello) = connect(Instant::now() + HELLO_TIMEOUT)?;
        *had_hello = true;
        if options.tuning {
            tuning::apply_to_adb();
        }
        session_started(Some(adb), shared, disconnected_at, &hello);
        let mut pinger = stream.try_clone()?;
        start_pinger(ping_interval(options), shared.clock, move |host_ns| pinger.write_all(&UpFrame::Ping { host_ns }.encode()));
        if options.busy_poll {
            stream.set_nonblocking(true)?;
        }
        let mut link = TcpLink { stream, busy: options.busy_poll };
        pump(&mut link, &hello, injector, shared, options, recorder)
    })();
    shared.connected.store(false, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// The direct path. adb.exe does the setup, since it knows how to install and start things, then
/// steps aside so this program can open the USB interface itself.
fn run_direct_session(options: &EngineOptions, shared: &Arc<Shared>, injector: &mut Injector, disconnected_at: &mut Option<Instant>, recorder: &mut Option<Recorder>, open_app: bool, had_hello: &mut bool) -> Result<io::Result<()>, io::Error> {
    let adb = &options.adb;
    if let Some(apk) = &options.app {
        prepare_tablet_app(adb, apk, shared, false, open_app);
    }
    if shared.settings().rotation.is_none() {
        shared.set_screen_rotation(adb.display_rotation().unwrap_or(Rotation::R90));
    }
    adb.kill_server()?;

    let key = adbkey::load_or_create(&adbkey::key_dir().ok_or_else(|| io::Error::other("no user profile folder"))?)?;
    let (link, hello) = DirectLink::start(&key, &direct_helper_args(options), control::APP_PORT)?;
    *had_hello = true;
    Ok(run_direct_link(link, &hello, options, shared, injector, disconnected_at, recorder))
}

fn direct_helper_args(options: &EngineOptions) -> String {
    let timeout_ms = HELPER_PING_TIMEOUT_MS.max(ping_interval(options).as_millis() as u64 * 10);
    format!("{} --ping-timeout-ms {timeout_ms}", options.helper_args())
}

/// True if the tablet can be set up without adb.exe. Following the screen's rotation needs it,
/// because the rotation has to be known before the pen starts.
fn can_start_quickly(options: &EngineOptions, shared: &Shared) -> bool {
    options.usb_path.unwrap_or(shared.settings().usb_path) == UsbPath::Direct
}

/// What the tablet should run alongside the helper, so its screen is as the settings ask.
fn tablet_command(open_app: bool, pin: bool, unpin: bool) -> Option<String> {
    let mut commands = Vec::new();
    if open_app {
        commands.push(adb::open_app_command());
    }
    if pin {
        commands.push(adb::pin_app_command());
    } else if unpin {
        commands.push(adb::UNPIN_COMMAND.to_string());
    }
    (!commands.is_empty()).then(|| commands.join("; "))
}

/// Starts over USB without adb.exe.
fn start_quickly(options: &EngineOptions, shared: &Shared, key: &AdbKey, open_app: bool) -> Result<QuickStarted, QuickStartError> {
    let mut stamps = vec![adb::helper_stamp(&options.helper)?];
    let mut command = None;
    let mut pin = false;
    let mut unpin = false;
    let settings = shared.settings();
    if let Some(apk) = &options.app {
        stamps.push(adb::app_stamp(apk)?);
        let pinned = shared.pinned.load(Ordering::SeqCst);
        pin = settings.screen_pinning && !pinned;
        unpin = !settings.screen_pinning && pinned;
        command = tablet_command(open_app && settings.open_tablet_app, pin, unpin);
    }
    let helper_args = direct_helper_args(options);
    let plan = QuickStart {
        helper_args: &helper_args,
        app_port: control::APP_PORT,
        helper: &options.helper,
        app: options.app.as_deref(),
        stamps: &stamps,
        tablet_command: command.as_deref(),
        read_rotation: settings.rotation.is_none(),
    };
    // While the tablet shows its "Allow USB debugging?" prompt, the Play tab says so.
    let asked = Instant::now();
    let mut waiting = || {
        shared.set_device_state(DeviceState::Unauthorized);
        !shared.stats.stop.load(Ordering::SeqCst) && asked.elapsed() < PROMPT_PATIENCE
    };
    let started = DirectLink::start_quick(&plan, key, &mut waiting)?;
    if pin || unpin {
        shared.pinned.store(pin, Ordering::SeqCst);
    }
    if let Some(rotation) = started.rotation {
        shared.set_screen_rotation(rotation);
    }
    Ok(started)
}

fn run_direct_link(mut link: DirectLink, hello: &Hello, options: &EngineOptions, shared: &Arc<Shared>, injector: &mut Injector, disconnected_at: &mut Option<Instant>, recorder: &mut Option<Recorder>) -> io::Result<()> {
    shared.direct_active.store(true, Ordering::SeqCst);
    shared.usb3_link.store(link.is_usb3(), Ordering::SeqCst);
    session_started(None, shared, disconnected_at, hello);
    let pinger = link.pinger();
    start_pinger(ping_interval(options), shared.clock, move |host_ns| pinger.ping(host_ns));
    if let Some(every) = options.test_drops {
        let stop_helper = link.helper_stopper();
        std::thread::spawn(move || {
            std::thread::sleep(every);
            let _ = stop_helper();
        });
    }
    let result = pump(&mut link, hello, injector, shared, options, recorder);
    shared.connected.store(false, Ordering::SeqCst);
    // Quitting while the tablet is still here, so this saves starting adb.exe just to do it.
    if shared.stats.stop.load(Ordering::SeqCst) && shared.pinned.load(Ordering::SeqCst) && link.run_before_closing(adb::UNPIN_COMMAND).is_ok() {
        shared.pinned.store(false, Ordering::SeqCst);
    }
    // Closes the stream, which makes the helper exit and gives the pen back straight away.
    link.close();
    shared.direct_active.store(false, Ordering::SeqCst);
    result
}

/// Tidies up after a session. Returns true if the program is stopping.
fn session_ended(shared: &Shared, injector: &mut Injector, disconnected_at: &mut Option<Instant>, outcome: io::Result<()>) -> bool {
    injector.reset();
    if shared.stats.stop.load(Ordering::SeqCst) {
        return true;
    }
    if disconnected_at.is_none() {
        *disconnected_at = Some(Instant::now());
        shared.stats.event(Event::Disconnected { why: why_it_ended(&outcome) });
    }
    if let Err(e) = &outcome {
        eprintln!("session ended: {e}");
    }
    false
}

/// In words, for the log.
fn why_it_ended(outcome: &io::Result<()>) -> String {
    match outcome {
        Ok(()) => "the session was stopped".to_string(),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => "the tablet closed the pen's stream".to_string(),
        Err(e) => format!("{} ({:?})", e, e.kind()),
    }
}

/// Runs until `stop` is set, reconnecting whenever the tablet goes away.
pub fn run(options: EngineOptions, shared: Arc<Shared>) {
    let stats = &shared.stats;
    while !shared.setup_done.load(Ordering::SeqCst) {
        if stats.stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let live = shared.live_config();
    let mut injector = Injector::new(live.target, live.desktop, options.dry_run);
    let mut recorder = options.stall_trace.clone().map(Recorder::new);
    if recorder.is_some() {
        start_pc_watchdog(shared.clone());
    }
    let mut server_started = false;
    let mut needs_adb = false;
    let mut quick_since: Option<Instant> = None;
    let mut key: Option<AdbKey> = None;
    let mut pushed = false;
    let mut disconnected_at: Option<Instant> = None;
    let mut waiting_reported = false;
    // The tablet app comes to the front once per plug-in. Doing it after every hiccup would take
    // the tablet away from whatever else is on it.
    let mut opened_app = false;
    let mut direct_failed = false;
    while !stats.stop.load(Ordering::SeqCst) {
        if !needs_adb && !direct_failed && can_start_quickly(&options, &shared) {
            match usbadb::adb_interface() {
                AdbInterface::Nothing => {
                    shared.set_device_state(DeviceState::NoDevice);
                    opened_app = false;
                    quick_since = None;
                    if !waiting_reported {
                        println!("waiting for tablet (USB connected, debugging authorized)...");
                        waiting_reported = true;
                    }
                    std::thread::sleep(DEVICE_POLL);
                    continue;
                }
                AdbInterface::Other => {
                    stats.event(Event::Note("using adb.exe, because the adb device here isn't this tablet".to_string()));
                    needs_adb = true;
                }
                AdbInterface::Tablet => {
                    if key.is_none() {
                        match adbkey::key_dir().ok_or_else(|| io::Error::other("no user profile folder")).and_then(|dir| adbkey::load_or_create(&dir)) {
                            Ok(loaded) => key = Some(loaded),
                            Err(e) => {
                                stats.event(Event::Note(format!("using adb.exe, because this PC's adb key couldn't be read or made: {e}")));
                                needs_adb = true;
                            }
                        }
                    }
                    let Some(key) = key.as_ref() else { continue };
                    match start_quickly(&options, &shared, key, !opened_app) {
                        Ok(started) => {
                            quick_since = None;
                            opened_app = true;
                            shared.set_device_state(DeviceState::Ready);
                            if started.set_up {
                                stats.event(Event::Note("put this build's helper and tablet app on the tablet".to_string()));
                            }
                            if waiting_reported {
                                stats.event(Event::TabletVisible);
                            }
                            waiting_reported = false;
                            let outcome = run_direct_link(started.link, &started.hello, &options, &shared, &mut injector, &mut disconnected_at, &mut recorder);
                            if session_ended(&shared, &mut injector, &mut disconnected_at, outcome) {
                                break;
                            }
                            continue;
                        }
                        Err(QuickStartError::Unauthorized) => {
                            // The prompt is up, or was turned down. Asking again shows it again.
                            shared.set_device_state(DeviceState::Unauthorized);
                            quick_since = None;
                            std::thread::sleep(DEVICE_POLL);
                            continue;
                        }
                        Err(QuickStartError::SetupFailed(e)) => {
                            stats.event(Event::Note(format!("using adb.exe, because setting up the tablet over USB failed: {e}")));
                            needs_adb = true;
                        }
                        Err(e) => {
                            let started = *quick_since.get_or_insert_with(Instant::now);
                            let (patience, what) = match &e {
                                QuickStartError::Busy => (BUSY_PATIENCE, "something else has the USB connection".to_string()),
                                QuickStartError::Failed(e) => (QUICK_START_PATIENCE, e.to_string()),
                                QuickStartError::Unauthorized | QuickStartError::SetupFailed(_) => unreachable!(),
                            };
                            if started.elapsed() < patience {
                                std::thread::sleep(QUICK_START_RETRY);
                                continue;
                            }
                            quick_since = None;
                            // Usually an adb server left running, by this program or another. Stopping
                            // it is what the adb.exe route would do anyway, only 5 s slower.
                            if matches!(e, QuickStartError::Busy) && adb::stop_server() {
                                stats.event(Event::Note("stopped an adb server that was holding the tablet".to_string()));
                                continue;
                            }
                            stats.event(Event::Note(format!("using adb.exe, because the direct connection kept failing: {what}")));
                            needs_adb = true;
                        }
                    }
                }
            }
        }
        if !server_started {
            // Noted first, because starting it takes seconds and the program could quit meanwhile.
            if !adb::server_running() {
                shared.adb_started_here.store(true, Ordering::SeqCst);
            }
            if let Err(e) = options.adb.start_server() {
                eprintln!("could not start adb: {e}");
            }
            server_started = true;
        }
        let state = options.adb.device_state();
        shared.set_device_state(state);
        if state != DeviceState::Ready {
            opened_app = false;
            direct_failed = false;
            // Gone, so the next plug-in can skip adb.exe.
            if needs_adb && state != DeviceState::AdbMissing && state != DeviceState::Unauthorized && state != DeviceState::Offline {
                needs_adb = false;
                quick_since = None;
                server_started = false;
                if shared.adb_started_here.load(Ordering::SeqCst) {
                    let _ = options.adb.kill_server();
                }
            }
        }
        if state == DeviceState::AdbMissing {
            // Only worth retrying slowly: it needs someone to put adb in place.
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if state != DeviceState::Ready {
            if !waiting_reported {
                println!("waiting for tablet (USB connected, debugging authorized)...");
                waiting_reported = true;
            }
            std::thread::sleep(DEVICE_POLL);
            continue;
        }
        if waiting_reported {
            stats.event(Event::TabletVisible);
        }
        waiting_reported = false;
        if !pushed {
            match options.adb.push_helper(&options.helper) {
                Ok(()) => pushed = true,
                Err(e) => {
                    eprintln!("could not push helper: {e}");
                    std::thread::sleep(DEVICE_POLL);
                    continue;
                }
            }
        }
        let open_app = !opened_app;
        opened_app = true;
        let mut had_hello = false;
        let wanted_path = options.usb_path.unwrap_or_else(|| shared.settings().usb_path);
        let outcome = if wanted_path == UsbPath::Direct && !direct_failed {
            match run_direct_session(&options, &shared, &mut injector, &mut disconnected_at, &mut recorder, open_app, &mut had_hello) {
                Ok(outcome) => {
                    *shared.direct_problem.lock().unwrap() = None;
                    outcome
                }
                Err(e) => {
                    // The pen matters more than the path: go back to adb.exe until the next plug-in.
                    eprintln!("direct USB failed: {e}");
                    *shared.direct_problem.lock().unwrap() = Some(e.to_string());
                    direct_failed = true;
                    Ok(())
                }
            }
        } else {
            if wanted_path == UsbPath::Adb {
                *shared.direct_problem.lock().unwrap() = None;
            }
            run_session(&options, &shared, &mut injector, &mut disconnected_at, &mut recorder, open_app, &mut had_hello)
        };
        // No answer from the helper may mean it's gone from the tablet, or is an old version.
        if !had_hello {
            pushed = false;
        } else if wanted_path == UsbPath::Direct {
            // It's all set up now, and the direct path has already stopped adb.exe.
            needs_adb = false;
            quick_since = None;
            server_started = false;
        }
        if session_ended(&shared, &mut injector, &mut disconnected_at, outcome) {
            break;
        }
    }
}
