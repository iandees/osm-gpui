//! How a layer of downloaded OSM GPS traces is drawn and filtered: one colour
//! or coloured by speed, direction or age, the line width, how old a trace may
//! be and whether the undated points of private traces show.
//!
//! The trackpoints API gives each point a position and (for public traces) a
//! time and nothing else, so speed and direction are worked out from
//! neighbouring points (see `reference::TrackAttrs`); there is no elevation or
//! accuracy to colour by.
//!
//! Coloured modes sort each segment into one of a few fixed classes, each with
//! its own colour, so a frame draws one path per class and the legend can name
//! what each colour means.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tr;

/// What decides a trace segment's colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ColorBy {
    #[default]
    Single,
    Speed,
    Direction,
    Age,
}

impl ColorBy {
    pub const ALL: [ColorBy; 4] = [
        ColorBy::Single,
        ColorBy::Speed,
        ColorBy::Direction,
        ColorBy::Age,
    ];

    pub fn label(self) -> String {
        match self {
            ColorBy::Single => tr!("gps_traces.style.color_by.single"),
            ColorBy::Speed => tr!("gps_traces.style.color_by.speed"),
            ColorBy::Direction => tr!("gps_traces.style.color_by.direction"),
            ColorBy::Age => tr!("gps_traces.style.color_by.age"),
        }
        .to_string()
    }
}

/// The colour of the lines (and dots) in `ColorBy::Single`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum LineColor {
    #[default]
    Cyan,
    Magenta,
    Orange,
    Yellow,
    Black,
}

impl LineColor {
    pub const ALL: [LineColor; 5] = [
        LineColor::Cyan,
        LineColor::Magenta,
        LineColor::Orange,
        LineColor::Yellow,
        LineColor::Black,
    ];

    /// `0xRRGGBB`.
    pub fn rgb(self) -> u32 {
        match self {
            LineColor::Cyan => 0x00a5d6,
            LineColor::Magenta => 0xe6007e,
            LineColor::Orange => 0xf08c00,
            LineColor::Yellow => 0xffd400,
            LineColor::Black => 0x202020,
        }
    }

    pub fn label(self) -> String {
        match self {
            LineColor::Cyan => tr!("gps_traces.style.color.cyan"),
            LineColor::Magenta => tr!("gps_traces.style.color.magenta"),
            LineColor::Orange => tr!("gps_traces.style.color.orange"),
            LineColor::Yellow => tr!("gps_traces.style.color.yellow"),
            LineColor::Black => tr!("gps_traces.style.color.black"),
        }
        .to_string()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum LineWidth {
    #[default]
    Thin,
    Medium,
    Thick,
}

impl LineWidth {
    pub const ALL: [LineWidth; 3] = [LineWidth::Thin, LineWidth::Medium, LineWidth::Thick];

    /// Half the line width, in pixels.
    pub fn half_width(self) -> f32 {
        match self {
            LineWidth::Thin => 0.75,
            LineWidth::Medium => 1.5,
            LineWidth::Thick => 2.5,
        }
    }

    /// Radius of a private point's dot, in pixels.
    pub fn dot_radius(self) -> f32 {
        match self {
            LineWidth::Thin => 1.5,
            LineWidth::Medium => 2.0,
            LineWidth::Thick => 2.75,
        }
    }

    pub fn label(self) -> String {
        match self {
            LineWidth::Thin => tr!("gps_traces.style.width.thin"),
            LineWidth::Medium => tr!("gps_traces.style.width.medium"),
            LineWidth::Thick => tr!("gps_traces.style.width.thick"),
        }
        .to_string()
    }
}

/// The oldest trace still drawn, by when it was recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MaxAge {
    #[default]
    All,
    Months6,
    Year1,
    Years2,
    Years5,
    Years10,
}

const YEAR_S: i64 = 365 * 86_400 + 86_400 / 4;

impl MaxAge {
    pub const ALL: [MaxAge; 6] = [
        MaxAge::All,
        MaxAge::Months6,
        MaxAge::Year1,
        MaxAge::Years2,
        MaxAge::Years5,
        MaxAge::Years10,
    ];

    /// The age limit in seconds; `None` for no limit.
    pub fn seconds(self) -> Option<i64> {
        match self {
            MaxAge::All => None,
            MaxAge::Months6 => Some(YEAR_S / 2),
            MaxAge::Year1 => Some(YEAR_S),
            MaxAge::Years2 => Some(2 * YEAR_S),
            MaxAge::Years5 => Some(5 * YEAR_S),
            MaxAge::Years10 => Some(10 * YEAR_S),
        }
    }

