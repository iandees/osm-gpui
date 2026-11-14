use gpui::{px, Pixels, Point as GpuiPoint, Size};
use std::f64::consts::PI;

/// A geographic position in degrees. Named fields carry the order, and the
/// type keeps it from being passed where Web Mercator metres are expected.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    #[inline]
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// For code that still passes `(lat, lon)` pairs. Each use marks a place
    /// not yet converted to `LatLon`.
    #[inline]
    pub const fn from_pair((lat, lon): (f64, f64)) -> Self {
        Self { lat, lon }
    }

    /// The `(lat, lon)` pair; see [`LatLon::from_pair`].
    #[inline]
    pub const fn to_pair(self) -> (f64, f64) {
        (self.lat, self.lon)
    }
}

/// A position in Web Mercator (EPSG:3857) metres: `x` grows east, `y` north.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Mercator {
    pub x: f64,
    pub y: f64,
}

impl Mercator {
    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Represents a geographic bounding box (axis-aligned, inclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    pub fn new(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> Self {
        Self {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        }
    }

    pub fn contains(&self, at: LatLon) -> bool {
        at.lat >= self.min_lat
            && at.lat <= self.max_lat
            && at.lon >= self.min_lon
            && at.lon <= self.max_lon
    }

    /// Grow `bounds` to include `at`; `None` becomes the box of that one point.
    pub fn extend(bounds: &mut Option<GeoBounds>, at: LatLon) {
        let b = bounds.get_or_insert(GeoBounds::new(at.lat, at.lat, at.lon, at.lon));
        b.min_lat = b.min_lat.min(at.lat);
        b.max_lat = b.max_lat.max(at.lat);
        b.min_lon = b.min_lon.min(at.lon);
        b.max_lon = b.max_lon.max(at.lon);
    }

    /// The smallest box holding both `self` and `other`.
    pub fn union(&self, other: &GeoBounds) -> GeoBounds {
        GeoBounds::new(
            self.min_lat.min(other.min_lat),
            self.max_lat.max(other.max_lat),
            self.min_lon.min(other.min_lon),
            self.max_lon.max(other.max_lon),
        )
    }

    pub fn width(&self) -> f64 {
        self.max_lon - self.min_lon
    }

    pub fn height(&self) -> f64 {
        self.max_lat - self.min_lat
    }

    pub fn center(&self) -> LatLon {
        LatLon::new(
            (self.min_lat + self.max_lat) / 2.0,
            (self.min_lon + self.max_lon) / 2.0,
        )
    }
}

/// Web Mercator projection utilities
pub fn lat_lon_to_mercator(at: LatLon) -> Mercator {
    // Clamp latitude to avoid projection issues at poles
    let clamped_lat = at.lat.clamp(-85.051128779807, 85.051128779807);

    let x = at.lon * MERCATOR_HALF_WORLD_M / 180.0;
    let y = ((PI / 4.0) + (clamped_lat * PI / 360.0)).tan().ln() * MERCATOR_HALF_WORLD_M / PI;

    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };

    Mercator { x, y }
}

/// Half the width of the Web Mercator world in metres (the projection's
/// radius times pi); the extent of EPSG:3857 on both axes.
pub const MERCATOR_HALF_WORLD_M: f64 = 20037508.34;

/// Equatorial circumference in metres, as the zoom-to-scale maths uses it.
/// Slightly different from `2 * MERCATOR_HALF_WORLD_M`; both are kept because
/// each is the value its callers are calibrated against.
pub const WORLD_WIDTH_M: f64 = 40075016.686;

/// Latitude where Web Mercator ends: the projection is undefined beyond it.
const MAX_MERCATOR_LAT: f64 = 85.051128779807;

/// Inverse of `lat_lon_to_mercator`: latitude clamped to the projection's
/// limit, longitude clamped to [-180, 180].
pub fn mercator_to_lat_lon(at: Mercator) -> LatLon {
    let LatLon { lat, lon } = unproject(at);
    let lon = if lon.is_finite() {
        lon.clamp(-180.0, 180.0)
    } else {
        0.0
    };
    LatLon { lat, lon }
}

/// Like `mercator_to_lat_lon`, but a longitude beyond the antimeridian wraps
/// around into [-180, 180] instead of sticking to the edge.
pub fn mercator_to_lat_lon_wrapped(at: Mercator) -> LatLon {
    let LatLon { lat, mut lon } = unproject(at);
    if !(-180.0..=180.0).contains(&lon) {
        lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    }
    LatLon {
        lat,
        lon: if lon.is_finite() { lon } else { 0.0 },
    }
}

/// Latitude (clamped, 0 if not finite) and unclamped longitude of a point.
fn unproject(at: Mercator) -> LatLon {
    let lon = at.x * 180.0 / MERCATOR_HALF_WORLD_M;
    let lat = (2.0 * (at.y * PI / MERCATOR_HALF_WORLD_M).exp().atan() - PI / 2.0) * 180.0 / PI;
    let lat = if lat.is_finite() {
        lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT)
    } else {
        0.0
    };
    LatLon { lat, lon }
}

