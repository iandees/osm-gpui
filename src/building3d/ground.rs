//! The imagery under the 3D view: which tiles cover the ground square, and
//! stitching them into one texture with its placement in the local frame.

use crate::geometry::LocalProjection;
use crate::imagery_offset::ImageryOffset;
use crate::tiles::{lat_lon_to_tile, TileCoord};

use super::model::Pt;

/// The texture is at most this many pixels on a side.
pub const MAX_TEXTURE_PX: u32 = 2048;
const TILE_PX: u32 = 256;
/// Equatorial circumference of Web Mercator's sphere.
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;

/// The block of tiles to fetch for the ground: columns `x0..=x1`, rows
/// `y0..=y1` at zoom `z`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileBlock {
    pub z: u32,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl TileBlock {
    pub fn cols(&self) -> u32 {
        self.x1 - self.x0 + 1
    }

    pub fn rows(&self) -> u32 {
        self.y1 - self.y0 + 1
    }

    pub fn tiles(&self) -> Vec<TileCoord> {
        (self.y0..=self.y1)
            .flat_map(|y| (self.x0..=self.x1).map(move |x| TileCoord::new(x, y, self.z)))
            .collect()
    }

    /// The block's corners in the local frame: (north-west, north-east,
    /// south-east, south-west), shifted by the imagery offset.
    pub fn corners(&self, frame: &LocalProjection, offset: ImageryOffset) -> [Pt; 4] {
        let nw = TileCoord::new(self.x0, self.y0, self.z).to_lat_lon_bounds();
        let se = TileCoord::new(self.x1, self.y1, self.z).to_lat_lon_bounds();
        // to_lat_lon_bounds: (lon_min, lat_min, lon_max, lat_max)
        let (west, north) = (nw.0, nw.3);
        let (east, south) = (se.2, se.1);
        let shift = |p: Pt| (p.0 + offset.east_m, p.1 + offset.north_m);
        [
            shift(frame.project((north, west))),
            shift(frame.project((north, east))),
            shift(frame.project((south, east))),
            shift(frame.project((south, west))),
        ]
    }
}

/// The tiles covering the square of half-width `radius` metres around
/// local point `center`, at the deepest zoom (up to `max_zoom`) whose block
/// still fits in [`MAX_TEXTURE_PX`].
pub fn tile_block(frame: &LocalProjection, center: Pt, radius: f64, max_zoom: u32) -> TileBlock {
    let (lat, _) = frame.unproject(center);
    let max_tiles = MAX_TEXTURE_PX / TILE_PX;
    let mut z = max_zoom.min(22);
    loop {
        let tile_m = EARTH_CIRCUMFERENCE_M * lat.to_radians().cos() / 2f64.powi(z as i32);
        let needed = (2.0 * radius / tile_m).ceil() as u32 + 1;
        if needed <= max_tiles || z == 0 {
            break;
        }
        z -= 1;
    }
    let nw = frame.unproject((center.0 - radius, center.1 + radius));
    let se = frame.unproject((center.0 + radius, center.1 - radius));
    let a = lat_lon_to_tile(nw.0, nw.1, z);
    let b = lat_lon_to_tile(se.0, se.1, z);
    let last = (1u32 << z).saturating_sub(1);
    TileBlock {
        z,
        x0: a.x.min(b.x),
        y0: a.y.min(b.y),
        x1: a.x.max(b.x).min(last),
        y1: a.y.max(b.y).min(last),
    }
}

/// An RGBA texture with its mip chain (level 0 first).
#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub levels: Vec<Vec<u8>>,
}

