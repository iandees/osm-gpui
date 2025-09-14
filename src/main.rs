use gpui::{
    actions, canvas, div, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    Menu, MenuItem, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render,
    ScrollWheelEvent, SystemMenuType, Window, WindowOptions,
};
use std::sync::{Arc, Mutex};

mod coordinates;
mod osm;
mod tile_cache;
mod tiles;
mod viewport;
mod layers;

use coordinates::lat_lon_to_mercator;
use tile_cache::TileCache;
use osm::{OsmData, OsmParser};
use viewport::Viewport;
use layers::{LayerManager, tile_layer::TileLayer, osm_layer::OsmLayer, grid_layer::GridLayer};

actions!(osm_gpui, [OpenOsmFile, Quit]);

// Replace single optional data store with a queue of datasets awaiting layer creation
static SHARED_OSM_DATA: std::sync::OnceLock<Arc<Mutex<Vec<(String, OsmData)>>>> =
    std::sync::OnceLock::new();

struct MapViewer {
    viewport: Viewport,
    layer_manager: LayerManager,
    tile_cache: Arc<Mutex<TileCache>>,
    show_tile_boundaries: bool,
    first_dataset_fitted: bool,
}

impl MapViewer {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let viewport = Viewport::new(40.7128, -74.0060, 11.0, gpui::size(px(800.0), px(600.0)));
        let executor = cx.background_executor().clone();
        let tile_cache = Arc::new(Mutex::new(TileCache::new(executor)));
        let mut layer_manager = LayerManager::new();
        layer_manager.add_layer(Box::new(TileLayer::new(tile_cache.clone())));
        layer_manager.add_layer(Box::new(GridLayer::new()));
        // No default OSM layer; loaded files add their own
        Self {
            viewport,
            layer_manager,
            tile_cache,
            show_tile_boundaries: false,
            first_dataset_fitted: false,
        }
    }

    /// Fit view to show OSM data
    fn fit_to_osm_data(&mut self, osm_data: &OsmData) {
        if osm_data.nodes.is_empty() {
            return;
        }

        let mut min_lat = f64::INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut min_lon = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;

        for node in osm_data.nodes.values() {
            min_lat = min_lat.min(node.lat);
            max_lat = max_lat.max(node.lat);
            min_lon = min_lon.min(node.lon);
            max_lon = max_lon.max(node.lon);
        }

        if min_lat != f64::INFINITY {
            let mut center_lat = (min_lat + max_lat) / 2.0;
            let mut center_lon = (min_lon + max_lon) / 2.0;

            // If bounding box height is zero, set to a small value
            if (max_lat - min_lat).abs() < 1e-6 {
                center_lat = min_lat;
                min_lat -= 0.005;
                max_lat += 0.005;
            }
            if (max_lon - min_lon).abs() < 1e-6 {
                center_lon = min_lon;
                min_lon -= 0.005;
                max_lon += 0.005;
            }

            // Calculate required zoom to fit bounding box
            let margin = 1.2; // Add 20% margin
            let viewport = &self.viewport;
            let screen_width = viewport.transform.screen_size.width.0 as f64;
            let screen_height = viewport.transform.screen_size.height.0 as f64;

            // Convert bounding box to Mercator
            let (min_x, min_y) = lat_lon_to_mercator(min_lat, min_lon);
            let (max_x, max_y) = lat_lon_to_mercator(max_lat, max_lon);
            let bbox_width = (max_x - min_x).abs();
            let bbox_height = (max_y - min_y).abs();

            // Calculate zoom to fit bbox in screen
            let world_width_meters = 40075016.686;
            let tile_size = 256.0;
            let zoom_x = ((screen_width * world_width_meters) / (bbox_width * tile_size * margin)).log2();
            let zoom_y = ((screen_height * world_width_meters) / (bbox_height * tile_size * margin)).log2();
            let zoom_level = zoom_x.min(zoom_y).max(1.0).min(18.0); // Clamp zoom to [1, 18]

            eprintln!("fit_to_osm_data: center=({:.6}, {:.6}), zoom_level={:.2}", center_lat, center_lon, zoom_level);

            self.viewport.pan_to(center_lat, center_lon);
            self.viewport.set_zoom(zoom_level);
        }
    }

    fn zoom(&mut self, delta: f64) {
        let new_zoom = (self.viewport.zoom_level() + delta).max(1.0).min(20.0);
        self.viewport.set_zoom(new_zoom);
    }

    fn toggle_layer_visibility(&mut self, layer_name: &str) {
        if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
            let current_visibility = layer.is_visible();
            layer.set_visible(!current_visibility);
            eprintln!("🔄 Toggled {} layer: {}", layer_name, if !current_visibility { "ON" } else { "OFF" });
        }
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent) {
        // Adjust mouse coordinates to account for header offset
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);

        self.viewport.handle_mouse_down(adjusted_position);
        println!(
            "🖱️ Mouse down at: {:.1}, {:.1} (adjusted: {:.1}, {:.1})",
            event.position.x.0, event.position.y.0,
            adjusted_position.x.0, adjusted_position.y.0
        );
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // Adjust mouse coordinates to account for header offset
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);

        if self.viewport.handle_mouse_move(adjusted_position) {
            cx.notify();
        }
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent) {
        self.viewport.handle_mouse_up();
        println!("🖱️ Mouse up");
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let scroll_delta = match event.delta {
            gpui::ScrollDelta::Lines(delta) => gpui::Point {
                x: px(delta.x),
                y: px(delta.y),
            },
            gpui::ScrollDelta::Pixels(delta) => gpui::Point {
                x: px(delta.x.0 / 10.0),
                y: px(delta.y.0 / 10.0),
            },
        };

        // Adjust mouse coordinates to account for header offset
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);

        if self.viewport.handle_scroll(adjusted_position, scroll_delta) {
            cx.notify();
        }
    }

    fn check_for_new_osm_data(&mut self, cx: &mut Context<Self>) {
        if let Some(queue) = SHARED_OSM_DATA.get() {
            if let Ok(mut guard) = queue.try_lock() {
                if guard.is_empty() { return; }
                for (name, data) in guard.drain(..) {
                    let file_name = if name.is_empty() { "OSM".to_string() } else { name };
                    // Ensure unique layer name
                    let mut candidate = file_name.clone();
                    let mut i = 2;
                    while self.layer_manager.find_layer(&candidate).is_some() {
                        candidate = format!("{} ({})", file_name, i);
                        i += 1;
                    }
                    let data_arc = Arc::new(data.clone());
                    let layer = OsmLayer::new_with_data(candidate.clone(), data_arc.clone());
                    self.layer_manager.add_layer(Box::new(layer));
                    if !self.first_dataset_fitted {
                        self.fit_to_osm_data(&data);
                        self.first_dataset_fitted = true;
                    }
                }
                cx.notify();
            }
        }
    }

    fn get_layer_stats(&self) -> (usize, usize, usize) {
        let mut cached_files = 0;
        let mut osm_nodes = 0;
        let mut osm_ways = 0;

        for layer in self.layer_manager.layers() {
            let stats = layer.stats();
            for (key, value) in stats {
                match key.as_str() {
                    "Cached Files" => cached_files = value.parse().unwrap_or(0),
                    "Nodes" => osm_nodes = value.parse().unwrap_or(0),
                    "Ways" => osm_ways = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }

        // Calculate visible tiles
        let zoom_level = self.viewport.zoom_level();
        let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;
        let bounds_geo = self.viewport.visible_bounds();
        let visible_tiles = tiles::get_tiles_for_bounds(
            bounds_geo.min_lat, bounds_geo.min_lon, bounds_geo.max_lat, bounds_geo.max_lon, tile_zoom
        );
        let total_tiles = visible_tiles.len();

        (total_tiles, cached_files, osm_nodes + osm_ways)
    }
}

impl Render for MapViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Update viewport size to actual window dimensions minus the right panel and header
        let window_size = window.bounds().size;
        let panel_width = px(280.0);
        let header_height = px(48.0); // h_12() = 12 * 4px = 48px
        let map_size = gpui::size(
            window_size.width - panel_width,
            window_size.height - header_height
        );
        self.viewport.update_size(map_size);

        // Process queued OSM datasets into layers before stats and listing
        self.check_for_new_osm_data(cx);

        // Update all layers
        self.layer_manager.update_all();

        let (center_lat, center_lon) = self.viewport.center();
        let zoom_level = self.viewport.zoom_level();
        let (total_tiles, cached_files, osm_objects) = self.get_layer_stats();

        // Collect layer information for the UI
        let layer_info: Vec<(String, bool)> = self.layer_manager.layers()
            .iter()
            .map(|layer| (layer.name().to_string(), layer.is_visible()))
            .collect();

        div()
            .size_full()
            .bg(rgb(0x1a202c))
            .flex()
            .flex_row()
            .child(
                // Main content area (header + map)
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        // Header with menu
                        div()
                            .h_12()
                            .bg(rgb(0x111827))
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .child(
                                div()
                                    .text_color(rgb(0xffffff))
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("🗺️ OSM-GPUI Map Viewer (Layered)"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0x9ca3af))
                                    .text_sm()
                                    .child("Mouse to pan/zoom | 'T' tiles | Click layers to toggle"),
                            ),
                    )
                    .child(
                        // Map area
                        div()
                            .flex_1()
                            .relative()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                                    this.handle_mouse_down(ev);
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                                this.handle_mouse_move(ev, cx);
                            }))
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseUpEvent, _, _| {
                                    this.handle_mouse_up(ev);
                                }),
                            )
                            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                                this.handle_scroll(ev, cx);
                            }))
                            .child(
                                div()
                                    .size_full()
                                    .relative()
                                    .overflow_hidden() // Add clipping to prevent tiles from drawing outside viewport
                                    // Render all layer elements (raster content like tiles)
                                    .children(self.layer_manager.render_all_elements(&self.viewport))
                                    // Render canvas layers (vector content)
                                    .child(
                                        canvas(
                                            |_, _, _| {},
                                            {
                                                let viewport_clone = self.viewport.clone();
                                                let layer_manager = std::ptr::addr_of!(self.layer_manager);
                                                move |bounds, _, window, _| {
                                                    // Print debug info to understand coordinate spaces
                                                    eprintln!("Canvas bounds: {:?}", bounds);
                                                    eprintln!("Viewport size: {:?}", viewport_clone.transform.screen_size);

                                                    let layer_manager = unsafe { &*layer_manager };
                                                    layer_manager.render_all_canvas(&viewport_clone, bounds, window);
                                                }
                                            }
                                        )
                                        .absolute()
                                        .size_full() // Ensure canvas fills the entire map area
                                    )
                            )
                            .child(
                                // Debug info overlay
                                div()
                                    .absolute()
                                    .top_4()
                                    .left_4()
                                    .p_3()
                                    .bg(gpui::black())
                                    .rounded_lg()
                                    .text_color(rgb(0xffffff))
                                    .text_sm()
                                    .opacity(0.9)
                                    .min_w_64()
                                    .child(format!("🔍 Zoom: {:.1}", zoom_level))
                                    .child(format!("🌍 Center: {:.4}°N, {:.4}°W", center_lat, center_lon.abs()))
                                    .child(format!("📊 Objects: {}", osm_objects))
                                    .child(format!("🗺️ Tiles: {} visible", total_tiles))
                                    .child(format!("💾 Cache: {} files", cached_files))
                            ),
                    )
            )
            .child(
                // Right panel with layer controls
                div()
                    .w(px(280.0))
                    .h_full()
                    .bg(rgb(0x111827))
                    .border_l_1()
                    .border_color(rgb(0x374151))
                    .flex()
                    .flex_col()
                    .child(
                        // Panel header
                        div()
                            .h_12()
                            .bg(rgb(0x1f2937))
                            .flex()
                            .items_center()
                            .px_4()
                            .border_b_1()
                            .border_color(rgb(0x374151))
                            .child(
                                div()
                                    .text_color(rgb(0xffffff))
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("🏗️ Layer Controls")
                            )
                    )
                    .child(
                        // Layer list container
                        div()
                            .flex_1()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(
                                layer_info.iter().enumerate().map(|(index, (name, is_visible))| {
                                    let layer_name = name.clone();
                                    div()
                                        .id(("layer", index))
                                        .p_3()
                                        .bg(rgb(0x1f2937))
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(if *is_visible { rgb(0x10b981) } else { rgb(0x374151) })
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |this, _event: &MouseDownEvent, _, cx| {
                                                this.toggle_layer_visibility(&layer_name);
                                                cx.notify();
                                            }),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    // Checkbox
                                                    div()
                                                        .w(px(20.0))
                                                        .h(px(20.0))
                                                        .rounded_sm()
                                                        .border_2()
                                                        .border_color(if *is_visible { rgb(0x10b981) } else { rgb(0x6b7280) })
                                                        .bg(if *is_visible { rgb(0x10b981) } else { rgb(0x374151) })
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when(*is_visible, |this| {
                                                            this.child(
                                                                div()
                                                                    .text_color(rgb(0xffffff))
                                                                    .text_sm()
                                                                    .font_weight(gpui::FontWeight::BOLD)
                                                                    .child("✓")
                                                            )
                                                        })
                                                )
                                                .child(
                                                    div()
                                                        .text_color(rgb(0xffffff))
                                                        .text_sm()
                                                        .font_weight(gpui::FontWeight::MEDIUM)
                                                        .child(name.clone())
                                                )
                                        )
                                        .child(
                                            // Layer order indicator
                                            div()
                                                .text_color(rgb(0x9ca3af))
                                                .text_xs()
                                                .child(format!("#{}", index + 1))
                                        )
                                })
                                .collect::<Vec<_>>()
                            )
                    )
            )
    }
}