/// Coordinate transformation system for converting between geographic and screen coordinates
/// Uses Web Mercator projection (EPSG:3857)
#[derive(Debug, Clone)]
pub struct CoordinateTransform {
    pub bounds: GeoBounds,
    pub screen_size: Size<Pixels>,
    pub zoom_level: f64,
    pub center_lat: f64,
    pub center_lon: f64,
    pub pixels_per_meter_x: f64,
    pub pixels_per_meter_y: f64,
    /// Visible extent in Web Mercator (EPSG:3857) meters. Cached so hot
    /// render paths can cull and project without recomputing lat/lon→mercator
    /// per node.
    pub mercator_min_x: f64,
    pub mercator_max_x: f64,
    pub mercator_min_y: f64,
    pub mercator_max_y: f64,
    pub mercator_center_x: f64,
    pub mercator_center_y: f64,
}

impl CoordinateTransform {
    pub fn new(center: LatLon, zoom_level: f64, screen_size: Size<Pixels>) -> Self {
        let LatLon {
            lat: center_lat,
            lon: center_lon,
        } = center;
        let Mercator {
            x: center_x,
            y: center_y,
        } = lat_lon_to_mercator(center);

        // At zoom level 0, the entire world (40075016.686 m) fits in 256 pixels
        let world_width_meters = WORLD_WIDTH_M;
        let tile_size = 256.0;
        let world_width_pixels = tile_size * 2.0_f64.powf(zoom_level);
        let meters_per_pixel = world_width_meters / world_width_pixels;

        let half_width_meters = (screen_size.width.to_f64() / 2.0) * meters_per_pixel;
        let half_height_meters = (screen_size.height.to_f64() / 2.0) * meters_per_pixel;

        let mercator_min_x = center_x - half_width_meters;
        let mercator_min_y = center_y - half_height_meters;
        let mercator_max_x = center_x + half_width_meters;
        let mercator_max_y = center_y + half_height_meters;

        let epsilon = 1e-6;
        let denom_x = (mercator_max_x - mercator_min_x).abs().max(epsilon);
        let denom_y = (mercator_max_y - mercator_min_y).abs().max(epsilon);
        let pixels_per_meter_x = screen_size.width.to_f64() / denom_x;
        let pixels_per_meter_y = screen_size.height.to_f64() / denom_y;

        let min = mercator_to_lat_lon(Mercator::new(mercator_min_x, mercator_min_y));
        let max = mercator_to_lat_lon(Mercator::new(mercator_max_x, mercator_max_y));
        let bounds = GeoBounds::new(
            min.lat.min(max.lat),
            max.lat.max(min.lat),
            min.lon.min(max.lon),
            max.lon.max(min.lon),
        );

        Self {
            bounds,
            screen_size,
            zoom_level,
            center_lat,
            center_lon,
            pixels_per_meter_x,
            pixels_per_meter_y,
            mercator_min_x,
            mercator_max_x,
            mercator_min_y,
            mercator_max_y,
            mercator_center_x: center_x,
            mercator_center_y: center_y,
        }
    }

