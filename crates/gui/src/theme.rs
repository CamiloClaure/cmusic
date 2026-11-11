//! User-editable look: every colour, radius, gap and font size the GUI draws with. Themes are
//! JSON files in `<config dir>/themes/`; a file may hold only the keys it changes.

use iced::Color;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Name of the built-in look (not a file).
pub const DEFAULT: &str = "Default";

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Look {
    /// Accent gradient start (primary buttons, focus ring, slider fill).
    #[serde(with = "hex")]
    pub accent_a: Color,
    /// Accent gradient end. Same as `accent_a` for a flat accent.
    #[serde(with = "hex")]
    pub accent_b: Color,
    /// Text on an accent fill, and the window background colour.
    #[serde(with = "hex")]
    pub ink: Color,
    #[serde(with = "hex")]
    pub text: Color,
    /// Secondary text: times, dimmed lyrics.
    #[serde(with = "hex")]
    pub text_dim: Color,
    /// Panel fill at the top edge, at 12 % of the height, and at the bottom.
    #[serde(with = "hex")]
    pub panel_top: Color,
    #[serde(with = "hex")]
    pub panel_mid: Color,
    #[serde(with = "hex")]
    pub panel_bottom: Color,
    #[serde(with = "hex")]
    pub panel_border: Color,
    /// Idle pill and field fill; its alpha is the base that hover states add to.
    #[serde(with = "hex")]
    pub control: Color,
    #[serde(with = "hex")]
    pub control_border: Color,
    /// Dimming layer over the backdrop, top and bottom.
    #[serde(with = "hex")]
    pub scrim_top: Color,
    #[serde(with = "hex")]
    pub scrim_bottom: Color,
    /// Fallback gradient behind (or instead of) the backdrop, at 0, 55 % and 100 %.
    #[serde(with = "hex_array")]
    pub base: [Color; 3],
    pub radius_panel: f32,
    pub radius_row: f32,
    pub gap: f32,
    pub pad: f32,
    /// Multiplier for every text size.
    pub font_scale: f32,
    /// Draw the blurred album art behind the UI.
    pub backdrop: bool,
    pub backdrop_blur: f32,
    pub backdrop_saturation: f32,
}

fn c(s: &str) -> Color {
    hex::parse(s).expect("valid built-in colour")
}

impl Default for Look {
    // ponytail: colours are the old float constants rounded to 8 bits so they survive a JSON round trip.
    fn default() -> Self {
        Self {
            accent_a: c("#7de8ff"),
            accent_b: c("#a68cfa"),
            ink: c("#0a0f1f"),
            text: c("#ffffffeb"),
            text_dim: c("#ffffffb3"),
            panel_top: c("#ffffff18"),
            panel_mid: c("#ffffff12"),
            panel_bottom: c("#ffffff0a"),
            panel_border: c("#ffffff38"),
            control: c("#ffffff1a"),
            control_border: c("#ffffff33"),
            scrim_top: c("#05081480"),
            scrim_bottom: c("#050814a0"),
            base: [c("#121a3d"), c("#29174d"), c("#083342")],
            radius_panel: 20.0,
            radius_row: 12.0,
            gap: 14.0,
            pad: 8.0,
            font_scale: 1.0,
            backdrop: true,
            backdrop_blur: 6.0,
            backdrop_saturation: 1.9,
        }
    }
}

impl Look {
    /// A typo must not make the UI unusable.
    fn clamped(mut self) -> Self {
        // The layout holds at 800x500 across these ranges.
        self.radius_panel = self.radius_panel.clamp(0.0, 32.0);
        self.radius_row = self.radius_row.clamp(0.0, 24.0);
        self.gap = self.gap.clamp(0.0, 16.0);
        self.pad = self.pad.clamp(0.0, 16.0);
        self.font_scale = self.font_scale.clamp(0.8, 1.25);
        self.backdrop_blur = self.backdrop_blur.clamp(0.0, 30.0);
        self.backdrop_saturation = self.backdrop_saturation.clamp(0.0, 4.0);
        self
    }
}

/// `"#rrggbb"` / `"#rrggbbaa"` <-> `iced::Color`.
mod hex {
    use super::*;
    use serde::{de, Deserializer, Serializer};

    pub fn parse(s: &str) -> Result<Color, String> {
        let h = s.strip_prefix('#').unwrap_or("");
        if !(h.len() == 6 || h.len() == 8) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "bad colour {s:?}, expected \"#rrggbb\" or \"#rrggbbaa\""
            ));
        }
        let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
        Ok(Color::from_rgba(
            b(0),
            b(2),
            b(4),
            if h.len() == 8 { b(6) } else { 1.0 },
        ))
    }

    pub fn show(c: &Color) -> String {
        let q = |v: f32| (v * 255.0).round() as u8;
        let rgb = format!("#{:02x}{:02x}{:02x}", q(c.r), q(c.g), q(c.b));
        if c.a >= 1.0 {
            rgb
        } else {
            format!("{rgb}{:02x}", q(c.a))
        }
    }

    pub fn serialize<S: Serializer>(c: &Color, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&show(c))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
        let s = String::deserialize(d)?;
        parse(&s).map_err(de::Error::custom)
    }
}

