//! Runs on the tablet as the adb `shell` user. Reads the S Pen's evdev node and streams one
//! frame per report over an abstract unix socket that the host reaches through `adb forward`.
//! It serves one client and then exits, which also releases the pen back to Android.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use osutab_core::*;

static WATCHDOG_MAX_GAP_US: AtomicU32 = AtomicU32::new(0);
// Fake reports use their own sequence numbers, so they never look like gaps in the real ones.
const BURST_FIRST_SEQ: u32 = 1;

const PEN_NAME: &str = "sec_e-pen";
const ACCEPT_TIMEOUT_MS: i32 = 10_000;
const CLOCK_MONOTONIC: i32 = 1;
const POLLIN: i16 = 0x001;
const EVENT_SIZE: usize = 24;

const EVIOCGRAB: u64 = 0x4004_4590;
const EVIOCSCLOCK: u64 = 0x4004_45a0;

const fn eviocgname(len: u64) -> u64 {
    0x8000_0000 | (len << 16) | 0x4506
}
const fn eviocgkey(len: u64) -> u64 {
    0x8000_0000 | (len << 16) | 0x4518
}
const fn eviocgabs(axis: u16) -> u64 {
    0x8018_4540 + axis as u64
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[repr(C)]
#[derive(Default)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
    fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
    fn sched_setaffinity(pid: i32, size: usize, mask: *const u64) -> i32;
}

fn monotonic_ns() -> u64 {
    let mut t = Timespec::default();
    unsafe { clock_gettime(CLOCK_MONOTONIC, &mut t) };
    t.sec as u64 * 1_000_000_000 + t.nsec as u64
}