    /// Project a point given in Mercator (EPSG:3857) meters directly to
    /// screen pixels: the trig-free path for layers that cache Mercator
    /// coordinates.
    #[inline]
    pub fn mercator_to_screen(&self, at: Mercator) -> GpuiPoint<Pixels> {
        let half_w = self.screen_size.width * 0.5;
        let half_h = self.screen_size.height * 0.5;
        let x = half_w + px(((at.x - self.mercator_center_x) * self.pixels_per_meter_x) as f32);
        let y = half_h - px(((at.y - self.mercator_center_y) * self.pixels_per_meter_y) as f32);
        let x = if x.as_f32().is_finite() { x } else { half_w };
        let y = if y.as_f32().is_finite() { y } else { half_h };
        GpuiPoint { x, y }
    }

    /// A copyable snapshot of the Mercator-to-screen mapping, for hot loops
    /// that project hundreds of thousands of vertices per frame.
    pub fn projector(&self) -> MercatorProjector {
        MercatorProjector {
            cx: self.mercator_center_x,
            cy: self.mercator_center_y,
            ppm_x: self.pixels_per_meter_x,
            ppm_y: self.pixels_per_meter_y,
            half_w: self.screen_size.width.as_f32() * 0.5,
            half_h: self.screen_size.height.as_f32() * 0.5,
        }
    }

    /// The view centre.
    #[inline]
    pub fn center(&self) -> LatLon {
        LatLon::new(self.center_lat, self.center_lon)
    }

    /// Convert a geographic position to screen coordinates
    pub fn geo_to_screen(&self, at: LatLon) -> GpuiPoint<Pixels> {
        let Mercator {
            x: merc_x,
            y: merc_y,
        } = lat_lon_to_mercator(at);
        let Mercator {
            x: center_merc_x,
            y: center_merc_y,
        } = lat_lon_to_mercator(self.center());

        let half_w = self.screen_size.width * 0.5;
        let half_h = self.screen_size.height * 0.5;
        let y_offset = (merc_y - center_merc_y) * self.pixels_per_meter_y;
        let x = half_w + px(((merc_x - center_merc_x) * self.pixels_per_meter_x) as f32);
        let y = half_h - px(y_offset as f32);

        let x = if x.as_f32().is_finite() { x } else { half_w };
        let y = if y.as_f32().is_finite() { y } else { half_h };

        GpuiPoint { x, y }
    }

    /// Convert screen coordinates to a geographic position
    pub fn screen_to_geo(&self, point: GpuiPoint<Pixels>) -> LatLon {
        let Mercator {
            x: center_merc_x,
            y: center_merc_y,
        } = lat_lon_to_mercator(self.center());

        let merc_x = center_merc_x
            + (point.x - self.screen_size.width * 0.5).to_f64() / self.pixels_per_meter_x;
        let merc_y = center_merc_y
            - (point.y - self.screen_size.height * 0.5).to_f64() / self.pixels_per_meter_y; // Flip Y axis

        let merc_x = if merc_x.is_finite() {
            merc_x
        } else {
            center_merc_x
        };
        let merc_y = if merc_y.is_finite() {
            merc_y
        } else {
            center_merc_y
        };

        mercator_to_lat_lon(Mercator::new(merc_x, merc_y))
    }

    /// Convert screen coordinates directly to Web Mercator (EPSG:3857) meters,
    /// skipping the final lat/lon conversion `screen_to_geo` does. Used by
    /// box-select hit-testing, which queries an R-tree built in Mercator space.
    pub fn screen_to_mercator(&self, point: GpuiPoint<Pixels>) -> Mercator {
        let merc_x = self.mercator_center_x
            + (point.x - self.screen_size.width * 0.5).to_f64() / self.pixels_per_meter_x;
        let merc_y = self.mercator_center_y
            - (point.y - self.screen_size.height * 0.5).to_f64() / self.pixels_per_meter_y;

        let merc_x = if merc_x.is_finite() {
            merc_x
        } else {
            self.mercator_center_x
        };
        let merc_y = if merc_y.is_finite() {
            merc_y
        } else {
            self.mercator_center_y
        };

        Mercator::new(merc_x, merc_y)
    }

