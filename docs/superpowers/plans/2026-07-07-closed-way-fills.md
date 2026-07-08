# Closed-Way Area Fills Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Closed ways render as translucent filled polygons driven by MapCSS `area`/`way:closed` rules with `fill-color`/`fill-opacity`, without per-frame tessellation.

**Architecture:** The MapCSS evaluator learns an `Area` selector kind that matches only closed ways (`way_style` gains a `closed: bool` param). A new derived cache `way_fill_tris` (earcut triangle indices per way, computed at data-load/edit time) parallels the existing `way_styles` cache and is refreshed at the same mutation points. Painting projects cached triangles per frame and batches them into `Path`s grouped by fill color, exactly mirroring the existing stroke batching.

**Tech Stack:** Rust, GPUI (raw `Path` vertex pushes), `earcutr` for ear-clipping triangulation.

**Spec:** `docs/superpowers/specs/2026-07-07-closed-way-fills-design.md`

## Global Constraints

- A way is closed iff it has ≥ 4 node refs and first == last.
- Default fill opacity when `fill-color` set without `fill-opacity`: **0.4**.
- `fill-opacity` clamped to 0..=1.
- Fills paint **before** way strokes.
- No per-frame tessellation: triangulation only at load/edit/stylesheet-swap time.
- Degenerate rings never panic — they just get no fill.
- Commit messages: single line, no body, no Co-Authored-By, no conventional-commit prefix.
- After each task: `cargo test` passes, `cargo clippy --all-targets` clean, `cargo fmt` applied.

---

### Task 1: `OsmWay::is_closed()`

**Files:**
- Modify: `src/osm.rs` (struct `OsmWay` is at ~line 25; add an `impl OsmWay` block after it, and tests at the bottom of the file — check whether a `#[cfg(test)] mod tests` already exists and extend it if so)

**Interfaces:**
- Produces: `impl OsmWay { pub fn is_closed(&self) -> bool }` — used by Tasks 2 and 3.

- [ ] **Step 1: Write the failing tests**

