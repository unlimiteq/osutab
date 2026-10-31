//! Clock alignment with the tablet, stage-by-stage latency numbers, and the latency log.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use osutab_core::{FLAG_BUTTON, FLAG_IN_RANGE, FLAG_TOUCH};

const SYNC_WINDOW: usize = 32;
const LATE_MS: f32 = 5.0;
const ROLLING_SECONDS: usize = 10;
const MAX_KEPT_SAMPLES: usize = 2_000_000;

/// Estimates tablet-clock minus host-clock from ping round trips. The shortest recent round
/// trip gives the tightest bound, so its offset is the one used.
#[derive(Debug, Default)]
pub struct ClockSync {
    window: Vec<(u64, i64)>,
    next: usize,
}

impl ClockSync {
    pub fn update(&mut self, sent_ns: u64, received_ns: u64, device_ns: u64) {
        let rtt = received_ns.saturating_sub(sent_ns);
        let midpoint = sent_ns as i128 + rtt as i128 / 2;
        let entry = (rtt, (device_ns as i128 - midpoint) as i64);
        if self.window.len() < SYNC_WINDOW {
            self.window.push(entry);
        } else {
            self.window[self.next] = entry;
        }
        self.next = (self.next + 1) % SYNC_WINDOW;
    }

    pub fn offset_ns(&self) -> Option<i64> {
        self.window.iter().min_by_key(|e| e.0).map(|e| e.1)
    }

    pub fn best_rtt_ns(&self) -> Option<u64> {
        self.window.iter().map(|e| e.0).min()
    }
}

pub fn percentile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((p * sorted.len() as f64) as usize).min(sorted.len() - 1);
    sorted[i]
}

/// Percentiles of a set of times in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Dist {
    pub count: usize,
    pub p50: f32,
    pub p90: f32,
    pub p99: f32,
    pub p999: f32,
    pub max: f32,
}

impl Dist {
    pub fn of(values: &mut [f32]) -> Dist {
        values.sort_by(f32::total_cmp);
        Dist {
            count: values.len(),
            p50: percentile(values, 0.5),
            p90: percentile(values, 0.9),
            p99: percentile(values, 0.99),
            p999: percentile(values, 0.999),
            max: values.last().copied().unwrap_or(0.0),
        }
    }
}

// Fields are only read through Debug when the event is printed.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum Event {
    TipDown,
    TipUp,
    ButtonDown { in_contact: bool },
    ButtonUp { in_contact: bool },
    Connected { recovery: Option<Duration> },
    Disconnected,
    TabletVisible,
}

pub fn wall_clock() -> String {
    let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    format!("{:02}:{:02}:{:02}.{:03}", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

/// One pen report's timings, in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct SampleTiming {
    /// Kernel timestamp to the helper sending. Measured on the tablet's clock alone.
    pub helper_ms: f32,
    /// Helper sending to the host reading. Depends on clock alignment.
    pub link_ms: Option<f32>,
    /// Host reading to SendInput returning. Measured on the PC's clock alone.
    pub host_ms: f32,
    /// SendInput returning to the cursor position changing. Only in test mode.
    pub cursor_ms: Option<f32>,
}

/// Precision test result for one report, in screen pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PrecisionSample {
    pub exact: (f64, f64),
    pub cursor: (i32, i32),
    /// Distance from the exact position to the centre of the cursor's pixel.
    pub error_px: f32,
    pub jump: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PrecisionView {
    pub latest: PrecisionSample,
    pub samples: u64,
    pub worst_px: f32,
    pub jumps: u64,
    pub worst_jump_px: f32,
    pub timed_out: u64,
    pub over_1px: u64,
}

impl PrecisionView {
    pub fn add(&mut self, s: PrecisionSample) {
        self.latest = s;
        self.samples += 1;
        self.worst_px = self.worst_px.max(s.error_px);
        if s.error_px > 1.0 {
            self.over_1px += 1;
        }
        if s.jump {
            self.jumps += 1;
            self.worst_jump_px = self.worst_jump_px.max(s.error_px);
        }
        if s.timed_out {
            self.timed_out += 1;
        }
    }
}

#[derive(Default)]
struct Buffers {
    total: Vec<f32>,
    helper: Vec<f32>,
    link: Vec<f32>,
    host: Vec<f32>,
    cursor: Vec<f32>,
}

impl Buffers {
    fn append(&mut self, other: &Buffers) {
        let keep = |to: &mut Vec<f32>, from: &[f32]| {
            if to.len() < MAX_KEPT_SAMPLES {
                to.extend_from_slice(from);
            }
        };
        keep(&mut self.total, &other.total);
        keep(&mut self.helper, &other.helper);
        keep(&mut self.link, &other.link);
        keep(&mut self.host, &other.host);
        keep(&mut self.cursor, &other.cursor);
    }

    fn dists(&mut self) -> Stages {
        Stages {
            total: Dist::of(&mut self.total),
            helper: Dist::of(&mut self.helper),
            link: Dist::of(&mut self.link),
            host: Dist::of(&mut self.host),
            cursor: Dist::of(&mut self.cursor),
        }
    }
}

#[derive(Default)]
struct Live {
    buffers: Buffers,
    samples: u32,
    gaps: u32,
    syn_dropped: u16,
    events: Vec<(f64, String, Event)>,
    flags: u8,
    unit: (f64, f64),
    best_rtt_ms: f32,
    round_trips: Vec<f32>,
    /// The first report after a pause, when anything that went idle has to wake up.
    after_pause: Vec<f32>,
}

/// Percentiles for each stage, and for the whole path.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stages {
    pub total: Dist,
    pub helper: Dist,
    pub link: Dist,
    pub host: Dist,
    pub cursor: Dist,
}