/// Stitch a block's tiles into one RGBA texture. `tile` returns a tile's
/// pixels as BGRA (gpui's `RenderImage` layout) with its width and height,
/// or `None` if it hasn't loaded (left as a neutral grey). Tiles of other
/// sizes are resampled to 256 px.
pub fn stitch<'a>(
    block: &TileBlock,
    mut tile: impl FnMut(TileCoord) -> Option<(&'a [u8], u32, u32)>,
) -> Texture {
    let (w, h) = (block.cols() * TILE_PX, block.rows() * TILE_PX);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for chunk in px.as_chunks_mut::<4>().0 {
        *chunk = [150, 150, 145, 255];
    }
    for coord in block.tiles() {
        let Some((bgra, tw, th)) = tile(coord) else {
            continue;
        };
        if tw == 0 || th == 0 || bgra.len() < (tw * th * 4) as usize {
            continue;
        }
        let ox = (coord.x - block.x0) * TILE_PX;
        let oy = (coord.y - block.y0) * TILE_PX;
        for y in 0..TILE_PX {
            let sy = y * th / TILE_PX;
            for x in 0..TILE_PX {
                let sx = x * tw / TILE_PX;
                let s = ((sy * tw + sx) * 4) as usize;
                let d = (((oy + y) * w + ox + x) * 4) as usize;
                px[d] = bgra[s + 2];
                px[d + 1] = bgra[s + 1];
                px[d + 2] = bgra[s];
                px[d + 3] = 255;
            }
        }
    }
    Texture {
        width: w,
        height: h,
        levels: mip_chain(px, w, h),
    }
}

/// Box-filtered mip levels down to 1x1.
fn mip_chain(level0: Vec<u8>, mut w: u32, mut h: u32) -> Vec<Vec<u8>> {
    let mut levels = vec![level0];
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let src = levels.last().unwrap();
        let mut dst = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        sum += src[((sy * w + sx) * 4 + c) as usize] as u32;
                    }
                    dst[((y * nw + x) * 4 + c) as usize] = (sum / 4) as u8;
                }
            }
        }
        levels.push(dst);
        w = nw;
        h = nh;
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_fits_the_texture_budget_and_covers_the_area() {
        let frame = LocalProjection::new((45.0, -93.0));
        for radius in [20.0, 80.0, 400.0, 3000.0] {
            let b = tile_block(&frame, (0.0, 0.0), radius, 19);
            assert!(b.cols() * TILE_PX <= MAX_TEXTURE_PX, "r={radius} {b:?}");
            assert!(b.rows() * TILE_PX <= MAX_TEXTURE_PX, "r={radius} {b:?}");
            let c = b.corners(&frame, ImageryOffset::ZERO);
            // NW corner is north-west of the square, SE south-east of it.
            assert!(c[0].0 <= -radius && c[0].1 >= radius, "r={radius} {c:?}");
            assert!(c[2].0 >= radius && c[2].1 <= -radius, "r={radius} {c:?}");
        }
        // Small areas use the deepest zoom offered.
        assert_eq!(tile_block(&frame, (0.0, 0.0), 20.0, 19).z, 19);
        assert_eq!(tile_block(&frame, (0.0, 0.0), 20.0, 17).z, 17);
    }

    #[test]
    fn offset_shifts_the_corners() {
        let frame = LocalProjection::new((45.0, -93.0));
        let b = tile_block(&frame, (0.0, 0.0), 50.0, 19);
        let plain = b.corners(&frame, ImageryOffset::ZERO);
        let moved = b.corners(&frame, ImageryOffset::new(2.0, -3.0));
        assert!((moved[0].0 - plain[0].0 - 2.0).abs() < 1e-9);
        assert!((moved[0].1 - plain[0].1 + 3.0).abs() < 1e-9);
    }

    #[test]
    fn stitch_places_tiles_and_swaps_channels() {
        let block = TileBlock {
            z: 1,
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 0,
        };
        // A 1x1 BGRA tile: blue=10, green=20, red=30.
        let tile = [10u8, 20, 30, 255];
        let tex = stitch(&block, |c| (c.x == 1).then_some((&tile[..], 1, 1)));
        assert_eq!((tex.width, tex.height), (512, 256));
        // Left tile missing: grey placeholder.
        assert_eq!(&tex.levels[0][0..4], &[150, 150, 145, 255]);
        // Right tile, resampled from 1x1: RGBA.
        let d = (300 * 4) as usize;
        assert_eq!(&tex.levels[0][d..d + 4], &[30, 20, 10, 255]);
        // Mips down to 1x1.
        assert_eq!(tex.levels.len(), 10);
        assert_eq!(tex.levels.last().unwrap().len(), 4);
    }
}