Add to `src/osm.rs` (extend the existing test module if there is one, otherwise create it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn way(nodes: Vec<i64>) -> OsmWay {
        OsmWay {
            id: 1,
            nodes,
            version: 1,
            tags: HashMap::new(),
        }
    }

    #[test]
    fn is_closed_triangle_ring() {
        assert!(way(vec![1, 2, 3, 1]).is_closed());
    }

    #[test]
    fn is_closed_open_way() {
        assert!(!way(vec![1, 2, 3]).is_closed());
    }

    #[test]
    fn is_closed_two_node_loop_is_not_closed() {
        // 3 refs (first == last) is a degenerate 2-point "ring": not an area.
        assert!(!way(vec![1, 2, 1]).is_closed());
        assert!(!way(vec![1, 1]).is_closed());
        assert!(!way(vec![]).is_closed());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib osm::tests -- is_closed`
Expected: compile error — `is_closed` not found.

- [ ] **Step 3: Implement**

Add after the `OsmWay` struct in `src/osm.rs`:

```rust
impl OsmWay {
    /// A way is closed (an area candidate) iff it has at least 4 node refs
    /// and the first and last refs are the same node — the minimal closed
    /// ring is a triangle `[a, b, c, a]`. OSM convention.
    pub fn is_closed(&self) -> bool {
        self.nodes.len() >= 4 && self.nodes.first() == self.nodes.last()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib osm::tests -- is_closed`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/osm.rs
git commit -m "Add OsmWay::is_closed helper"
```

---

### Task 2: MapCSS `area` selector, `:closed`, and fill properties

**Files:**
- Modify: `src/style/mapcss.rs`
- Modify: `src/layers/osm_layer.rs` (5 call sites of `way_style(` at ~lines 289, 324, 883, 1218, 1781 — signature change)

**Interfaces:**
- Consumes: `OsmWay::is_closed()` from Task 1.
- Produces:
  - `pub struct Fill { pub color: u32, pub opacity: f32 }` (Copy, PartialEq, Debug, Clone)
  - `WayStyle` gains field `pub fill: Option<Fill>` (default `None`)
  - `pub const DEFAULT_FILL_OPACITY: f32 = 0.4;`
  - **Signature change:** `Stylesheet::way_style(&self, tags: &HashMap<String, String>, closed: bool) -> WayStyle`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/style/mapcss.rs`. Note: this signature change breaks ~17 existing test call sites of `way_style(&tags)` in this file — update them all to pass `false` as the second arg (they exercise stroke behavior on generic ways), EXCEPT where a test below says otherwise.

```rust
#[test]
fn area_rule_fills_closed_ways_only() {
    let s = Stylesheet::parse("area[building] { fill-color: #808080; fill-opacity: 0.4; }").unwrap();
    let closed = s.way_style(&tags(&[("building", "yes")]), true);
    let fill = closed.fill.expect("closed building way must have a fill");
    assert_eq!(fill.color, 0x808080);
    assert!((fill.opacity - 0.4).abs() < 1e-6);
    // Same tags, open way: area rule must not match.
    assert_eq!(s.way_style(&tags(&[("building", "yes")]), false).fill, None);
}

#[test]
fn way_closed_pseudo_class_is_area_equivalent() {
    let s = Stylesheet::parse("way:closed[building] { fill-color: #112233; }").unwrap();
    assert!(s.way_style(&tags(&[("building", "yes")]), true).fill.is_some());
    assert_eq!(s.way_style(&tags(&[("building", "yes")]), false).fill, None);
}

#[test]
fn fill_color_without_opacity_defaults() {
    let s = Stylesheet::parse("area { fill-color: red; }").unwrap();
    let fill = s.way_style(&tags(&[]), true).fill.unwrap();
    assert_eq!(fill.color, 0xFF0000);
    assert!((fill.opacity - DEFAULT_FILL_OPACITY).abs() < 1e-6);
}

#[test]
fn fill_opacity_without_color_is_no_fill() {
    let s = Stylesheet::parse("area { fill-opacity: 0.9; }").unwrap();
    assert_eq!(s.way_style(&tags(&[]), true).fill, None);
}

#[test]
fn fill_opacity_is_clamped() {
    let s = Stylesheet::parse("area { fill-color: red; fill-opacity: 7; }").unwrap();
    assert!((s.way_style(&tags(&[]), true).fill.unwrap().opacity - 1.0).abs() < 1e-6);
}

#[test]
fn fill_on_plain_way_rule_applies_only_when_closed() {
    // JOSM semantics: `way { fill-color: … }` fills closed ways, is
    // ignored on open ways.
    let s = Stylesheet::parse("way { fill-color: blue; }").unwrap();
    assert!(s.way_style(&tags(&[]), true).fill.is_some());
    assert_eq!(s.way_style(&tags(&[]), false).fill, None);
}

#[test]
fn area_rule_stroke_props_apply_to_closed_way() {
    let s = Stylesheet::parse("area[building] { color: #ff0000; width: 2; }").unwrap();
    let st = s.way_style(&tags(&[("building", "yes")]), true);
    assert_eq!(st.color, 0xFF0000);
    assert_eq!(st.width, 2.0);
    // Open way with same tags keeps defaults.
    let st = s.way_style(&tags(&[("building", "yes")]), false);
    assert_eq!(st.color, DEFAULT_WAY_COLOR);
}

#[test]
fn area_selector_never_matches_nodes() {
    let s = Stylesheet::parse("area { color: red; }").unwrap();
    assert_eq!(s.node_style(&tags(&[])).color, DEFAULT_NODE_COLOR);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib style::mapcss`
Expected: compile errors (`fill` field, `DEFAULT_FILL_OPACITY`, arity of `way_style`).

- [ ] **Step 3: Implement in `src/style/mapcss.rs`**

3a. New public types/consts next to `WayStyle`:

```rust
pub const DEFAULT_FILL_OPACITY: f32 = 0.4;

/// Resolved area fill for a closed way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fill {
    pub color: u32,
    /// 0.0..=1.0
    pub opacity: f32,
}
```

Add `pub fill: Option<Fill>` to `WayStyle`; add `fill: None` to its `Default`.

3b. Extend `TargetKind` with `Area`. `Selector::matches` gains a `closed: bool` param:

```rust
fn matches(&self, kind: TargetKind, closed: bool, tags: &HashMap<String, String>) -> bool {
    let kind_ok = match self.kind {
        TargetKind::Area => kind == TargetKind::Way && closed,
        k => k == kind,
    };
    kind_ok && self.tests.iter().all(|t| t.matches(tags))
}
```

`node_style` passes `false` for `closed` at its `matches` call site.

3c. Parser: in `parse_selector`, recognize `"area"` → `TargetKind::Area`. In the selector-suffix loop, replace the `Some(':')` arm so `:closed` on a `way` selector normalizes to `Area` instead of being skipped:

```rust
Some(':') => {
    self.bump();
    let pseudo = self.read_ident();
    if pseudo == "closed" && kind == TargetKind::Way {
        kind = TargetKind::Area;
    } else {
        if !*warned {
            eprintln!("mapcss: ignoring unsupported pseudo-class ':{}'", pseudo);
            *warned = true;
        }
        self.skip_selector_tail();
        break;
    }
}
```

(`kind` becomes `let mut kind = …`. The `Some('|')` zoom arm keeps its current warn-and-skip behavior — split the old combined `'|' | ':'` arm.)

3d. New declarations `FillColor(u32)` and `FillOpacity(f32)` in the `Declaration` enum, parsed in `parse_block`:

```rust
"fill-color" => match parse_color(&value) {
    Some(c) => decls.push(Declaration::FillColor(c)),
    None => {
        if !*warned {
            eprintln!("mapcss: ignoring unrecognized fill-color '{}'", value);
            *warned = true;
        }
    }
},
"fill-opacity" => match value.trim().parse::<f32>() {
    Ok(o) if o.is_finite() => decls.push(Declaration::FillOpacity(o.clamp(0.0, 1.0))),
    _ => {
        if !*warned {
            eprintln!("mapcss: ignoring invalid fill-opacity '{}'", value);
            *warned = true;
        }
    }
},
```

3e. `way_style` signature and evaluation:

```rust
pub fn way_style(&self, tags: &HashMap<String, String>, closed: bool) -> WayStyle {
    let mut s = WayStyle::default();
    let mut fill_color: Option<u32> = None;
    let mut fill_opacity: Option<f32> = None;
    for rule in &self.rules {
        if !rule
            .selectors
            .iter()
            .any(|sel| sel.matches(TargetKind::Way, closed, tags))
        {
            continue;
        }
        for d in &rule.declarations {
            match d {
                Declaration::Color(c) => s.color = *c,
                Declaration::Width(w) => s.width = *w,
                Declaration::SymbolSize(_) => {}
                Declaration::FillColor(c) => fill_color = Some(*c),
                Declaration::FillOpacity(o) => fill_opacity = Some(*o),
            }
        }
    }
    if closed {
        if let Some(color) = fill_color {
            s.fill = Some(Fill {
                color,
                opacity: fill_opacity.unwrap_or(DEFAULT_FILL_OPACITY),
            });
        }
    }
    s
}
```

`node_style`'s declaration match gains `Declaration::FillColor(_) | Declaration::FillOpacity(_) => {}` arms.

3f. Update `src/style/mod.rs` re-exports if it re-exports style types by name (check; add `Fill` if so).

3g. Update the 5 production call sites in `src/layers/osm_layer.rs` to pass closedness (all have a `way: &OsmWay` in scope):

- ~289 (`compute_way_tables`): `stylesheet.way_style(&way.tags, way.is_closed())`
- ~324 (`apply_style_refresh`): `stylesheet.way_style(&way.tags, way.is_closed())`
- ~883 (`commit_node_moves`): `self.stylesheet.way_style(&way.tags, way.is_closed())`
- ~1218 (`restore_way`): `self.stylesheet.way_style(&way.tags, way.is_closed())`
- ~1781 (selection highlight): `self.stylesheet.way_style(&way.tags, way.is_closed())`

3h. Update all pre-existing `way_style(&tags(...))` test call sites in `mapcss.rs` to `way_style(&tags(...), false)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: all pass (new fill tests + all pre-existing).

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets
git add src/style/mapcss.rs src/style/mod.rs src/layers/osm_layer.rs
git commit -m "Add MapCSS area selector, :closed pseudo-class, and fill properties"
```

---

### Task 3: Cached fill triangulation (`way_fill_tris`)

**Files:**
- Modify: `Cargo.toml` (add `earcutr = "0.4"` to `[dependencies]`, alphabetical order)
- Modify: `src/layers/osm_layer.rs`

**Interfaces:**
- Consumes: `WayStyle.fill` from Task 2.
- Produces:
  - `fn compute_fill_tris(verts: &[(i64, f64, f64)], style: &WayStyle) -> Option<Vec<u32>>` (free fn in `osm_layer.rs`)
  - `OsmLayer.way_fill_tris: Vec<Option<Vec<u32>>>` — parallel to `way_vertices`/`way_styles`; indices reference `verts[..verts.len()-1]` (closing duplicate excluded). Task 4 reads this in `paint`.
  - `WayTables` type alias gains a 5th element `Vec<Option<Vec<u32>>>`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/layers/osm_layer.rs`. The default stylesheet has `way[building] { color: #808080; width: 1; }` but no fill yet (that's Task 5), so these tests install an explicit stylesheet via the existing `set_stylesheet` method.

```rust
// -- Fill triangulation cache --

fn square_ring_data() -> Arc<OsmData> {
    // A unit-ish square around (40, -74): nodes 1-4, way 10 closed via ref 1.
    let mk = |id, dlat: f64, dlon: f64| OsmNode {
        id,
        lat: 40.0 + dlat,
        lon: -74.0 + dlon,
        version: 1,
        tags: empty_tags(),
    };
    let mut way_tags = HashMap::new();
    way_tags.insert("building".to_string(), "yes".to_string());
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4, 1],
        version: 1,
        tags: way_tags,
    };
    data_with(
        vec![
            mk(1, 0.0, 0.0),
            mk(2, 0.0, 0.001),
            mk(3, 0.001, 0.001),
            mk(4, 0.001, 0.0),
        ],
        vec![way],
    )
}

fn fill_stylesheet() -> Arc<Stylesheet> {
    Arc::new(Stylesheet::parse("area[building] { fill-color: #808080; fill-opacity: 0.4; }").unwrap())
}

#[test]
fn closed_way_with_fill_style_gets_triangulated() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
    layer.set_stylesheet(fill_stylesheet());
    let tris = layer.way_fill_tris[0]
        .as_ref()
        .expect("closed building way must have fill triangles");
    // A quad triangulates into 2 triangles = 6 indices, all within the
    // 4-vertex deduped ring.
    assert_eq!(tris.len(), 6);
    assert!(tris.iter().all(|&i| i < 4));
}

#[test]
fn open_way_gets_no_fill_triangles() {
    let mut data = square_ring_data();
    Arc::make_mut(&mut data).ways.get_mut(&10).unwrap().nodes = vec![1, 2, 3, 4];
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris[0], None);
}

#[test]
fn closed_way_without_fill_style_gets_no_triangles() {
    // Default stylesheet has no fill rules yet.
    let layer = OsmLayer::new_with_data(LayerId(1), "L", square_ring_data());
    assert_eq!(layer.way_fill_tris[0], None);
}

#[test]
fn set_tag_refreshes_fill_triangles() {
    let mut data = square_ring_data();
    Arc::make_mut(&mut data).ways.get_mut(&10).unwrap().tags.clear();
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris[0], None, "no building tag yet");
    layer.set_tag(FeatureKind::Way, 10, "building", "yes");
    assert!(layer.way_fill_tris[0].is_some(), "tag edit must recompute fill");
    layer.remove_tag(FeatureKind::Way, 10, "building");
    assert_eq!(layer.way_fill_tris[0], None, "tag removal must clear fill");
}

#[test]
fn degenerate_ring_yields_no_fill_without_panic() {
    // All four ring nodes collinear.
    let mk = |id, dlon: f64| OsmNode {
        id,
        lat: 40.0,
        lon: -74.0 + dlon,
        version: 1,
        tags: empty_tags(),
    };
    let mut way_tags = HashMap::new();
    way_tags.insert("building".to_string(), "yes".to_string());
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4, 1],
        version: 1,
        tags: way_tags,
    };
    let data = data_with(
        vec![mk(1, 0.0), mk(2, 0.001), mk(3, 0.002), mk(4, 0.003)],
        vec![way],
    );
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris[0], None);
}

#[test]
fn delete_way_shifts_fill_cache() {
    // Two ways; deleting the first must shift the second's fill entry down.
    let mut data = square_ring_data();
    {
        let d = Arc::make_mut(&mut data);
        let mut tags = HashMap::new();
        tags.insert("building".to_string(), "yes".to_string());
        d.ways.insert(
            20,
            OsmWay {
                id: 20,
                nodes: vec![1, 2, 3, 1],
                version: 1,
                tags,
            },
        );
    }
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_stylesheet(fill_stylesheet());
    assert_eq!(layer.way_fill_tris.len(), 2);
    layer.delete_feature(FeatureKind::Way, 10);
    assert_eq!(layer.way_fill_tris.len(), 1);
    assert!(
        layer.way_fill_tris[0].is_some(),
        "surviving way 20 (a closed triangle) keeps its fill entry"
    );
}
```

Note: check the actual public deletion API name before using `delete_feature(FeatureKind::Way, 10)` — `delete_way` at ~line 1103 is private; grep for its public caller (`grep -n "delete_way\|delete_feature" src/layers/osm_layer.rs`) and use whatever existing tests use.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib layers::osm_layer::tests -- fill`
Expected: compile error — `way_fill_tris` not found.

- [ ] **Step 3: Implement**

3a. `Cargo.toml`: add `earcutr = "0.4"`.

3b. Free function near `compute_way_tables`:

```rust
/// Earcut triangle indices for a closed, fill-styled way's projected ring,
/// or `None` when the way shouldn't be filled (open ring, no fill style,
/// degenerate geometry). Indices reference `verts[..verts.len()-1]` — the
/// duplicated closing vertex is excluded. Never panics: earcut failure or
/// an empty result just means no fill.
fn compute_fill_tris(verts: &[(i64, f64, f64)], style: &WayStyle) -> Option<Vec<u32>> {
    style.fill?;
    if verts.len() < 4 || verts.first()?.0 != verts.last()?.0 {
        return None;
    }
    let ring = &verts[..verts.len() - 1];
    let mut flat = Vec::with_capacity(ring.len() * 2);
    for &(_, x, y) in ring {
        flat.push(x);
        flat.push(y);
    }
    let tris = earcutr::earcut(&flat, &[], 2).ok()?;
    if tris.is_empty() {
        return None;
    }
    Some(tris.into_iter().map(|i| i as u32).collect())
}
```

Note the ring check uses the **projected** vertex list (`verts`), not raw refs — if a member node failed to resolve, `verts` may be shorter and no longer ring-shaped; the id equality check handles that safely.

3c. Extend `WayTables` to a 5-tuple with `Vec<Option<Vec<u32>>>` last; in `compute_way_tables`, after `styles.push(...)`:

```rust
let style = stylesheet.way_style(&way.tags, way.is_closed());
fill_tris.push(compute_fill_tris(&verts, &style));
styles.push(style);
```

(reorder so `verts` is pushed after use, or compute before pushing — keep borrowck happy by computing `fill_tris` entry before `vertices.push(verts)`).

3d. `OsmLayer` struct: add field

```rust
/// Cached earcut triangle indices per way (into `way_vertices[i]` minus
/// the closing duplicate), `Some` only for closed ways whose resolved
/// style has a fill. Parallel to `way_vertices`/`way_styles`. See
/// `compute_fill_tris`.
way_fill_tris: Vec<Option<Vec<u32>>>,
```

Update every place the parallel arrays are touched (grep `way_styles` — every hit needs a matching `way_fill_tris` line):

- constructor (`way_fill_tris: Vec::new()` at ~531)
- data-load destructuring of `compute_way_tables` (~553 and in `set_stylesheet` ~1270): 5-tuple now
- `commit_node_moves` touched-way patch (~883): after updating style, `self.way_fill_tris[way_idx] = compute_fill_tris(&self.way_vertices[way_idx], &self.way_styles[way_idx]);`
- `apply_style_refresh`: add params `way_vertices: &[Vec<(i64, f64, f64)>], way_fill_tris: &mut [Option<Vec<u32>>]`; in the `Way` arm:

```rust
let style = stylesheet.way_style(&way.tags, way.is_closed());
way_styles[idx] = style;
way_fill_tris[idx] = compute_fill_tris(&way_vertices[idx], &style);
```

Update its two callers (`set_tag`, `remove_tag`) to pass `&self.way_vertices, &mut self.way_fill_tris`.

- `delete_way` (~1121): `self.way_fill_tris.remove(way_idx);`
- `restore_way` (~1218):

```rust
let style = self.stylesheet.way_style(&way.tags, way.is_closed());
self.way_fill_tris.push(compute_fill_tris(&verts, &style));
self.way_styles.push(style);
```

(compute before `self.way_vertices.push(verts)` or use the pushed slice — match local code flow)

- `clear_osm_data` (~1245): `self.way_fill_tris.clear();`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: all pass, including the 6 new fill-cache tests.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets
git add Cargo.toml Cargo.lock src/layers/osm_layer.rs
git commit -m "Cache earcut fill triangulation for closed styled ways"
```

---

### Task 4: Paint area fills

**Files:**
- Modify: `src/layers/osm_layer.rs` (the way-painting section of `render_canvas`/`paint`, ~lines 1350–1470, and a new `push_triangle` helper next to `push_segment_quad` at ~line 406)

**Interfaces:**
- Consumes: `way_fill_tris` (Task 3), `WayStyle.fill` (Task 2).
- Produces: rendering only — no new public API.

- [ ] **Step 1: Add the `push_triangle` helper**

Next to `push_segment_quad` (mirror its structure — raw `PathVertex` pushes, `st_position` `point(0., 1.)`, accumulated bounds):

```rust
/// Push one solid triangle into `path`, creating the path on first use and
/// widening `bounds_min_max`. Same raw-vertex batching approach as
/// `push_segment_quad` — no Lyon tessellation.
fn push_triangle(
    path: &mut Option<Path<Pixels>>,
    bounds_min_max: &mut (f32, f32, f32, f32),
    a: Point<Pixels>,
    b: Point<Pixels>,
    c: Point<Pixels>,
) {
    let (min_x, max_x, min_y, max_y) = bounds_min_max;
    for p in [a, b, c] {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        if x < *min_x {
            *min_x = x;
        }
        if x > *max_x {
            *max_x = x;
        }
        if y < *min_y {
            *min_y = y;
        }
        if y > *max_y {
            *max_y = y;
        }
    }
    let st = point(0., 1.);
    let p = path.get_or_insert_with(|| Path::new(a));
    for xy in [a, b, c] {
        p.vertices.push(PathVertex {
            xy_position: xy,
            st_position: st,
            content_mask: Default::default(),
        });
    }
}
```

(Confirm the exact `PathVertex` field set against `push_segment_quad`'s pushes and copy it verbatim — if that code sets more fields, match it.)

- [ ] **Step 2: Add the fill pass before the stroke pass**

In the paint method, immediately BEFORE the existing `struct WayGroup` stroke block (so fills render under strokes and nodes), add:

```rust
// Area fills: paint before strokes so outlines and nodes stay on top.
// Triangulation is cached (`way_fill_tris`, computed at load/edit time);
// per-frame work is projection + batched raw-triangle emission, grouped
// by premultiplied RGBA color — mirroring the stroke batching below.
struct FillGroup {
    rgba: u32,
    path: Option<Path<Pixels>>,
    bounds_min_max: (f32, f32, f32, f32),
}
let mut fill_groups: HashMap<u32, FillGroup> = HashMap::new();
let mut fill_pts: Vec<Point<Pixels>> = Vec::new();

for (i, tris) in self.way_fill_tris.iter().enumerate() {
    let Some(tris) = tris else { continue };
    let Some(fill) = self.way_styles[i].fill else { continue };
    let bbox = match self.way_bboxes.get(i).and_then(|b| b.as_ref()) {
        Some(b) => b,
        None => continue,
    };
    if bbox.max_x < vmin_x || bbox.min_x > vmax_x || bbox.max_y < vmin_y || bbox.min_y > vmax_y
    {
        continue;
    }

    let verts = &self.way_vertices[i];
    let ring_len = verts.len() - 1; // closing duplicate excluded
    fill_pts.clear();
    for &(node_id, mx, my) in &verts[..ring_len] {
        let mut sp = viewport.mercator_to_screen(mx, my);
        if !is_point_valid(sp) {
            break;
        }
        sp += self.drag_preview_offset(node_id);
        fill_pts.push(point(sp.x + origin_x, sp.y + origin_y));
    }
    // Any invalid projection breaks index alignment with the cached
    // triangle indices — skip the fill (stroke still draws).
    if fill_pts.len() != ring_len {
        continue;
    }

    let alpha = (fill.opacity * 255.0).round() as u32;
    let rgba_key = (fill.color << 8) | alpha;
    let group = fill_groups.entry(rgba_key).or_insert_with(|| FillGroup {
        rgba: rgba_key,
        path: None,
        bounds_min_max: (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ),
    });
    for t in tris.chunks_exact(3) {
        push_triangle(
            &mut group.path,
            &mut group.bounds_min_max,
            fill_pts[t[0] as usize],
            fill_pts[t[1] as usize],
            fill_pts[t[2] as usize],
        );
    }
}

for (_, g) in fill_groups {
    if let Some(mut path) = g.path {
        let (min_x, max_x, min_y, max_y) = g.bounds_min_max;
        path.bounds = Bounds {
            origin: point(px(min_x), px(min_y)),
            size: size(px(max_x - min_x), px(max_y - min_y)),
        };
        window.paint_path(path, rgba(g.rgba));
    }
}
```

(`rgba` is gpui's `rgba(0xRRGGBBAA)` — already in scope via `use gpui::*`.)

- [ ] **Step 3: Build, test, and eyeball**

Run: `cargo test --lib && cargo clippy --all-targets`
Expected: all pass, no warnings.

Manual check (only if running interactively with a display): load any OSM extract with buildings and a stylesheet containing `area[building] { fill-color: #808080; }`; buildings show translucent fills under their outlines. Skip if headless — Task 5's default-stylesheet change plus the cache tests cover the data path, and paint code has no unit-test seam (same as the existing stroke pass).

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/layers/osm_layer.rs
git commit -m "Paint cached area fills under way strokes"
```

---

### Task 5: Default stylesheet fills

**Files:**
- Modify: `assets/default.mapcss`
- Modify: `src/style/mapcss.rs` (extend `default_stylesheet_parses` test)

**Interfaces:**
- Consumes: everything above.
- Produces: filled buildings/water/landuse out of the box.

- [ ] **Step 1: Extend the default-stylesheet test**

In `default_stylesheet_parses` in `src/style/mapcss.rs`, add:

```rust
let building = s.way_style(&tags(&[("building", "yes")]), true);
let fill = building.fill.expect("default stylesheet must fill closed buildings");
assert_eq!(fill.color, 0x808080);
// Open way with the same tag gets no fill.
assert_eq!(s.way_style(&tags(&[("building", "yes")]), false).fill, None);
let water = s.way_style(&tags(&[("natural", "water")]), true);
assert_eq!(water.fill.expect("water fill").color, 0x1e90ff);
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib style::mapcss -- default_stylesheet_parses`
Expected: FAIL — `fill` is `None` (no fill rules in the asset yet).

- [ ] **Step 3: Update `assets/default.mapcss`**

Append after the existing way rules (strokes unchanged):

```mapcss
/* Area fills — closed ways only */
area[building]  { fill-color: #808080; fill-opacity: 0.4; }
area[natural=water], area[waterway=riverbank] { fill-color: #1e90ff; fill-opacity: 0.4; }
area[leisure=park], area[landuse=grass], area[landuse=forest] { fill-color: #228b22; fill-opacity: 0.35; }
area[landuse]   { fill-color: #9acd32; fill-opacity: 0.25; }
```

Note ordering: the specific `landuse=grass`/`landuse=forest` rules come BEFORE the generic `area[landuse]` rule, and later rules override earlier ones — so move the generic `area[landuse]` rule ABOVE the park/grass/forest line, or grass/forest will be overridden to `#9acd32`. Final order:

```mapcss
/* Area fills — closed ways only */
area[building]  { fill-color: #808080; fill-opacity: 0.4; }
area[natural=water], area[waterway=riverbank] { fill-color: #1e90ff; fill-opacity: 0.4; }
area[landuse]   { fill-color: #9acd32; fill-opacity: 0.25; }
area[leisure=park], area[landuse=grass], area[landuse=forest] { fill-color: #228b22; fill-opacity: 0.35; }
```

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets
git add assets/default.mapcss src/style/mapcss.rs
git commit -m "Fill common area features in the default stylesheet"
```

---

## Final verification (after all tasks)

- [ ] `cargo test` — full suite green
- [ ] `cargo clippy --all-targets` — no warnings
- [ ] `cargo fmt --check` — clean
- [ ] Push branch and open draft PR