    /// Update the transform for a new center point (for panning)
    pub fn pan_to(&mut self, new_center: LatLon) {
        let lat = if new_center.lat.is_finite() {
            new_center.lat.clamp(-85.0, 85.0)
        } else {
            self.center_lat
        };
        let lon = if new_center.lon.is_finite() {
            new_center.lon.clamp(-180.0, 180.0)
        } else {
            self.center_lon
        };

        *self = Self::new(LatLon::new(lat, lon), self.zoom_level, self.screen_size);
    }

    /// Update the transform for a new zoom level
    pub fn zoom_to(&mut self, new_zoom_level: f64) {
        let zoom = if new_zoom_level.is_finite() {
            new_zoom_level.clamp(1.0, MAX_MAP_ZOOM)
        } else {
            self.zoom_level
        };

        *self = Self::new(self.center(), zoom, self.screen_size);
    }

    /// Zoom in/out by a delta while keeping the zoom centered on a specific screen point
    pub fn zoom_at_point(&mut self, screen_point: GpuiPoint<Pixels>, zoom_delta: f64) {
        if !zoom_delta.is_finite() {
            return;
        }

        let Mercator {
            x: center_merc_x,
            y: center_merc_y,
        } = lat_lon_to_mercator(self.center());
        let dx = (screen_point.x - self.screen_size.width * 0.5).to_f64() / self.pixels_per_meter_x;
        let dy =
            -(screen_point.y - self.screen_size.height * 0.5).to_f64() / self.pixels_per_meter_y;
        let mouse_merc_x = center_merc_x + dx;
        let mouse_merc_y = center_merc_y + dy;

        let new_zoom = (self.zoom_level + zoom_delta).clamp(1.0, MAX_MAP_ZOOM);
        let new_transform = Self::new(self.center(), new_zoom, self.screen_size);

        // The new center keeps mouse_merc_x/y under the same screen pixel.
        let new_dx = (screen_point.x - self.screen_size.width * 0.5).to_f64()
            / new_transform.pixels_per_meter_x;
        let new_dy = -(screen_point.y - self.screen_size.height * 0.5).to_f64()
            / new_transform.pixels_per_meter_y;
        let new_center_merc_x = mouse_merc_x - new_dx;
        let new_center_merc_y = mouse_merc_y - new_dy;
        let new_center = mercator_to_lat_lon(Mercator::new(new_center_merc_x, new_center_merc_y));

        *self = Self::new(new_center, new_zoom, self.screen_size);
    }

    /// Pan by a screen pixel offset
    pub fn pan_by_pixels(&mut self, dx: Pixels, dy: Pixels) {
        if !dx.as_f32().is_finite() || !dy.as_f32().is_finite() {
            return;
        }

        let mercator_dx = dx.to_f64() / self.pixels_per_meter_x;
        let mercator_dy = -dy.to_f64() / self.pixels_per_meter_y; // Negative because screen Y increases downward

        let mercator_dx = if mercator_dx.is_finite() {
            mercator_dx
        } else {
            0.0
        };
        let mercator_dy = if mercator_dy.is_finite() {
            mercator_dy
        } else {
            0.0
        };

        let center_merc = lat_lon_to_mercator(self.center());
        let new_center_merc_x = center_merc.x + mercator_dx;
        let new_center_merc_y = center_merc.y + mercator_dy;
        let new_center = mercator_to_lat_lon(Mercator::new(new_center_merc_x, new_center_merc_y));

        self.pan_to(new_center);
    }

    pub fn is_visible(&self, at: LatLon) -> bool {
        self.bounds.contains(at)
    }

    pub fn visible_bounds(&self) -> GeoBounds {
        self.bounds
    }

    /// Get the current zoom level as used by tile servers (integer)
    pub fn tile_zoom_level(&self) -> u32 {
        self.zoom_level.round().clamp(0.0, MAX_MAP_ZOOM) as u32
    }
}