fn main() {
    // Initialize simple logging to stderr
    eprintln!("🚀 Starting OSM-GPUI Map Viewer with Tile Loading");

    // Initialize shared OSM data
    SHARED_OSM_DATA.set(Arc::new(Mutex::new(Vec::new()))).unwrap();

    Application::new().run(|cx: &mut App| {
        // Bring the menu bar to the foreground
        cx.activate(true);

        // Register the open file action
        cx.on_action(open_osm_file);
        cx.on_action(quit);

        // Set up OS menu system
        cx.set_menus(vec![
            Menu {
                name: "OSM Viewer".into(),
                items: vec![
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("Open…", OpenOsmFile)],
            },
        ]);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(Bounds {
                    origin: point(px(100.0), px(100.0)),
                    size: size(px(1200.0), px(800.0)),
                })),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("OSM-GPUI Map Viewer".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                focus: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| MapViewer::new(window, cx)),
        )
        .unwrap();

        cx.on_window_closed(|cx| {
            cx.quit();
        })
        .detach();
    });
}

// Handle the File > Open OSM File menu action
fn open_osm_file(_: &OpenOsmFile, cx: &mut App) {
    println!("🗂️ File > Open OSM File menu action triggered");

    let executor = cx.background_executor().clone();
    let shared_queue = SHARED_OSM_DATA.get().unwrap().clone();

    // Spawn async file dialog
    executor
        .spawn(async move {
            if let Some(file_path) = rfd::AsyncFileDialog::new()
                .add_filter("OSM files", &["osm", "xml"])
                .add_filter("All files", &["*"])
                .set_title("Select OSM file to open")
                .pick_file()
                .await
            {
                let path = file_path.path().to_path_buf();
                let path_str = path.to_string_lossy().to_string();
                println!("📁 Selected file: {}", path.display());

                // Parse OSM file in background
                let parser = OsmParser::new();
                match parser.parse_file(&path_str) {
                    Ok(osm_data) => {
                        println!("✅ Parsed OSM: {} nodes, {} ways", osm_data.nodes.len(), osm_data.ways.len());

                        if let Ok(mut q) = shared_queue.lock() {
                            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("OSM").to_string();
                            q.push((stem, osm_data));
                        }
                        println!("📊 Queued dataset for layer creation");
                    }
                    Err(e) => println!("❌ Failed to parse OSM file: {}", e),
                }
            } else {
                println!("❌ No file selected");
            }
        })
        .detach();
}

// Define the quit function that is registered with the App
fn quit(_: &Quit, cx: &mut App) {
    println!("Gracefully quitting the application . . .");
    cx.quit();
}
