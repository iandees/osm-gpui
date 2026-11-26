# Label paint cost, 2026-11-25 (issue #390)

Measures what point/area labels and street names cost to paint, by halo mode and
label count, on an otherwise idle machine. Used to set `OUTLINE_MAX_LABELS`
(`src/layers/labels.rs`), the text-label count up to which point and area labels get
the outline halo (`HaloMode::Offsets`) before the backing quad (`HaloMode::Quad`)
takes over.

## Short version

- **The outline is not within the 0.8 ms goal at 40 labels.** With 39 text labels in
  view the label paint is 1.24 ms (fixture, 1.30 ms in a second launch), and 1.81 ms
  on real Paris data with 36 text labels. The quad paints the same views in 0.34 ms.
- The outline adds about **15 to 30 us per text label** on the fixture (four glyph
  passes) and up to about 45 us on the Paris data (longer names). The quad costs
  about the same as no halo at all (within 0.1 ms, inside noise).
- Crossover: on the fixture the outline reaches 0.8 ms at about 27 text labels (18
  labels: 0.47 ms, 33: 0.99 ms). The Paris case implies a lower one. **`OUTLINE_MAX_LABELS`
  is lowered from 40 to 20**, which keeps the outline for the sparse views where it
  looks best (Paris z17: 16 text labels, 0.21 ms) and stays near or under 0.8 ms in both
  data sets.
- Frame time: at 120 Hz no point-label view missed vsync except the forced outline
  with 258 text labels (9.5 ms paint, 10.5 ms interval). With the shipped rule
  300 labels paint in 1.85 ms and every frame is 8.33 ms. Street names (168 placed)
  paint in 1.2 ms with the default coarse halo; the GPU/scene side only shows up when the
  batches are painted 8 times (see below).

## Machine and build

Apple M4 Pro (14 cores, 48 GB), macOS 15.8.3, rustc 1.100.0, built-in Liquid Retina XDR
(3024 x 1964, scale 2.0). **The display ran at 120 Hz** this time: `frame.interval`
clusters at 8.33 ms. `cargo build --release --locked`; no other builds or agents were
running during the launches. Four launches in total, one per script plus a repeat of
the halo sweep.

Every number is a median over 50 to 340 frames (most views 230 or more) of
`OSM_GPUI_FRAME_STATS=1` CPU wall-clock timers; `labels.paint` covers the whole label
pass of a layer (street names, icons, text and halos). `frame.interval` is the time
between canvas paints and includes the GPU and scene work after paint, quantised by
vsync. There are no GPU timestamps.

## Scripts

| script | what | launches |
|---|---|---|
| `docs/perf/point-labels-halo.osmscript` (new) | `labels-stress.osm` (350 POIs), 1200 x 800 window, zoom 17 to 19.5, each view with the halo forced to auto / offsets / quad / shadow / off | 2 (run 1 had zooms 16.5 to 19 and no text count) |
| `docs/perf/line-labels-gpu.osmscript` | street names of `line-labels-stress.osm`, halo variants and 8x repeat | 1 |
| `docs/perf/render-benchmark.osmscript` | Paris A (24k nodes) and B (290k nodes), regenerated from the OSM API as in `2026-11-render-measurement.md` | 1 |

Two benchmark tools were added for this: the `LabelBench` script action now also takes
`points=offsets|shadow|quad|off|auto` (force the point/area label halo; `auto` is the
count rule) and `outline_max=N`, and the label pass counts `labels.text` (the number
the count rule uses) next to `labels.placed` (which includes icon-only labels).
Raw output: `results/label-halo-run1.txt`, `results/label-halo-run2.txt`,
`results/line-labels-gpu.txt`, `results/render-benchmark-labels.txt`.

## Point and area labels, by halo mode

`labels.paint` median, ms, run 2 (run 1 in brackets where the view was in both).
"auto" is the rule as shipped before this change (outline up to 40 text labels).

| zoom | placed / text | auto | offsets | quad | shadow | off | offsets minus quad, per text label |
|---|---|---|---|---|---|---|---|
| 17 | 302 / 258 | 1.85 (1.84) | 9.49 (9.51) | 1.85 (1.84) | 2.74 (2.65) | 1.69 (1.68) | 30 us |
| 17.5 | 275 / 155 | 1.38 (1.27) | 4.04 (4.15) | 1.21 (1.32) | 1.79 (1.88) | 1.14 (1.14) | 18 us |
| 18 | 138 / 89 | 0.81 (0.80) | 2.13 (2.27) | 0.78 (0.80) | 1.37 (1.27) | 0.73 (0.71) | 15 us |
| 18.25 | 96 / 60 | 0.50 | 1.48 | 0.57 | 0.87 | 0.55 | 15 us |
| 18.5 | 63 / 39 | **1.26 (1.33)** | 1.24 (1.30) | 0.34 (0.33) | 0.56 (0.56) | 0.40 (0.42) | 23 us |
| 18.75 | 53 / 33 | 1.01 | 0.99 | 0.27 | 0.42 | 0.32 | 22 us |
| 19 | 33 / 18 | 0.51 (0.48) | 0.47 (0.60) | 0.18 (0.12) | 0.36 (0.21) | 0.20 (0.20) | 16 us |
| 19.5 | 17 / 13 | 0.58 | 0.58 | 0.18 | 0.25 | 0.17 | 31 us |