/// The zoom [`fit_bounds_to_viewport`] uses for bounds with no extent.
pub const POINT_ZOOM: f64 = 19.0;

/// Deepest zoom the map can be zoomed to. Tile layers over-zoom (scale their
/// deepest tiles) past their own max zoom.
pub const MAX_MAP_ZOOM: f64 = 23.0;

/// Compute the centre and zoom level that fit a geographic bounding box
/// within a viewport of `screen_width`×`screen_height` pixels, with a 20%
/// margin and zoom clamped to `[1, 18]`.
///
/// Degenerate bounds: with no extent on one axis (all data at one latitude)
/// only the other axis decides the zoom; with none on either (a single
/// point) the result is centred on it at [`POINT_ZOOM`].
///
/// The caller (`MapViewer::fit_to_osm_data`) is responsible for first
/// reducing a dataset's nodes down to a min/max lat/lon bounding box.
pub fn fit_bounds_to_viewport(
    bounds: GeoBounds,
    screen_width: f64,
    screen_height: f64,
) -> (LatLon, f64) {
    let GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    } = bounds;
    let mut center_lat = (min_lat + max_lat) / 2.0;
    let mut center_lon = (min_lon + max_lon) / 2.0;

    // An axis with no extent does not constrain the zoom: padding it (say by
    // 0.005 degrees, about a kilometre) would zoom a point or a perfectly
    // horizontal or vertical way out to about z16. Both axes without extent
    // is a single point: centre on it at an editing zoom.
    let flat_lat = (max_lat - min_lat).abs() < 1e-6;
    let flat_lon = (max_lon - min_lon).abs() < 1e-6;
    if flat_lat && flat_lon {
        return (LatLon::new(center_lat, center_lon), POINT_ZOOM);
    }
    if flat_lat {
        center_lat = min_lat;
    }
    if flat_lon {
        center_lon = min_lon;
    }

    let margin = 1.2; // 20% margin

    let min = lat_lon_to_mercator(LatLon::new(min_lat, min_lon));
    let max = lat_lon_to_mercator(LatLon::new(max_lat, max_lon));
    let bbox_width = (max.x - min.x).abs();
    let bbox_height = (max.y - min.y).abs();

    let world_width_meters = WORLD_WIDTH_M;
    let tile_size = 256.0;
    let zoom_x = if flat_lon {
        f64::INFINITY
    } else {
        ((screen_width * world_width_meters) / (bbox_width * tile_size * margin)).log2()
    };
    let zoom_y = if flat_lat {
        f64::INFINITY
    } else {
        ((screen_height * world_width_meters) / (bbox_height * tile_size * margin)).log2()
    };
    let zoom_level = zoom_x.min(zoom_y).clamp(1.0, 18.0);

    (LatLon::new(center_lat, center_lon), zoom_level)
}

/// The Mercator-to-screen mapping of a [`CoordinateTransform`] as plain
/// numbers: the same arithmetic as `mercator_to_screen`, without the
/// `Pixels` wrapping and the per-call finite checks, so a hot loop can inline
/// it and test finiteness once per point.
#[derive(Debug, Clone, Copy)]
pub struct MercatorProjector {
    cx: f64,
    cy: f64,
    ppm_x: f64,
    ppm_y: f64,
    half_w: f32,
    half_h: f32,
}

impl MercatorProjector {
    /// Screen position (view-relative pixels) of a Web Mercator point. The
    /// result is not finite only if the input is not.
    #[inline(always)]
    pub fn to_screen(&self, mx: f64, my: f64) -> (f32, f32) {
        (
            self.half_w + ((mx - self.cx) * self.ppm_x) as f32,
            self.half_h - ((my - self.cy) * self.ppm_y) as f32,
        )
    }

    /// Pixels per Web Mercator meter along x and y.
    pub fn pixels_per_meter(&self) -> (f64, f64) {
        (self.ppm_x, self.ppm_y.abs())
    }
}

