# OSM-GPUI Map Viewer

A high-performance map rendering system built with Rust and the GPUI framework. This application provides a scrollable, zoomable map view for displaying geographic data including points, lines, and polygons with full **OpenStreetMap XML data support**.

## Features

- **🗺️ OpenStreetMap Integration**: Full OSM XML parsing and rendering
- **🖱️ Interactive Map View**: Pan by dragging, zoom with mouse wheel
- **📊 Multiple Data Formats**: Support for OSM XML, GeoJSON, and custom data
- **🎨 Smart Feature Classification**: Automatic categorization of POIs, roads, buildings
- **📋 Layer Management**: Toggle visibility of different map layers
- **🎯 Web Mercator Projection**: Industry-standard EPSG:3857 projection with coordinate grid
- **⚡ Performance Optimized**: Built on GPUI for smooth GPU-accelerated rendering
- **✨ Customizable Styling**: Configure colors, line widths, and point sizes per layer
- **🔍 Intelligent Filtering**: POI-only, roads-only, buildings-only view modes

## Getting Started

### Prerequisites

- Rust 1.70 or later
- Git

### Installation

1. Clone the repository:
```bash
git clone <repository-url>
cd osm-gpui
```

2. Build and run:
```bash
cargo run
```

## Usage

### Basic Controls

- **Pan**: Click and drag to move around the map
- **Zoom**: Use mouse wheel to zoom in/out
- **Tile Toggle**: Press `T` to show/hide tile boundaries
- **Load Sample**: Press `L` to load sample OSM data
- **Coordinate Grid**: Always visible - shows Web Mercator coordinate system
- **Debug Information**: Always visible - shows map stats and coordinates

### Working with Data

#### 🗺️ OpenStreetMap Data (Primary Feature)

Load and display real OpenStreetMap XML data:

```rust
use osm_gpui::{OsmParser, OsmToMapConverter};

// Parse OSM XML data
let parser = OsmParser::new();
let osm_data = parser.parse_file("sample_data.osm")?;

// Convert to different feature types
let poi_features = OsmToMapConverter::city_poi().convert(&osm_data);
let road_features = OsmToMapConverter::transportation().convert(&osm_data);
let building_features = OsmToMapConverter::buildings().convert(&osm_data);

// Add to map viewer
for feature in poi_features {
    map_view.add_osm_feature(feature);
}
```

**Supported OSM Elements:**
- ✅ **Nodes**: Points of interest (restaurants, hospitals, schools, banks, etc.)
- ✅ **Ways**: Roads, waterways, building outlines, park boundaries
- ✅ **Relations**: Complex features like bus routes
- ✅ **Tags**: Full tag-based feature classification
- ✅ **Bounds**: Automatic viewport fitting

**Interactive Controls:**
- **L** - Load sample OSM data
- **O** - Toggle OSM feature visibility
- **P** - Toggle POI display
- **R** - Toggle roads display  
- **B** - Toggle buildings display

#### 📊 Built-in Sample Data

The application includes sample data for immediate testing:
- NYC landmarks (Central Park, Brooklyn Bridge, Empire State Building, etc.)
- World cities (New York, London, Tokyo, Paris, etc.)
- Transportation networks and shipping routes
- Sample geographic regions

#### 🌐 GeoJSON Data Support

```rust
use osm_gpui::data::MapDataLoader;

// Load from file
let loader = MapDataLoader::new();
let layer = loader.load_geojson_file("path/to/data.geojson", "My Layer")?;

// Load from URL (async)
let layer = loader.load_geojson_url("https://example.com/data.geojson", "Remote Layer").await?;

// Add to map
map_view.add_layer(layer);
```

#### Creating Custom Features

```rust
use osm_gpui::map::{MapFeature, FeatureGeometry, MapLayer, LayerStyle};
use std::collections::HashMap;

// Create a point feature
let point_feature = MapFeature {
    id: "my_point".to_string(),
    geometry: FeatureGeometry::Point { lat: 40.7128, lon: -74.0060 },
    properties: {
        let mut props = HashMap::new();
        props.insert("name".to_string(), "My Location".to_string());
        props
    },
};

// Create a line feature
let line_feature = MapFeature {
    id: "my_line".to_string(),
    geometry: FeatureGeometry::LineString {
        points: vec![
            (40.7128, -74.0060),
            (40.7589, -73.9851),
            (40.7831, -73.9712),
        ],
    },
    properties: HashMap::new(),
};

// Create a layer with custom styling
let mut layer = MapLayer {
    name: "Custom Layer".to_string(),
    features: vec![point_feature, line_feature],
    visible: true,
    style: LayerStyle {
        stroke_color: gpui::rgb(0xff0000).into(), // Red
        stroke_width: 2.0,
        fill_color: Some(gpui::hsla(0.0, 1.0, 0.5, 0.3)), // Semi-transparent red
        point_radius: 8.0,
    },
};
```

## Architecture

### Core Components

