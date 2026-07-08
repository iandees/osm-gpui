# Area fills for closed ways — design

Date: 2026-07-07
Status: approved

## Goal

Closed ways (buildings, water, landuse, …) currently render as stroked
outlines only. Add MapCSS-driven fill styling so closed ways can be drawn
as translucent filled polygons, without regressing the per-frame render
budget (the codebase deliberately avoids per-frame Lyon tessellation).

## Scope

- MapCSS: `area` selector and `way:closed` pseudo-class (JOSM semantics),
  plus `fill-color` and `fill-opacity` declarations.
- Rendering: cached polygon triangulation (earcutr) computed at data-load /
  edit time; per-frame work is projection + batched triangle emission only.
- Default stylesheet gains fills for common area features.

Out of scope: relations/multipolygons (holes), `fill-image`, casing,
zoom-dependent styles.

## 1. MapCSS parser (`src/style/mapcss.rs`)

- New `TargetKind::Area`, parsed from the `area` selector ident.
- `way:closed` is normalized to `TargetKind::Area` at parse time; other
  pseudo-classes remain skipped with a warning. `:closed` on `node` is
  meaningless and skipped.
- `way` selectors continue to match **all** ways. A closed way receives
  both `way` and `area` rules, applied in stylesheet order (JOSM behavior).
- New declarations: `FillColor(u32)` (reuses `parse_color`) and
  `FillOpacity(f32)` clamped to 0..=1.
- `WayStyle` gains `fill: Option<Fill>` with
  `struct Fill { color: u32, opacity: f32 }`. When `fill-color` is set and
  no `fill-opacity` was given, opacity defaults to 0.4.
- `Stylesheet::way_style(&self, tags, closed: bool)` — signature change;
  all call sites updated. For a non-closed way, `area`/`:closed` rules
  don't match and `fill-*` declarations in plain `way` rules are ignored,
  so `fill` is always `None`.

## 2. Closedness (`src/layers/osm_layer.rs`)

A way is closed iff it has ≥ 4 node refs and `refs.first() == refs.last()`
(OSM convention; 4 refs = minimal triangle ring). Helper
`fn is_closed(way: &Way) -> bool`.

## 3. Fill cache

- New derived table `way_fill_tris: Vec<Option<Vec<u32>>>`, parallel to
  `way_vertices`/`way_bboxes`/`way_styles`. Entries are earcut triangle
  indices into that way's vertex slice with the duplicated closing vertex
  excluded. `Some` only for closed ways whose resolved style has a fill.
- Triangulation input is the Mercator-projected vertex ring (already in
  `way_vertices`).
- Computed in `compute_way_tables`; refreshed at every mutation point that
  already refreshes `way_styles`: tag edits, node-move commit, stylesheet
  swap, way add/remove. Degenerate rings (collinear, self-intersecting,
  < 3 distinct points, earcut returning empty) yield `None` — never panic.
- Dependency: `earcutr` crate (ear-clipping port of Mapbox earcut).

## 4. Painting (`OsmLayer::paint`)

- Fills paint **before** way strokes so outlines and nodes stay on top.
- Same Mercator-bbox cull as strokes. Visible fills group by
  `(fill_color, opacity.to_bits())`; each group is one batched
  `Path<Pixels>` of raw triangles with accumulated bounds — mirroring the
  existing stroke `WayGroup` pattern, no per-frame tessellation.
- Vertices are projected per frame with drag-preview offsets applied. A
  concave polygon's cached triangulation may be briefly stale mid-drag;
  it refreshes on drag commit. Accepted tradeoff.
- Opacity via `rgba((color << 8) | alpha)` in the `paint_path` call.

## 5. Default stylesheet (`assets/default.mapcss`)

Existing stroke rules unchanged. Added:

```mapcss
area[building]  { fill-color: #808080; fill-opacity: 0.4; }
area[natural=water], area[waterway=riverbank] { fill-color: #1e90ff; fill-opacity: 0.4; }
area[leisure=park], area[landuse=grass], area[landuse=forest] { fill-color: #228b22; fill-opacity: 0.35; }
area[landuse]   { fill-color: #9acd32; fill-opacity: 0.25; }
```

## 6. Error handling

- Parser: invalid `fill-color` / `fill-opacity` values are ignored with
  the existing once-per-parse warning; hard syntax errors still `Err`.
- Triangulation failure for a ring → no fill for that way; stroke still
  renders.

## 7. Testing

Parser tests:
- `area` rules match closed ways only; `way:closed` behaves identically.
- Fill default opacity (0.4) and clamping.
- Non-closed way resolves `fill: None` even when `way { fill-color: … }`
  is present.
- Updated default stylesheet parses; spot-check building fill.

Layer tests:
- `is_closed` edge cases (open way, 2-ref loop, triangle ring).
- Fill cache populated on load; refreshed on tag edit and stylesheet swap;
  cleared when a way is opened/deleted.
- Degenerate rings produce `None` without panicking.
