//! The input path: starts the tablet helper over adb, then one read and one SendInput per pen report.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use osutab_core::*;

use crate::adb::{Adb, DeviceState};
use crate::control;
use crate::inject::{self, Injector};
use crate::mapping::{to_unit, Rotation};
use crate::otd;
use crate::settings::Output;
use crate::shared::{LiveConfig, Shared};
use crate::stats::{ClockSync, Event, PrecisionSample, SampleTiming};
use crate::trace::{Entry, Recorder};

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

/// Brings up the tablet app on another thread, so a slow install can't delay the pen.
fn start_tablet_app(adb: Adb, apk: PathBuf) {
    std::thread::spawn(move || {
        let result = adb
            .reverse(control::APP_PORT)
            .and_then(|()| adb.ensure_app(&apk))
            .and_then(|()| adb.start_app());
        if let Err(e) = result {
            eprintln!("could not start the tablet app: {e}");
        }
    });
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

fn read_frame(stream: &mut TcpStream, buf: &mut [u8; DOWN_FRAME_LEN], busy: bool, shared: &Shared) -> io::Result<()> {
    if !busy {
        return stream.read_exact(buf);
    }
    let mut filled = 0;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
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

/// In test mode, waits for the cursor to reach the new position and checks where it landed.
fn measure_cursor(shared: &Shared, before: (i32, i32), exact: (f64, f64), previous: &mut Option<(f64, f64)>) -> Option<f32> {
    let expected = shared.target.pixel_index(exact);
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
fn pump(stream: &mut TcpStream, hello: &Hello, injector: &mut Injector, shared: &Shared, options: &EngineOptions, recorder: &mut Option<Recorder>) -> io::Result<()> {
    let (busy, ping_bench) = (options.busy_poll, options.ping_bench);
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
        read_frame(stream, &mut buf, busy, shared)?;
        let now = clock.elapsed();
        if stats.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match DownFrame::decode(&buf) {
            Some(DownFrame::Sample(s)) => {
                let latest = shared.live_generation();
                if latest != generation {
                    generation = latest;
                    live = shared.live_config();
                    // Don't leave a button held by the mode we've just left.
                    injector.reset();
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

                let cursor_ms = if testing && s.flags & FLAG_IN_RANGE != 0 {
                    measure_cursor(shared, before, injector.target.pixel(u, v), &mut previous_exact)
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

fn run_session(options: &EngineOptions, shared: &Arc<Shared>, injector: &mut Injector, disconnected_at: &mut Option<Instant>, recorder: &mut Option<Recorder>) -> io::Result<()> {
    let adb = &options.adb;
    adb.forward(FORWARD_PORT, SOCKET_NAME)?;
    let mut child = adb.spawn_helper(&options.helper_args())?;
    if let Some(apk) = &options.app {
        start_tablet_app(adb.clone(), apk.clone());
    }
    let result = (|| {
        let (mut stream, hello) = connect(Instant::now() + HELLO_TIMEOUT)?;
        shared.stats.event(Event::Connected { recovery: disconnected_at.take().map(|t| t.elapsed()) });
        shared.connected.store(true, Ordering::SeqCst);

        // Only needed when following the screen, because reading it takes a while.
        if shared.settings().rotation.is_none() {
            shared.set_screen_rotation(adb.display_rotation().unwrap_or(Rotation::R90));
        }
        let live = shared.live_config();
        println!(
            "connected: pen range {}x{}, pressure {}, rotation {:?}, pen area {:.1} x {:.1} mm",
            hello.max_x, hello.max_y, hello.max_pressure, live.rotation, live.area.width_mm, live.area.height_mm
        );

        let mut pinger = stream.try_clone()?;
        let clock = shared.clock;
        let ping_interval = match options.ping_ms {
            Some(ms) => Duration::from_millis(ms),
            None if options.ping_bench => BENCH_PING_INTERVAL,
            None if options.stall_trace.is_some() => TRACE_PING_INTERVAL,
            None => PING_INTERVAL,
        };
        std::thread::spawn(move || loop {
            let host_ns = clock.elapsed().as_nanos() as u64;
            if pinger.write_all(&UpFrame::Ping { host_ns }.encode()).is_err() {
                break;
            }
            if ping_interval < Duration::from_millis(5) {
                // Sleep can't reliably wait this short, and the benchmark needs a steady rate.
                let t0 = Instant::now();
                while t0.elapsed() < ping_interval {
                    std::hint::spin_loop();
                }
            } else {
                std::thread::sleep(ping_interval);
            }
        });

        if options.busy_poll {
            stream.set_nonblocking(true)?;
        }
        pump(&mut stream, &hello, injector, shared, options, recorder)
    })();
    shared.connected.store(false, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    result
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

    let mut injector = Injector::new(shared.target, inject::virtual_desktop(), options.dry_run);
    let mut recorder = options.stall_trace.clone().map(Recorder::new);
    if recorder.is_some() {
        start_pc_watchdog(shared.clone());
    }
    if let Err(e) = options.adb.start_server() {
        eprintln!("could not start adb: {e}");
    }
    let mut pushed = false;
    let mut disconnected_at: Option<Instant> = None;
    let mut waiting_reported = false;
    while !stats.stop.load(Ordering::SeqCst) {
        let state = options.adb.device_state();
        shared.set_device_state(state);
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
        let outcome = run_session(&options, &shared, &mut injector, &mut disconnected_at, &mut recorder);
        injector.reset();
        if stats.stop.load(Ordering::SeqCst) {
            break;
        }
        if disconnected_at.is_none() {
            disconnected_at = Some(Instant::now());
            stats.event(Event::Disconnected);
        }
        match outcome {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof || e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => eprintln!("session ended: {e}"),
        }
    }
}
