//! Themes shared by the settings window and the tablet app. The PC owns them and tells the app.

use crate::settings::clean_name;

pub const DEFAULT_THEME: &str = "Midnight";
pub const MIN_THICKNESS: f32 = 1.0;
pub const MAX_THICKNESS: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Reads `#rrggbb`.
    pub fn parse(text: &str) -> Option<Rgb> {
        let hex = text.trim().strip_prefix('#')?;
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(channel(0)?, channel(2)?, channel(4)?))
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// `amount` runs from 0 (this color) to 1 (`other`).
    pub fn blend(self, other: Rgb, amount: f32) -> Rgb {
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
        Rgb(mix(self.0, other.0), mix(self.1, other.1), mix(self.2, other.2))
    }

    pub fn is_light(self) -> bool {
        0.299 * self.0 as f32 + 0.587 * self.1 as f32 + 0.114 * self.2 as f32 > 140.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub background: Rgb,
    pub panel: Rgb,
    pub text: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub outline: Rgb,
    pub outline_thickness: f32,
    /// How dark the tablet is outside the pen area, from 0 to 100.
    pub outside_dim: u8,
}

impl Theme {
    pub fn outside(&self) -> Rgb {
        self.background.blend(Rgb(0, 0, 0), self.outside_dim as f32 / 100.0)
    }
}

/// Changes made on top of a theme. Empty means the theme's own value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ThemeOverrides {
    pub background: Option<Rgb>,
    pub panel: Option<Rgb>,
    pub text: Option<Rgb>,
    pub muted: Option<Rgb>,
    pub accent: Option<Rgb>,
    pub outline: Option<Rgb>,
    pub outline_thickness: Option<f32>,
    pub outside_dim: Option<u8>,
}

impl ThemeOverrides {
    pub fn apply(&self, theme: &Theme) -> Theme {
        Theme {
            name: theme.name.clone(),
            background: self.background.unwrap_or(theme.background),
            panel: self.panel.unwrap_or(theme.panel),
            text: self.text.unwrap_or(theme.text),
            muted: self.muted.unwrap_or(theme.muted),
            accent: self.accent.unwrap_or(theme.accent),
            outline: self.outline.unwrap_or(theme.outline),
            outline_thickness: self.outline_thickness.unwrap_or(theme.outline_thickness),
            outside_dim: self.outside_dim.unwrap_or(theme.outside_dim),
        }
    }
}

const fn hex(value: u32) -> Rgb {
    Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn theme(name: &str, colors: [u32; 6], outline_thickness: f32, outside_dim: u8) -> Theme {
    Theme {
        name: name.to_string(),
        background: hex(colors[0]),
        panel: hex(colors[1]),
        text: hex(colors[2]),
        muted: hex(colors[3]),
        accent: hex(colors[4]),
        outline: hex(colors[5]),
        outline_thickness,
        outside_dim,
    }
}

pub fn built_in() -> Vec<Theme> {
    vec![
        theme(DEFAULT_THEME, [0x14161a, 0x1d2026, 0xeceef2, 0x9aa1ad, 0xff5fa2, 0xff5fa2], 2.0, 55),
        theme("Daylight", [0xf4f4f6, 0xffffff, 0x1c1e23, 0x62666f, 0xd62f7c, 0xd62f7c], 2.0, 35),
        theme("OLED Black", [0x000000, 0x0e0e10, 0xf2f2f2, 0x9a9a9a, 0x4db2ff, 0x4db2ff], 2.0, 100),
        theme("Candy", [0xfff0f6, 0xffffff, 0x3a1a2b, 0x8a5f74, 0xe0318a, 0xe0318a], 2.0, 30),
        theme("High Contrast", [0x000000, 0x000000, 0xffffff, 0xe0e0e0, 0xffe600, 0xffe600], 3.0, 100),
    ]
}

pub fn is_built_in(name: &str) -> bool {
    built_in().iter().any(|t| t.name == name)
}

/// A name a saved theme can use.
pub fn clean_theme_name(name: &str) -> Result<String, String> {
    let name = clean_name(name, "theme")?;
    if is_built_in(&name) {
        return Err(format!("{name} is a built-in theme. Pick another name."));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip() {
        assert_eq!(Rgb::parse("#ff5fa2"), Some(Rgb(255, 95, 162)));
        assert_eq!(Rgb(255, 95, 162).to_hex(), "#ff5fa2");
        for bad in ["", "ff5fa2", "#ff5fa", "#ff5fa2a", "#gg5fa2", "#ÿÿÿ"] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn built_in_themes_include_dark_and_light() {
        let themes = built_in();
        assert!(themes.len() >= 2);
        assert!(themes.iter().any(|t| t.background.is_light()));
        assert!(themes.iter().any(|t| !t.background.is_light()));
        assert_eq!(themes[0].name, DEFAULT_THEME);
    }

    #[test]
    fn text_stands_out_on_every_built_in_theme() {
        for t in built_in() {
            assert_ne!(t.background.is_light(), t.text.is_light(), "{}", t.name);
        }
    }

    #[test]
    fn overrides_only_change_what_they_name() {
        let base = &built_in()[0];
        let changed = ThemeOverrides { outline: Some(Rgb(1, 2, 3)), outside_dim: Some(10), ..Default::default() }.apply(base);
        assert_eq!(changed.outline, Rgb(1, 2, 3));
        assert_eq!(changed.outside_dim, 10);
        assert_eq!(changed.accent, base.accent);
        assert_eq!(ThemeOverrides::default().apply(base), *base);
    }

    #[test]
    fn outside_darkens_with_the_setting() {
        let mut t = built_in()[1].clone();
        t.outside_dim = 0;
        assert_eq!(t.outside(), t.background);
        t.outside_dim = 100;
        assert_eq!(t.outside(), Rgb(0, 0, 0));
    }

    #[test]
    fn saved_themes_cannot_take_a_built_in_name() {
        assert!(clean_theme_name("Daylight").is_err());
        assert!(clean_theme_name("a]b").is_err());
        assert_eq!(clean_theme_name("  Mine ").unwrap(), "Mine");
    }
}