    pub fn label(self) -> String {
        match self {
            MaxAge::All => tr!("gps_traces.style.age.all").to_string(),
            MaxAge::Months6 => tr!("gps_traces.style.age.months6").to_string(),
            MaxAge::Year1 => tr!("gps_traces.style.age.year1").to_string(),
            MaxAge::Years2 => tr!("gps_traces.style.age.years", n = 2).to_string(),
            MaxAge::Years5 => tr!("gps_traces.style.age.years", n = 5).to_string(),
            MaxAge::Years10 => tr!("gps_traces.style.age.years", n = 10).to_string(),
        }
    }
}

/// Everything about how a GPS Traces layer draws.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TraceStyle {
    pub color_by: ColorBy,
    pub color: LineColor,
    pub width: LineWidth,
    pub max_age: MaxAge,
    /// Draw the undated points of private traces (they have no time, so the
    /// age filter cannot apply to them).
    pub show_private: bool,
    /// The speeds the `Speed` gradient spreads over.
    pub speed_range: SpeedRange,
}

impl Default for TraceStyle {
    fn default() -> Self {
        TraceStyle {
            color_by: ColorBy::default(),
            color: LineColor::default(),
            width: LineWidth::default(),
            max_age: MaxAge::default(),
            show_private: true,
            speed_range: SpeedRange::default(),
        }
    }
}

/// The speeds the `Speed` gradient runs over, from 0 to the top of the
/// range; anything faster gets the top colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SpeedRange {
    Walking,
    Cycling,
    #[default]
    Town,
    Highway,
}

impl SpeedRange {
    pub const ALL: [SpeedRange; 4] = [
        SpeedRange::Walking,
        SpeedRange::Cycling,
        SpeedRange::Town,
        SpeedRange::Highway,
    ];

    /// The top of the range in km/h, a multiple of 4 so the legend's
    /// quarter ticks are whole numbers.
    pub fn max_kmh(self) -> u32 {
        match self {
            SpeedRange::Walking => 12,
            SpeedRange::Cycling => 40,
            SpeedRange::Town => 80,
            SpeedRange::Highway => 160,
        }
    }

    pub fn label(self) -> String {
        let max = self.max_kmh();
        match self {
            SpeedRange::Walking => tr!("gps_traces.style.speed_range.walking", max = max),
            SpeedRange::Cycling => tr!("gps_traces.style.speed_range.cycling", max = max),
            SpeedRange::Town => tr!("gps_traces.style.speed_range.town", max = max),
            SpeedRange::Highway => tr!("gps_traces.style.speed_range.highway", max = max),
        }
        .to_string()
    }
}

/// One change to a `TraceStyle`, as a menu item makes it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum StyleChange {
    ColorBy(ColorBy),
    Color(LineColor),
    Width(LineWidth),
    MaxAge(MaxAge),
    ShowPrivate(bool),
    /// Also colours by speed: picking a range means wanting to see it.
    SpeedRange(SpeedRange),
    Reset,
}

impl TraceStyle {
    pub fn apply(&mut self, change: StyleChange) {
        match change {
            StyleChange::ColorBy(c) => self.color_by = c,
            StyleChange::Color(c) => self.color = c,
            StyleChange::Width(w) => self.width = w,
            StyleChange::MaxAge(a) => self.max_age = a,
            StyleChange::ShowPrivate(on) => self.show_private = on,
            StyleChange::SpeedRange(r) => {
                self.speed_range = r;
                self.color_by = ColorBy::Speed;
            }
            StyleChange::Reset => *self = TraceStyle::default(),
        }
    }

    /// Traces recorded before this (Unix seconds) are hidden; `None` hides
    /// nothing.
    pub fn cutoff(&self, now: i64) -> Option<i64> {
        self.max_age.seconds().map(|s| now - s)
    }

    /// The colour of every class, `0xRRGGBBAA`: one for `Single`, else one per
    /// class with the "unknown" class last.
    pub fn palette(&self) -> Vec<u32> {
        let (rgb, alpha): (&[u32], u32) = match self.color_by {
            ColorBy::Single => (&[], SINGLE_ALPHA),
            ColorBy::Speed => (speed_colors(), CLASS_ALPHA),
            ColorBy::Direction => (direction_colors(), CLASS_ALPHA),
            ColorBy::Age => (&AGE_COLORS, CLASS_ALPHA),
        };
        if rgb.is_empty() {
            return vec![self.color.rgb() << 8 | alpha];
        }
        rgb.iter()
            .chain(std::iter::once(&UNKNOWN_COLOR))
            .map(|c| c << 8 | alpha)
            .collect()
    }

