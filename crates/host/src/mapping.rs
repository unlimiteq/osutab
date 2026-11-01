//! Converts raw pen coordinates to SendInput's normalized absolute coordinates.
//! Every step is linear and uses f64; nothing is rounded before the final conversion.

// The digitizer's 19589 x 31376 range at 0.01 mm per unit, which matches the 14.6" panel.
pub const TABLET_LONG_MM: f64 = 313.76;
pub const TABLET_SHORT_MM: f64 = 195.89;
// The tablet's 2960 x 1848 display across that range.
pub const TABLET_PX_PER_MM: f64 = 2960.0 / TABLET_LONG_MM;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub fn from_android(value: u32) -> Option<Rotation> {
        match value {
            0 => Some(Rotation::R0),
            1 => Some(Rotation::R90),
            2 => Some(Rotation::R180),
            3 => Some(Rotation::R270),
            _ => None,
        }
    }

    pub fn is_portrait(self) -> bool {
        matches!(self, Rotation::R0 | Rotation::R180)
    }

    /// The tablet's width and height in mm as the user sees it.
    pub fn tablet_size_mm(self) -> (f64, f64) {
        if self.is_portrait() {
            (TABLET_SHORT_MM, TABLET_LONG_MM)
        } else {
            (TABLET_LONG_MM, TABLET_SHORT_MM)
        }
    }
}

/// Raw sensor position to a 0..1 position as the user sees the tablet, using the same
/// transform Android applies for each display rotation.
pub fn to_unit(x: u16, y: u16, max_x: u16, max_y: u16, rotation: Rotation) -> (f64, f64) {
    let nx = x as f64 / max_x.max(1) as f64;
    let ny = y as f64 / max_y.max(1) as f64;
    match rotation {
        Rotation::R0 => (nx, ny),
        Rotation::R90 => (ny, 1.0 - nx),
        Rotation::R180 => (1.0 - nx, 1.0 - ny),
        Rotation::R270 => (1.0 - ny, nx),
    }
}

/// Pen area as the user sees the tablet. Offsets are from the centre of the tablet, and the
/// rotation turns the area clockwise around its own centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AreaMm {
    pub width: f64,
    pub height: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    pub rotation_deg: f64,
}

impl AreaMm {
    /// Fits the area on the tablet in this orientation. It is shrunk and moved if it doesn't fit.
    pub fn resolve(&self, rotation: Rotation) -> ActiveArea {
        let (tablet_w, tablet_h) = rotation.tablet_size_mm();
        let width = self.width.min(tablet_w);
        let height = self.height.min(tablet_h);
        let (sin, cos) = self.rotation_deg.to_radians().sin_cos();
        let half_x = (width * cos.abs() + height * sin.abs()) / 2.0;
        let half_y = (width * sin.abs() + height * cos.abs()) / 2.0;
        let fit = |wanted: f64, half: f64, size: f64| {
            if half * 2.0 >= size {
                size / 2.0
            } else {
                wanted.clamp(half, size - half)
            }
        };
        ActiveArea {
            center_x: fit(tablet_w / 2.0 + self.offset_x, half_x, tablet_w),
            center_y: fit(tablet_h / 2.0 + self.offset_y, half_y, tablet_h),
            sin,
            cos,
            width_mm: width,
            height_mm: height,
            tablet_w,
            tablet_h,
        }
    }
}

/// A resolved pen area in mm on the tablet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActiveArea {
    pub center_x: f64,
    pub center_y: f64,
    sin: f64,
    cos: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    pub tablet_w: f64,
    pub tablet_h: f64,
}

impl ActiveArea {
    /// A position on the whole tablet to a position in the area. Outside the area, it sticks to the edge.
    pub fn map(&self, u: f64, v: f64) -> (f64, f64) {
        let dx = u * self.tablet_w - self.center_x;
        let dy = v * self.tablet_h - self.center_y;
        let x = dx * self.cos + dy * self.sin;
        let y = -dx * self.sin + dy * self.cos;
        (
            ((x + self.width_mm / 2.0) / self.width_mm).clamp(0.0, 1.0),
            ((y + self.height_mm / 2.0) / self.height_mm).clamp(0.0, 1.0),
        )
    }