1. **🗺️ OSM Integration** (`src/osm.rs`, `src/osm_converter.rs`): OpenStreetMap support
   - Full OSM XML parsing (nodes, ways, relations)
   - Smart feature classification and styling
   - Multiple conversion presets (POI, transportation, buildings)
   - Efficient data filtering and processing

2. **🖥️ MapView** (`src/map.rs`): Main rendering component
   - Handles feature rendering and user interaction
   - Manages multiple layers with different styles
   - Provides viewport control and event handling
   - GPU-accelerated rendering with GPUI

3. **📷 Viewport** (`src/viewport.rs`): Camera and interaction system
   - Manages pan and zoom operations
   - Handles mouse and keyboard events
   - Provides smooth interaction feedback

4. **🌐 CoordinateTransform** (`src/coordinates.rs`): Geographic coordinate system
   - Converts between lat/lon and screen coordinates
   - Handles zoom-level calculations
   - Manages geographic bounds

5. **📊 MapDataLoader** (`src/data.rs`): Multi-format data loading
   - GeoJSON format support
   - Sample data generation
   - Feature filtering and bounds calculation

### Data Structures

#### OSM Data Types
- **OsmNode**: Geographic point with tags (lat/lon + metadata)
- **OsmWay**: Sequence of nodes forming lines or areas
- **OsmRelation**: Complex features composed of multiple elements
- **OsmData**: Complete parsed OSM dataset

#### Map Rendering Types  
- **MapFeature**: Processed geographic feature ready for rendering
- **MapLayer**: Collection of features with shared styling
- **FeatureGeometry**: Geometric representation (Point, LineString, Polygon)
- **FeatureType**: Classification (Restaurant, Road, Building, etc.)
- **Viewport**: Camera state and interaction handling

## Performance Considerations

### Rendering Optimizations
- **Viewport Culling**: Features only rendered if visible in current view
- **GPU Acceleration**: GPUI provides smooth, hardware-accelerated rendering
- **Efficient Coordinate Transforms**: Optimized for current zoom level
- **Layer Management**: Toggle visibility to improve performance with large datasets
- **Smart Feature Limits**: Configurable maximum features per converter (e.g., 500-2000)

### OSM Data Handling
- **Streaming Parser**: Memory-efficient XML processing
- **Selective Conversion**: Only convert needed feature types
- **Intelligent Filtering**: POI-only, major roads, significant buildings
- **Lazy Loading**: Features loaded and converted on demand

## Supported Data Formats

### 🗺️ OpenStreetMap XML (Primary)
Full OSM XML format support including:
- **Nodes**: Points with geographic coordinates and tags
- **Ways**: Connected sequences of nodes (roads, boundaries, buildings)
- **Relations**: Complex features (bus routes, multipolygons)
- **Tags**: Key-value metadata for feature classification
- **Bounds**: Geographic bounding boxes
- **Complete tag-based feature classification**

### 🌐 GeoJSON (Secondary)
Standard GeoJSON format with support for:
- Point geometries
- LineString geometries  
- Polygon geometries (with holes)
- Feature properties

### 🛠️ Custom Data (Programmatic)
Direct creation of MapFeature objects for programmatic data generation.

## Coordinate System

The system exclusively uses Web Mercator projection (EPSG:3857) for all coordinate transformations. Geographic coordinates (WGS84) are converted to Web Mercator meters and then to screen coordinates. The coordinate system:

- **Projection**: Web Mercator (EPSG:3857) - same as Google Maps, OpenStreetMap
- **Latitude Range**: ±85.05° (Web Mercator limits)
- **Longitude Range**: -180° to +180° (West to East)
- **Zoom Levels**: 0-20 (0 = world view, 20 = maximum detail)
- **Tile Compatibility**: Standard 256x256 pixel web map tiles
- **Coordinate Grid**: Always visible, adapts spacing to zoom level
- **Debug Information**: Always displayed with real-time map statistics

## Examples

### 🗺️ OSM Data Loading Example

```rust
use gpui::*;
use osm_gpui::{OsmParser, OsmToMapConverter};

fn main() {
    App::new().run(|cx: &mut AppContext| {
        cx.open_window(
            WindowOptions::default(),
            |cx| {
                cx.new_view(|cx| {
                    let mut map_view = MapView::new(cx);
                    
                    // Load OSM data
                    let parser = OsmParser::new();
                    let osm_data = parser.parse_file("sample_data.osm").unwrap();
                    
                    // Convert different feature types
                    let poi_features = OsmToMapConverter::city_poi().convert(&osm_data);
                    let road_features = OsmToMapConverter::transportation().convert(&osm_data);
                    
                    // Add to map
                    for feature in poi_features {
                        map_view.add_osm_feature(feature);
                    }
                    
                    map_view.fit_to_features();
                    map_view
                })
            },
        );
    });
}
```

### 📊 Basic Map Application (Built-in Data)

```rust
use gpui::*;
use osm_gpui::{MapView, MapDataLoader};

fn create_map() -> View<MapView> {
    let mut map_view = MapView::new(cx);
    
    // Load sample data
    let layers = MapDataLoader::create_sample_data();
    for layer in layers {
        map_view.add_layer(layer);
    }
    
    map_view.fit_to_features();
    map_view
}
```

