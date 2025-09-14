# Web Mercator Implementation Summary

This document summarizes the complete implementation of Web Mercator projection and tile coordinate system in OSM-GPUI.

## Overview

The OSM-GPUI application now exclusively uses Web Mercator projection (EPSG:3857) for all coordinate transformations and map rendering. This provides compatibility with standard web mapping tile servers and ensures consistent behavior with other web mapping applications.

## Key Changes Made

### 1. Coordinate System Overhaul

**Before**: Mixed projection system with optional Web Mercator support
**After**: Exclusive Web Mercator projection (EPSG:3857)

- Removed all projection switching options
- Simplified coordinate transformation code
- Unified all coordinate calculations under Web Mercator

### 2. Core Components Implemented

#### `coordinates.rs`
- **CoordinateTransform**: Always uses Web Mercator projection
- **lat_lon_to_mercator()**: Converts WGS84 to Web Mercator meters
- **mercator_to_lat_lon()**: Converts Web Mercator meters back to WGS84
- **pixels_per_meter_x/y**: Accurate scaling for Web Mercator coordinates

#### `viewport.rs`
- **Viewport**: Simplified to always use Web Mercator
- Removed projection switching methods
- Clean mouse interaction handling with proper Web Mercator transformations
- **Improved Zoom Math**: The zoom logic now uses correct Mercator math to ensure the geographic point under the mouse remains fixed during zoom, resulting in a stable and intuitive user experience.

#### `tiles.rs`
- **TileCoord**: Web Mercator tile coordinate system
- **lat_lon_to_tile()**: Converts geographic coordinates to tile indices
- **get_tiles_for_bounds()**: Finds all tiles visible in viewport
- **to_lat_lon_bounds()**: Converts tile indices back to geographic bounds

#### `main.rs`
- Integrated tile boundary visualization
- Proper Web Mercator coordinate transformations for all rendering
- Interactive tile grid display

### 3. Tile System Features

#### Tile Boundary Visualization
- Shows exact 256x256 pixel tile boundaries
- Updates in real-time as user pans and zooms
- Demonstrates proper tile alignment with Web Mercator grid

#### Interactive Controls
- **'T' key**: Toggle tile boundary display
- **'G' key**: Toggle coordinate grid
- **'D' key**: Toggle debug information
- **Mouse**: Pan and zoom with proper Web Mercator transformations

#### Debug Information
When enabled, shows:
- Current zoom level and corresponding tile zoom level
- Number of visible tiles in current viewport
- Center coordinates in WGS84
- OSM data statistics
- Projection status (always Web Mercator)

## Technical Specifications

### Web Mercator Projection (EPSG:3857)
- **Coordinate Range**: ±20,037,508.34 meters
- **Latitude Limits**: ±85.05°
- **Used By**: Google Maps, OpenStreetMap, Bing Maps, etc.
- **Tile Size**: 256x256 pixels (standard)

### Zoom Levels
- **Range**: 0-20 (extended from typical 0-18)
- **Zoom 0**: Entire world in one tile
- **Zoom 20**: Maximum detail level
- **Tile Count**: 2^(2*zoom) total tiles at each zoom level

### Coordinate Transformations
All transformations flow through Web Mercator:
1. **WGS84** ↔ **Web Mercator meters** ↔ **Screen pixels**
2. **WGS84** ↔ **Tile coordinates** (via Web Mercator)
3. **Tile coordinates** ↔ **Screen pixels** (via Web Mercator)

## OSM Tile Server Integration

### URL Template
```
https://tile.openstreetmap.org/{z}/{x}/{y}.png
```

### Tile Coordinate System
- **X**: Increases eastward (0 = 180°W)
- **Y**: Increases southward (0 = ~85.05°N)  
- **Z**: Zoom level (0 = world view)

### Example Tile Coordinates
- **New York City** (zoom 10): Multiple tiles around x=301, y=384
- **London** (zoom 10): Multiple tiles around x=511, y=340
- **World** (zoom 0): Single tile at x=0, y=0

## Usage Examples

### Running the Application
```bash
cargo run
```

### Keyboard Controls
- **'T'**: Toggle tile boundaries on/off
- **'G'**: Toggle coordinate grid on/off
- **'D'**: Toggle debug information display
- **'L'**: Load sample OSM data
- **'+'/'-'**: Zoom in/out
- **Mouse**: Pan and zoom

### Understanding Tile Boundaries
1. Enable tile display with 'T' key
2. Pan and zoom around the map
3. Observe how tile boundaries align with 256x256 pixel grid
4. Note how tile coordinates change with zoom level

## Implementation Benefits

### 1. Standards Compliance
- Compatible with all major web mapping services
- Standard EPSG:3857 projection used worldwide
- Consistent with OpenStreetMap tile server expectations

### 2. Performance
- Optimized coordinate transformations
- Efficient tile coordinate calculations
- No projection switching overhead

### 3. Accuracy
- Proper Web Mercator distortion handling
- Precise tile alignment
- Accurate geographic coordinate conversions

### 4. Extensibility
- Ready for actual tile image loading
- Foundation for tile caching system
- Support for multiple tile servers

## Future Development Path

### Phase 1: Current Implementation ✅
- Web Mercator projection system
- Tile coordinate calculations
- Tile boundary visualization
- Interactive controls

### Phase 2: Image Loading (Next)
- HTTP requests to tile servers
- Asynchronous tile downloading
- Image decoding and caching
- Error handling for missing tiles

### Phase 3: Performance Optimization
- Tile cache management
- Viewport-based tile culling
- Progressive loading strategies
- Memory usage optimization

### Phase 4: Advanced Features
- Multiple tile servers
- Satellite imagery support
- Custom tile sources
- Offline tile storage

## Testing and Validation

### Coordinate Accuracy
- Tested with known geographic locations
- Verified tile coordinate calculations
- Confirmed round-trip coordinate conversions

### Interactive Behavior
- Smooth panning and zooming
- Proper tile boundary updates
- Consistent coordinate transformations

### Edge Cases
- High zoom levels (18-20)
- Polar regions (latitude limits)
- Antimeridian crossing (±180° longitude)

## Conclusion

The Web Mercator implementation in OSM-GPUI provides a solid foundation for web-compatible map rendering. The system exclusively uses industry-standard Web Mercator projection, ensuring compatibility with OpenStreetMap and other tile servers while providing accurate coordinate transformations and smooth interactive behavior.

The tile boundary visualization clearly demonstrates the correctness of the implementation, showing exact alignment with the Web Mercator tile grid used by major mapping services worldwide.