# Render cost measurement, 2026-11-10

Measurement only. No optimisation was implemented. All numbers are CPU wall-clock
times of the code that records draw commands, taken from `--release` builds
with the `OSM_GPUI_FRAME_STATS=1` instrumentation added in this PR.

## Short version

- The recently added rendering (multipolygon fills, route overlays, restriction
  markers, hatched two-tone downloaded-area wash, area-fill detection) costs
  **about 0.3 to 0.8 ms per frame on top of v0.3.0 at its worst measured case
  (dense Paris, 290k nodes) and under 0.4 ms on the 24k-node dataset**. Most of
  that is within run-to-run noise. The wash itself is 0.00 to 0.07 ms on the CPU.
- The cost that matters is **node markers**: one `paint_quad` per visible node, about
  1.1 to 1.6 microseconds each. That loop is byte-for-byte the same in v0.3.0 and main.
  It is 70 to 90 percent of the OSM layer paint whenever vertices are shown
  (zoom 16 and up) over many nodes, and also at low zoom because every tagged node
  is drawn at every zoom.
- No regression from v0.3.0 to main was detectable. Run-to-run spread of the node loop
  (up to +/-30 percent between two launches of the same binary) is larger than any
  difference between the two builds, and its sign flips between datasets.
- Against a 16.7 ms budget main fits at zoom 18.5 and above and at zoom 14 on the small
  dataset. It breaks at zoom 17 on the 24k-node dataset (38 ms layer paint, 46 ms
  frame interval) and everywhere from zoom 14 to 17 on the 290k-node dataset
  (37 to 190 ms). The 8.3 ms budget fails in the same places plus A z15.5.
  This is an old problem, not caused by the recent features.

## Machine and display

Apple M4 Pro (14 cores, 48 GB), macOS 15.8.2, rustc 1.99.1, built-in Retina display
(3024 x 1794 px, scale factor 2.0). Window set to 1400 x 900 logical points by the
script; map area 1064 x 876 points.

**The display ran at 60 Hz**: `frame.interval` clusters at 16.66 ms whenever the frame
fits. So vsync caps what can be observed, a 120 Hz ProMotion rate could not be
exercised, and every 8.3 ms statement below is derived from CPU time only.
While a script runs the app requests an animation frame continuously, so each `wait`
yields roughly one painted frame per refresh.

What the numbers cover:

