//! Checks the output half without the tablet: moves the real cursor through the same injection
//! code to known positions and reads back where it landed. Run with `--self-test`.

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener};
use std::time::{Duration, Instant};

use osutab_core::{DownFrame, Sample, FLAG_IN_RANGE};

use crate::inject::{self, Injector};
use crate::mapping::Rect;
use crate::otd;
use crate::stats::Dist;

const WAIT: Duration = Duration::from_millis(50);
const RANDOM_JUMPS: usize = 500;

struct Landed {
    error_px: f64,
    wait_ms: Option<f32>,
    cursor: (i32, i32),
    exact: (f64, f64),
}

fn move_and_check(injector: &mut Injector, target: &Rect, u: f64, v: f64) -> Landed {
    let exact = target.pixel(u, v);
    let before = inject::cursor_position();
    let expected = target.pixel_index(exact);
    let sample = Sample { flags: FLAG_IN_RANGE, ..Sample::default() };
    injector.apply(&sample, u, v, false, None);
    let t0 = Instant::now();
    let mut cursor = inject::cursor_position();
    while expected != before && cursor == before && t0.elapsed() < WAIT {
        std::hint::spin_loop();
        cursor = inject::cursor_position();
    }
    let wait_ms = (expected != before && cursor != before).then(|| t0.elapsed().as_secs_f32() * 1e3);
    let error_px = (exact.0 - (cursor.0 as f64 + 0.5)).hypot(exact.1 - (cursor.1 as f64 + 0.5));
    Landed { error_px, wait_ms, cursor, exact }
}

/// Times SendInput alone at the pen's rate, with zero-distance moves so the cursor stays put.
/// Slow calls here can't be USB, adb or the tablet, because none of them are involved.
pub fn sendinput_bench(seconds: u64) {
    const SLOW_MS: f32 = 5.0;
    let start = Instant::now();
    let mut times = Vec::with_capacity((seconds * 500) as usize);
    let mut slow = 0;
    while start.elapsed() < Duration::from_secs(seconds) {
        let t0 = Instant::now();
        inject::send_zero_move();
        let ms = t0.elapsed().as_secs_f32() * 1e3;
        times.push(ms);
        if ms >= SLOW_MS {
            slow += 1;
            println!("[{}] SendInput took {ms:.2} ms", crate::stats::wall_clock());
        }
        // Sleeps rather than spins, so it can run alongside a game without taking a CPU core.
        std::thread::sleep(Duration::from_millis(2));
    }
    let d = Dist::of(&mut times);
    println!(
        "SendInput ms: p50 {:.3} p90 {:.3} p99 {:.3} p99.9 {:.3} max {:.3} (n={}), {slow} calls of {SLOW_MS} ms or more",
        d.p50, d.p90, d.p99, d.p999, d.max, d.count
    );
}

