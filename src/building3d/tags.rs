//! Reading (and writing) the Simple 3D Buildings tags of one building or
//! building part: heights, levels, roof shape and colours.
//!
//! See <https://wiki.openstreetmap.org/wiki/Simple_3D_Buildings>.

use std::collections::HashMap;

/// Height of one storey, used to turn `building:levels` / `min_level` /
/// `roof:levels` into metres when no explicit height is tagged.
pub const LEVEL_HEIGHT_M: f64 = 3.0;

/// Height given to a building or part with no height or level tags at all.
pub const DEFAULT_HEIGHT_M: f64 = 6.0;

/// Roof height for a non-flat roof with no `roof:height` / `roof:levels`.
pub const DEFAULT_ROOF_HEIGHT_M: f64 = 3.0;

/// Thinnest a part is ever drawn, so a zero-height (or inverted) part stays
/// visible and pickable.
pub const MIN_THICKNESS_M: f64 = 0.2;

/// The roof shapes the 3D view draws. Shapes it doesn't model are drawn as a
/// pyramid over the footprint, the closest one-size-fits-all shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoofShape {
    Flat,
    Pyramidal,
    /// Two slopes meeting at a ridge, which runs along the footprint's
    /// longest side — or across it with `roof:orientation=across`.
    Gabled {
        across: bool,
    },
    /// Any other tagged shape (`gabled`, `hipped`, `dome`, ...), drawn as a
    /// pyramid; the tag value is kept for display.
    Other(String),
}

impl RoofShape {
    pub fn is_flat(&self) -> bool {
        matches!(self, RoofShape::Flat)
    }
}

/// The vertical extent of a building part, in metres above the ground.
#[derive(Clone, Debug, PartialEq)]
pub struct Heights {
    /// Bottom of the walls (`min_height`, or `building:min_level` storeys).
    pub min_height: f64,
    /// Top of the whole part, roof included (`height`).
    pub height: f64,
    /// How much of `height` is roof; 0 for a flat roof.
    pub roof_height: f64,
}

impl Heights {
    /// Top of the walls, where the roof starts.
    pub fn wall_top(&self) -> f64 {
        (self.height - self.roof_height).max(self.min_height)
    }
}