/// What the settings window shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HudView {
    pub rate_hz: f32,
    /// Over the last few seconds with pen activity.
    pub recent: Stages,
    pub jitter_ms: f32,
    pub late: usize,
    pub dropped: u64,
    pub overruns: u16,
    pub best_rtt_ms: f32,
    pub total_samples: u64,
}

pub struct Stats {
    live: Mutex<Live>,
    hud: Mutex<HudView>,
    precision: Mutex<PrecisionView>,
    pub stop: AtomicBool,
    start: Instant,
}

impl Stats {
    pub fn new(start: Instant) -> Self {
        Stats {
            live: Mutex::new(Live::default()),
            hud: Mutex::new(HudView::default()),
            precision: Mutex::new(PrecisionView::default()),
            stop: AtomicBool::new(false),
            start,
        }
    }

    pub fn record_sample(
        &self,
        timing: SampleTiming,
        gaps: u32,
        syn_dropped: u16,
        flags: u8,
        unit: (f64, f64),
        best_rtt_ms: Option<f32>,
    ) {
        let mut l = self.live.lock().unwrap();
        l.samples += 1;
        l.buffers.helper.push(timing.helper_ms);
        l.buffers.host.push(timing.host_ms);
        if let Some(link) = timing.link_ms {
            l.buffers.link.push(link);
            l.buffers.total.push(timing.helper_ms + link + timing.host_ms + timing.cursor_ms.unwrap_or(0.0));
        }
        if let Some(cursor) = timing.cursor_ms {
            l.buffers.cursor.push(cursor);
        }
        l.gaps += gaps;
        l.syn_dropped = syn_dropped;
        l.flags = flags;
        l.unit = unit;
        if let Some(r) = best_rtt_ms {
            l.best_rtt_ms = r;
        }
    }

    pub fn record_after_pause(&self, ms: f32) {
        self.live.lock().unwrap().after_pause.push(ms);
    }

    pub fn record_round_trip(&self, ms: f32) {
        self.live.lock().unwrap().round_trips.push(ms);
    }

    pub fn record_precision(&self, sample: PrecisionSample) {
        self.precision.lock().unwrap().add(sample);
    }

    pub fn precision(&self) -> PrecisionView {
        *self.precision.lock().unwrap()
    }

    pub fn reset_precision(&self) {
        *self.precision.lock().unwrap() = PrecisionView::default();
    }

    pub fn hud(&self) -> HudView {
        self.hud.lock().unwrap().clone()
    }

    pub fn event(&self, event: Event) {
        let at = self.start.elapsed().as_secs_f64();
        let wall = wall_clock();
        self.live.lock().unwrap().events.push((at, wall, event));
    }

    fn take(&self) -> Live {
        let mut l = self.live.lock().unwrap();
        let carried = Live {
            flags: l.flags,
            unit: l.unit,
            syn_dropped: l.syn_dropped,
            best_rtt_ms: l.best_rtt_ms,
            ..Live::default()
        };
        std::mem::replace(&mut *l, carried)
    }