/// Measures OpenTabletDriver's own delay, without the tablet: acts as osu!tab's link to the bridge
/// plugin, sends made-up pen positions, and times how long the real cursor takes to move. It
/// moves the cursor for the whole run, then puts it back.
pub fn otd_bench(seconds: u64, target: Rect) {
    const PERIOD: Duration = Duration::from_micros(2080);
    const WAIT: Duration = Duration::from_millis(30);
    const MAX_X: f64 = 31376.0;
    const MAX_Y: f64 = 19589.0;

    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, otd::RELAY_PORT)) {
        Ok(l) => l,
        Err(e) => {
            println!("could not listen for OpenTabletDriver: {e}");
            return;
        }
    };
    println!("waiting for OpenTabletDriver's bridge plugin to connect...");
    let Ok((mut stream, _)) = listener.accept() else { return };
    let _ = stream.set_nodelay(true);
    let start_cursor = inject::cursor_position();
    println!("connected, sending for {seconds} s");

    let start = Instant::now();
    let mut next = start;
    let mut seq = 0u32;
    let mut times = Vec::new();
    let mut timeouts = 0;
    let mut offset_sum = (0.0f64, 0.0f64);
    // Sweeps the middle of the tablet, so every report should move the cursor.
    let mut position = 0.0f64;
    while start.elapsed() < Duration::from_secs(seconds) {
        position = (position + 0.002) % 1.0;
        let (x, y) = (0.25 + position * 0.5, 0.25 + ((position * 7.0) % 1.0) * 0.5);
        let sample = Sample {
            seq,
            flags: FLAG_IN_RANGE,
            x: (x * MAX_X) as u16,
            y: (y * MAX_Y) as u16,
            ..Sample::default()
        };
        seq = seq.wrapping_add(1);
        let before = inject::cursor_position();
        let t0 = Instant::now();
        if stream.write_all(&DownFrame::Sample(sample).encode()).is_err() {
            println!("the plugin disconnected");
            break;
        }
        let mut moved = false;
        while t0.elapsed() < WAIT {
            if inject::cursor_position() != before {
                moved = true;
                break;
            }
            std::hint::spin_loop();
        }
        if moved {
            times.push(t0.elapsed().as_secs_f32() * 1e3);
            let cursor = inject::cursor_position();
            offset_sum.0 += cursor.0 as f64 - x * target.width;
            offset_sum.1 += cursor.1 as f64 - y * target.height;
        } else {
            timeouts += 1;
        }
        next += PERIOD;
        while Instant::now() < next {
            std::hint::spin_loop();
        }
    }
    inject::set_cursor_position(start_cursor.0, start_cursor.1);
    let moves = times.len().max(1) as f64;
    println!("cursor was on average {:.1} px right and {:.1} px down of the report's position", offset_sum.0 / moves, offset_sum.1 / moves);
    let d = Dist::of(&mut times);
    println!(
        "report sent to cursor moved, ms: p50 {:.3} p90 {:.3} p99 {:.3} p99.9 {:.3} max {:.3} (n={}), no move within {} ms: {timeouts}",
        d.p50, d.p90, d.p99, d.p999, d.max, d.count, WAIT.as_millis()
    );
}

/// Returns true when every position landed within 1 px.
pub fn run(target: Rect) -> bool {
    let desktop = inject::virtual_desktop();
    let mut injector = Injector::new(target, desktop, false);
    let start = inject::cursor_position();
    println!("screen {}x{} real pixels, desktop {}x{}", target.width, target.height, desktop.width, desktop.height);

    let mut worst = 0.0f64;
    for (name, u, v) in [
        ("top left", 0.0, 0.0),
        ("top right", 1.0, 0.0),
        ("bottom left", 0.0, 1.0),
        ("bottom right", 1.0, 1.0),
        ("centre", 0.5, 0.5),
    ] {
        let l = move_and_check(&mut injector, &target, u, v);
        worst = worst.max(l.error_px);
        println!(
            "{name:12}: wanted {:7.1},{:7.1}  cursor {},{}  error {:.2} px",
            l.exact.0, l.exact.1, l.cursor.0, l.cursor.1, l.error_px
        );
    }

    // Small fixed-point generator, so a failing run can be repeated exactly.
    let mut seed = 0x2545F491u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f64 / u32::MAX as f64
    };
    let mut waits = Vec::with_capacity(RANDOM_JUMPS);
    let mut worst_jump = 0.0f64;
    let mut over = 0;
    for _ in 0..RANDOM_JUMPS {
        let l = move_and_check(&mut injector, &target, next(), next());
        worst_jump = worst_jump.max(l.error_px);
        if l.error_px > 1.0 {
            over += 1;
        }
        if let Some(w) = l.wait_ms {
            waits.push(w);
        }
    }
    let d = Dist::of(&mut waits);
    println!("{RANDOM_JUMPS} random jumps: worst error {worst_jump:.2} px, over 1 px: {over}");
    println!(
        "SendInput return to cursor moved, ms: p50 {:.3} p99 {:.3} max {:.3} (n={})",
        d.p50, d.p99, d.max, d.count
    );
    inject::set_cursor_position(start.0, start.1);
    let passed = worst.max(worst_jump) <= 1.0;
    println!("{}", if passed { "PASS: every position within 1 px" } else { "FAIL: some positions were more than 1 px off" });
    passed
}
