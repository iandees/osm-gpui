# Development Guide

This guide provides information for developers who want to contribute to or extend the OSM-GPUI map rendering system.

## Development Environment Setup

### Prerequisites

- Rust 1.70 or later
- Git
- A modern GPU with OpenGL 3.3+ support (for GPUI)

### Getting Started

1. Clone the repository:
```bash
git clone <repository-url>
cd osm-gpui
```

2. Build the project:
```bash
cargo build
```

3. Run the main application:
```bash
cargo run
```

4. Run examples:
```bash
cargo run --example simple_map
cargo run --example geojson_loader
```

5. Run tests:
```bash
cargo test
```

## Project Structure

```
osm-gpui/
├── src/
│   ├── lib.rs              # Library entry point and public API
│   ├── main.rs             # Main application entry point
│   ├── coordinates.rs      # Geographic coordinate system
│   ├── data.rs             # Data loading and parsing
│   ├── map.rs              # Main map view component
│   └── viewport.rs         # Camera and interaction system
├── examples/
│   ├── simple_map.rs       # Basic usage example
│   └── geojson_loader.rs   # GeoJSON loading example
├── Cargo.toml              # Package configuration
├── README.md               # User documentation
└── DEVELOPMENT.md          # This file
```

## Architecture Overview

### Core Components

1. **MapView** (`src/map.rs`)
   - Main rendering component that displays map features
   - Manages multiple layers with different styles
   - Handles user input events (mouse, keyboard)
   - Renders features based on current viewport

2. **Viewport** (`src/viewport.rs`)
   - Manages camera position, zoom level, and viewport size
   - Handles pan and zoom interactions
   - Provides coordinate transformation between screen and geographic coordinates
   - Maintains interaction state (dragging, mouse position)

3. **CoordinateTransform** (`src/coordinates.rs`)
   - Converts between geographic (lat/lon) and screen (pixel) coordinates
   - Handles zoom level calculations and geographic bounds
   - Provides utility functions for coordinate system operations

4. **MapDataLoader** (`src/data.rs`)
   - Loads and parses geographic data from various sources
   - Supports GeoJSON format and custom data structures
   - Provides utility functions for data manipulation and filtering

### Data Model

- **MapFeature**: Represents a single geographic feature (point, line, or polygon)
- **FeatureGeometry**: Defines the geometric shape of a feature
- **MapLayer**: Groups features with shared styling and visibility
- **LayerStyle**: Defines visual appearance (colors, line widths, etc.)

## Development Workflow

### Adding New Features

1. Create a feature branch:
```bash
git checkout -b feature/new-feature-name
```

2. Implement your changes following the existing code patterns
3. Add tests for new functionality
4. Update documentation as needed
5. Submit a pull request

### Code Style

- Follow standard Rust formatting (use `cargo fmt`)
- Use `cargo clippy` to check for common issues
- Write comprehensive documentation for public APIs
- Include examples in documentation when appropriate

### Testing

- Write unit tests for all new functionality
- Test coordinate transformations with known values
- Test data loading with sample GeoJSON files
- Include integration tests for complex interactions

Example test structure:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_functionality() {
        // Test setup
        let feature = create_test_feature();
        
        // Test execution
        let result = feature.some_method();
        
        // Assertions
        assert_eq!(result, expected_value);
    }
}
```

## Performance Considerations

### Rendering Optimization

- Features are only rendered if visible in the current viewport
- Use efficient data structures for large datasets
- Consider implementing spatial indexing for very large datasets
- Profile rendering performance with large numbers of features

### Memory Management

- Avoid loading entire large datasets into memory at once
- Implement streaming for large GeoJSON files
- Use appropriate data structures for different use cases

### GPU Utilization

- GPUI handles most GPU optimization automatically
- Minimize state changes during rendering
- Batch similar rendering operations when possible

## Debugging

### Debug Features

The application includes several debug features:

- Press `D` to show debug information overlay
- Press `G` to toggle coordinate grid
- Console logging for data loading operations

### Common Issues

1. **Coordinate System Problems**
   - Verify lat/lon order (latitude first, longitude second)
   - Check coordinate bounds are reasonable
   - Test coordinate transformation with known values

2. **Rendering Issues**
   - Ensure features are within the visible viewport
   - Check layer visibility settings
   - Verify styling parameters are valid

3. **Data Loading Problems**
   - Validate GeoJSON format with online tools
   - Check file paths and permissions
   - Verify network connectivity for remote data

### Logging

Add logging for debugging:
```rust
use log::{debug, info, warn, error};

info!("Loading layer: {}", layer_name);
debug!("Transformed coordinates: {:?}", screen_point);
```

## Extending the System

### Adding New Data Formats

To add support for a new data format:

1. Create parsing functions in `src/data.rs`
2. Convert to internal `MapFeature` representation
3. Add tests for the new format
4. Update documentation

Example:
```rust
impl MapDataLoader {
    pub fn load_shapefile(&self, path: &str) -> Result<MapLayer> {
        // Implementation here
    }
}
```

### Custom Rendering

To add custom rendering for specific feature types:

1. Extend `FeatureGeometry` enum if needed
2. Add rendering logic in `MapView::render_feature`
3. Update styling options in `LayerStyle`

### New Interaction Modes

To add new interaction modes:

1. Extend event handling in `MapView`
2. Add state management in `Viewport`
3. Update keyboard/mouse event handlers

## API Design Principles

- **Composability**: Components should work well together
- **Performance**: Optimize for smooth interaction with large datasets
- **Flexibility**: Allow customization without requiring core changes
- **Simplicity**: Keep the public API simple and intuitive

## Future Enhancements

### Planned Features

- Tile-based rendering for better performance
- Web Mercator projection support
- Additional data format support (Shapefile, KML)
- Label rendering for features
- Animation support for dynamic data
- Cluster rendering for dense point data

### Architecture Improvements

- Plugin system for custom renderers
- Async data loading with progress indicators
- Multi-threading for data processing
- Spatial indexing for large datasets

## Contributing Guidelines

1. Read and understand the codebase
2. Start with small, focused changes
3. Include tests for all new functionality
4. Update documentation for API changes
5. Follow the existing code style and patterns
6. Be responsive to code review feedback

## Resources

- [GPUI Documentation](https://github.com/zed-industries/zed)
- [GeoJSON Specification](https://tools.ietf.org/html/rfc7946)
- [Rust Geographic Libraries](https://github.com/georust)
- [OpenStreetMap Data](https://www.openstreetmap.org/)

## Support

For questions or issues:
1. Check existing GitHub issues
2. Review the documentation and examples
3. Create a new issue with detailed description and reproduction steps