    /// Prints and logs a line each second until `stop` is set, then the whole-run summary.
    pub fn report_loop(&self, mut log: Option<File>) {
        let mut all = Buffers::default();
        let mut all_round_trips: Vec<f32> = Vec::new();
        let mut all_after_pause: Vec<f32> = Vec::new();
        let mut rolling: VecDeque<Buffers> = VecDeque::new();
        let (mut total_samples, mut total_gaps, mut overruns) = (0u64, 0u64, 0u16);
        let (mut tip_edges, mut button_downs_hover, mut button_downs_contact) = (0u32, 0u32, 0u32);
        let mut active_seconds = 0u32;
        let mut last = Instant::now();
        let mut write_log = |line: &str| {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        };
        write_log("time,rate_hz,total_p50,total_p99,total_max,helper_p50,link_p50,host_p50,cursor_p50,jitter,late,dropped,overruns,rtt");

        while !self.stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            if last.elapsed() < Duration::from_secs(1) {
                continue;
            }
            let elapsed = last.elapsed().as_secs_f64();
            last = Instant::now();
            let mut l = self.take();
            all_round_trips.append(&mut l.round_trips);
            all_after_pause.append(&mut l.after_pause);
            for (at, wall, event) in &l.events {
                match event {
                    Event::TipDown | Event::TipUp => tip_edges += 1,
                    Event::ButtonDown { in_contact: true } => button_downs_contact += 1,
                    Event::ButtonDown { in_contact: false } => button_downs_hover += 1,
                    _ => {}
                }
                match event {
                    Event::TipDown | Event::TipUp => {}
                    other => {
                        let line = format!("[{wall} {at:7.2}s] {other:?}");
                        println!("{line}");
                        write_log(&format!("# {line}"));
                    }
                }
            }
            total_gaps += l.gaps as u64;
            total_samples += l.samples as u64;
            overruns = l.syn_dropped;
            let rate = l.samples as f32 / elapsed as f32;
            if l.samples == 0 {
                self.hud.lock().unwrap().rate_hz = 0.0;
                continue;
            }
            active_seconds += 1;
            all.append(&l.buffers);
            rolling.push_back(std::mem::take(&mut l.buffers));
            if rolling.len() > ROLLING_SECONDS {
                rolling.pop_front();
            }
            let mut this_second = rolling.back().map(clone_buffers).unwrap_or_default();
            let second = this_second.dists();
            let late = this_second.total.iter().filter(|v| **v > LATE_MS).count();
            let mut recent_buffers = Buffers::default();
            for b in &rolling {
                recent_buffers.append(b);
            }
            let recent = recent_buffers.dists();

            *self.hud.lock().unwrap() = HudView {
                rate_hz: rate,
                recent,
                jitter_ms: recent.total.p99 - recent.total.p50,
                late,
                dropped: total_gaps,
                overruns,
                best_rtt_ms: l.best_rtt_ms,
                total_samples,
            };

            let state = match (l.flags & FLAG_TOUCH != 0, l.flags & FLAG_IN_RANGE != 0) {
                (true, _) => "contact",
                (false, true) => "hover",
                _ => "away",
            };
            println!(
                "{:4.0} Hz | total p50 {:5.2} p99 {:5.2} max {:6.2} ms | helper {:4.2} link {:4.2} host {:4.2} cursor {:4.2} | late {:3} | rtt {:4.2} | dropped {} | {}{}",
                rate,
                second.total.p50,
                second.total.p99,
                second.total.max,
                second.helper.p50,
                second.link.p50,
                second.host.p50,
                second.cursor.p50,
                late,
                l.best_rtt_ms,
                l.gaps,
                state,
                if l.flags & FLAG_BUTTON != 0 { "+button" } else { "" },
            );
            write_log(&format!(
                "{},{:.0},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{},{:.3}",
                wall_clock(),
                rate,
                second.total.p50,
                second.total.p99,
                second.total.max,
                second.helper.p50,
                second.link.p50,
                second.host.p50,
                second.cursor.p50,
                second.total.p99 - second.total.p50,
                late,
                total_gaps,
                overruns,
                l.best_rtt_ms,
            ));
        }

