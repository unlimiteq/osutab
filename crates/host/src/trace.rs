//! A flight recorder for finding latency stalls. It keeps the last few seconds of every sample
//! and ping, and when something takes too long it saves what happened around that moment.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

const CAPACITY: usize = 12_000;
const STALL_MS: f32 = 20.0;
const KEEP_BEFORE_MS: f64 = 2_000.0;
const KEEP_AFTER_MS: f64 = 1_000.0;
const COOLDOWN_MS: f64 = 5_000.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Entry {
    pub at_ms: f64,
    pub is_ping: bool,
    pub seq: u32,
    /// For a sample: kernel timestamp to host read.
    pub transport_ms: f32,
    pub helper_ms: f32,
    pub host_ms: f32,
    /// For a ping: the whole round trip, and each direction by the aligned clocks.
    pub rtt_ms: f32,
    pub up_ms: f32,
    pub down_ms: f32,
    /// Longest scheduling gaps seen on each side since the previous ping.
    pub tablet_gap_ms: f32,
    pub pc_gap_ms: f32,
}

impl Entry {
    /// What about this entry makes it a stall, if anything.
    fn stall_reason(&self) -> Option<&'static str> {
        if self.transport_ms >= STALL_MS {
            Some("sample")
        } else if self.rtt_ms >= STALL_MS {
            Some("ping")
        } else if self.tablet_gap_ms >= STALL_MS {
            Some("tablet-gap")
        } else if self.pc_gap_ms >= STALL_MS {
            Some("pc-gap")
        } else if self.host_ms >= STALL_MS {
            Some("sendinput")
        } else {
            None
        }
    }
}

pub struct Recorder {
    ring: VecDeque<Entry>,
    pending: Option<(Entry, &'static str)>,
    last_stall_ms: f64,
    pub stalls: u32,
    dir: PathBuf,
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Self {
        Recorder {
            ring: VecDeque::with_capacity(CAPACITY),
            pending: None,
            last_stall_ms: f64::NEG_INFINITY,
            stalls: 0,
            dir,
        }
    }

    /// Adds an entry. When a stall's window is complete, returns the stall and what surrounded it.
    fn push_inner(&mut self, entry: Entry) -> Option<(Entry, &'static str, Vec<Entry>)> {
        if self.ring.len() == CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(entry);
        if self.pending.is_none() && entry.at_ms - self.last_stall_ms >= COOLDOWN_MS {
            if let Some(reason) = entry.stall_reason() {
                self.pending = Some((entry, reason));
                self.last_stall_ms = entry.at_ms;
                self.stalls += 1;
            }
        }
        let (stall, reason) = self.pending?;
        if entry.at_ms - stall.at_ms < KEEP_AFTER_MS {
            return None;
        }
        self.pending = None;
        let window = self.ring.iter().filter(|e| e.at_ms >= stall.at_ms - KEEP_BEFORE_MS).copied().collect();
        Some((stall, reason, window))
    }

    pub fn push(&mut self, entry: Entry) {
        if let Some((stall, reason, window)) = self.push_inner(entry) {
            let dir = self.dir.clone();
            let number = self.stalls;
            // Writing a file doesn't belong on the input thread.
            std::thread::spawn(move || save(&dir, number, &stall, reason, &window));
        }
    }
}

fn save(dir: &PathBuf, number: u32, stall: &Entry, reason: &str, window: &[Entry]) {
    println!(
        "STALL {number} about 1 s before {} ({:.3}s) [{reason}]: transport {:.2} helper {:.2} host {:.2} rtt {:.2} up {:.2} down {:.2} tablet gap {:.2} pc gap {:.2} ms",
        crate::stats::wall_clock(), stall.at_ms / 1000.0, stall.transport_ms, stall.helper_ms, stall.host_ms, stall.rtt_ms, stall.up_ms, stall.down_ms, stall.tablet_gap_ms, stall.pc_gap_ms
    );
    let _ = fs::create_dir_all(dir);
    let path = dir.join(format!("stall-{number:03}.csv"));
    let Ok(mut file) = fs::File::create(&path) else { return };
    let _ = writeln!(file, "at_ms,kind,seq,transport_ms,helper_ms,host_ms,rtt_ms,up_ms,down_ms,tablet_gap_ms,pc_gap_ms");
    for e in window {
        let _ = writeln!(
            file,
            "{:.3},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
            e.at_ms,
            if e.is_ping { "ping" } else { "sample" },
            e.seq,
            e.transport_ms,
            e.helper_ms,
            e.host_ms,
            e.rtt_ms,
            e.up_ms,
            e.down_ms,
            e.tablet_gap_ms,
            e.pc_gap_ms
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at_ms: f64, transport_ms: f32) -> Entry {
        Entry { at_ms, transport_ms, ..Entry::default() }
    }

    #[test]
    fn normal_entries_are_not_stalls() {
        let mut r = Recorder::new(PathBuf::new());
        for i in 0..5000 {
            assert!(r.push_inner(sample(i as f64 * 2.0, 1.0)).is_none());
        }
        assert_eq!(r.stalls, 0);
    }

    #[test]
    fn a_stall_returns_the_time_around_it_after_a_delay() {
        let mut r = Recorder::new(PathBuf::new());
        for i in 0..2000 {
            r.push_inner(sample(i as f64, 1.0));
        }
        assert!(r.push_inner(sample(2000.0, 80.0)).is_none(), "waits to capture what follows");
        let mut result = None;
        for i in 2001..3100 {
            if let Some(found) = r.push_inner(sample(i as f64, 1.0)) {
                result = Some(found);
                break;
            }
        }
        let (stall, reason, window) = result.expect("window should complete");
        assert_eq!((stall.at_ms, reason), (2000.0, "sample"));
        assert_eq!(window.first().unwrap().at_ms, 0.0);
        assert_eq!(window.last().unwrap().at_ms, 3000.0);
        assert_eq!(r.stalls, 1);
    }

    #[test]
    fn each_kind_of_stall_is_recognised() {
        assert_eq!(Entry { rtt_ms: 25.0, ..Entry::default() }.stall_reason(), Some("ping"));
        assert_eq!(Entry { tablet_gap_ms: 25.0, ..Entry::default() }.stall_reason(), Some("tablet-gap"));
        assert_eq!(Entry { pc_gap_ms: 25.0, ..Entry::default() }.stall_reason(), Some("pc-gap"));
        assert_eq!(Entry { host_ms: 25.0, ..Entry::default() }.stall_reason(), Some("sendinput"));
    }

    #[test]
    fn repeats_within_the_cooldown_count_once() {
        let mut r = Recorder::new(PathBuf::new());
        r.push_inner(sample(0.0, 80.0));
        r.push_inner(sample(100.0, 80.0));
        r.push_inner(sample(4000.0, 80.0));
        assert_eq!(r.stalls, 1);
        r.push_inner(sample(6000.0, 80.0));
        assert_eq!(r.stalls, 2);
    }
}