fn device_name(file: &File) -> Option<String> {
    let mut buf = [0u8; 128];
    let n = unsafe { ioctl(file.as_raw_fd(), eviocgname(buf.len() as u64), buf.as_mut_ptr()) };
    if n <= 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(n as usize);
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

fn find_pen() -> io::Result<File> {
    for entry in fs::read_dir("/dev/input")? {
        let path = entry?.path();
        if let Ok(file) = File::open(&path) {
            if device_name(&file).as_deref() == Some(PEN_NAME) {
                eprintln!("[helper] pen device: {}", path.display());
                return Ok(file);
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("no input device named {PEN_NAME}")))
}

fn abs_info(file: &File, axis: u16) -> io::Result<AbsInfo> {
    let mut info = AbsInfo::default();
    if unsafe { ioctl(file.as_raw_fd(), eviocgabs(axis), &mut info as *mut AbsInfo) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

fn read_snapshot(file: &File) -> io::Result<PenSnapshot> {
    let mut keys = [0u8; 96];
    if unsafe { ioctl(file.as_raw_fd(), eviocgkey(keys.len() as u64), keys.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let key = |code: u16| keys[code as usize / 8] & (1 << (code % 8)) != 0;
    Ok(PenSnapshot {
        x: abs_info(file, ABS_X)?.value,
        y: abs_info(file, ABS_Y)?.value,
        pressure: abs_info(file, ABS_PRESSURE)?.value,
        distance: abs_info(file, ABS_DISTANCE)?.value,
        tilt_x: abs_info(file, ABS_TILT_X)?.value,
        tilt_y: abs_info(file, ABS_TILT_Y)?.value,
        tool_pen: key(BTN_TOOL_PEN),
        tool_rubber: key(BTN_TOOL_RUBBER),
        touch: key(BTN_TOUCH),
        button: key(BTN_STYLUS),
    })
}

/// Left to itself, Android can run this on a slow core. Staying off those took about 0.13 ms off
/// the median. It's a small program that mostly sleeps, so this costs very little battery.
fn prefer_fast_cores() {
    let speeds: Vec<u64> = (0..64)
        .map_while(|core| fs::read_to_string(format!("/sys/devices/system/cpu/cpu{core}/cpufreq/cpuinfo_max_freq")).ok())
        .filter_map(|text| text.trim().parse().ok())
        .collect();
    if let Some(mask) = fast_core_mask(&speeds) {
        if unsafe { sched_setaffinity(0, 8, &mask) } != 0 {
            eprintln!("[helper] could not choose cores: {}", io::Error::last_os_error());
        }
    }
}

fn accept_client(listener: &UnixListener) -> io::Result<UnixStream> {
    let mut fds = [PollFd { fd: listener.as_raw_fd(), events: POLLIN, revents: 0 }];
    let ready = unsafe { poll(fds.as_mut_ptr(), 1, ACCEPT_TIMEOUT_MS) };
    if ready <= 0 {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "no client connected"));
    }
    Ok(listener.accept()?.0)
}

/// Measures how late this process gets scheduled, to tell tablet stalls from USB stalls.
fn start_watchdog() {
    std::thread::spawn(|| {
        let mut last = monotonic_ns();
        loop {
            std::thread::sleep(Duration::from_millis(1));
            let now = monotonic_ns();
            let gap_us = (now.saturating_sub(last) / 1000).min(u32::MAX as u64) as u32;
            WATCHDOG_MAX_GAP_US.fetch_max(gap_us, Ordering::Relaxed);
            last = now;
        }
    });
}

/// Test mode: sends fake pen reports at the pen's rate in bursts, so pausing and resuming can
/// be reproduced without anyone holding the pen.
fn start_bursts(sock: &UnixStream, on_ms: u64, off_ms: u64) -> io::Result<()> {
    let mut out = sock.try_clone()?;
    std::thread::spawn(move || {
        let period = Duration::from_micros(2080);
        let mut seq = BURST_FIRST_SEQ;
        loop {
            std::thread::sleep(Duration::from_millis(off_ms));
            let start = Instant::now();
            let mut next = start;
            while start.elapsed() < Duration::from_millis(on_ms) {
                let sample = Sample { seq, timestamp_ns: monotonic_ns(), ..Sample::default() };
                seq = seq.wrapping_add(1);
                if send_sample(&mut out, sample).is_err() {
                    return;
                }
                next += period;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
        }
    });
    Ok(())
}

fn send_sample(sock: &mut UnixStream, mut sample: Sample) -> io::Result<()> {
    sample.helper_delay_ns = monotonic_ns().saturating_sub(sample.timestamp_ns).min(u32::MAX as u64) as u32;
    sock.write_all(&DownFrame::Sample(sample).encode())
}

/// `ping_timeout_ms` ends the session when the PC goes quiet. Direct USB needs it: unlike a closed
/// socket, a PC program that has died sends nothing to say it's gone, which would keep the pen held.
fn serve(pen: &mut File, sock: &mut UnixStream, busy: bool, ping_timeout_ms: Option<i32>) -> io::Result<()> {
    let hello = Hello {
        version: PROTOCOL_VERSION,
        max_x: abs_info(pen, ABS_X)?.maximum as u16,
        max_y: abs_info(pen, ABS_Y)?.maximum as u16,
        max_pressure: abs_info(pen, ABS_PRESSURE)?.maximum as u16,
    };
    sock.write_all(&DownFrame::Hello(hello).encode())?;

    let mut decoder = PenDecoder::new();
    let first = decoder.apply_snapshot(read_snapshot(pen)?, monotonic_ns());
    send_sample(sock, first)?;

    let mut events = [0u8; EVENT_SIZE * 64];
    let mut up = [0u8; UP_FRAME_LEN];
    let mut fds = [
        PollFd { fd: pen.as_raw_fd(), events: POLLIN, revents: 0 },
        PollFd { fd: sock.as_raw_fd(), events: POLLIN, revents: 0 },
    ];
    // Busy mode trades tablet battery and heat for a possibly faster wake-up.
    let timeout = if busy { 0 } else { ping_timeout_ms.unwrap_or(-1) };
    let mut last_ping_ns = monotonic_ns();
    loop {
        if unsafe { poll(fds.as_mut_ptr(), 2, timeout) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if fds[0].revents != 0 {
            let n = pen.read(&mut events)?;
            for ev in events[..n].chunks_exact(EVENT_SIZE) {
                let sec = i64::from_ne_bytes(ev[0..8].try_into().unwrap());
                let usec = i64::from_ne_bytes(ev[8..16].try_into().unwrap());
                let ev_type = u16::from_ne_bytes(ev[16..18].try_into().unwrap());
                let code = u16::from_ne_bytes(ev[18..20].try_into().unwrap());
                let value = i32::from_ne_bytes(ev[20..24].try_into().unwrap());
                let ts = sec as u64 * 1_000_000_000 + usec as u64 * 1_000;
                match decoder.push(ev_type, code, value, ts) {
                    Decoded::Nothing => {}
                    Decoded::Sample(s) => send_sample(sock, s)?,
                    Decoded::NeedSnapshot => {
                        let s = decoder.apply_snapshot(read_snapshot(pen)?, monotonic_ns());
                        send_sample(sock, s)?;
                    }
                }
            }
        }
        if fds[1].revents != 0 {
            sock.read_exact(&mut up)?;
            if ping_timeout_ms.is_some() {
                last_ping_ns = monotonic_ns();
            }
            if let Some(UpFrame::Ping { host_ns }) = UpFrame::decode(&up) {
                let tablet_gap_us = WATCHDOG_MAX_GAP_US.swap(0, Ordering::Relaxed);
                let pong = Pong { echo_ns: host_ns, device_ns: monotonic_ns(), tablet_gap_us };
                sock.write_all(&DownFrame::Pong(pong).encode())?;
            }
        }
        if let Some(limit) = ping_timeout_ms {
            if monotonic_ns().saturating_sub(last_ping_ns) > limit as u64 * 1_000_000 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
    }
}

fn run() -> io::Result<()> {
    let grab = !std::env::args().any(|a| a == "--no-grab");
    let args: Vec<String> = std::env::args().collect();
    let busy = args.iter().any(|a| a == "--busy");
    let watch = args.iter().any(|a| a == "--watch");
    let burst = args.iter().position(|a| a == "--burst").and_then(|i| {
        Some((args.get(i + 1)?.parse::<u64>().ok()?, args.get(i + 2)?.parse::<u64>().ok()?))
    });
    if !args.iter().any(|a| a == "--any-core") {
        prefer_fast_cores();
    }
    let ping_timeout_ms = args.iter().position(|a| a == "--ping-timeout-ms").and_then(|i| args.get(i + 1)?.parse::<i32>().ok());
    let mut pen = find_pen()?;
    let fd = pen.as_raw_fd();
    let clock = CLOCK_MONOTONIC;
    if unsafe { ioctl(fd, EVIOCSCLOCK, &clock as *const i32) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let addr = SocketAddr::from_abstract_name(SOCKET_NAME)?;
    let listener = UnixListener::bind_addr(&addr)?;
    let mut sock = accept_client(&listener)?;

    if grab {
        // Android stops seeing the pen until this process exits.
        if unsafe { ioctl(fd, EVIOCGRAB, 1 as std::ffi::c_int) } < 0 {
            eprintln!("[helper] exclusive grab failed: {}", io::Error::last_os_error());
        } else {
            eprintln!("[helper] pen grabbed exclusively");
        }
    }
    if watch {
        start_watchdog();
    }
    // The hello frame must go first, so bursts start after a delay long enough for it.
    if let Some((on_ms, off_ms)) = burst {
        start_bursts(&sock, on_ms, off_ms)?;
    }
    serve(&mut pen, &mut sock, busy, ping_timeout_ms)
}

fn main() {
    match run() {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof || e.kind() == io::ErrorKind::BrokenPipe => {
            eprintln!("[helper] client disconnected");
        }
        Err(e) => {
            eprintln!("[helper] error: {e}");
            std::process::exit(1);
        }
    }
}
