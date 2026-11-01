//! The program's logo, drawn in code so one definition serves the exe, the window and the tray.
//! The build script includes this file too, to put the logo in the exe.

/// A ring in the default theme's accent color, as RGBA pixels.
pub fn ring_rgba(size: u32) -> Vec<u8> {
    let centre = (size as f32 - 1.0) / 2.0;
    let (outer, inner) = (size as f32 * 0.48, size as f32 * 0.28);
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = (x as f32 - centre).hypot(y as f32 - centre);
            // Soft edges, one pixel wide.
            let coverage = (outer - d + 0.5).clamp(0.0, 1.0) * (d - inner + 0.5).clamp(0.0, 1.0);
            pixels.extend_from_slice(&[255, 95, 162, (coverage * 255.0) as u8]);
        }
    }
    pixels
}