    /// The index of the "unknown" class in `palette`.
    pub fn unknown_class(&self) -> usize {
        self.palette().len() - 1
    }

    /// The class of a segment: `speed` in m/s and `bearing` in degrees
    /// clockwise from north (NaN when unknown), `start` the trace's first
    /// time, all against `now`.
    pub fn class(&self, speed: f32, bearing: f32, start: Option<i64>, now: i64) -> usize {
        match self.color_by {
            ColorBy::Single => 0,
            ColorBy::Speed => speed_class(speed, self.speed_range),
            ColorBy::Direction => direction_class(bearing),
            ColorBy::Age => start.map_or(AGE_COLORS.len(), |t| age_class(now - t)),
        }
    }

    /// What the colours of a coloured mode mean; `None` for `Single`.
    pub fn legend(&self) -> Option<Legend> {
        let unknown = tr!("gps_traces.legend.unknown").to_string();
        let (colors, labels): (&[u32], Vec<String>) = match self.color_by {
            ColorBy::Single => return None,
            ColorBy::Direction => {
                // The strip ends on the colour it starts with: both ends
                // are north.
                let mut colors = direction_colors().to_vec();
                colors.push(colors[0]);
                return Some(Legend::Gradient {
                    colors,
                    ticks: [
                        tr!("gps_traces.legend.dir.n"),
                        tr!("gps_traces.legend.dir.e"),
                        tr!("gps_traces.legend.dir.s"),
                        tr!("gps_traces.legend.dir.w"),
                        tr!("gps_traces.legend.dir.n"),
                    ]
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect(),
                    unknown: (UNKNOWN_COLOR, unknown),
                });
            }
            ColorBy::Speed => {
                let max = self.speed_range.max_kmh();
                let mut ticks: Vec<String> = (0..4).map(|q| (max * q / 4).to_string()).collect();
                ticks.push(tr!("gps_traces.legend.kmh_max", n = max).to_string());
                return Some(Legend::Gradient {
                    colors: speed_colors().to_vec(),
                    ticks,
                    unknown: (UNKNOWN_COLOR, unknown),
                });
            }
            ColorBy::Age => (
                &AGE_COLORS,
                range_labels(
                    &AGE_EDGES_YEARS,
                    |n| tr!("gps_traces.legend.years.below", n = n),
                    |a, b| tr!("gps_traces.legend.years.range", a = a, b = b),
                    |n| tr!("gps_traces.legend.years.above", n = n),
                ),
            ),
        };
        Some(Legend::Classes(
            colors
                .iter()
                .copied()
                .zip(labels)
                .chain(std::iter::once((UNKNOWN_COLOR, unknown)))
                .collect(),
        ))
    }
}

/// What the colours of a coloured mode mean.
#[derive(Clone, Debug, PartialEq)]
pub enum Legend {
    /// One `(0xRRGGBB, label)` row per class, "unknown" last.
    Classes(Vec<(u32, String)>),
    /// A colour that changes smoothly: `colors` (`0xRRGGBB`) from one end to
    /// the other, `ticks` labels spread evenly along them (the first and last
    /// at the ends), and the colour of the unknown.
    Gradient {
        colors: Vec<u32>,
        ticks: Vec<String>,
        unknown: (u32, String),
    },
}

/// `< a`, `a–b`, ..., `≥ z` for class edges.
fn range_labels<S: ToString, R: ToString, A: ToString>(
    edges: &[u32],
    below: impl Fn(u32) -> S,
    range: impl Fn(u32, u32) -> R,
    above: impl Fn(u32) -> A,
) -> Vec<String> {
    let mut out = Vec::with_capacity(edges.len() + 1);
    out.push(below(edges[0]).to_string());
    out.extend(edges.windows(2).map(|w| range(w[0], w[1]).to_string()));
    out.push(above(edges[edges.len() - 1]).to_string());
    out
}

/// The translucent single colour lets well-travelled paths build up darker.
const SINGLE_ALPHA: u32 = 0xb3;
/// Coloured classes are more opaque, so the colour reads.
const CLASS_ALPHA: u32 = 0xd9;
const UNKNOWN_COLOR: u32 = 0x8c8c8c;

/// Shades of the speed gradient.
const SPEED_CLASSES: usize = 32;
/// The speed ramp, slow (dark blue) to fast (red), as `(position, 0xRRGGBB)`
/// stops; the shades between are mixed from the stops either side.
const SPEED_STOPS: [(f32, u32); 6] = [
    (0.0, 0x2c3e9e),
    (0.25, 0x1fa2d6),
    (0.5, 0x4cb82e),
    (0.7, 0xd9b800),
    (0.85, 0xf08c00),
    (1.0, 0xc8102e),
];