mod hex_array {
    use super::*;
    use serde::{de, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(a: &[Color; 3], s: S) -> Result<S::Ok, S::Error> {
        a.iter().map(hex::show).collect::<Vec<_>>().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[Color; 3], D::Error> {
        let v = Vec::<String>::deserialize(d)?;
        let v: Vec<Color> = v
            .iter()
            .map(|s| hex::parse(s).map_err(de::Error::custom))
            .collect::<Result<_, _>>()?;
        v.try_into()
            .map_err(|_| de::Error::custom("base needs exactly 3 colours"))
    }
}

pub fn themes_dir() -> PathBuf {
    let cookie = cmusic_core::auth::cookie_path();
    cookie
        .parent()
        .map_or_else(|| PathBuf::from("themes"), |p| p.join("themes"))
}

/// The bundled presets, written to the themes dir on first run.
const PRESETS: [(&str, &str); 4] = [
    (
        "flat-dark.json",
        include_str!("../../../themes/flat-dark.json"),
    ),
    ("glass.json", include_str!("../../../themes/glass.json")),
    ("light.json", include_str!("../../../themes/light.json")),
    (
        "midnight-amber.json",
        include_str!("../../../themes/midnight-amber.json"),
    ),
];

/// Creates the themes dir with the presets in it, only if the dir does not exist yet.
pub fn install_presets() {
    install_into(&themes_dir());
}

fn install_into(dir: &std::path::Path) {
    if dir.exists() || std::fs::create_dir_all(dir).is_err() {
        return;
    }
    for (name, json) in PRESETS {
        let _ = std::fs::write(dir.join(name), json);
    }
}

/// Sorted file stems of `*.json` in the themes dir.
pub fn list() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(themes_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    v.sort();
    v
}

pub fn load(name: &str) -> Result<Look, String> {
    if name == DEFAULT {
        return Ok(Look::default());
    }
    let file = format!("{name}.json");
    let text =
        std::fs::read_to_string(themes_dir().join(&file)).map_err(|e| format!("{file}: {e}"))?;
    // serde lists every valid key after "unknown field"; the culprit is what the user needs.
    parse(&text).map_err(|e| {
        let e = e.to_string();
        format!("{file}: {}", e.split(", expected one of").next().unwrap_or(&e))
    })
}

fn parse(text: &str) -> Result<Look, serde_json::Error> {
    serde_json::from_str::<Look>(text).map(Look::clamped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips() {
        let json = serde_json::to_string_pretty(&Look::default()).unwrap();
        assert_eq!(parse(&json).unwrap(), Look::default());
    }

    #[test]
    fn partial_file_changes_only_its_keys() {
        let l = parse(r##"{"accent_a":"#ff0000"}"##).unwrap();
        let want = Look {
            accent_a: Color::from_rgb(1.0, 0.0, 0.0),
            ..Look::default()
        };
        assert_eq!(l, want);
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(parse(r##"{"accent_a":"#ff00"}"##).is_err());
        assert!(parse(r##"{"accent_a":"red"}"##).is_err());
        assert!(parse(r##"{"base":["#000000"]}"##).is_err());
        assert!(parse(r##"{"colour":"#ffffff"}"##).is_err());
    }

    #[test]
    fn numbers_are_clamped() {
        let l =
            parse(r#"{"font_scale":50,"gap":-3,"radius_panel":999,"backdrop_blur":1e6}"#).unwrap();
        assert_eq!((l.font_scale, l.gap, l.radius_panel), (1.25, 0.0, 32.0));
        let l = parse(r#"{"font_scale":0.1,"gap":99,"pad":99,"radius_row":99}"#).unwrap();
        assert_eq!((l.font_scale, l.gap, l.pad, l.radius_row), (0.8, 16.0, 16.0, 24.0));
        assert_eq!(l.backdrop_blur, 30.0);
    }

    #[test]
    fn bundled_presets_parse() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../themes");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let text = std::fs::read_to_string(&p).unwrap();
            parse(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            n += 1;
        }
        assert_eq!(n, PRESETS.len(), "every file in themes/ must be in PRESETS");
        for (name, json) in PRESETS {
            assert!(std::fs::read_to_string(format!("{dir}/{name}")).unwrap() == json);
        }
    }

    #[test]
    fn first_run_installs_once_and_never_overwrites() {
        let d = std::env::temp_dir().join(format!("cmusic-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        install_into(&d);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), PRESETS.len());
        std::fs::write(d.join("glass.json"), "mine").unwrap();
        std::fs::remove_file(d.join("light.json")).unwrap();
        install_into(&d);
        assert_eq!(
            std::fs::read_to_string(d.join("glass.json")).unwrap(),
            "mine"
        );
        assert!(!d.join("light.json").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn glass_preset_is_the_default() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../themes/glass.json"
        ))
        .unwrap();
        assert_eq!(parse(&text).unwrap(), Look::default());
    }
}
