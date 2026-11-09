//! Smoothing for direct output, so a shaky hand doesn't need OpenTabletDriver just for its filters.
//!
//! Both filters work in millimetres on the tablet, before the pen area clamps the position to its
//! edges, and both start over whenever the pen leaves range, so the cursor never slides in from
//! where the pen was lifted. With both off, positions pass through untouched.
//!
//! Radial follow has the same settings as AbstractQbit's Radial Follow Smoothing plugin for
//! OpenTabletDriver, and is written from that plugin's published description. None of its code is
//! used.

use std::ops::RangeInclusive;

use osutab_core::FLAG_IN_RANGE;

use crate::mapping::Rotation;
use crate::settings::Output;
use crate::shared::LiveConfig;

pub const MAX_RADIUS_MM: f64 = 20.0;
pub const RADIUS_MM: RangeInclusive<f64> = 0.0..=MAX_RADIUS_MM;
pub const MIN_SMOOTHING: f64 = 0.0001;
pub const SMOOTHING: RangeInclusive<f64> = MIN_SMOOTHING..=1.0;
pub const MAX_SOFT_KNEE: f64 = 100.0;
pub const SOFT_KNEE: RangeInclusive<f64> = 0.0..=MAX_SOFT_KNEE;
pub const LEAK: RangeInclusive<f64> = 0.0..=1.0;
pub const MIN_TIME_MS: f64 = 0.5;
pub const MAX_TIME_MS: f64 = 100.0;
pub const TIME_MS: RangeInclusive<f64> = MIN_TIME_MS..=MAX_TIME_MS;

/// Holds the cursor still through small wobbles, and follows bigger moves quickly. The cursor
/// trails the pen by at most the outer radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialFollow {
    pub on: bool,
    /// The furthest the cursor can trail the pen, in mm. With some leak, it's where smoothing
    /// starts to fade instead.
    pub outer_radius_mm: f64,
    /// Pen moves within this distance of the cursor don't move it, in mm. Treated as no more than
    /// the outer radius.
    pub inner_radius_mm: f64,
    /// How much of the cursor's lag beyond the inner radius each report keeps. Higher is smoother
    /// and slower to settle. Per report, like the plugin, so values carry over from OpenTabletDriver
    /// with this tablet.
    pub smoothing: f64,
    /// How gently following with smoothing turns into holding at the outer radius. 0 is a sharp
    /// corner. Most of the change happens between 0 and 3.
    pub soft_knee: f64,
    /// How much smoothing carries on past the outer radius, from none to all of it.
    pub leak: f64,
}

impl Default for RadialFollow {
    /// The plugin's defaults for tablet millimetres.
    fn default() -> Self {
        RadialFollow { on: false, outer_radius_mm: 1.0, inner_radius_mm: 0.0, smoothing: 0.95, soft_knee: 1.0, leak: 0.0 }
    }
}

/// An average over time: each report moves the cursor part of the way to the pen, by how much time
/// has passed. Timed by the reports' own timestamps, so the report rate doesn't change how it feels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeSmoothing {
    pub on: bool,
    /// After this long, the cursor has caught up about two-thirds of a move. Slow moves trail the
    /// pen by about this much time.
    pub ms: f64,
}