### 🎨 OSM Feature Classification

Features are automatically classified and styled based on OSM tags:

```rust
use osm_gpui::{OsmFeatureClassifier, FeatureType};

// Automatic classification
let restaurant = FeatureType::Restaurant;  // amenity=restaurant  
let major_road = FeatureType::MajorRoad;   // highway=primary
let hospital = FeatureType::Healthcare;    // amenity=hospital

// Get appropriate styling
let style = OsmFeatureClassifier::get_feature_style(&restaurant);
// Returns: Red color, 6px radius for restaurants
```

### 🎨 Custom Styling

```rust
use gpui::*;
use osm_gpui::map::LayerStyle;

let custom_style = LayerStyle {
    stroke_color: rgb(0x00ff00).into(),     // Green
    stroke_width: 3.0,
    fill_color: Some(hsla(0.33, 0.8, 0.6, 0.4)), // Semi-transparent green
    point_radius: 10.0,
};
```

## Building from Source

### Debug Build
```bash
cargo build
```

### Release Build
```bash
cargo build --release
```

### Running Tests
```bash
cargo test
```

## Troubleshooting

### Common Issues

#### "there is no reactor running, must be called from the context of a Tokio 1.x runtime"

**Solved**: This error was resolved by implementing isolated Tokio runtimes within GPUI's executor context. The tile cache system now creates dedicated Tokio runtimes for HTTP operations while maintaining compatibility with GPUI's async system.

**Technical Details**: The fix involves creating a `tokio::runtime::Runtime` within each download task and using `rt.block_on()` to execute HTTP operations within the proper Tokio context, while the overall task runs in GPUI's executor.

**📖 For complete technical details, see**: [TOKIO_RUNTIME_FIX.md](TOKIO_RUNTIME_FIX.md)

#### Tile Loading Issues

If tiles are not loading properly:
1. Check network connectivity
2. Verify console output for download progress
3. Look for error messages in the debug overlay
4. Ensure proper tile server URLs in the implementation

## Dependencies

- **gpui**: Modern GPU-accelerated UI framework
- **quick-xml**: Fast XML parsing for OSM data
- **geo/geo-types**: Geographic data types and algorithms
- **serde/serde_json**: Serialization for GeoJSON parsing
- **reqwest**: HTTP client for remote data loading
- **tokio**: Async runtime for network operations (with runtime isolation fix)
- **uuid**: Unique identifier generation

### Async Runtime Architecture

This project uses a hybrid async runtime approach to integrate GPUI with Tokio-based HTTP operations:

- **GPUI Executor**: Handles UI tasks and general async operations
- **Isolated Tokio Runtime**: Created locally for HTTP operations that require Tokio context
- **Runtime Isolation**: Each background download task creates its own Tokio runtime to avoid "no reactor running" errors

This design ensures compatibility between GPUI's async system and reqwest's Tokio dependency without requiring the entire application to run under a Tokio runtime.

**📖 Complete Implementation Guide**: See [TOKIO_RUNTIME_FIX.md](TOKIO_RUNTIME_FIX.md) for detailed technical documentation of this solution, including code examples and usage patterns for other GPUI applications.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests for new functionality
5. Submit a pull request

## License

This project is open source. Please see the LICENSE file for details.

## Examples & Usage

### 🚀 Running Examples

```bash
# Main application with built-in sample data
cargo run --bin osm-gpui

# OSM viewer with Berlin sample data  
cargo run --example osm_viewer

# Simple map with custom features
cargo run --example simple_map

# GeoJSON data loading example
cargo run --example geojson_loader

# Standalone OSM parsing demo (no GUI)
rustc osm_demo.rs && ./osm_demo
```

### 📊 Sample OSM Data

The project includes `sample_data.osm` with NYC landmarks:
- 🏛️ NYC City Hall, Times Square, Empire State Building
- 🍽️ Restaurants and cafes
- 🏥 Hospitals and healthcare facilities  
- 🏫 Schools and universities
- 🛣️ Major streets and transportation
- 🌳 Parks and recreational areas

## Roadmap

### 🔄 Current Status
- ✅ Full OSM XML parsing and rendering
- ✅ Interactive GPUI-based map viewer
- ✅ Smart feature classification and styling
- ✅ Multiple data format support
- ✅ Performance-optimized rendering
- ✅ Async tile loading with Tokio runtime isolation
- ✅ HTTP operations working correctly with GPUI integration

### 🎯 Future Enhancements
- [x] ~~Background raster tile loading from OSM tile servers~~ ✅ **Implemented**
- [ ] Additional data format support (Shapefile, KML, PBF)
- [ ] Advanced styling options (graduated symbols, labels)
- [ ] Animation support for dynamic data
- [x] ~~Tile caching and offline support~~ ✅ **Basic caching implemented**
- [ ] Multi-threading for large dataset processing
- [ ] Plugin system for custom renderers
- [ ] Real-time OSM data fetching via Overpass API
- [ ] Customizable debug information layout
- [ ] Persistent disk caching for tiles
- [ ] Multiple tile server support