/// Parse an OSM length such as `12`, `12.5 m`, `40 ft`, `40'` or `12'6"`
/// into metres. A decimal comma is accepted. `None` for anything else.
pub fn parse_length(raw: &str) -> Option<f64> {
    let s = raw.trim().replace(',', ".");
    if s.is_empty() {
        return None;
    }
    // Feet and inches: 12'6" (inches optional).
    if let Some((feet, rest)) = s.split_once('\'') {
        let feet: f64 = feet.trim().parse().ok()?;
        let rest = rest.trim().trim_end_matches('"').trim();
        let inches: f64 = if rest.is_empty() {
            0.0
        } else {
            rest.parse().ok()?
        };
        return Some((feet * 12.0 + inches) * 0.0254);
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    match unit.trim().to_ascii_lowercase().as_str() {
        "" | "m" | "meter" | "meters" | "metre" | "metres" => Some(value),
        "ft" | "feet" | "foot" => Some(value * 0.3048),
        "cm" => Some(value / 100.0),
        _ => None,
    }
}

fn number(tags: &HashMap<String, String>, key: &str) -> Option<f64> {
    tags.get(key)
        .and_then(|v| v.trim().replace(',', ".").parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn length(tags: &HashMap<String, String>, key: &str) -> Option<f64> {
    tags.get(key).and_then(|v| parse_length(v))
}

/// The roof shape a part is tagged with (`roof:shape`), flat by default.
pub fn roof_shape(tags: &HashMap<String, String>) -> RoofShape {
    match tags.get("roof:shape").map(|s| s.trim()) {
        None | Some("") | Some("flat") => RoofShape::Flat,
        Some("pyramidal") => RoofShape::Pyramidal,
        Some("gabled") => RoofShape::Gabled {
            across: tags
                .get("roof:orientation")
                .is_some_and(|o| o.trim() == "across"),
        },
        Some(other) => RoofShape::Other(other.to_string()),
    }
}

/// Work out a part's vertical extent from its tags, the way 3D renderers
/// do: explicit metres win over storeys, storeys over the default.
pub fn heights(tags: &HashMap<String, String>) -> Heights {
    let shape = roof_shape(tags);
    let roof_height = if shape.is_flat() {
        0.0
    } else {
        length(tags, "roof:height")
            .or_else(|| number(tags, "roof:levels").map(|l| l * LEVEL_HEIGHT_M))
            .unwrap_or(DEFAULT_ROOF_HEIGHT_M)
            .max(0.0)
    };
    let min_height = length(tags, "min_height")
        .or_else(|| number(tags, "building:min_level").map(|l| l * LEVEL_HEIGHT_M))
        .unwrap_or(0.0)
        .max(0.0);
    let height = length(tags, "height")
        .or_else(|| {
            number(tags, "building:levels").map(|l| {
                // `building:levels` counts storeys below the roof; a roof
                // with its own levels sits on top.
                let roof_levels = number(tags, "roof:levels").unwrap_or(0.0);
                (l + roof_levels) * LEVEL_HEIGHT_M
                    + if roof_levels > 0.0 { 0.0 } else { roof_height }
            })
        })
        .unwrap_or(min_height.max(0.0) + DEFAULT_HEIGHT_M);
    let height = height.max(min_height + MIN_THICKNESS_M);
    let roof_height = roof_height.min(height - min_height);
    Heights {
        min_height,
        height,
        roof_height,
    }
}

/// Format a height in metres as a tag value: at most two decimals, no
/// trailing zeros (`12`, `12.5`, `3.25`).
pub fn format_height(m: f64) -> String {
    let rounded = (m * 100.0).round() / 100.0;
    let s = format!("{rounded:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Parse a `building:colour` / `roof:colour` value: `#rgb`, `#rrggbb`, or
/// one of the common colour names. Linear-ish RGB in 0..1.
pub fn parse_colour(raw: &str) -> Option<[f32; 3]> {
    let s = raw.trim().to_ascii_lowercase();
    let hex = |h: &str| u32::from_str_radix(h, 16).ok();
    if let Some(h) = s.strip_prefix('#') {
        let v = match h.len() {
            3 => {
                let v = hex(h)?;
                let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                (r * 17) << 16 | (g * 17) << 8 | (b * 17)
            }
            6 => hex(h)?,
            _ => return None,
        };
        return Some(rgb(v));
    }
    let v = match s.as_str() {
        "white" => 0xffffff,
        "black" => 0x202020,
        "grey" | "gray" => 0x9a9a9a,
        "lightgrey" | "lightgray" | "light_grey" => 0xc8c8c8,
        "darkgrey" | "darkgray" | "dark_grey" => 0x5a5a5a,
        "silver" => 0xc0c0c0,
        "red" => 0xb83a2e,
        "darkred" => 0x7a1e18,
        "brown" => 0x7b5338,
        "maroon" => 0x6e2a22,
        "orange" => 0xd98a3a,
        "yellow" => 0xe6cf5c,
        "beige" => 0xe0d2a8,
        "tan" => 0xc8a878,
        "cream" => 0xf0e6c8,
        "green" => 0x4f8a45,
        "darkgreen" => 0x2f5a2a,
        "olive" => 0x7a7a3a,
        "blue" => 0x3d6fb3,
        "lightblue" => 0x9cc3e6,
        "navy" => 0x26365c,
        "teal" => 0x2e7d7a,
        "pink" => 0xe0a0b0,
        "purple" => 0x7a4a8a,
        "sandstone" => 0xd8c39a,
        "brick" => 0x9c4a32,
        _ => return None,
    };
    Some(rgb(v))
}

fn rgb(v: u32) -> [f32; 3] {
    [
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_lengths_in_common_units() {
        assert_eq!(parse_length("12"), Some(12.0));
        assert_eq!(parse_length(" 12.5 m"), Some(12.5));
        assert_eq!(parse_length("12,5"), Some(12.5));
        assert!((parse_length("10 ft").unwrap() - 3.048).abs() < 1e-9);
        assert!((parse_length("10'").unwrap() - 3.048).abs() < 1e-9);
        assert!((parse_length("10'6\"").unwrap() - 3.2004).abs() < 1e-9);
        assert_eq!(parse_length("tall"), None);
        assert_eq!(parse_length(""), None);
        assert_eq!(parse_length("12 furlongs"), None);
    }

    #[test]
    fn explicit_height_wins_over_levels() {
        let h = heights(&tags(&[("height", "20"), ("building:levels", "3")]));
        assert_eq!(h.height, 20.0);
        assert_eq!(h.min_height, 0.0);
        assert_eq!(h.roof_height, 0.0);
    }

    #[test]
    fn levels_give_three_metres_each() {
        let h = heights(&tags(&[
            ("building:levels", "4"),
            ("building:min_level", "1"),
        ]));
        assert_eq!(h.height, 12.0);
        assert_eq!(h.min_height, 3.0);
    }

    #[test]
    fn untagged_part_gets_the_default_height() {
        let h = heights(&tags(&[]));
        assert_eq!(h.height, DEFAULT_HEIGHT_M);
        let h = heights(&tags(&[("min_height", "10")]));
        assert_eq!(h.height, 10.0 + DEFAULT_HEIGHT_M);
    }

    #[test]
    fn inverted_heights_keep_a_minimum_thickness() {
        let h = heights(&tags(&[("height", "5"), ("min_height", "8")]));
        assert_eq!(h.min_height, 8.0);
        assert!((h.height - (8.0 + MIN_THICKNESS_M)).abs() < 1e-9);
    }

    #[test]
    fn pitched_roof_height_is_part_of_height() {
        let h = heights(&tags(&[
            ("height", "10"),
            ("roof:shape", "gabled"),
            ("roof:height", "4"),
        ]));
        assert_eq!(h.height, 10.0);
        assert_eq!(h.roof_height, 4.0);
        assert_eq!(h.wall_top(), 6.0);
        // No roof height tagged: the default.
        let h = heights(&tags(&[("height", "10"), ("roof:shape", "pyramidal")]));
        assert_eq!(h.roof_height, DEFAULT_ROOF_HEIGHT_M);
        // Flat roofs ignore roof:height.
        let h = heights(&tags(&[("height", "10"), ("roof:height", "4")]));
        assert_eq!(h.roof_height, 0.0);
    }

    #[test]
    fn levels_plus_pitched_roof() {
        let h = heights(&tags(&[("building:levels", "2"), ("roof:shape", "hipped")]));
        assert_eq!(h.height, 6.0 + DEFAULT_ROOF_HEIGHT_M);
        assert_eq!(h.wall_top(), 6.0);
        let h = heights(&tags(&[
            ("building:levels", "2"),
            ("roof:levels", "1"),
            ("roof:shape", "hipped"),
        ]));
        assert_eq!(h.height, 9.0);
        assert_eq!(h.roof_height, 3.0);
    }

    #[test]
    fn roof_shapes() {
        assert_eq!(roof_shape(&tags(&[])), RoofShape::Flat);
        assert_eq!(
            roof_shape(&tags(&[("roof:shape", "pyramidal")])),
            RoofShape::Pyramidal
        );
        assert_eq!(
            roof_shape(&tags(&[("roof:shape", "gabled")])),
            RoofShape::Gabled { across: false }
        );
        assert_eq!(
            roof_shape(&tags(&[
                ("roof:shape", "gabled"),
                ("roof:orientation", "across")
            ])),
            RoofShape::Gabled { across: true }
        );
        assert_eq!(
            roof_shape(&tags(&[("roof:shape", "dome")])),
            RoofShape::Other("dome".into())
        );
    }

    #[test]
    fn formats_heights_without_noise() {
        assert_eq!(format_height(12.0), "12");
        assert_eq!(format_height(12.5), "12.5");
        assert_eq!(format_height(3.25631), "3.26");
        assert_eq!(format_height(0.001), "0");
        assert_eq!(format_height(-0.001), "0");
    }

    #[test]
    fn parses_colours() {
        assert_eq!(parse_colour("#ffffff"), Some([1.0, 1.0, 1.0]));
        assert_eq!(parse_colour("#F00"), Some([1.0, 0.0, 0.0]));
        assert!(parse_colour("brown").is_some());
        assert!(parse_colour("Grey").is_some());
        assert_eq!(parse_colour("#12"), None);
        assert_eq!(parse_colour("plaid"), None);
    }
}
