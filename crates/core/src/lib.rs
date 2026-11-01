//! Wire format and evdev pen decoding shared by the tablet helper and the Windows host.

pub const PROTOCOL_VERSION: u16 = 3;
pub const SOCKET_NAME: &str = "osutab";
pub const DOWN_FRAME_LEN: usize = 32;
pub const UP_FRAME_LEN: usize = 16;

pub const FLAG_IN_RANGE: u8 = 1 << 0;
pub const FLAG_TOUCH: u8 = 1 << 1;
pub const FLAG_BUTTON: u8 = 1 << 2;
pub const FLAG_ERASER: u8 = 1 << 3;

const KIND_HELLO: u8 = 1;
const KIND_SAMPLE: u8 = 2;
const KIND_PONG: u8 = 3;
const KIND_PING: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub max_x: u16,
    pub max_y: u16,
    pub max_pressure: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sample {
    pub seq: u32,
    /// Kernel timestamp of the report, CLOCK_MONOTONIC on the tablet.
    pub timestamp_ns: u64,
    pub x: u16,
    pub y: u16,
    pub pressure: u16,
    pub distance: u8,
    pub tilt_x: i8,
    pub tilt_y: i8,
    pub flags: u8,
    /// Total SYN_DROPPED overruns seen by the helper this session.
    pub syn_dropped: u16,
    /// Time from the kernel timestamp to the helper sending this frame.
    pub helper_delay_ns: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub echo_ns: u64,
    pub device_ns: u64,
    /// Longest scheduling gap the helper's watchdog saw since the last pong. 0 when not watching.
    pub tablet_gap_us: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownFrame {
    Hello(Hello),
    Sample(Sample),
    Pong(Pong),
}

impl DownFrame {
    pub fn encode(&self) -> [u8; DOWN_FRAME_LEN] {
        let mut b = [0u8; DOWN_FRAME_LEN];
        match self {
            DownFrame::Hello(h) => {
                b[0] = KIND_HELLO;
                b[2..4].copy_from_slice(&h.version.to_le_bytes());
                b[4..6].copy_from_slice(&h.max_x.to_le_bytes());
                b[6..8].copy_from_slice(&h.max_y.to_le_bytes());
                b[8..10].copy_from_slice(&h.max_pressure.to_le_bytes());
            }
            DownFrame::Sample(s) => {
                b[0] = KIND_SAMPLE;
                b[1] = s.flags;
                b[2..4].copy_from_slice(&s.syn_dropped.to_le_bytes());
                b[4..8].copy_from_slice(&s.seq.to_le_bytes());
                b[8..16].copy_from_slice(&s.timestamp_ns.to_le_bytes());
                b[16..18].copy_from_slice(&s.x.to_le_bytes());
                b[18..20].copy_from_slice(&s.y.to_le_bytes());
                b[20..22].copy_from_slice(&s.pressure.to_le_bytes());
                b[22] = s.distance;
                b[23] = s.tilt_x as u8;
                b[24] = s.tilt_y as u8;
                b[28..32].copy_from_slice(&s.helper_delay_ns.to_le_bytes());
            }
            DownFrame::Pong(p) => {
                b[0] = KIND_PONG;
                b[8..16].copy_from_slice(&p.echo_ns.to_le_bytes());
                b[16..24].copy_from_slice(&p.device_ns.to_le_bytes());
                b[24..28].copy_from_slice(&p.tablet_gap_us.to_le_bytes());
            }
        }
        b
    }

    pub fn decode(b: &[u8; DOWN_FRAME_LEN]) -> Option<DownFrame> {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        match b[0] {
            KIND_HELLO => Some(DownFrame::Hello(Hello {
                version: u16_at(2),
                max_x: u16_at(4),
                max_y: u16_at(6),
                max_pressure: u16_at(8),
            })),
            KIND_SAMPLE => Some(DownFrame::Sample(Sample {
                flags: b[1],
                syn_dropped: u16_at(2),
                seq: u32::from_le_bytes(b[4..8].try_into().unwrap()),
                timestamp_ns: u64_at(8),
                x: u16_at(16),
                y: u16_at(18),
                pressure: u16_at(20),
                distance: b[22],
                tilt_x: b[23] as i8,
                tilt_y: b[24] as i8,
                helper_delay_ns: u32::from_le_bytes(b[28..32].try_into().unwrap()),
            })),
            KIND_PONG => Some(DownFrame::Pong(Pong {
                echo_ns: u64_at(8),
                device_ns: u64_at(16),
                tablet_gap_us: u32::from_le_bytes(b[24..28].try_into().unwrap()),
            })),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpFrame {
    Ping { host_ns: u64 },
}

impl UpFrame {
    pub fn encode(&self) -> [u8; UP_FRAME_LEN] {
        let mut b = [0u8; UP_FRAME_LEN];
        match self {
            UpFrame::Ping { host_ns } => {
                b[0] = KIND_PING;
                b[8..16].copy_from_slice(&host_ns.to_le_bytes());
            }
        }
        b
    }

    pub fn decode(b: &[u8; UP_FRAME_LEN]) -> Option<UpFrame> {
        match b[0] {
            KIND_PING => Some(UpFrame::Ping {
                host_ns: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            }),
            _ => None,
        }
    }
}

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_ABS: u16 = 3;
pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_PRESSURE: u16 = 0x18;
pub const ABS_DISTANCE: u16 = 0x19;
pub const ABS_TILT_X: u16 = 0x1a;
pub const ABS_TILT_Y: u16 = 0x1b;
pub const BTN_TOOL_PEN: u16 = 0x140;
pub const BTN_TOOL_RUBBER: u16 = 0x141;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_STYLUS: u16 = 0x14b;

/// Full device state read back with ioctls after an evdev buffer overrun.
#[derive(Debug, Clone, Copy, Default)]
pub struct PenSnapshot {
    pub x: i32,
    pub y: i32,
    pub pressure: i32,
    pub distance: i32,
    pub tilt_x: i32,
    pub tilt_y: i32,
    pub tool_pen: bool,
    pub tool_rubber: bool,
    pub touch: bool,
    pub button: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Nothing,
    Sample(Sample),
    /// An overrun ended; the caller must read a snapshot and call `apply_snapshot`.
    NeedSnapshot,
}

/// Turns evdev events into one `Sample` per SYN_REPORT.
#[derive(Debug, Default)]
pub struct PenDecoder {
    state: PenSnapshot,
    seq: u32,
    syn_dropped: u16,
    discarding: bool,
}

impl PenDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, ev_type: u16, code: u16, value: i32, timestamp_ns: u64) -> Decoded {
        if ev_type == EV_SYN {
            match code {
                SYN_DROPPED => {
                    self.discarding = true;
                    self.syn_dropped = self.syn_dropped.wrapping_add(1);
                }
                SYN_REPORT if self.discarding => {
                    self.discarding = false;
                    return Decoded::NeedSnapshot;
                }
                SYN_REPORT => return Decoded::Sample(self.emit(timestamp_ns)),
                _ => {}
            }
            return Decoded::Nothing;
        }
        if self.discarding {
            return Decoded::Nothing;
        }
        match (ev_type, code) {
            (EV_ABS, ABS_X) => self.state.x = value,
            (EV_ABS, ABS_Y) => self.state.y = value,
            (EV_ABS, ABS_PRESSURE) => self.state.pressure = value,
            (EV_ABS, ABS_DISTANCE) => self.state.distance = value,
            (EV_ABS, ABS_TILT_X) => self.state.tilt_x = value,
            (EV_ABS, ABS_TILT_Y) => self.state.tilt_y = value,
            (EV_KEY, BTN_TOOL_PEN) => self.state.tool_pen = value != 0,
            (EV_KEY, BTN_TOOL_RUBBER) => self.state.tool_rubber = value != 0,
            (EV_KEY, BTN_TOUCH) => self.state.touch = value != 0,
            (EV_KEY, BTN_STYLUS) => self.state.button = value != 0,
            _ => {}
        }
        Decoded::Nothing
    }

    pub fn apply_snapshot(&mut self, snapshot: PenSnapshot, timestamp_ns: u64) -> Sample {
        self.state = snapshot;
        self.emit(timestamp_ns)
    }

    fn emit(&mut self, timestamp_ns: u64) -> Sample {
        let s = &self.state;
        let mut flags = 0;
        if s.tool_pen || s.tool_rubber || s.touch {
            flags |= FLAG_IN_RANGE;
        }
        if s.touch {
            flags |= FLAG_TOUCH;
        }
        if s.button {
            flags |= FLAG_BUTTON;
        }
        if s.tool_rubber {
            flags |= FLAG_ERASER;
        }
        let sample = Sample {
            seq: self.seq,
            timestamp_ns,
            x: s.x.clamp(0, u16::MAX as i32) as u16,
            y: s.y.clamp(0, u16::MAX as i32) as u16,
            pressure: s.pressure.clamp(0, u16::MAX as i32) as u16,
            distance: s.distance.clamp(0, u8::MAX as i32) as u8,
            tilt_x: s.tilt_x.clamp(i8::MIN as i32, i8::MAX as i32) as i8,
            tilt_y: s.tilt_y.clamp(i8::MIN as i32, i8::MAX as i32) as i8,
            flags,
            syn_dropped: self.syn_dropped,
            helper_delay_ns: 0,
        };
        self.seq = self.seq.wrapping_add(1);
        sample
    }
}

/// Which CPU cores to prefer, as a bitmask, given each core's top speed: everything except the
/// slowest group. `None` when there's no choice to make.
pub fn fast_core_mask(max_speeds: &[u64]) -> Option<u64> {
    let slowest = *max_speeds.iter().min()?;
    let mask = max_speeds
        .iter()
        .enumerate()
        .filter(|(core, speed)| *core < 64 && **speed > slowest)
        .fold(0u64, |mask, (core, _)| mask | 1 << core);
    (mask != 0).then_some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_cores_leave_out_only_the_slowest_group() {
        // This tablet: four slow cores, three faster and one fastest.
        let tab_s8_ultra = [1785600, 1785600, 1785600, 1785600, 2496000, 2496000, 2496000, 2995200];
        assert_eq!(fast_core_mask(&tab_s8_ultra), Some(0b1111_0000));
        assert_eq!(fast_core_mask(&[2000, 2000, 2000]), None, "all the same");
        assert_eq!(fast_core_mask(&[]), None, "unknown");
    }

    #[test]
    fn sample_round_trip() {
        let s = Sample {
            seq: 0xDEADBEEF,
            timestamp_ns: 48484494702000,
            x: 19589,
            y: 31376,
            pressure: 4095,
            distance: 110,
            tilt_x: -63,
            tilt_y: 63,
            flags: FLAG_IN_RANGE | FLAG_TOUCH | FLAG_BUTTON,
            syn_dropped: 7,
            helper_delay_ns: 123_456,
        };
        let f = DownFrame::Sample(s);
        assert_eq!(DownFrame::decode(&f.encode()), Some(f));
    }

    #[test]
    fn hello_and_pong_round_trip() {
        let h = DownFrame::Hello(Hello { version: 1, max_x: 19589, max_y: 31376, max_pressure: 4095 });
        assert_eq!(DownFrame::decode(&h.encode()), Some(h));
        let p = DownFrame::Pong(Pong { echo_ns: 12345, device_ns: u64::MAX - 1, tablet_gap_us: 8000 });
        assert_eq!(DownFrame::decode(&p.encode()), Some(p));
        let u = UpFrame::Ping { host_ns: 987654321 };
        assert_eq!(UpFrame::decode(&u.encode()), Some(u));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        assert_eq!(DownFrame::decode(&[0xFF; DOWN_FRAME_LEN]), None);
        assert_eq!(UpFrame::decode(&[0xFF; UP_FRAME_LEN]), None);
    }

    fn sample_of(d: Decoded) -> Sample {
        match d {
            Decoded::Sample(s) => s,
            other => panic!("expected a sample, got {other:?}"),
        }
    }

    // Replays the start of a real capture from the tablet: pen enters range, then touches.
    #[test]
    fn decodes_hover_then_contact() {
        let mut d = PenDecoder::new();
        assert_eq!(d.push(EV_ABS, ABS_X, 0x25cd, 1), Decoded::Nothing);
        d.push(EV_ABS, ABS_Y, 0x2863, 1);
        d.push(EV_ABS, ABS_TILT_X, -18, 1);
        d.push(EV_ABS, ABS_TILT_Y, -14, 1);
        d.push(EV_KEY, BTN_TOOL_PEN, 1, 1);
        let hover = sample_of(d.push(EV_SYN, SYN_REPORT, 0, 1));
        assert_eq!((hover.x, hover.y), (0x25cd, 0x2863));
        assert_eq!((hover.tilt_x, hover.tilt_y), (-18, -14));
        assert_eq!(hover.flags, FLAG_IN_RANGE);
        assert_eq!(hover.seq, 0);

        d.push(EV_ABS, ABS_X, 0x25e3, 2);
        d.push(EV_KEY, BTN_TOUCH, 1, 2);
        d.push(EV_ABS, ABS_PRESSURE, 1, 2);
        let contact = sample_of(d.push(EV_SYN, SYN_REPORT, 0, 2));
        assert_eq!(contact.flags, FLAG_IN_RANGE | FLAG_TOUCH);
        assert_eq!(contact.y, 0x2863, "unchanged axes keep their last value");
        assert_eq!(contact.seq, 1);
    }

    #[test]
    fn button_and_leaving_range() {
        let mut d = PenDecoder::new();
        d.push(EV_KEY, BTN_TOOL_PEN, 1, 1);
        d.push(EV_KEY, BTN_STYLUS, 1, 1);
        assert_eq!(sample_of(d.push(EV_SYN, SYN_REPORT, 0, 1)).flags, FLAG_IN_RANGE | FLAG_BUTTON);
        d.push(EV_KEY, BTN_STYLUS, 0, 2);
        d.push(EV_KEY, BTN_TOOL_PEN, 0, 2);
        assert_eq!(sample_of(d.push(EV_SYN, SYN_REPORT, 0, 2)).flags, 0);
    }

    #[test]
    fn overrun_discards_until_snapshot() {
        let mut d = PenDecoder::new();
        d.push(EV_KEY, BTN_TOOL_PEN, 1, 1);
        d.push(EV_KEY, BTN_TOUCH, 1, 1);
        d.push(EV_SYN, SYN_REPORT, 0, 1);

        assert_eq!(d.push(EV_SYN, SYN_DROPPED, 0, 2), Decoded::Nothing);
        // A partial report after the overrun must not change state or produce a sample.
        assert_eq!(d.push(EV_ABS, ABS_X, 999, 2), Decoded::Nothing);
        assert_eq!(d.push(EV_SYN, SYN_REPORT, 0, 2), Decoded::NeedSnapshot);

        let s = d.apply_snapshot(PenSnapshot { x: 5, tool_pen: true, ..Default::default() }, 3);
        assert_eq!(s.x, 5);
        assert_eq!(s.flags, FLAG_IN_RANGE, "the touch released during the overrun is cleared");
        assert_eq!(s.syn_dropped, 1);
    }
}