`frame.interval` was 8.33 ms in every view except z17 offsets (10.5 ms, CPU-bound) and
the last eight views of run 2 (z19 shadow onwards), where it jumped to about 40 ms with
paint unchanged; the same 40 ms pattern appears in the second half of the render
benchmark. Paint is unaffected, so this is something outside the app (window
occlusion or system throttling, not identified) and those intervals are ignored.

## Real data (render benchmark, auto rule)

| view | placed / text | halo | labels.paint | osm.total |
|---|---|---|---|---|
| A z17 | 23 / 16 | offsets | 0.21 | 4.25 |
| A z18.5 | 109 / 42 | quad | 0.35 | 1.18 |
| A z19.5 hover | 160 / 112 | quad | 0.41 | 0.45 |
| B z17 | 9 / 3 | offsets | 0.07 | 9.34 |
| B z18.5 | 101 / 36 | offsets | **1.81** | 3.09 |
| B z19.5 | 114 / 74 | quad | 0.52 | 2.09 |

B z18.5 is the case the old rule handles worst: 36 text labels stay outlined and the
label pass costs more than half as much as the whole layer paint (`osm.total`). With the
new limit it gets the quad.

## Street names (line-labels-gpu)

168 street names placed; these use their own cached mesh halo (`MeshHalo`) and do not
follow `OUTLINE_MAX_LABELS`. `labels.paint` median / `frame.interval` median, ms:

| halo | pan | static | zoom steps | static x8 paint / interval |
|---|---|---|---|---|
| labels off | 0.00 / 8.36 | 0.00 / 8.33 | 0.00 / 8.34 | |
| stroke | 1.70 / 8.33 | 1.84 / 8.33 | 1.55 / 8.33 | 8.37 / 33.0 |
| coarse (default) | 1.23 / 8.33 | 1.27 / 8.33 | 1.10 / 8.33 | 5.96 / 22.4 |
| none | 0.71 / 8.32 | 0.78 / 8.33 | 0.63 / 8.33 | 2.89 / 12.1 |

At 1x every variant fits 120 Hz. At 8x the interval is 3 to 4 times the CPU paint, so
the work after paint (scene processing and GPU) dominates: about 2.7 ms per copy for the
coarse halo, 3.9 for stroke, 1.4 for none, estimated as interval / 8. That puts the
default street-name batches at roughly 1.2 ms CPU plus 2 to 3 ms after paint for 168
names, well inside 8.3 ms.

## Decision

`OUTLINE_MAX_LABELS` 40 -> 20. At 40 the outline view costs 1.2 to 1.8 ms; at 20 the
fixture predicts about 0.5 to 0.6 ms (18 text labels measured 0.47 to 0.60 ms) and the
measured real views below 20 text labels are 0.07 to 0.21 ms. The quad fallback stays
as it is: it is as cheap as no halo. Shadow stays rejected (more expensive than the quad
and less legible, see #228).

## Noise and confidence

- Two launches of the halo sweep agree within about 5 percent on views with 1 ms or
  more of paint (z17 offsets 9.49 / 9.51, z18 offsets 2.13 / 2.27, z18.5 offsets 1.24 /
  1.30); small views vary by up to 0.1 ms (quad and off swap order at z18.5).
- The per-label outline cost varies 15 to 45 us with the view (glyph count per label),
  so the 0.8 ms crossover is between about 15 and 27 text labels depending on the names
  in view; 20 is a compromise, not a sharp optimum.
- Not measured: GPU time directly, 60 Hz behaviour (the display was at 120 Hz), other
  machines, legibility of the quad at 20 to 40 labels (unchanged code, see #228 screenshots).

## Reproducing

```
OSM_GPUI_FRAME_STATS=1 target/release/osm-gpui --script docs/perf/point-labels-halo.osmscript | grep '^FRAMESTATS'
OSM_GPUI_FRAME_STATS=1 target/release/osm-gpui --script docs/perf/line-labels-gpu.osmscript | grep '^FRAMESTATS'
OSM_GPUI_FRAME_STATS=1 target/release/osm-gpui --script docs/perf/render-benchmark.osmscript | grep '^FRAMESTATS'
```

The render benchmark needs `perf-data/paris_{a,b}.osm` (see "Reproducing" in
`2026-11-render-measurement.md`).
