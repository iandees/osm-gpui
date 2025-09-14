use crate::coordinates::GeoBounds;
use crate::map::{FeatureGeometry, LayerStyle, MapFeature, MapLayer};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// GeoJSON-compatible data structures for loading map data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoJsonFeatureCollection {
    #[serde(rename = "type")]
    pub feature_type: String,
    pub features: Vec<GeoJsonFeature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoJsonFeature {
    #[serde(rename = "type")]
    pub feature_type: String,
    pub id: Option<serde_json::Value>,
    pub geometry: Option<GeoJsonGeometry>,
    pub properties: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoJsonGeometry {
    #[serde(rename = "type")]
    pub geometry_type: String,
    pub coordinates: serde_json::Value,
}

/// Simple OSM XML node for basic OSM data parsing
#[derive(Debug, Clone)]
pub struct OsmNode {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OsmWay {
    pub id: i64,
    pub nodes: Vec<i64>,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OsmData {
    pub nodes: HashMap<i64, OsmNode>,
    pub ways: Vec<OsmWay>,
}

/// Data loader for various map data formats
pub struct MapDataLoader {
    pub cache_dir: Option<std::path::PathBuf>,
}

impl MapDataLoader {
    pub fn new() -> Self {
        Self { cache_dir: None }
    }

    pub fn with_cache_dir<P: Into<std::path::PathBuf>>(mut self, cache_dir: P) -> Self {
        self.cache_dir = Some(cache_dir.into());
        self
    }

    /// Load GeoJSON data from a string
    pub fn load_geojson_str(&self, geojson_str: &str, layer_name: &str) -> Result<MapLayer> {
        let feature_collection: GeoJsonFeatureCollection = serde_json::from_str(geojson_str)?;
        self.convert_geojson_to_layer(feature_collection, layer_name)
    }

    /// Load GeoJSON data from a file
    pub fn load_geojson_file<P: AsRef<std::path::Path>>(
        &self,
        path: P,
        layer_name: &str,
    ) -> Result<MapLayer> {
        let content = std::fs::read_to_string(path)?;
        self.load_geojson_str(&content, layer_name)
    }

    /// Load GeoJSON data from a URL
    pub async fn load_geojson_url(&self, url: &str, layer_name: &str) -> Result<MapLayer> {
        let response = reqwest::get(url).await?;
        let geojson_str = response.text().await?;
        self.load_geojson_str(&geojson_str, layer_name)
    }

    /// Convert GeoJSON feature collection to MapLayer
    fn convert_geojson_to_layer(
        &self,
        collection: GeoJsonFeatureCollection,
        layer_name: &str,
    ) -> Result<MapLayer> {
        let mut features = Vec::new();

        for geojson_feature in collection.features {
            if let Some(geometry) = geojson_feature.geometry.clone() {
                let feature = self.convert_geojson_feature(geojson_feature, geometry)?;
                features.push(feature);
            }
        }

        Ok(MapLayer {
            name: layer_name.to_string(),
            features,
            visible: true,
            style: LayerStyle::default(),
        })
    }

    /// Convert a single GeoJSON feature
    fn convert_geojson_feature(
        &self,
        geojson_feature: GeoJsonFeature,
        geometry: GeoJsonGeometry,
    ) -> Result<MapFeature> {
        let id = match geojson_feature.id {
            Some(serde_json::Value::String(s)) => s,
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => uuid::Uuid::new_v4().to_string(),
        };

        let properties = geojson_feature
            .properties
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, v.to_string().trim_matches('"').to_string()))
            .collect();

        let feature_geometry = self.convert_geometry(geometry)?;

        Ok(MapFeature {
            id,
            geometry: feature_geometry,
            properties,
        })
    }

    /// Convert GeoJSON geometry to internal geometry format
    fn convert_geometry(&self, geometry: GeoJsonGeometry) -> Result<FeatureGeometry> {
        match geometry.geometry_type.as_str() {
            "Point" => {
                let coords = geometry
                    .coordinates
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid Point coordinates"))?;

                if coords.len() >= 2 {
                    let lon = coords[0]
                        .as_f64()
                        .ok_or_else(|| anyhow!("Invalid longitude"))?;
                    let lat = coords[1]
                        .as_f64()
                        .ok_or_else(|| anyhow!("Invalid latitude"))?;
                    Ok(FeatureGeometry::Point { lat, lon })
                } else {
                    Err(anyhow!("Point must have at least 2 coordinates"))
                }
            }
            "LineString" => {
                let coords_array = geometry
                    .coordinates
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid LineString coordinates"))?;

                let mut points = Vec::new();
                for coord in coords_array {
                    let coord_array = coord
                        .as_array()
                        .ok_or_else(|| anyhow!("Invalid coordinate in LineString"))?;

                    if coord_array.len() >= 2 {
                        let lon = coord_array[0]
                            .as_f64()
                            .ok_or_else(|| anyhow!("Invalid longitude"))?;
                        let lat = coord_array[1]
                            .as_f64()
                            .ok_or_else(|| anyhow!("Invalid latitude"))?;
                        points.push((lat, lon));
                    }
                }

                Ok(FeatureGeometry::LineString { points })
            }
            "Polygon" => {
                let rings = geometry
                    .coordinates
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid Polygon coordinates"))?;

                if rings.is_empty() {
                    return Err(anyhow!("Polygon must have at least one ring"));
                }

                // Extract exterior ring
                let exterior_ring = rings[0]
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid exterior ring"))?;

                let mut exterior = Vec::new();
                for coord in exterior_ring {
                    let coord_array = coord
                        .as_array()
                        .ok_or_else(|| anyhow!("Invalid coordinate in Polygon"))?;

                    if coord_array.len() >= 2 {
                        let lon = coord_array[0]
                            .as_f64()
                            .ok_or_else(|| anyhow!("Invalid longitude"))?;
                        let lat = coord_array[1]
                            .as_f64()
                            .ok_or_else(|| anyhow!("Invalid latitude"))?;
                        exterior.push((lat, lon));
                    }
                }

                // Extract holes (interior rings)
                let mut holes = Vec::new();
                for i in 1..rings.len() {
                    let hole_ring = rings[i]
                        .as_array()
                        .ok_or_else(|| anyhow!("Invalid hole ring"))?;

                    let mut hole = Vec::new();
                    for coord in hole_ring {
                        let coord_array = coord
                            .as_array()
                            .ok_or_else(|| anyhow!("Invalid coordinate in hole"))?;

                        if coord_array.len() >= 2 {
                            let lon = coord_array[0]
                                .as_f64()
                                .ok_or_else(|| anyhow!("Invalid longitude"))?;
                            let lat = coord_array[1]
                                .as_f64()
                                .ok_or_else(|| anyhow!("Invalid latitude"))?;
                            hole.push((lat, lon));
                        }
                    }
                    holes.push(hole);
                }

                Ok(FeatureGeometry::Polygon { exterior, holes })
            }
            _ => Err(anyhow!(
                "Unsupported geometry type: {}",
                geometry.geometry_type
            )),
        }
    }

    /// Create a simple sample dataset for testing
    pub fn create_sample_data() -> Vec<MapLayer> {
        let mut layers = Vec::new();

        // Create cities layer
        let mut cities_layer = MapLayer {
            name: "World Cities".to_string(),
            features: Vec::new(),
            visible: true,
            style: LayerStyle {
                stroke_color: gpui::rgb(0xff6b35).into(),
                stroke_width: 2.0,
                fill_color: None,
                point_radius: 6.0,
            },
        };

        // Add major world cities
        let cities = vec![
            ("New York", 40.7128, -74.0060),
            ("London", 51.5074, -0.1278),
            ("Tokyo", 35.6762, 139.6503),
            ("Paris", 48.8566, 2.3522),
            ("Sydney", -33.8688, 151.2093),
            ("São Paulo", -23.5505, -46.6333),
            ("Cairo", 30.0444, 31.2357),
            ("Mumbai", 19.0760, 72.8777),
        ];

        for (name, lat, lon) in cities {
            let mut properties = HashMap::new();
            properties.insert("name".to_string(), name.to_string());
            properties.insert("type".to_string(), "city".to_string());

            cities_layer.features.push(MapFeature {
                id: format!("city_{}", name.to_lowercase().replace(" ", "_")),
                geometry: FeatureGeometry::Point { lat, lon },
                properties,
            });
        }

        layers.push(cities_layer);

        // Create sample shipping routes layer
        let mut routes_layer = MapLayer {
            name: "Shipping Routes".to_string(),
            features: Vec::new(),
            visible: true,
            style: LayerStyle {
                stroke_color: gpui::rgb(0x3b82f6).into(),
                stroke_width: 3.0,
                fill_color: None,
                point_radius: 2.0,
            },
        };

        // Add sample shipping routes
        routes_layer.features.push(MapFeature {
            id: "route_transatlantic".to_string(),
            geometry: FeatureGeometry::LineString {
                points: vec![
                    (40.7128, -74.0060), // New York
                    (42.0, -50.0),       // Mid-Atlantic
                    (45.0, -30.0),       // Approaching Europe
                    (51.5074, -0.1278),  // London
                ],
            },
            properties: {
                let mut props = HashMap::new();
                props.insert("name".to_string(), "Transatlantic Route".to_string());
                props.insert("type".to_string(), "shipping".to_string());
                props
            },
        });

        routes_layer.features.push(MapFeature {
            id: "route_transpacific".to_string(),
            geometry: FeatureGeometry::LineString {
                points: vec![
                    (37.7749, -122.4194), // San Francisco
                    (35.0, -150.0),       // Mid-Pacific
                    (30.0, 140.0),        // Approaching Japan
                    (35.6762, 139.6503),  // Tokyo
                ],
            },
            properties: {
                let mut props = HashMap::new();
                props.insert("name".to_string(), "Transpacific Route".to_string());
                props.insert("type".to_string(), "shipping".to_string());
                props
            },
        });

        layers.push(routes_layer);

        // Create sample regions layer
        let mut regions_layer = MapLayer {
            name: "Sample Regions".to_string(),
            features: Vec::new(),
            visible: true,
            style: LayerStyle {
                stroke_color: gpui::rgb(0x10b981).into(),
                stroke_width: 2.0,
                fill_color: Some(gpui::hsla(0.33, 0.6, 0.5, 0.2)),
                point_radius: 3.0,
            },
        };

        // Add a sample region (simplified Great Lakes)
        regions_layer.features.push(MapFeature {
            id: "great_lakes_region".to_string(),
            geometry: FeatureGeometry::Polygon {
                exterior: vec![
                    (48.0, -92.0),
                    (48.0, -76.0),
                    (41.0, -76.0),
                    (41.0, -88.0),
                    (46.0, -92.0),
                    (48.0, -92.0),
                ],
                holes: vec![],
            },
            properties: {
                let mut props = HashMap::new();
                props.insert("name".to_string(), "Great Lakes Region".to_string());
                props.insert("type".to_string(), "region".to_string());
                props
            },
        });

        layers.push(regions_layer);

        layers
    }

    /// Calculate bounding box for a set of features
    pub fn calculate_bounds(features: &[MapFeature]) -> Option<GeoBounds> {
        if features.is_empty() {
            return None;
        }

        let mut min_lat = f64::INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut min_lon = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;

        for feature in features {
            match &feature.geometry {
                FeatureGeometry::Point { lat, lon } => {
                    min_lat = min_lat.min(*lat);
                    max_lat = max_lat.max(*lat);
                    min_lon = min_lon.min(*lon);
                    max_lon = max_lon.max(*lon);
                }
                FeatureGeometry::LineString { points } => {
                    for (lat, lon) in points {
                        min_lat = min_lat.min(*lat);
                        max_lat = max_lat.max(*lat);
                        min_lon = min_lon.min(*lon);
                        max_lon = max_lon.max(*lon);
                    }
                }
                FeatureGeometry::Polygon { exterior, holes: _ } => {
                    for (lat, lon) in exterior {
                        min_lat = min_lat.min(*lat);
                        max_lat = max_lat.max(*lat);
                        min_lon = min_lon.min(*lon);
                        max_lon = max_lon.max(*lon);
                    }
                }
            }
        }

        if min_lat != f64::INFINITY {
            Some(GeoBounds::new(min_lat, max_lat, min_lon, max_lon))
        } else {
            None
        }
    }

    /// Filter features by a bounding box
    pub fn filter_by_bounds(features: &[MapFeature], bounds: &GeoBounds) -> Vec<MapFeature> {
        features
            .iter()
            .filter(|feature| match &feature.geometry {
                FeatureGeometry::Point { lat, lon } => bounds.contains(*lat, *lon),
                FeatureGeometry::LineString { points } => {
                    points.iter().any(|(lat, lon)| bounds.contains(*lat, *lon))
                }
                FeatureGeometry::Polygon { exterior, holes: _ } => exterior
                    .iter()
                    .any(|(lat, lon)| bounds.contains(*lat, *lon)),
            })
            .cloned()
            .collect()
    }
}

impl Default for MapDataLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_data_creation() {
        let layers = MapDataLoader::create_sample_data();
        assert!(!layers.is_empty());

        let cities_layer = layers.iter().find(|l| l.name == "World Cities").unwrap();
        assert!(!cities_layer.features.is_empty());
    }

    #[test]
    fn test_bounds_calculation() {
        let features = vec![
            MapFeature {
                id: "test1".to_string(),
                geometry: FeatureGeometry::Point {
                    lat: 40.0,
                    lon: -74.0,
                },
                properties: HashMap::new(),
            },
            MapFeature {
                id: "test2".to_string(),
                geometry: FeatureGeometry::Point {
                    lat: 41.0,
                    lon: -73.0,
                },
                properties: HashMap::new(),
            },
        ];

        let bounds = MapDataLoader::calculate_bounds(&features).unwrap();
        assert_eq!(bounds.min_lat, 40.0);
        assert_eq!(bounds.max_lat, 41.0);
        assert_eq!(bounds.min_lon, -74.0);
        assert_eq!(bounds.max_lon, -73.0);
    }

    #[test]
    fn test_geojson_point_parsing() {
        let geojson_str = r#"
        {
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Point",
                        "coordinates": [-74.006, 40.7128]
                    },
                    "properties": {
                        "name": "New York"
                    }
                }
            ]
        }
        "#;

        let loader = MapDataLoader::new();
        let layer = loader.load_geojson_str(geojson_str, "test").unwrap();

        assert_eq!(layer.features.len(), 1);
        match &layer.features[0].geometry {
            FeatureGeometry::Point { lat, lon } => {
                assert_eq!(*lat, 40.7128);
                assert_eq!(*lon, -74.006);
            }
            _ => panic!("Expected Point geometry"),
        }
    }
}