        let late_total = all.total.iter().filter(|v| **v > LATE_MS).count();
        let whole = all.dists();
        let mut lines = vec![
            "--- summary ---".to_string(),
            format!("samples {total_samples} over {active_seconds} active s | dropped {total_gaps} | evdev overruns {overruns}"),
            format!("over {LATE_MS} ms: {late_total}"),
        ];
        for (name, d) in [
            ("total ", whole.total),
            ("helper", whole.helper),
            ("link  ", whole.link),
            ("host  ", whole.host),
            ("cursor", whole.cursor),
        ] {
            lines.push(format!(
                "{name} ms: p50 {:.3} p90 {:.3} p99 {:.3} p99.9 {:.3} max {:.3} (n={})",
                d.p50, d.p90, d.p99, d.p999, d.max, d.count
            ));
        }
        if !all_round_trips.is_empty() {
            let d = Dist::of(&mut all_round_trips);
            lines.push(format!(
                "round trip ms: p50 {:.3} p90 {:.3} p99 {:.3} p99.9 {:.3} max {:.3} (n={})",
                d.p50, d.p90, d.p99, d.p999, d.max, d.count
            ));
        }
        if !all_after_pause.is_empty() {
            let d = Dist::of(&mut all_after_pause);
            lines.push(format!(
                "first report after a pause, ms: p50 {:.3} p90 {:.3} p99 {:.3} max {:.3} (n={})",
                d.p50, d.p90, d.p99, d.max, d.count
            ));
        }
        let p = self.precision();
        if p.samples > 0 {
            lines.push(format!(
                "precision: {} samples, worst {:.2} px, over 1 px: {}, jumps {} worst {:.2} px, cursor timeouts {}",
                p.samples, p.worst_px, p.over_1px, p.jumps, p.worst_jump_px, p.timed_out
            ));
        }
        lines.push(format!("tip edges {tip_edges} | button presses: {button_downs_hover} hovering, {button_downs_contact} in contact"));
        for line in lines {
            println!("{line}");
            write_log(&format!("# {line}"));
        }
    }
}

fn clone_buffers(b: &Buffers) -> Buffers {
    Buffers {
        total: b.total.clone(),
        helper: b.helper.clone(),
        link: b.link.clone(),
        host: b.host.clone(),
        cursor: b.cursor.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_uses_shortest_round_trip() {
        let mut c = ClockSync::default();
        assert_eq!(c.offset_ns(), None);
        // True offset is 1000. A slow reply skews its own estimate and must be ignored.
        c.update(0, 100, 1050);
        c.update(1000, 3000, 2000 + 900);
        assert_eq!(c.offset_ns(), Some(1000));
        assert_eq!(c.best_rtt_ns(), Some(100));
    }

    #[test]
    fn window_forgets_old_entries() {
        let mut c = ClockSync::default();
        c.update(0, 10, 5);
        for i in 0..SYNC_WINDOW as u64 {
            c.update(i * 1000, i * 1000 + 50, i * 1000 + 25 + 7);
        }
        assert_eq!(c.best_rtt_ns(), Some(50));
        assert_eq!(c.offset_ns(), Some(7));
    }

    #[test]
    fn percentile_bounds() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        let v = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 1.0), 4.0);
    }

    #[test]
    fn dist_sorts_and_summarises() {
        let mut v = [5.0, 1.0, 3.0, 2.0, 4.0];
        let d = Dist::of(&mut v);
        assert_eq!((d.count, d.p50, d.max), (5, 3.0, 5.0));
        assert_eq!(Dist::of(&mut []), Dist::default());
    }

    #[test]
    fn total_adds_every_stage() {
        let stats = Stats::new(Instant::now());
        let timing = SampleTiming { helper_ms: 0.1, link_ms: Some(0.5), host_ms: 0.05, cursor_ms: Some(0.02) };
        stats.record_sample(timing, 0, 0, 1, (0.0, 0.0), None);
        let live = stats.take();
        assert!((live.buffers.total[0] - 0.67).abs() < 1e-5);
        assert_eq!(live.samples, 1);
    }

    #[test]
    fn no_total_before_clocks_are_aligned() {
        let stats = Stats::new(Instant::now());
        let timing = SampleTiming { helper_ms: 0.1, link_ms: None, host_ms: 0.05, cursor_ms: None };
        stats.record_sample(timing, 0, 0, 1, (0.0, 0.0), None);
        assert!(stats.take().buffers.total.is_empty());
    }

    #[test]
    fn precision_tracks_worst_and_jumps() {
        let mut p = PrecisionView::default();
        p.add(PrecisionSample { error_px: 0.4, ..Default::default() });
        p.add(PrecisionSample { error_px: 1.5, jump: true, ..Default::default() });
        p.add(PrecisionSample { error_px: 0.2, timed_out: true, ..Default::default() });
        assert_eq!((p.samples, p.worst_px, p.over_1px, p.jumps, p.worst_jump_px, p.timed_out), (3, 1.5, 1, 1, 1.5, 1));
        assert_eq!(p.latest.error_px, 0.2);
    }
}