/// Validates that a point has finite coordinates
pub fn is_point_valid(point: GpuiPoint<Pixels>) -> bool {
    point.x.as_f32().is_finite() && point.y.as_f32().is_finite()
}

/// `Some(at)` when both are finite and within [-90, 90] / [-180, 180].
pub fn validate_coords(at: LatLon) -> Option<LatLon> {
    if at.lat.is_finite()
        && at.lon.is_finite()
        && (-90.0..=90.0).contains(&at.lat)
        && (-180.0..=180.0).contains(&at.lon)
    {
        Some(at)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    #[test]
    fn mercator_to_lat_lon_clamps_longitude_past_the_antimeridian() {
        let at = mercator_to_lat_lon(Mercator::new(MERCATOR_HALF_WORLD_M * 1.5, 0.0));
        assert_eq!(at.lon, 180.0);
        let at = mercator_to_lat_lon_wrapped(Mercator::new(MERCATOR_HALF_WORLD_M * 1.5, 0.0));
        assert!((at.lon - -90.0).abs() < 1e-9);
    }

    #[test]
    fn mercator_round_trips_over_a_grid() {
        let lats = [
            -85.0, -80.0, -60.0, -33.9, -1e-6, 0.0, 1e-6, 0.5, 35.0, 51.5, 70.0, 85.0,
        ];
        let lons = [
            -180.0,
            -179.999999,
            -179.5,
            -120.0,
            -0.000001,
            0.0,
            0.5,
            90.0,
            179.5,
            179.999999,
            180.0,
        ];
        for &lat in &lats {
            for &lon in &lons {
                let m = lat_lon_to_mercator(LatLon::new(lat, lon));
                let back = mercator_to_lat_lon(m);
                assert!(
                    (lat - back.lat).abs() < 1e-7 && (lon - back.lon).abs() < 1e-7,
                    "({lat}, {lon}) -> ({}, {})",
                    back.lat,
                    back.lon
                );
            }
        }
    }

    #[test]
    fn mercator_clamps_beyond_the_poles() {
        let y_max = lat_lon_to_mercator(LatLon::new(85.051128779807, 0.0)).y;
        let y_over = lat_lon_to_mercator(LatLon::new(89.9, 0.0)).y;
        assert_eq!(y_max, y_over);
        let at = mercator_to_lat_lon(Mercator::new(0.0, y_over));
        assert!((at.lat - 85.051128779807).abs() < 1e-6);
    }

    #[test]
    fn longitude_maps_to_x_and_latitude_to_y() {
        // Longitude drives x, latitude drives y: the order the two types
        // exist to keep straight.
        let east = lat_lon_to_mercator(LatLon::new(0.0, 180.0));
        assert!((east.x - MERCATOR_HALF_WORLD_M).abs() < 1e-6);
        assert!(east.y.abs() < 1e-6);
        let north = lat_lon_to_mercator(LatLon::new(45.0, 0.0));
        assert!(north.x.abs() < 1e-6);
        assert!(north.y > 5.0e6);
        let back = mercator_to_lat_lon(Mercator::new(MERCATOR_HALF_WORLD_M / 2.0, 0.0));
        assert!((back.lon - 90.0).abs() < 1e-9 && back.lat.abs() < 1e-9);
    }

    #[test]
    fn non_finite_input_projects_to_the_origin() {
        let m = lat_lon_to_mercator(LatLon::new(f64::NAN, f64::NAN));
        assert_eq!((m.x, m.y), (0.0, 0.0));
        let at = mercator_to_lat_lon(Mercator::new(f64::NAN, f64::NAN));
        assert_eq!((at.lat, at.lon), (0.0, 0.0));
    }

    #[test]
    fn validate_coords_keeps_the_position_or_rejects_it() {
        let ok = LatLon::new(40.0, -74.0);
        assert_eq!(validate_coords(ok), Some(ok));
        assert_eq!(validate_coords(LatLon::new(91.0, 0.0)), None);
        assert_eq!(validate_coords(LatLon::new(0.0, 181.0)), None);
        assert_eq!(validate_coords(LatLon::new(f64::NAN, 0.0)), None);
        // A swapped pair is only caught when a value does not fit the other range.
        assert_eq!(validate_coords(LatLon::new(120.0, -74.0)), None);
    }

    #[test]
    fn geo_bounds_contains_center_and_extend() {
        let b = GeoBounds::new(10.0, 20.0, 30.0, 50.0);
        assert!(b.contains(LatLon::new(15.0, 40.0)));
        assert!(!b.contains(LatLon::new(40.0, 15.0)));
        assert_eq!(b.center(), LatLon::new(15.0, 40.0));

        let mut grown = None;
        GeoBounds::extend(&mut grown, LatLon::new(1.0, 2.0));
        assert_eq!(grown, Some(GeoBounds::new(1.0, 1.0, 2.0, 2.0)));
        GeoBounds::extend(&mut grown, LatLon::new(-3.0, 5.0));
        GeoBounds::extend(&mut grown, LatLon::new(0.0, -1.0));
        assert_eq!(grown, Some(GeoBounds::new(-3.0, 1.0, -1.0, 5.0)));
    }

    #[test]
    fn projector_agrees_with_mercator_to_screen() {
        let t = CoordinateTransform::new(
            LatLon::new(40.7128, -74.0060),
            12.0,
            size(px(800.0), px(600.0)),
        );
        let p = t.projector();
        let c = lat_lon_to_mercator(LatLon::new(40.7128, -74.0060));
        for (dx, dy) in [
            (0.0, 0.0),
            (1.0, -1.0),
            (-1234.5, 987.25),
            (20_000.0, 15_000.0),
            (-1.0e6, 1.0e6),
        ] {
            let at = Mercator::new(c.x + dx, c.y + dy);
            let a = t.mercator_to_screen(at);
            let (bx, by) = p.to_screen(at.x, at.y);
            assert!((a.x.as_f32() - bx).abs() < 1e-3, "x at ({dx}, {dy})");
            assert!((a.y.as_f32() - by).abs() < 1e-3, "y at ({dx}, {dy})");
        }
    }

    #[test]
    fn zoom_can_go_past_20_up_to_max() {
        let mut t =
            CoordinateTransform::new(LatLon::new(40.0, -74.0), 19.0, size(px(800.0), px(600.0)));
        t.zoom_to(22.0);
        assert_eq!(t.zoom_level, 22.0);
        t.zoom_at_point(gpui::point(px(100.0), px(100.0)), 10.0);
        assert_eq!(t.zoom_level, MAX_MAP_ZOOM);
        assert_eq!(t.tile_zoom_level(), 23);
    }

    #[test]
    fn test_coordinate_conversion() {
        let screen_size = size(px(800.0), px(600.0));
        let transform = CoordinateTransform::new(LatLon::new(40.7128, -74.0060), 10.0, screen_size);

        let screen_center = transform.geo_to_screen(LatLon::new(40.7128, -74.0060));
        assert!((screen_center.x.as_f32() - 400.0).abs() < 1.0);
        assert!((screen_center.y.as_f32() - 300.0).abs() < 1.0);

        let original = LatLon::new(40.7500, -73.9000);
        let screen_point = transform.geo_to_screen(original);
        let converted = transform.screen_to_geo(screen_point);

        assert!((converted.lat - original.lat).abs() < 0.001);
        assert!((converted.lon - original.lon).abs() < 0.001);
    }

    #[test]
    fn the_centre_round_trips_through_the_transform() {
        let t = CoordinateTransform::new(LatLon::new(12.5, 99.25), 8.0, size(px(640.0), px(480.0)));
        assert_eq!(t.center(), LatLon::new(12.5, 99.25));
        let m = lat_lon_to_mercator(t.center());
        assert_eq!((m.x, m.y), (t.mercator_center_x, t.mercator_center_y));
    }

    #[test]
    fn test_screen_to_mercator_round_trip() {
        let screen_size = size(px(800.0), px(600.0));
        let transform = CoordinateTransform::new(LatLon::new(40.7128, -74.0060), 12.0, screen_size);

        // Offsets from the viewport's own mercator center, not absolute
        // mercator coordinates: real map viewports are always centered near
        // their content, so this exercises the round trip at a screen-pixel
        // magnitude the f32 `Pixels` type can represent precisely.
        let original = Mercator::new(
            transform.mercator_center_x + 100.0,
            transform.mercator_center_y - 50.0,
        );
        let screen_point = transform.mercator_to_screen(original);
        let Mercator { x: mx, y: my } = transform.screen_to_mercator(screen_point);

        assert!(
            (mx - original.x).abs() < 0.01,
            "mx: {} vs {}",
            mx,
            original.x
        );
        assert!(
            (my - original.y).abs() < 0.01,
            "my: {} vs {}",
            my,
            original.y
        );
    }

    fn fit(
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
        w: f64,
        h: f64,
    ) -> (LatLon, f64) {
        fit_bounds_to_viewport(GeoBounds::new(min_lat, max_lat, min_lon, max_lon), w, h)
    }

    #[test]
    fn fit_bounds_centers_on_the_bbox_midpoint() {
        let (center, zoom) = fit(40.0, 41.0, -74.5, -73.5, 800.0, 600.0);
        assert!((center.lat - 40.5).abs() < 1e-9);
        assert!((center.lon - (-74.0)).abs() < 1e-9);
        assert!((1.0..=18.0).contains(&zoom));
    }

    #[test]
    fn fit_bounds_zoom_clamped_to_max_for_a_tiny_bbox() {
        let (_, zoom) = fit(40.7127, 40.7129, -74.0061, -74.0059, 800.0, 600.0);
        assert!((zoom - 18.0).abs() < 1e-9);
    }

    #[test]
    fn fit_bounds_zoom_clamped_to_min_for_a_huge_bbox() {
        let (_, zoom) = fit(-85.0, 85.0, -180.0, 180.0, 800.0, 600.0);
        assert!((zoom - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fit_bounds_degenerate_single_point_does_not_panic_and_centers_correctly() {
        let (center, zoom) = fit(40.7128, 40.7128, -74.0060, -74.0060, 800.0, 600.0);
        assert!((center.lat - 40.7128).abs() < 1e-9);
        assert!((center.lon - (-74.0060)).abs() < 1e-9);
        assert!(zoom.is_finite());
        // Centred on the point at an editing zoom, not zoomed out to ~z16.
        assert!((zoom - POINT_ZOOM).abs() < 1e-9);
    }

    #[test]
    fn fit_bounds_zero_extent_never_zooms_out_below_editing_zoom() {
        // Whatever the window size, a point is centred at z19.
        for (w, h) in [(300.0, 200.0), (800.0, 600.0), (3000.0, 2000.0)] {
            let (_, zoom) = fit(10.0, 10.0, 20.0, 20.0, w, h);
            assert!(zoom >= 19.0, "{w}x{h} gave z{zoom}");
        }
    }

    #[test]
    fn fit_bounds_degenerate_zero_height_only() {
        // Same latitude, distinct longitudes: only the longitude axis sets
        // the zoom.
        let (center, zoom) = fit(40.0, 40.0, -75.0, -73.0, 800.0, 600.0);
        assert!((center.lat - 40.0).abs() < 1e-9);
        assert!((center.lon - (-74.0)).abs() < 1e-9);
        assert!(zoom.is_finite());
    }

    #[test]
    fn fit_bounds_a_flat_short_line_fits_by_its_length_not_a_padded_kilometre() {
        // A way 20 m long and exactly horizontal must not be padded to 1 km
        // tall (z16).
        let (_, zoom) = fit(40.712, 40.712, -74.0062, -74.0060, 800.0, 600.0);
        assert!((zoom - 18.0).abs() < 1e-9, "z{zoom}");
        // Likewise exactly vertical.
        let (_, zoom) = fit(40.7120, 40.7122, -74.006, -74.006, 800.0, 600.0);
        assert!((zoom - 18.0).abs() < 1e-9, "z{zoom}");
    }
}