fn speed_colors() -> &'static [u32; SPEED_CLASSES] {
    static COLORS: std::sync::OnceLock<[u32; SPEED_CLASSES]> = std::sync::OnceLock::new();
    COLORS.get_or_init(|| {
        std::array::from_fn(|i| ramp(&SPEED_STOPS, i as f32 / (SPEED_CLASSES - 1) as f32))
    })
}

/// The colour at `t` (0..=1) along `stops`.
fn ramp(stops: &[(f32, u32)], t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let i = stops
        .windows(2)
        .position(|w| t <= w[1].0)
        .unwrap_or(stops.len() - 2);
    let ((t0, c0), (t1, c1)) = (stops[i], stops[i + 1]);
    let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
    let mix = |shift: u32| {
        let (a, b) = (((c0 >> shift) & 0xff) as f32, ((c1 >> shift) & 0xff) as f32);
        ((a + (b - a) * f).round() as u32) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// Directions are coloured in steps of this many degrees.
const DIRECTION_STEP_DEG: f32 = 10.0;
const DIRECTION_CLASSES: usize = (360.0 / DIRECTION_STEP_DEG) as usize;

/// A colour wheel with the hue following the bearing (north red, east
/// yellow-green, south cyan, west violet): the colour turns smoothly with the
/// road, and opposite directions get opposite hues, so the two sides of a
/// dual carriageway stand apart.
fn direction_colors() -> &'static [u32; DIRECTION_CLASSES] {
    static COLORS: std::sync::OnceLock<[u32; DIRECTION_CLASSES]> = std::sync::OnceLock::new();
    COLORS
        .get_or_init(|| std::array::from_fn(|i| hsl_rgb(i as f32 * DIRECTION_STEP_DEG, 0.85, 0.45)))
}

/// `hue` in degrees, `s` and `l` in 0..=1, as `0xRRGGBB`.
fn hsl_rgb(hue: f32, s: f32, l: f32) -> u32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let byte = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// Age class edges in years.
const AGE_EDGES_YEARS: [u32; 5] = [1, 2, 3, 5, 10];
/// New (red) to old (purple).
const AGE_COLORS: [u32; 6] = [0xd7191c, 0xf08c00, 0xc9b200, 0x5cb82e, 0x1fa2d6, 0x6a51a3];

fn speed_class(mps: f32, range: SpeedRange) -> usize {
    if !mps.is_finite() || mps < 0.0 {
        return SPEED_CLASSES;
    }
    let t = (mps * 3.6 / range.max_kmh() as f32).min(1.0);
    (t * (SPEED_CLASSES - 1) as f32).round() as usize
}

fn direction_class(bearing: f32) -> usize {
    if !bearing.is_finite() {
        return DIRECTION_CLASSES;
    }
    let b = bearing.rem_euclid(360.0);
    ((b / DIRECTION_STEP_DEG).round() as usize) % DIRECTION_CLASSES
}

fn age_class(age_s: i64) -> usize {
    AGE_EDGES_YEARS
        .iter()
        .position(|&e| age_s < e as i64 * YEAR_S)
        .unwrap_or(AGE_EDGES_YEARS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_spread_over_the_chosen_range() {
        let mut s = TraceStyle {
            color_by: ColorBy::Speed,
            ..TraceStyle::default()
        };
        let kmh = |s: &TraceStyle, v: f32| s.class(v / 3.6, f32::NAN, None, 0);
        // Town: 0–80 km/h over 32 shades.
        assert_eq!(kmh(&s, 0.0), 0);
        assert_eq!(kmh(&s, 40.0), 16);
        assert_eq!(kmh(&s, 80.0), 31);
        assert_eq!(kmh(&s, 200.0), 31);
        assert_eq!(s.class(f32::NAN, 0.0, None, 0), s.unknown_class());
        assert_eq!(s.palette().len(), 33);
        // Cycling: 20 km/h is half way, 40 the top.
        s.apply(StyleChange::SpeedRange(SpeedRange::Cycling));
        assert_eq!(kmh(&s, 20.0), 16);
        assert_eq!(kmh(&s, 50.0), 31);
        let Some(Legend::Gradient { colors, ticks, .. }) = s.legend() else {
            panic!("speed is a gradient");
        };
        assert_eq!(colors.len(), 32);
        assert_eq!(ticks[..4], ["0", "10", "20", "30"]);
        assert!(ticks[4].contains("40"));
    }

    #[test]
    fn picking_a_speed_range_colours_by_speed() {
        let mut s = TraceStyle::default();
        s.apply(StyleChange::SpeedRange(SpeedRange::Highway));
        assert_eq!(s.color_by, ColorBy::Speed);
        assert_eq!(s.speed_range.max_kmh(), 160);
    }

    #[test]
    fn the_speed_ramp_runs_blue_to_red_through_its_stops() {
        let c = speed_colors();
        assert_eq!(c[0], 0x2c3e9e);
        assert_eq!(c[SPEED_CLASSES - 1], 0xc8102e);
        assert_eq!(ramp(&SPEED_STOPS, 0.5), 0x4cb82e);
        // Half way between two stops is half way between their colours.
        assert_eq!(ramp(&[(0.0, 0x000000), (1.0, 0x204060)], 0.5), 0x102030);
    }

    #[test]
    fn bearings_turn_through_a_colour_wheel() {
        let s = TraceStyle {
            color_by: ColorBy::Direction,
            ..TraceStyle::default()
        };
        let c = |b: f32| s.class(1.0, b, None, 0);
        assert_eq!(c(0.0), 0);
        assert_eq!(c(4.0), 0);
        assert_eq!(c(356.0), 0);
        assert_eq!(c(-10.0), 35);
        assert_eq!(c(90.0), 9);
        assert_eq!(c(180.0), 18);
        assert_eq!(c(270.0), 27);
        assert_eq!(c(f32::NAN), s.unknown_class());
        assert_eq!(s.palette().len(), 37);
        // North red, south cyan.
        let rgb = |class: usize| s.palette()[class] >> 8;
        assert_eq!(rgb(0), 0xd41111);
        assert_eq!(rgb(18) >> 16, 0x11);
        // Neighbouring steps are close in colour: a gradient, not classes.
        let channels = |c: u32| [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
        for k in 0..36 {
            let (a, b) = (channels(rgb(k)), channels(rgb((k + 1) % 36)));
            let d: u32 = a.iter().zip(b).map(|(x, y)| x.abs_diff(y)).sum();
            assert!(d <= 80, "steps {k} and {} differ by {d}", k + 1);
        }
        let Some(Legend::Gradient { colors, ticks, .. }) = s.legend() else {
            panic!("direction is a gradient");
        };
        // All the way round, back to north.
        assert_eq!(colors.len(), 37);
        assert_eq!(colors[0], colors[36]);
        assert_eq!(ticks.len(), 5);
    }

    #[test]
    fn ages_count_back_from_now() {
        let s = TraceStyle {
            color_by: ColorBy::Age,
            ..TraceStyle::default()
        };
        let now = 2_000_000_000;
        assert_eq!(s.class(0.0, 0.0, Some(now - 86_400), now), 0);
        assert_eq!(s.class(0.0, 0.0, Some(now - 4 * YEAR_S), now), 3);
        assert_eq!(s.class(0.0, 0.0, Some(now - 30 * YEAR_S), now), 5);
        assert_eq!(s.class(0.0, 0.0, None, now), s.unknown_class());
    }

    #[test]
    fn single_colour_is_one_translucent_class() {
        let mut s = TraceStyle::default();
        assert_eq!(s.palette(), vec![0x00a5d6b3]);
        assert!(s.legend().is_none());
        s.apply(StyleChange::Color(LineColor::Black));
        assert_eq!(s.palette(), vec![0x202020b3]);
        assert_eq!(s.class(99.0, 99.0, None, 0), 0);
    }

    #[test]
    fn the_age_filter_sets_a_cutoff() {
        let mut s = TraceStyle::default();
        assert_eq!(s.cutoff(1000), None);
        s.apply(StyleChange::MaxAge(MaxAge::Year1));
        assert_eq!(s.cutoff(YEAR_S * 3), Some(YEAR_S * 2));
        s.apply(StyleChange::Reset);
        assert_eq!(s, TraceStyle::default());
    }

    #[test]
    fn a_style_round_trips_through_json_and_old_files_fill_in_defaults() {
        let s = TraceStyle {
            color_by: ColorBy::Age,
            color: LineColor::Orange,
            width: LineWidth::Thick,
            max_age: MaxAge::Years5,
            show_private: false,
            speed_range: SpeedRange::Cycling,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<TraceStyle>(&json).unwrap(), s);
        let partial: TraceStyle = serde_json::from_str(r#"{"width":"Medium"}"#).unwrap();
        assert_eq!(partial.width, LineWidth::Medium);
        assert!(partial.show_private);
    }
}