    /// The area's corners in mm on the tablet, clockwise from its top-left.
    pub fn corners(&self) -> [(f64, f64); 4] {
        let (hw, hh) = (self.width_mm / 2.0, self.height_mm / 2.0);
        [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(x, y)| {
            (self.center_x + x * self.cos - y * self.sin, self.center_y + x * self.sin + y * self.cos)
        })
    }

    /// Screen pixels crossed per mm of pen movement, across and down the area.
    pub fn px_per_mm(&self, target: &Rect) -> (f64, f64) {
        (target.width / self.width_mm, target.height / self.height_mm)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn pixel(&self, u: f64, v: f64) -> (f64, f64) {
        (self.left + u * self.width, self.top + v * self.height)
    }

    /// The whole pixel holding this position. The far edges belong to the last pixel, because a
    /// position of exactly 2560 on a 2560-wide screen can only ever show on pixel 2559.
    pub fn pixel_index(&self, exact: (f64, f64)) -> (i32, i32) {
        (
            exact.0.floor().min(self.left + self.width - 1.0) as i32,
            exact.1.floor().min(self.top + self.height - 1.0) as i32,
        )
    }
}

/// A 0..1 position on `target` to the 0..=65535 range SendInput uses across the virtual desktop.
/// The cursor can only sit on whole pixels, so this aims at the middle of the right one. Aiming at
/// the exact position instead could land a pixel short, because that range doesn't divide evenly
/// into most screen widths, and Windows doesn't document how it rounds.
pub fn to_absolute(u: f64, v: f64, target: &Rect, desktop: &Rect) -> (i32, i32) {
    let (column, row) = target.pixel_index(target.pixel(u, v));
    let nx = (column as f64 + 0.5 - desktop.left) / desktop.width * 65536.0;
    let ny = (row as f64 + 0.5 - desktop.top) / desktop.height * 65536.0;
    (nx.clamp(0.0, 65535.0) as i32, ny.clamp(0.0, 65535.0) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_X: u16 = 19589;
    const MAX_Y: u16 = 31376;

    fn area(width: f64, height: f64, offset_x: f64, offset_y: f64, rotation_deg: f64) -> AreaMm {
        AreaMm { width, height, offset_x, offset_y, rotation_deg }
    }

    #[test]
    fn rotation_corners() {
        let f = |x, y, r| to_unit(x, y, MAX_X, MAX_Y, r);
        assert_eq!(f(0, 0, Rotation::R0), (0.0, 0.0));
        assert_eq!(f(MAX_X, MAX_Y, Rotation::R0), (1.0, 1.0));
        assert_eq!(f(0, 0, Rotation::R90), (0.0, 1.0));
        assert_eq!(f(MAX_X, MAX_Y, Rotation::R90), (1.0, 0.0));
        assert_eq!(f(0, 0, Rotation::R180), (1.0, 1.0));
        assert_eq!(f(0, 0, Rotation::R270), (1.0, 0.0));
        assert_eq!(f(MAX_X, MAX_Y, Rotation::R270), (0.0, 1.0));
    }

    fn close(a: (f64, f64), b: (f64, f64)) {
        assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9, "{a:?} != {b:?}");
    }

    #[test]
    fn full_area_is_identity() {
        let active = area(TABLET_LONG_MM, TABLET_SHORT_MM, 0.0, 0.0, 0.0).resolve(Rotation::R90);
        close(active.map(0.0, 0.0), (0.0, 0.0));
        close(active.map(0.25, 0.75), (0.25, 0.75));
        close(active.map(1.0, 1.0), (1.0, 1.0));
    }

    #[test]
    fn mapping_is_linear_and_repeatable() {
        // No acceleration: equal pen steps give equal output steps, wherever they start.
        let active = area(100.0, 62.5, 0.0, 0.0, 0.0).resolve(Rotation::R90);
        let at = |mm: f64| active.map(0.5 + mm / TABLET_LONG_MM, 0.5).0;
        let step = at(1.0) - at(0.0);
        for start in [-40.0, -10.0, 0.0, 25.0, 45.0] {
            assert!((at(start + 1.0) - at(start) - step).abs() < 1e-12);
        }
        assert_eq!(at(12.5), at(12.5));
    }

    #[test]
    fn centred_area_covers_whole_range_and_clamps() {
        let active = area(100.0, 62.5, 0.0, 0.0, 0.0).resolve(Rotation::R90);
        close(active.map(0.5, 0.5), (0.5, 0.5));
        // 50 mm right of centre and 31.25 mm below centre is the area's corner.
        close(active.map(0.5 + 50.0 / TABLET_LONG_MM, 0.5 + 31.25 / TABLET_SHORT_MM), (1.0, 1.0));
        close(active.map(0.5 - 50.0 / TABLET_LONG_MM, 0.5 - 31.25 / TABLET_SHORT_MM), (0.0, 0.0));
        close(active.map(0.0, 1.0), (0.0, 1.0));
    }

    #[test]
    fn offset_moves_the_area() {
        let active = area(100.0, 100.0, 20.0, -10.0, 0.0).resolve(Rotation::R90);
        close(active.map(0.5 + 20.0 / TABLET_LONG_MM, 0.5 - 10.0 / TABLET_SHORT_MM), (0.5, 0.5));
    }

    #[test]
    fn oversized_area_is_fitted_to_portrait() {
        let active = area(300.0, 100.0, 50.0, 0.0, 0.0).resolve(Rotation::R0);
        assert_eq!(active.width_mm, TABLET_SHORT_MM);
        close(active.map(0.0, 0.5), (0.0, 0.5));
        close(active.map(1.0, 0.5), (1.0, 0.5));
    }

    #[test]
    fn offset_past_the_edge_stays_on_the_tablet() {
        let active = area(100.0, 100.0, 1000.0, 1000.0, 0.0).resolve(Rotation::R90);
        close(active.map(1.0, 1.0), (1.0, 1.0));
    }

    #[test]
    fn rotated_area_maps_its_corners() {
        let active = area(100.0, 50.0, 0.0, 0.0, 30.0).resolve(Rotation::R90);
        let corners = active.corners();
        let to_uv = |c: (f64, f64)| (c.0 / TABLET_LONG_MM, c.1 / TABLET_SHORT_MM);
        close(active.map(to_uv(corners[0]).0, to_uv(corners[0]).1), (0.0, 0.0));
        close(active.map(to_uv(corners[1]).0, to_uv(corners[1]).1), (1.0, 0.0));
        close(active.map(to_uv(corners[2]).0, to_uv(corners[2]).1), (1.0, 1.0));
        close(active.map(to_uv(corners[3]).0, to_uv(corners[3]).1), (0.0, 1.0));
        close(active.map(0.5, 0.5), (0.5, 0.5));
    }

    #[test]
    fn quarter_turn_swaps_directions() {
        let active = area(100.0, 100.0, 0.0, 0.0, 90.0).resolve(Rotation::R90);
        // Turned clockwise, the area's "right" points down the tablet.
        close(active.map(0.5, 0.5 + 25.0 / TABLET_SHORT_MM), (0.75, 0.5));
    }

    #[test]
    fn every_pixel_is_reached_however_windows_rounds() {
        // The two ways Windows could plausibly turn the range back into a pixel.
        let down = |n: i32, size: f64| (n as f64 * size / 65536.0).floor() as i32;
        let across = |n: i32, size: f64| (n as f64 * (size - 1.0) / 65535.0).round() as i32;
        for size in [1280.0, 1366.0, 1600.0, 1920.0, 2160.0, 2560.0, 3440.0, 3840.0] {
            let screen = Rect { left: 0.0, top: 0.0, width: size, height: size };
            for pixel in 0..size as i32 {
                // A position just inside the pixel, its middle, and just before the next one.
                for fraction in [0.001, 0.5, 0.999] {
                    let u = (pixel as f64 + fraction) / size;
                    let (n, _) = to_absolute(u, 0.5, &screen, &screen);
                    assert_eq!(down(n, size), pixel, "size {size} pixel {pixel} fraction {fraction}");
                    assert!((across(n, size) - pixel).abs() <= 1, "size {size} pixel {pixel}");
                }
            }
        }
    }

    #[test]
    fn a_second_monitor_lands_on_the_right_pixels() {
        let right = Rect { left: 2560.0, top: 0.0, width: 1920.0, height: 1080.0 };
        let left = Rect { left: -1920.0, top: 0.0, width: 1920.0, height: 1080.0 };
        let desktop = Rect { left: 0.0, top: 0.0, width: 4480.0, height: 1600.0 };
        let leftmost = Rect { left: -1920.0, top: 0.0, width: 4480.0, height: 1600.0 };
        let pixel = |n: i32, d: &Rect| d.left + (n as f64 * d.width / 65536.0).floor();
        let (x, _) = to_absolute(0.0, 0.0, &right, &desktop);
        assert_eq!(pixel(x, &desktop), 2560.0);
        let (x, _) = to_absolute(1.0, 1.0, &right, &desktop);
        assert_eq!(pixel(x, &desktop), 2560.0 + 1919.0);
        let (x, _) = to_absolute(0.5, 0.5, &left, &leftmost);
        assert_eq!(pixel(x, &leftmost), -960.0);
    }

    #[test]
    fn the_far_edges_stay_on_the_screen() {
        let screen = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        let (x, y) = to_absolute(1.0, 1.0, &screen, &screen);
        assert!(x <= 65535 && y <= 65535);
        assert_eq!((x as f64 * 2560.0 / 65536.0).floor(), 2559.0);
        assert_eq!((y as f64 * 1600.0 / 65536.0).floor(), 1599.0);
    }

    #[test]
    fn sensitivity_readout() {
        let screen = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        let active = area(100.0, 62.5, 0.0, 0.0, 0.0).resolve(Rotation::R90);
        assert_eq!(active.px_per_mm(&screen), (25.6, 25.6));
    }

    #[test]
    fn far_edges_belong_to_the_last_pixel() {
        // A pen pinned past the area's edge maps to exactly the screen's width and height. Waiting
        // for a pixel that doesn't exist once slowed every report down by a full timeout.
        let screen = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        assert_eq!(screen.pixel_index(screen.pixel(1.0, 1.0)), (2559, 1599));
        assert_eq!(screen.pixel_index(screen.pixel(0.0, 0.0)), (0, 0));
        assert_eq!(screen.pixel_index((1234.9, 567.1)), (1234, 567));
        let second = Rect { left: -1920.0, top: 100.0, width: 1920.0, height: 1080.0 };
        assert_eq!(second.pixel_index(second.pixel(1.0, 1.0)), (-1, 1179));
    }

    #[test]
    fn single_display_range() {
        let screen = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        // The middles of the first, last and center pixels.
        assert_eq!(to_absolute(0.0, 0.0, &screen, &screen), (12, 20));
        assert_eq!(to_absolute(1.0, 1.0, &screen, &screen), (65523, 65515));
        assert_eq!(to_absolute(0.5, 0.5, &screen, &screen), (32780, 32788));
    }

    #[test]
    fn target_within_larger_desktop() {
        // A second monitor to the left puts the primary display in the right half.
        let desktop = Rect { left: -2560.0, top: 0.0, width: 5120.0, height: 1600.0 };
        let primary = Rect { left: 0.0, top: 0.0, width: 2560.0, height: 1600.0 };
        assert_eq!(to_absolute(0.0, 0.0, &primary, &desktop), (32774, 20));
        assert_eq!(to_absolute(1.0, 1.0, &primary, &desktop), (65529, 65515));
    }
}
