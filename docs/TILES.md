# Tile System Implementation

This document describes the tile-based map background system implemented in OSM-GPUI.

## Overview

The tile system uses Web Mercator projection (EPSG:3857) exclusively and provides the foundation for loading raster map tiles from OpenStreetMap's tile server. The current implementation shows tile boundaries and demonstrates the coordinate system needed for proper tile rendering.

## Features Implemented

### 1. Web Mercator Projection
- Exclusively uses Web Mercator projection (EPSG:3857) - no other projection options
- Support for zoom levels 0-20 (extended range for high detail)
- Accurate conversion between geographic coordinates and tile coordinates
- Proper handling of Web Mercator distortion characteristics

### 2. Tile Coordinate System
The `tiles.rs` module provides:
- `TileCoord` struct for representing tile coordinates (x, y, z)
- `lat_lon_to_tile()` function to convert lat/lon to tile coordinates
- `get_tiles_for_bounds()` function to find all tiles visible in a viewport
- Tile boundary calculations for proper positioning

### 3. Visual Tile Grid
- Displays tile boundaries as gray rectangles over the map
- Shows exactly where each 256x256 pixel tile would be positioned
- Helps visualize the Web Mercator tile grid at different zoom levels

### 4. Interactive Controls
- **T key**: Toggle tile boundary display on/off
- **G key**: Toggle coordinate grid on/off  
- **D key**: Toggle debug information display
- Mouse pan and zoom work correctly with the tile coordinate system

### 5. Debug Information
When debug mode is enabled (D key), the overlay shows:
- Current zoom level and corresponding tile zoom level
- Number of visible tiles in the current viewport
- Center coordinates
- OSM data statistics
- Tile and grid status

## Usage

1. **Run the application**: `cargo run`
2. **View tile boundaries**: Press 'T' to toggle tile overlay
3. **Pan and zoom**: Use mouse to navigate - tile boundaries update correctly
4. **Debug mode**: Press 'D' to see detailed tile information

## Tile Server Integration

The system is designed to work with the OpenStreetMap tile server:
- URL template: `https://tile.openstreetmap.org/{z}/{x}/{y}.png`
- Standard 256x256 pixel tiles
- Web Mercator projection (EPSG:3857)

## Architecture

### Coordinate Transformations
The system maintains proper coordinate transformations between:
1. **Geographic coordinates** (latitude/longitude in WGS84)
2. **Web Mercator coordinates** (x, y in meters using EPSG:3857)
3. **Tile coordinates** (x, y, z in Web Mercator grid)
4. **Screen coordinates** (pixels on the display)

All coordinate transformations use Web Mercator projection internally for consistency with web mapping standards.

### Key Functions

#### `lat_lon_to_tile(lat: f64, lon: f64, zoom: u32) -> TileCoord`
Converts geographic coordinates to tile coordinates at a specific zoom level.

#### `get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, zoom) -> Vec<TileCoord>`
Returns all tile coordinates that intersect with the given geographic bounds.

#### `geo_to_screen(lat, lon, bounds_width, bounds_height, center_lat, center_lon, zoom_level) -> (f32, f32)`
Converts geographic coordinates to screen pixel coordinates.

## Future Enhancements

The current implementation provides the foundation for:

1. **Asynchronous Tile Loading**
   - HTTP requests to fetch tile images
   - Background loading with progress indication
   - Tile caching for performance

2. **Image Rendering**
   - Display actual map tiles instead of just boundaries
   - Proper tile positioning and scaling
   - Handle tile loading states (loading, error, success)

3. **Performance Optimizations**
   - Tile cache management
   - Viewport-based tile culling
   - Progressive loading (load center tiles first)

4. **Additional Tile Sources**
   - Support for different tile servers
   - Satellite imagery, terrain maps, etc.
   - Custom tile URL templates

## Technical Notes

### Web Mercator Projection
The implementation exclusively uses the standard Web Mercator projection formulas:
- Used by all major web mapping services (Google Maps, OpenStreetMap, etc.)
- Preserves shape but distorts area, especially near poles
- Limited to approximately ±85.05° latitude
- No fallback to other projection systems - Web Mercator only

### Zoom Levels
- Zoom 0: Entire world in one 256x256 tile
- Each zoom level doubles the number of tiles in each dimension
- Zoom 18: Standard maximum detail level for most tile servers
- Current implementation supports zoom levels 0-20 (extended for high detail)

### Tile Numbering
- X coordinate: increases eastward (0 = 180°W, increases toward 180°E)
- Y coordinate: increases southward (0 = ~85.05°N, increases toward ~85.05°S)
- Z coordinate: zoom level (0 = world view, 18 = maximum detail)

## Dependencies

The tile system requires:
- `std::f64::consts::PI` for mathematical calculations
- Basic Rust standard library for collections and math

Future tile loading will require:
- `reqwest` for HTTP requests
- `image` for image decoding
- `tokio` for async runtime
- GPUI's image rendering capabilities

## Testing

The implementation includes unit tests for:
- Tile coordinate conversions
- Boundary calculations
- URL generation
- Edge cases and known coordinates

Run tests with: `cargo test tiles`
