#!/usr/bin/env python3
"""Generate docs/perf/render-benchmark.osmscript.

usage: gen_benchmark_script.py A.osm B.osm > render-benchmark.osmscript

A.osm is one OSM API download of central Paris (bbox 2.335,48.860,2.345,48.866).
B.osm is that file tiled 3x4 with shifted ids/coordinates and moved 0.5 degrees
east by tile_osm.py, so it never overlaps A. Both load into one app run: A is
measured first, then B is added as a second layer (A is then culled by its
layer bounding box, which costs next to nothing per frame).

Every measurement is "viewport, wait for warm-up frames, frame_stats discard,
wait for samples, frame_stats LABEL". While a script runs the app requests an
animation frame continuously, so `wait` produces one painted frame per display
refresh and the phase timings in `frame_stats` are per frame.
"""
import sys

A, B = sys.argv[1], sys.argv[2]
# Window 1400x900: map area is 1064x876 starting at x=56 (mode panel 56, side
# panel 280, status bar 24), so the map centre is window (588, 438).
CX, CY = 588, 438
ZOOMS = [14, 15.5, 17, 18.5, 19.5]
# A node of multipolygon 541006 (an apartments building outer ring), used for
# the node-drag edit test. Copy 0 of B keeps the same id.
NODE = (48.863148, 2.3402247)

out = []
w = out.append


def section(name, lat, lon, node_lat, node_lon, lon_shift):
    w(f"# ---- dataset {name} ----")
    for z in ZOOMS:
        w(f"viewport {lat} {lon} {z}")
        w("wait 1500ms")
        w("frame_stats discard")
        w("wait 3s")
        w(f"frame_stats {name}_z{z}_static")
        # Same view without the downloaded-area wash.
        w("action ToggleDownloadedArea")
        w("wait 1500ms")
        w("frame_stats discard")
        w("wait 3s")
        w(f"frame_stats {name}_z{z}_nowash")
        w("action ToggleDownloadedArea")
        w("wait 500ms")
    # Pans (drags): the view moves every frame it is dispatched.
    for z in (17, 18.5):
        w(f"viewport {lat} {lon} {z}")
        w("wait 1s")
        w("frame_stats discard")
        for i in range(40):
            if i % 2 == 0:
                w(f"drag {CX},{CY} {CX - 90},{CY - 50} duration=100ms")
            else:
                w(f"drag {CX - 90},{CY - 50} {CX},{CY} duration=100ms")
            w("wait 60ms")
        w(f"frame_stats {name}_z{z}_pan")
    # Hover sweeps over a grid in Select mode: hit-test plus hover highlight.
    for z in (17, 19.5):
        w(f"viewport {lat} {lon} {z}")
        w("wait 1s")
        w("frame_stats discard")
        pts = []
        for j in range(7):
            xs = range(12) if j % 2 == 0 else range(11, -1, -1)
            for i in xs:
                pts.append((200 + 70 * i, 150 + 100 * j))
        for x, y in pts:
            w(f"hover {x},{y}")
        w(f"frame_stats {name}_z{z}_hover")
    # Node drag: select a vertex of a multipolygon outer ring and move it
    # back and forth (each release commits the move and refreshes MpCache).
    w(f"viewport {node_lat} {node_lon + lon_shift} 19.5")
    w("wait 1s")
    w("frame_stats discard")
    w(f"click {CX},{CY}")
    w("wait 500ms")
    w(f"capture target/perf/capture-{name}-selected.png")
    for i in range(6):
        w(f"drag {CX},{CY} {CX + 40},{CY + 25} button=left")
        w("wait 300ms")
        w(f"drag {CX + 40},{CY + 25} {CX},{CY} button=left")
        w("wait 300ms")
    w(f"capture target/perf/capture-{name}-after-drag.png")
    w(f"frame_stats {name}_nodedrag")


w("window 1400 900")
w("wait 1s")
w("frame_stats startup")
w(f"load_osm {A}")
w("wait 3s")
w("frame_stats A_load")
section("A", 48.863, 2.340, NODE[0], NODE[1], 0.0)
w(f"load_osm {B}")
w("wait 4s")
w("frame_stats B_load")
section("B", 48.872, 2.850, NODE[0], NODE[1], 0.5)
w("log done")
print("\n".join(out))
