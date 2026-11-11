# Way drawing cost, 2026-11-10

Follow-up to `2026-11-render-measurement.md`. Same datasets (A = 24k nodes, B = 290k
nodes, central Paris, B is A tiled 3 x 4), same machine, `--release`, 1400 x 900 window
(map area 1064 x 876 pt), 60 Hz display. The generated benchmark script visits z14,
15.5, 16, 17, 18.5, 19 (A only) and 19.5 for 1.5 s warm-up then 3 s (A) or 4 s (B) of
frames, A first and then B loaded as a second layer. Medians, ms.

## Where the time went (B, before)

Node markers were already cheap (PR #149). What was left:

- `osm.ways_build` 10.0 / 16.3 / 12.0 ms at z14 / 15.5 / 16 and `osm.ways_paint` 5.5 /
  16.7 / 9.6 ms: one pass over all 40k ways projecting every vertex, plus handing
  1 to 2.3 million path vertices (32 bytes each) to GPUI, which copies them again in
  `Path::scale` and once more into the Metal instance buffer.
- The path vertex `Vec`s were grown by doubling while they were filled (tens of MB
  copied and page-faulted every frame). Reserving last frame's size removed most of
  the CPU cost by itself: in `examples/way_bench` the z16 build went from 7.5 to 3.6 ms
  after the vertex count reduction below, and the reservation alone halves that kind of
  build.
- The casing was drawn from z15 and doubled the vertex count at z15 to z16.
- Most vertices of a dense city are minor ways, buildings and tiny ways that are a few
  pixels across.
- Unmeasured GPU/encoding work (`frame.interval - canvas.paint`) is proportional to the
  path vertex count (28 to 33 ms at B z14 to z16 before). The number of paths (30) is
  the number of stylesheet colours and is not the problem.

## What changed

All in `src/layers/osm_layer.rs` (`build_way_scene`, `WayScene`, `WayLod`, `WayGeom`):

- **Douglas-Peucker significance per vertex** (`WayGeom`, computed when a way is
  projected). The renderer drops vertices whose significance is under a tolerance
  (1 px below z16, 0.35 px from z16) and only projects the survivors.
- **Level of detail below z16** (`WayLod`): ways under 3 px across are drawn as one
  stroke across their bounding box, under 0.75 px not at all, areas under 2 px across get
  no fill, merged segments are at least 2 px long. Below z15, ways drawn 1.5 px wide or
  less (footpaths, building and land-use outlines, barriers) get no outline (areas keep
  their fill). Casing starts at z16 instead of z15.
- Vertex capacity hints (`VertexHints`) so batches are not grown by doubling.
- Inline projection (`MercatorProjector`), no per-way allocation, a last-style memo
  instead of a hash lookup per way.
- From z16 on (editing zooms) only the 0.35 px tolerance applies; nothing else changes.

Caches and invalidation: the only new cached data is the significance in `WayGeom`,
stored next to `way_vertices` and built by the same function that builds the vertices
(`project_way_vertices`), so every edit path that re-projects a way (node move, add,
remove, set nodes, restore, load) replaces it. `VertexHints` are only capacities. While
nodes are being dragged nothing is simplified. Tests in `way_lod_tests` compare the cached
geometry and the scene of an edited layer with a layer freshly built from the same data
after a node move and its undo, add way, remove way and set nodes.

## Results (`--release`, medians of per-frame ms)

`before` is main, `after` this branch (two launches, second one shown; the first showed
the same within 0.5 ms for B). `layer` is `osm.total`; `interval` is `frame.interval`
(quantised to 16.7 ms at 60 Hz). Node markers are included and unchanged.

| view | layer before | layer after | interval before | interval after | path vertices after |
|---|---|---|---|---|---|
| A z14 | 1.9 | 1.1 | 16.7 | 16.7 | 35k |
| A z15.5 | 4.2 | 1.3 | 49.9 (noisy) | 16.7 | 96k |
| A z16 | 4.0 | 2.3 | 16.7 | 16.7 | 234k |
| A z17 | 4.8 | 2.5 | 16.7 | 16.7 | 280k |
| A z18.5 | 2.2 | 2.7 (node loop noise) | 16.7 | 16.7 | 66k |
| A z19.5 | 1.3 | 1.6 (node loop noise) | 16.7 | 16.7 | 43k |
| B z14 | 24.5 | 7.7 | 53.4 | 20.4 | 421k |
| B z15.5 | 41.1 | 12.2 | 74.5 | 28.3 | 858k |
| B z16 | 27.3 | 13.4 | 59.6 | 32.7 | 1.20M |
| B z17 | 8.4 | 5.2 | 18.5 | 16.7 | 465k |
| B z18.5 | 4.7 | 4.2 | 16.7 | 16.7 | 156k |
| B z19.5 | 1.7 | 1.7 | 16.7 | 16.7 | 113k |

Not met: the target of 8 ms for B at every zoom. B z15.5 and z16 are at 12 to 13 ms of
CPU (and 28 to 33 ms between frames, the rest being GPUI scene and GPU work). The B
z14 figure of 7.7 ms contains 4.5 ms of tagged-node squares, which a separate change
(lighter low-zoom nodes) addresses. At z16 the 1.2M vertices are all real segments
(stroke plus casing) of a 290k-node view; going lower would mean a visible change at
an editing zoom.

`examples/way_bench.rs` times just `build_way_scene` without a window:
B z14 / 15 / 15.5 / 16 / 17: 1.1 / 4.2 / 3.5 / 3.2 / 1.4 ms.

Visual differences (A, same viewport before and after, z15.5, z17): at z17 and above
the captures are indistinguishable by eye. At z15.5 the dark casing is gone, so
lines are lighter and less contrasty over imagery. Below z15 (not captured for B) minor
ways and building outlines are not drawn, so the map shows roads and area fills only.
Same-colour ways of different widths now share one path, so the overlap order of two
different colours that interleave in the data can differ from before (the order was already
arbitrary: by id).

## Not verified

GPU time, 120 Hz, other machines. The background load on the machine varied between
launches (a screen-recording process was running), which made one launch unusable; the
tables use quiet launches. Hover and click hit-testing read the unsimplified geometry and
were not changed, but the GUI was not exercised by hand.

## Line joins and end caps, 2026-11-10

`src/layers/way_stroke.rs` builds the stroke geometry. From z16 (not in the coarse path,
not for collapsed ways, not for ways under 12 px across) the bends of a stroke are mitered
(limit 3, bevel beyond it; a miter costs no extra vertices because neighbouring segment
quads share the miter edge) and open ways of 2 px or wider get round caps (2 triangles
under 4 px half-width). Joins need a layer at least 1.5 px wide, so the casing is joined for
nearly every way and the fill from 1.5 px. Casing, fill and route overlay are stroked in one
pass over the same polyline.

`examples/way_bench`, medians of 30 builds in ms, same machine, before and after run
back to back (`before` is main at 762679a). Path vertices in thousands.

| view | before ms | after ms | before verts | after verts |
|---|---|---|---|---|
| A z16 | 0.73 | 0.87 | 234 | 252 |
| A z17 | 0.80 | 1.18 | 280 | 301 |
| A z18 | 0.30 | 0.44 | 112 | 122 |
| A z19 | 0.12 | 0.19 | 50 | 54 |
| B z14 | 1.11 | 1.18 | 421 | 421 |
| B z15.5 | 3.11 | 3.19 | 861 | 861 |
| B z16 | 3.14 | 3.96 | 1205 | 1302 |
| B z17 | 1.52 | 1.86 | 475 | 509 |
| B z18 | 0.60 | 0.80 | 215 | 234 |
| B z19 | 0.36 | 0.47 | 128 | 137 |
| B z20 | 0.32 | 0.36 | 109 | 112 |

Below z16 nothing changed (the differences are noise: the code path is the old one, 5 to
10% run to run). From z16 the way pass costs 0.1 to 0.8 ms more (B z16 +26%, z17 +22%,
z18 +33%), over the 10% target. The added vertices are bevels (where a miter does not fit) and
caps; most of the time is the per-vertex join work rather than vertex volume: turning the
caps off entirely changed B z16 by only 0.05 ms in an earlier run, and routing the same
polyline through the new code with plain rectangles was already about 0.7 ms slower than
the old loop. Thresholds already tightened for this:
caps only for ways 2 px or wider, nothing for ways under 12 px across, the plain
rectangle path for everything below z16. Not measured: GPU time, a harness run with
`OSM_GPUI_FRAME_STATS`.