impl Default for TimeSmoothing {
    fn default() -> Self {
        TimeSmoothing { on: false, ms: 5.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FilterSettings {
    pub radial: RadialFollow,
    pub time: TimeSmoothing,
}

impl FilterSettings {
    pub fn any_on(&self) -> bool {
        self.radial.on || self.time.on
    }
}

impl RadialFollow {
    /// How far the cursor trails the pen after this report, given how far it is from the pen's
    /// new position. Never more than `distance`, so the cursor only ever moves toward the pen.
    ///
    /// Within the inner radius the cursor stays put. Beyond it, each report keeps `smoothing` of
    /// the lag past the inner radius, until that would leave the cursor further than the outer
    /// radius. From there the lag stops growing, or with leak grows at a fraction of the rate. The
    /// soft knee rounds off the corner between the two.
    pub fn lag_after(&self, distance: f64) -> f64 {
        let outer = self.outer_radius_mm.max(0.0);
        let inner = self.inner_radius_mm.clamp(0.0, outer);
        if distance <= inner {
            return distance;
        }
        let keep = self.smoothing.clamp(MIN_SMOOTHING, 1.0);
        let leak = self.leak.clamp(0.0, 1.0);
        let beyond = distance - inner;
        // Where plain smoothing would leave the cursor exactly at the outer radius.
        let corner = (outer - inner) / keep;
        // Never wider than the room down to the inner radius, so the deadzone's edge stays exact.
        let half_width = corner * (1.0 - (-self.soft_knee.max(0.0)).exp());
        inner + keep * beyond - keep * (1.0 - leak) * soft_ramp(beyond - corner, half_width)
    }
}

/// `x.max(0.0)` with its corner rounded off between `-half_width` and `half_width`, by a parabola
/// that meets both sides at the same slope.
fn soft_ramp(x: f64, half_width: f64) -> f64 {
    if x <= -half_width {
        0.0
    } else if x >= half_width {
        x
    } else {
        (x + half_width).powi(2) / (4.0 * half_width)
    }
}

/// The filters' memory between reports. Lives on the input thread, one per session.
#[derive(Debug, Default)]
pub struct Filters {
    /// Where radial follow left the cursor, in mm.
    radial: Option<(f64, f64)>,
    /// Where time smoothing left the cursor, in mm, and the report's timestamp.
    smoothed: Option<((f64, f64), u64)>,
    /// The orientation the positions above were measured in.
    rotation: Option<Rotation>,
}

impl Filters {
    pub fn reset(&mut self) {
        self.radial = None;
        self.smoothed = None;
    }

    /// The input thread's way in. `u` and `v` are the pen's 0..1 position on the whole tablet, and
    /// come back filtered the same way. Only direct output is filtered: OpenTabletDriver mode has
    /// its own filters.
    pub fn apply(&mut self, live: &LiveConfig, flags: u8, timestamp_ns: u64, u: f64, v: f64) -> (f64, f64) {
        if live.output != Output::Direct || !live.filters.any_on() || flags & FLAG_IN_RANGE == 0 {
            self.reset();
            return (u, v);
        }
        // Turning the tablet moves every position, so the last ones mean nothing now.
        if self.rotation != Some(live.rotation) {
            self.reset();
            self.rotation = Some(live.rotation);
        }
        let (width, height) = (live.area.tablet_w, live.area.tablet_h);
        let pen = (u * width, v * height);
        let filtered = self.filter(&live.filters, true, timestamp_ns, pen);
        // Untouched positions come back exactly, rather than through a round trip in mm.
        if filtered == pen {
            (u, v)
        } else {
            (filtered.0 / width, filtered.1 / height)
        }
    }

    /// One report, with the pen's position in mm on the tablet.
    pub fn filter(&mut self, settings: &FilterSettings, in_range: bool, timestamp_ns: u64, pen: (f64, f64)) -> (f64, f64) {
        if !in_range {
            self.reset();
            return pen;
        }
        let mut position = pen;
        // A filter that's off forgets, so turning it on starts from the pen.
        if settings.time.on {
            position = self.smooth_over_time(settings.time.ms, timestamp_ns, position);
        } else {
            self.smoothed = None;
        }
        if settings.radial.on {
            position = self.follow(&settings.radial, position);
        } else {
            self.radial = None;
        }
        position
    }

    fn smooth_over_time(&mut self, ms: f64, timestamp_ns: u64, pen: (f64, f64)) -> (f64, f64) {
        let position = match self.smoothed {
            Some((last, last_ns)) if ms > 0.0 => {
                // Reports out of order count as no time passing.
                let elapsed_ms = timestamp_ns.saturating_sub(last_ns) as f64 / 1e6;
                let keep = (-elapsed_ms / ms).exp();
                (pen.0 + (last.0 - pen.0) * keep, pen.1 + (last.1 - pen.1) * keep)
            }
            _ => pen,
        };
        self.smoothed = Some((position, timestamp_ns));
        position
    }

    fn follow(&mut self, settings: &RadialFollow, pen: (f64, f64)) -> (f64, f64) {
        let position = match self.radial {
            None => pen,
            Some(cursor) => {
                let (dx, dy) = (cursor.0 - pen.0, cursor.1 - pen.1);
                let distance = dx.hypot(dy);
                let lag = settings.lag_after(distance);
                if lag >= distance {
                    // Exactly where it was, rather than nearly, so a still cursor stays still.
                    cursor
                } else {
                    let scale = lag / distance;
                    (pen.0 + dx * scale, pen.1 + dy * scale)
                }
            }
        };
        self.radial = Some(position);
        position
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::mapping::Rect;
    use crate::settings::Settings;
    use crate::shared::{Overrides, Shared};

    // The S Pen reports about 480 times a second.
    const STEP_NS: u64 = 2_080_000;

    fn radial(outer: f64, inner: f64) -> FilterSettings {
        FilterSettings { radial: RadialFollow { on: true, outer_radius_mm: outer, inner_radius_mm: inner, ..RadialFollow::default() }, ..FilterSettings::default() }
    }

    fn timed(ms: f64) -> FilterSettings {
        FilterSettings { time: TimeSmoothing { on: true, ms }, ..FilterSettings::default() }
    }

    fn both() -> FilterSettings {
        FilterSettings { radial: radial(1.0, 0.2).radial, time: timed(5.0).time }
    }

    /// Feeds positions in range, one report apart, and returns what came out.
    fn run(filters: &mut Filters, settings: &FilterSettings, pen: &[(f64, f64)]) -> Vec<(f64, f64)> {
        pen.iter().enumerate().map(|(i, &p)| filters.filter(settings, true, i as u64 * STEP_NS, p)).collect()
    }

    fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
        (a.0 - b.0).hypot(a.1 - b.1)
    }

    /// How far positions stray from their average, as a root mean square, in mm.
    fn spread(points: &[(f64, f64)]) -> f64 {
        let n = points.len() as f64;
        let mean = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
        (points.iter().map(|&p| distance(p, mean).powi(2)).sum::<f64>() / n).sqrt()
    }

    /// A shaky but still hand: up to 0.1 mm either way on each axis, in no simple rhythm.
    fn jitter(count: usize) -> Vec<(f64, f64)> {
        (0..count).map(|i| (120.0 + 0.1 * (i as f64 * 1.7).sin(), 80.0 + 0.1 * (i as f64 * 2.3).cos())).collect()
    }

    fn live(settings: Settings) -> LiveConfig {
        let target = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        Shared::new(settings, PathBuf::new(), Overrides::default(), target, true).live_config()
    }

    #[test]
    fn a_still_pen_stays_still() {
        for settings in [radial(1.0, 0.0), radial(1.0, 0.3), timed(5.0), both()] {
            let pen = vec![(100.3, 50.7); 200];
            assert_eq!(run(&mut Filters::default(), &settings, &pen), pen, "{settings:?}");
        }
    }

    #[test]
    fn jitter_smaller_than_the_inner_radius_is_held_still() {
        // Any two of these positions are less than 0.3 mm apart.
        let pen = jitter(300);
        let out = run(&mut Filters::default(), &radial(1.0, 0.3), &pen);
        assert!(out.iter().all(|&p| p == out[0]), "the cursor moved");
        assert!(spread(&pen) > 0.05);
        // Not exactly 0 only because averaging the positions rounds.
        assert!(spread(&out) < 1e-9);
    }

    #[test]
    fn jitter_is_reduced_by_smoothing_alone() {
        let pen = jitter(300);
        for settings in [radial(1.0, 0.0), timed(5.0), both()] {
            let out = run(&mut Filters::default(), &settings, &pen);
            // After it has settled on the hand.
            let (before, after) = (spread(&pen[100..]), spread(&out[100..]));
            assert!(after < before / 2.0, "{settings:?}: {after} vs {before}");
        }
    }

    /// A fast jump across most of a small pen area: 120 mm at about 1.9 m/s, then still.
    fn fast_move() -> (Vec<(f64, f64)>, usize) {
        let moving = 30;
        let pen: Vec<_> = (0..=moving).map(|i| (60.0 + 4.0 * i as f64, 100.0)).chain(std::iter::repeat((180.0, 100.0)).take(100)).collect();
        (pen, moving)
    }

    #[test]
    fn radial_follow_keeps_up_with_a_fast_long_move() {
        let (pen, stop) = fast_move();
        let settings = radial(1.0, 0.0);
        let out = run(&mut Filters::default(), &settings, &pen);
        // Never further behind than the outer radius, from the first report of the move.
        for (i, (&p, &o)) in pen.iter().zip(&out).enumerate() {
            assert!(distance(p, o) <= 1.0 + 1e-9, "report {i} trailed by {}", distance(p, o));
        }
        // Once the pen stops, the last millimetre closes up too.
        let settled = out[stop..].iter().position(|&o| distance(o, pen[stop]) < 0.05).unwrap();
        assert!(settled <= 60, "took {settled} reports to settle");
    }

    #[test]
    fn time_smoothing_catches_up_within_a_few_reports_of_a_fast_move() {
        let (pen, stop) = fast_move();
        let out = run(&mut Filters::default(), &timed(5.0), &pen);
        let settled = out[stop..].iter().position(|&o| distance(o, pen[stop]) < 0.1).unwrap();
        // About 25 ms, or five times the smoothing time.
        assert!(settled <= 12, "took {settled} reports to catch up");
    }

    #[test]
    fn a_big_jump_is_followed_at_once() {
        let mut filters = Filters::default();
        let settings = radial(1.0, 0.3);
        let out = run(&mut filters, &settings, &[(50.0, 50.0), (150.0, 90.0)]);
        assert!((distance(out[1], (150.0, 90.0)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn lifting_the_pen_clears_the_state() {
        for settings in [radial(1.0, 0.3), timed(20.0), both()] {
            let mut filters = Filters::default();
            run(&mut filters, &settings, &[(40.0, 40.0); 50]);
            let lifted = filters.filter(&settings, false, 51 * STEP_NS, (40.0, 40.0));
            assert_eq!(lifted, (40.0, 40.0));
            let back = filters.filter(&settings, true, 52 * STEP_NS, (200.0, 150.0));
            assert_eq!(back, (200.0, 150.0), "{settings:?} slid in from where the pen was lifted");
        }
    }

    #[test]
    fn lifting_the_pen_clears_the_state_on_the_input_thread_too() {
        let mut settings = Settings::default();
        settings.filters = both();
        let live = live(settings);
        let mut filters = Filters::default();
        for i in 0..50 {
            filters.apply(&live, FLAG_IN_RANGE, i * STEP_NS, 0.2, 0.2);
        }
        filters.apply(&live, 0, 50 * STEP_NS, 0.2, 0.2);
        assert_eq!(filters.apply(&live, FLAG_IN_RANGE, 51 * STEP_NS, 0.9, 0.7), (0.9, 0.7));
    }

    #[test]
    fn with_the_filters_off_the_output_is_exactly_the_input() {
        let positions = [(0.0, 0.0), (1.0, 1.0), (0.1, 0.2), (0.123456789, 0.987654321), (1.0 / 3.0, 2.0 / 3.0), (0.5, 0.5)];
        let live = live(Settings::default());
        assert!(!live.filters.any_on(), "the filters start off");
        let mut filters = Filters::default();
        for (i, &(u, v)) in positions.iter().enumerate() {
            for flags in [FLAG_IN_RANGE, 0] {
                assert_eq!(filters.apply(&live, flags, i as u64 * STEP_NS, u, v), (u, v));
            }
        }
        let off = FilterSettings::default();
        let pen: Vec<_> = positions.iter().map(|&(u, v)| (u * 313.76, v * 195.89)).collect();
        assert_eq!(run(&mut Filters::default(), &off, &pen), pen);
    }

    #[test]
    fn opentabletdriver_mode_is_never_filtered() {
        let mut settings = Settings { output: Output::OpenTabletDriver, ..Settings::default() };
        settings.filters = both();
        let live = live(settings);
        let mut filters = Filters::default();
        for i in 0..20 {
            let u = 0.1 + i as f64 * 0.04;
            assert_eq!(filters.apply(&live, FLAG_IN_RANGE, i * STEP_NS, u, 0.5), (u, 0.5));
        }
    }

    #[test]
    fn time_smoothing_feels_the_same_at_any_report_rate() {
        // The same move, reported at 480 and 240 times a second, compared at the same moments.
        let pen_at = |t_ns: u64| if t_ns == 0 { (0.0, 0.0) } else { (30.0 + (t_ns as f64 / 1e7).sin(), 10.0) };
        let settings = timed(8.0);
        let mut fast = Filters::default();
        let mut slow = Filters::default();
        for i in 0..200u64 {
            let t = i * STEP_NS;
            let quick = fast.filter(&settings, true, t, pen_at(t));
            if i % 2 == 0 {
                let steady = slow.filter(&settings, true, t, pen_at(t));
                // Not identical, because the slow one misses the pen's in-between positions.
                assert!(distance(quick, steady) < 0.15, "at {t} ns: {quick:?} vs {steady:?}");
            }
        }
        // For a pen that holds still, the two agree exactly however the reports fall.
        let mut fast = Filters::default();
        let mut slow = Filters::default();
        fast.filter(&settings, true, 0, (0.0, 0.0));
        slow.filter(&settings, true, 0, (0.0, 0.0));
        for i in 1..=10u64 {
            let quick = fast.filter(&settings, true, i * STEP_NS, (10.0, 0.0));
            if i % 2 == 0 {
                let steady = slow.filter(&settings, true, i * STEP_NS, (10.0, 0.0));
                assert!((quick.0 - steady.0).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn turning_a_filter_on_starts_from_the_pen() {
        let mut filters = Filters::default();
        run(&mut filters, &timed(20.0), &[(10.0, 10.0); 20]);
        // Off for a while, during which the pen moves far away.
        run(&mut filters, &FilterSettings::default(), &[(90.0, 90.0); 5]);
        assert_eq!(filters.filter(&timed(20.0), true, 100 * STEP_NS, (90.0, 90.0)), (90.0, 90.0));
    }

    #[test]
    fn turning_the_tablet_starts_over() {
        let mut settings = Settings { rotation: Some(Rotation::R90), ..Settings::default() };
        settings.filters = timed(50.0);
        let mut filters = Filters::default();
        let landscape = live(settings.clone());
        for i in 0..20 {
            filters.apply(&landscape, FLAG_IN_RANGE, i * STEP_NS, 0.1, 0.1);
        }
        settings.rotation = Some(Rotation::R270);
        let turned = live(settings);
        assert_eq!(filters.apply(&turned, FLAG_IN_RANGE, 20 * STEP_NS, 0.9, 0.9), (0.9, 0.9));
    }

    #[test]
    fn the_radial_curve_only_ever_closes_in_and_stays_inside_the_outer_radius() {
        for outer in [0.0, 0.5, 1.0, 3.0] {
            for inner in [0.0, 0.3, 1.0, 5.0] {
                for smoothing in [MIN_SMOOTHING, 0.5, 0.95, 1.0] {
                    for soft_knee in [0.0, 0.5, 1.0, 10.0, MAX_SOFT_KNEE] {
                        for leak in [0.0, 0.3, 1.0] {
                            let r = RadialFollow { on: true, outer_radius_mm: outer, inner_radius_mm: inner, smoothing, soft_knee, leak };
                            let mut previous = 0.0;
                            for step in 0..=2000 {
                                let d = step as f64 * 0.01;
                                let lag = r.lag_after(d);
                                assert!(lag.is_finite() && lag >= 0.0 && lag <= d + 1e-12, "{r:?} at {d}: {lag}");
                                assert!(lag >= previous - 1e-12, "{r:?} at {d}: went backwards");
                                if leak == 0.0 {
                                    assert!(lag <= outer + 1e-9, "{r:?} at {d}: {lag} past the outer radius");
                                }
                                previous = lag;
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_deadzone_edge_has_no_step() {
        // Just past the inner radius, the cursor barely moves, whatever the knee.
        for soft_knee in [0.0, 1.0, MAX_SOFT_KNEE] {
            let r = RadialFollow { on: true, inner_radius_mm: 0.5, soft_knee, ..RadialFollow::default() };
            assert_eq!(r.lag_after(0.5), 0.5);
            assert!(0.5001 - r.lag_after(0.5001) < 1e-4, "knee {soft_knee}");
        }
    }

    #[test]
    fn full_smoothing_with_no_knee_drags_the_cursor_at_the_outer_radius() {
        let r = RadialFollow { on: true, smoothing: 1.0, soft_knee: 0.0, ..RadialFollow::default() };
        assert_eq!(r.lag_after(0.7), 0.7, "inside the outer radius the cursor stays put");
        assert_eq!(r.lag_after(3.0), 1.0);
        let leaky = RadialFollow { leak: 1.0, ..r };
        assert_eq!(leaky.lag_after(3.0), 3.0, "full leak never holds the cursor back");
    }
}