| Phase | What it is |
|---|---|
| `osm.*` | CPU time inside `OsmLayer::render_canvas`, per sub-phase (recording draw commands into GPUI's scene) |
| `wash.*` | `LayerManager::render_all_download_bounds` |
| `canvas.paint` | whole canvas paint closure (wash, layers, highlights) |
| `frame.render_start_to_canvas_end` | from start of `MapViewer::render` to end of the map canvas paint |
| `view.render` | building the element tree in `MapViewer::render` |
| `frame.interval` | time between consecutive canvas paints. This is the closest thing to true frame time: it includes everything the CPU and GPU pipeline needs, but is quantised to the 16.67 ms vsync |
| `hit_test`, `highlight.*` | hover hit-testing and highlight painting |
| `load.*`, `cache.*`, `edit.*` | one-shot load and edit costs |

Not covered: GPU rasterisation and presentation time (no GPU timestamps), scene
sorting and Metal encoding inside GPUI after paint, layout and paint of the side
panel and toolbars, raster tile decoding. The raster tile layer was present in every
run (the welcome flow adds it) from the tile cache; its cost is in `frame.interval`
but not in `osm.*`. The gap `frame.interval - canvas.paint` is the unmeasured
remainder: 7 ms at A z17, 28 to 34 ms at B z14/z15.5, which suggests scene
processing and GPU work scale with the primitive count (see optimisation 1).

## Datasets

Both come from one OSM API read-only download (`GET /api/0.6/map`,
bbox 2.335,48.860,2.345,48.866, central Paris, one request). Overpass was not
reachable from the sandbox. Raw files are not committed (8.6 MB and 106 MB).

| | A (realistic) | B (about 12x A) |
|---|---|---|
| nodes | 24,184 | 290,208 |
| tagged nodes | 6,149 | about 73,800 |
| ways | 3,365 (1,942 closed, 510 buildings, 1,314 highways) | 40,380 |
| relations | 356 | 4,272 |
| multipolygon relations (complete / total) | 123 / 134 | 1,476 / 1,608 |
| boundary relations | 13 | 156 |
| route relations | 47 | 564 |
| way members of routes (in file / listed) | 354 / 11,999 | about 4,250 / 144,000 |
| turn restrictions | 4 | 48 |
| downloaded-area rectangles | 1 | 12 |

B is A tiled 3 columns x 4 rows with shifted ids and coordinates and moved 0.5 degrees
east (`docs/perf/tile_osm.py`), so it never overlaps A. It is a replica of one real
area, not twelve different ones (all copies are equally dense and identical, so it
over-represents cache-friendly repetition if anything). Route relations are mostly
incomplete, as in any bbox download: only about 3 percent of route way members are
present, so route overlay cost is understated relative to a download that includes
whole routes. The restriction count is low (4 / 48), so restriction marker cost
is understated too.

## Method

1. `docs/perf/gen_benchmark_script.py` generates the script
   (`docs/perf/render-benchmark.osmscript` is the output for `perf-data/paris_{a,b}.osm`).
   One launch per build: load A, measure everything, load B as a second layer (A is then
   culled by its layer bounding box), measure everything.
2. Per view: set viewport, 1.5 s warm-up (discarded), 3 s of samples (about 180 frames
   at 60 Hz, fewer when frames are slow). Views: z14, z15.5, z17, z18.5, z19.5 static;
   40 alternating pan drags at z17 and z18.5; an 84-point hover grid at z17 and z19.5;
   node drag edits. main also repeats each static view with the downloaded-area wash
   toggled off (`ToggleDownloadedArea`).
3. Two launches each for v0.3.0 and main (r1 / r2 in the tables), plus one launch of a
   main build with temporary per-feature switches (not committed) to switch features off one
   at a time. That is 5 launches in total.
4. v0.3.0 got the same instrumentation by hand (phases that do not exist there are absent).
   v0.3.0 has no `ToggleDownloadedArea`, so its script omits those steps.
   The old run-1 edit test failed to select the node (the click was about 24 px off
   because of the title bar), so run 2 used a box select; run 1 numbers for edits
   are therefore not used.

Per-phase raw output (min / median / p95 / max / mean for every label) is in
`docs/perf/results/` (`main.txt` and `v030.txt` are run 1, `main2.txt` and
`v030b.txt` run 2, `sw.txt` the switches run). Run 1 of main/v0.3.0 set
`OSM_GPUI_FRAME_STATS_NO_TILES=1` (it had no effect, the tile layer was loaded anyway);
run 2 did not set it.

## Results: OSM layer paint, v0.3.0 vs main

Medians and p95 in ms, run 1 / run 2. "Nodes" is the node marker loop alone.
Every view has 60 to 280 frames unless marked; the heavy views have 10 to 60.
Heavy-view p95 on 10 to 20 frames (B z14, B z15.5) is a rough estimate.

| view | build | layer paint median | p95 | nodes median | frame interval median |
|---|---|---|---|---|---|
| A z14 | v0.3.0 | 6.35 / 5.62 | 6.74 / 5.96 | 5.25 / 4.51 | 16.67 |
| A z14 | main | 5.53 / 5.71 | 5.76 / 5.92 | 4.18 / 4.36 | 16.67 |
| A z15.5 | v0.3.0 | 6.53 / 8.31 | 6.98 / 9.08 | 3.49 / 5.25 | 16.66 |
| A z15.5 | main | 8.74 / 8.05 | 9.25 / 8.64 | 5.53 / 4.91 | 16.65 |
| A z17 | v0.3.0 | 26.96 / 32.04 | 28.39 / 33.50 | 22.76 / 27.56 | 34.9 / 41.0 |
| A z17 | main | 38.54 / 37.60 | 40.80 / 41.74 | 33.74 / 32.38 | 46.0 / 47.1 |
| A z18.5 | v0.3.0 | 2.10 / 2.57 | 2.46 / 2.90 | 1.14 / 1.54 | 16.66 |
| A z18.5 | main | 2.66 / 2.30 | 3.36 / 2.95 | 1.77 / 1.26 | 16.67 |
| A z19.5 | v0.3.0 | 1.66 / 1.54 | 2.21 / 2.12 | 0.55 / 0.53 | 16.66 |
| A z19.5 | main | 1.74 / 1.71 | 2.52 / 2.40 | 0.46 / 0.52 | 16.67 |
| B z14 | v0.3.0 | 285.9 / 242.6 | 301.4 / 249.3 | 267.4 / 225.8 | 307 / 269 |
| B z14 | main | 186.5 / 190.0 | 187.0 / 192.5 | 164.9 / 169.4 | 215 / 218 |
| B z15.5 | v0.3.0 | 130.7 / 103.4 | 134.6 / 105.6 | 95.6 / 72.7 | 163 / 133 |
| B z15.5 | main | 109.6 / 109.5 | 115.0 / 112.6 | 73.5 / 75.4 | 144 / 141 |
| B z17 | v0.3.0 | 51.0 / 47.1 | 55.5 / 50.0 | 42.9 / 39.4 | 64.8 / 61.4 |
| B z17 | main | 43.7 / 36.8 | 45.7 / 39.3 | 35.8 / 29.0 | 56.9 / 50.5 |
| B z18.5 | v0.3.0 | 3.75 / 4.09 | 4.27 / 4.57 | 2.46 / 2.67 | 16.67 |
| B z18.5 | main | 4.09 / 3.85 | 4.54 / 4.24 | 2.58 / 2.46 | 16.66 |
| B z19.5 | v0.3.0 | 1.97 / 1.54 | 3.05 / 2.02 | 0.74 / 0.70 | 16.67 |
| B z19.5 | main | 1.75 / 1.78 | 2.49 / 2.57 | 0.64 / 0.74 | 11.5 / 16.67 |

Pans (40 drags) and hover sweeps (84 hovers) give the same picture as the static
views at the same zoom (see `results/`): A z17 pan, main 34.7 / 35.4 ms layer paint
vs v0.3.0 28.3 / 24.9; z18.5 and z19.5 stay at 1.7 to 4.3 ms.

Whole frame: the "layer paint" column is below the frame interval everywhere, and
`canvas.paint` is within 0.1 ms of it (the layers dominate the closure). The
interval equals the 16.67 ms refresh whenever the paint fits, otherwise it is paint
plus 7 to 34 ms of unmeasured work.

### New-feature cost: layer paint excluding node markers

Median of `osm.total - osm.nodes`, ms, run 1 / run 2. This removes the noisiest, unchanged
loop and isolates what the recent work could affect.

| view | v0.3.0 | main | difference |
|---|---|---|---|
| A z14 | 1.11 / 1.12 | 1.34 / 1.34 | +0.2 |
| A z15.5 | 3.04 / 3.05 | 3.21 / 3.14 | +0.1 |
| A z17 | 4.19 / 4.48 | 4.79 / 5.22 | +0.6 |
| A z18.5 | 0.96 / 1.02 | 0.89 / 1.04 | 0 |
| A z19.5 | 1.11 / 1.02 | 1.28 / 1.20 | +0.2 |
| B z14 | 18.55 / 16.86 | 21.65 / 20.52 | +3.1 |
| B z15.5 | 35.12 / 30.67 | 36.10 / 34.09 | +1.0 to +3.4 |
| B z17 | 8.09 / 7.70 | 7.91 / 7.85 | 0 |
| B z18.5 | 1.29 / 1.42 | 1.50 / 1.39 | +0.1 |
| B z19.5 | 1.23 / 0.84 | 1.11 / 1.04 | 0 |

### Per-feature attribution on main

Direct timers (median, main): multipolygon fill building `osm.mp_fill_build` is
0.02 to 0.07 ms on A and 0.04 to 0.78 ms on B (the 0.78 ms is B z14, where 1,608
relations are in view); restriction markers 0.00 to 0.01 ms; the downloaded-area
wash with hatch and halo `wash.total` is 0.04 to 0.07 ms where the view reaches
outside the data and 0.00 where it does not (v0.3.0's plain wash: 0.00 to 0.01 ms).
Switching the wash off with the View menu action changes nothing measurable beyond that.
Route overlays are built inside the way loop, so they have no direct timer; the
temporary-switch run (`results/sw.txt`, labels `SW_*`) gives, for `osm.ways_build`
with routes on vs off, A z15.5 1.87 vs 1.73 ms, A z17 2.29 vs 2.63 (wrong sign: noise),
B z17 4.20 vs 3.98: about 0 to 0.15 ms on A and 0 to 0.2 ms on B, inside noise.
Switching all of {multipolygon fills, routes, restrictions, hatch, halo} off
removes about 0.3 ms at A z17 and about 0.5 to 1 ms at B z15.5 / z17
(non-node portion), consistent with the direct timers.
Closed-way area fills (#131) existed before v0.3.0: `osm.way_fill_build` plus
`osm.fill_paint` is 0.1 to 0.3 ms on A and about 2 to 5 ms on B at z14/z15.5,
and is the same in v0.3.0. Casing (pre-existing) is the largest non-node item:
switching it off saves 1 to 5 ms on B (e.g. 18.1 to 11.9 ms `ways_build` at B z15.5).

Ranking of the *new* features by per-frame cost (all small, differences within
noise): 1. multipolygon fills (about 0.5 to 0.8 ms at B z14 to z15.5, 0.05 ms on A),
2. route overlays (0 to 0.2 ms), 3. wash hatch plus halo (0.06 ms), 4. restriction
markers (0.01 ms, only 48 restrictions in the data). **Multi-colour preset icons
(`img()` vs `svg()` in the side panel and preset picker) were NOT isolated**: they
live in the element tree, outside the canvas timers; `view.render` (element-tree
build) is 0.1 ms on A and 0.1 to 0.27 ms on B in both v0.3.0 and main, but layout and
paint of those elements are not covered.

### Hit-testing, hover, edits

| item | value | source |
|---|---|---|
| `hit_test` per hover, A | 0.02 ms median (p95 0.04) | `A_z17_hover`, 84 samples |
| `hit_test` per hover, B | 0.03 to 0.09 ms median | `B_z*_hover`, 84 samples |
| `highlight.hovered` | 0.006 to 0.01 ms | 35 to 94 samples |
| `edit.commit_node_moves_total`, main (move of a 12-node building) | A 0.13 ms median, B 0.10 ms | 12 samples each, run 2 |
| same, v0.3.0 | 0.07 ms | 12 samples each |
| `cache.mp_refresh` (part of the above) | A 0.05 ms, B 0.03 ms | 12 samples each |

The multipolygon in the node-drag test is a single small building; a large
boundary relation would cost more, and **that case was not measured**.

### Cache and load cost (one sample each, so not statistics)

| step | A | B |
|---|---|---|
| `load.node_cache` | 4.7 to 4.9 ms | 49 ms |
| `load.way_tables_and_fill_tris` | 3.1 to 3.3 ms | 47 ms |
| `cache.mp_rebuild_all` (multipolygons, route and restriction caches) | 2.4 ms | 28 ms |
| `load.new_with_data_total` (main) | 15.6 to 16.5 ms | 205 to 210 ms |

`MpCache` is therefore about 15 percent of layer construction on main and is new since
v0.3.0 (v0.3.0 has no such step). The first frame after loading B also shows a
`view.render` outlier of about 360 ms (adding the layer, parsing is on a background thread).

## Frame budget analysis

Layer paint (median / p95 across both main runs), compared with 16.7 ms (60 Hz) and
8.3 ms (120 Hz). Using CPU time only, which is a lower bound on the frame cost:

| view | A | B |
|---|---|---|
| z14 | 5.6 / 5.9: fits both | 186 / 192: fails both |
| z15.5 | 8.4 / 8.9: fits 60, borderline-fails 120 | 110 / 114: fails both |
| z17 | 38 / 41: fails both (interval 46 ms, 22 fps) | 37 to 44 / 39 to 46: fails both (interval 51 to 57 ms) |
| z18.5 | 2.3 to 2.7 / 3.0 to 3.4: fits both | 3.9 to 4.1 / 4.2 to 4.5: fits both |
| z19.5 | 1.7 / 2.4: fits both | 1.8 / 2.5: fits both |

At 60 Hz the observed `frame.interval` stays at 16.66 ms (p95 16.8 to 18) in every
fitting view, so the full pipeline fits there. Where it does not, the frame interval
is paint plus 7 to 34 ms. The 120 Hz column is an inference: the machine's display ran
at 60 Hz in these runs. The historical reference in `Cargo.toml` (about 2 ms for 8k
nodes / 4k ways) matches the z18.5 to z19.5 results, where only a few thousand nodes
are in view; it does not describe the z16 to z17 whole-dataset view.

The degradation is a step at zoom 16, where untagged vertex markers turn on, plus a
background cost at all zooms from the tagged nodes. Node cost per visible node is
about 1.3 us at A and 0.6 to 2.3 us at B.

## Top costs, ranked

1. **Node marker loop** (`render_canvas`, `paint_node_marker`): 23 to 34 ms at A z17,
   29 to 43 ms at B z17, 165 to 270 ms at B z14 (74k tagged nodes at z14). 70 to 90
   percent of the layer paint. Pre-existing.
2. **Way casing, stroke and arrow building**: `ways_build` plus `ways_paint`
   2 to 5 ms at A z15.5 to z17 and 20 to 32 ms at B z15.5 (casing alone is about 5 ms
   of it). Pre-existing.
3. **GPUI scene processing and GPU work for the primitive count**: the unmeasured gap
   between `frame.interval` and `canvas.paint`: 7 ms at A z17, 28 to 34 ms at B z14 and z15.5.
   Estimated from the interval, not measured directly.
4. Closed-way fill paths: 2 to 5 ms at B z14/z15.5 (pre-existing).
5. Multipolygon fills (new): 0.05 ms on A, up to 0.8 ms on B.
6. Route overlays, wash hatch plus halo, restriction markers (new): 0.2 ms or less each.

## Suggested optimisations (not implemented)

Gains are estimates from the numbers above.

1. **Cull node markers by density and zoom** (`src/layers/osm_layer.rs`, node loop in
   `render_canvas`, about lines 3590 to 3612; `NodeCache`). Skip untagged vertices
   whose projected neighbours are under about 3 px apart, and decimate tagged nodes
   when zoomed out. At A z17 this is the 33 ms loop; at B z14 the 165 ms loop. Expected
   gain: 50 to 90 percent of the node loop, i.e. 15 to 30 ms at A z17, 100+ ms at B z14.
   Biggest single win; also shrinks the scene.
2. **Batch node markers into one instanced or sprite pass**: replace one `paint_quad` per
   node (about 1.3 us each, plus per-quad scene and GPU cost) with a small number of
   `paint_path` batches (like the way fills) or a pre-baked marker atlas drawn as
   `paint_monochrome_sprite`. The code comment says Lyon fill batching was tried and was
   slower, so this needs a measured prototype (try rectangles built as raw triangles like
   the way quads). Expected gain: 2 to 5x of the remaining loop at high node counts.
3. **Cache projected geometry across frames while only panning**
   (`way_vertices`, `render_canvas`). Each frame re-projects every visible vertex; pans and
   zooms shift a constant offset. Expected: 20 to 40 percent of `ways_build`, 0.5 to 6 ms
   at B z15.5.
4. **Skip casing below zoom ~16 or draw it cheaply**: casing is about 5 ms of 16 at B z15.5
   and 1 to 3 ms elsewhere. Expected gain 1 to 5 ms at B (`CASING_MIN_ZOOM`, line 40).
5. **Build the layer scene into a retained picture** and redraw only on viewport change
   (not on hover): hover and animation-driven repaints currently redo the whole layer
   (no change in `frame.interval` when nothing moved). Gain: removes the per-repaint cost
   entirely for hover; changes the architecture, so measure first.
6. **Not worth optimising now**: multipolygon, route and restriction rendering, the wash
   hatch, hit-testing (0.02 to 0.09 ms) and hover highlight (under 0.01 ms). Mp refresh
   after an edit (0.05 ms for a small relation) and the full `MpCache` rebuild (2.4 ms
   per 24k nodes, 28 ms per 290k) are fine. One thing to watch: `MpCache::refresh` does
   `data.relations.iter().find(...)` per refreshed relation, linear in the relation
   count; harmless at 4k relations, but would matter in a whole-country relation set.

## Noise and confidence

- Run-to-run spread of the same binary on the same view is 10 to 30 percent for the
  node loop (e.g. A z17 v0.3.0 nodes 22.8 vs 27.6 ms; B z14 v0.3.0 267 vs 226 ms; B z14 main
  vs v0.3.0 differ by 30 percent in the opposite direction to A z17). Trust differences
  smaller than 3 ms or 20 percent only through the non-node table.
- Switch-run rows show outliers with the wrong sign (e.g. "routes off" slower than base
  at A z17; B z18.5 `mp` off `ways_build` 3.6 ms vs 0.7): background interference.
  Individual switch deltas are good to about 0.5 ms for A and 1 to 2 ms for B.
- B z14 and B z15.5 have 10 to 22 samples per view; their p95 is close to the max.
- One launch per configuration pair beyond the two repeats; no thermal control.
- `frame.interval` is quantised to 16.67 ms by vsync: it tells whether the whole
  pipeline fits, not by how much when it does.
- The datasets are one Paris area, and route and restriction coverage is thin (see Datasets).

## What was NOT measured

- GPU time (rasterisation, blending of the wash and fills, presentation) and anything
  after paint inside GPUI (scene sorting, Metal encoding).
- True 120 Hz behaviour (the display ran at 60 Hz).
- Side-panel and preset-picker icon cost (`img()` vs `svg()`): not isolated.
- Raster tile layer cost (present, not isolated).
- Large multipolygon, complete route relation, or many-restriction cases; incremental
  `MpCache` refresh for a large relation; a node drag in a dense route/restriction area.
- Wash behaviour with many layers or many rectangles beyond 12.
- Memory use, load time end to end (only the layer construction, on the main thread).
- Other OS, GPU, or window sizes (single 1400 x 900 window).

## Reproducing

```
mkdir perf-data
curl -A "perf-measurement" -o perf-data/paris_a.osm \
  "https://api.openstreetmap.org/api/0.6/map?bbox=2.335,48.860,2.345,48.866"
python3 docs/perf/tile_osm.py perf-data/paris_a.osm perf-data/paris_b.osm 3 4 0.0100 0.0060 0.5
mkdir -p target/perf
cargo build --release
OSM_GPUI_FRAME_STATS=1 target/release/osm-gpui --script docs/perf/render-benchmark.osmscript \
  | grep '^FRAMESTATS' > run.txt
```

(Each launch asks for the macOS Keychain password twice. Use a v0.3.0 checkout with the
same instrumentation applied and `python3 docs/perf/gen_benchmark_script.py ... notoggle`
for the comparison script.) `frame_stats LABEL` is a script op that prints and clears the
per-phase min / median / p95 / max / mean since the previous call.

## After: node marker density rule (PR node-marker-culling)

The rule is in `src/layers/node_markers.rs` (see its module doc). Each node gets
a reveal zoom at load time from a stable density pass (tagged nodes first, then
junctions and way ends, then plain vertices; world-keyed grid cells whose pixel
size shrinks from 26 px at zoom 16 to 0 at zoom 18.5; untagged nodes never below
zoom 15). Drawing, hover and click targeting, and box select all use it, so a
hidden vertex is never a target. Selected nodes and the nodes of a selected way are
always shown and targetable. From zoom 18.5 everything is shown as before.

Same machine, `--release`, 1400 x 900 window, 60 Hz display. Medians; "before" is
the build with only the counters added (`docs/perf/results/culling-before-A.txt`,
`culling-before-B.txt`), "after" is `culling-after-full.txt`. Raw output of the v0.3.0
comparison is `culling-v030-z17.txt`. Script: `gen_culling_script.py`.

| view | layer paint med / p95 before | after | node phase before | after | markers drawn / nodes in view before | after | frame interval before | after |
|---|---|---|---|---|---|---|---|---|
| A z14 | 7.32 / 8.11 | 1.36 / 1.79 | 5.55 | 0.05 | 6149 / 24127 | 71 / 24127 | 49.9 | 16.7 |
| A z15.5 | 10.01 / 10.91 | 3.61 / 4.28 | 6.19 | 0.17 | 6133 / 23867 | 386 / 23867 | 40.6 | 16.7 |
| A z17 | 32.59 / 33.23 | 5.18 / 6.12 | 28.40 | 0.73 | 21059 / 21059 | 1550 / 21059 | 41.1 | 16.7 |
| A z18.5 | 4.85 / 5.84 | 2.60 / 3.43 | 2.80 | 1.11 | 2344 / 2344 | 2344 / 2344 | 49.7 (noisy run) | 16.7 |
| A z19.5 | 2.39 / 3.53 | 1.58 / 2.09 | 0.71 | 0.56 | 640 / 640 | 640 / 640 | 49.7 (noisy run) | 16.7 |
| B z14 | 204.95 / 207.43 | 19.76 / 22.29 | 183.93 | 0.58 | 73768 / 289450 | 465 / 289450 | 235.4 | 45.7 |
| B z15.5 | 104.89 / 106.23 | 37.19 / 38.79 | 71.33 | 1.82 | 49674 / 193767 | 1441 / 193767 | 135.6 | 67.9 |
| B z17 | 51.29 / 53.76 | 8.38 / 9.87 | 42.94 | 1.19 | 27630 / 27630 | 1627 / 27630 | 66.6 | 21.0 |
| B z18.5 | 4.46 / 6.04 | 3.72 / 4.82 | 2.54 | 2.30 | 3583 / 3583 | 3581 / 3581 | 16.7 | 16.7 |
| B z19.5 | 3.20 / 5.41 | 1.63 / 3.12 | 1.14 | 0.71 | 1002 / 1002 | 1001 / 1001 | 49.9 | 16.7 |

The "before" frame intervals at A z18.5 / z19.5 (about 50 ms with 2 to 5 ms of paint)
came from a noisy stretch of that run and should not be read as a property of the old
code. Layer paint at z18.5 and closer is not changed by the rule (all markers drawn).

- Target 1, A under 8 ms at every zoom: met (worst 5.2 ms at z17; 5.4 ms in the 20 s z17 run,
  pans 5.5, hover 5.0).
- Target 2, B under 16 ms at z15.5 and closer: met at z17 (8.4 ms), z18.5 and z19.5;
  **not met at z15.5 (37 ms)**. At B z15.5 the node phase is now 1.8 ms; what remains
  is way building and painting (16 + 15 ms: casing, strokes, arrows) and fills. B z14 is
  19.8 ms for the same reason (ways 15 ms).
- Hover hit test: 0.03 ms (A), 0.09 ms (B) median, unchanged.
- Load cost: `load.node_cache` rose from 4.7 to 7.0 ms on A and from 49 to 103 ms on
  B (the density pass: a sort and about 17 hash-set levels).

### zoom 17 on A, v0.3.0 against main (200+ frames each)

Both builds ran the same script, back to back in time order (v0.3.0 first, then main
before the change), 300 frames per view for v0.3.0 and 200 to 300 for main:

| build | layer paint med / p95 | node loop med | ways phases med | frame interval med |
|---|---|---|---|---|
| v0.3.0 rep 1 / 2 | 25.1 / 26.1, 25.1 / 25.7 | 21.1, 21.2 | 3.9, 3.8 | 33.0, 32.8 |
| main rep 1 / 2 | 32.8 / 33.5, 32.8 / 33.5 | 28.4, 28.5 | 4.3 (build 2.4 + paint 1.7) | 41.1, 41.1 |

There is a real regression of about 7.5 ms (30 percent), reproducible across reps and
runs, and the node phase carries all of it (+7.3 ms; the ways phases differ by 0.3 ms).
The per-node loop and `paint_node_marker` are identical in the two builds, so the cause
is not in that code; it is about 0.35 us more per `paint_quad`. Not identified (not
isolated: candidates are a different scene state when the quads are recorded, such as
the larger number of paths already in the scene, or something in the window chrome).
The density rule removes 90 percent of the quads and so most of it.
