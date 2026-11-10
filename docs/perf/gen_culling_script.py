#!/usr/bin/env python3
"""Generate the node-marker culling benchmark script.

usage: gen_culling_script.py A.osm B.osm TAG MODE > script.osmscript

MODE is one of
  z17    only the long zoom-17 view of dataset A (200+ frames, twice), for the
         v0.3.0 versus main comparison;
  A      the z17 view first (so one launch also covers it), then a static sweep
         over zooms 14 to 19.5 on A, pans, hover grids, and the screenshots
         (captures go to target/perf/TAG-*.png);
  full   A, then the same sweep on B.

Run the app WITHOUT the .osm file on its command line: the script loads A
itself, and loading it twice renders it twice.

A.osm and B.osm are the datasets described in 2026-11-render-measurement.md.
The way used for the "selected way at low zoom" screenshots is picked from A:
the longest highway whose longest segment is near the view centre at zoom 15.5.
Because the click offset in the harness was about 24 px off in the earlier
measurement, the click is tried three times (y as computed, 24 px higher, 24 px lower), with
a capture after each; the first one that shows a selected way is used.
"""
import math
import re
import sys

A, B, TAG, MODE = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

# Window 1400x900: map area is 1064x876 starting at x=56 (mode panel 56, side
# panel 280, status bar 24), so the map centre is window (588, 438).
CX, CY = 588, 438
LAT0, LON0 = 48.863, 2.340
R = 6378137.0


def merc(lat, lon):
    return R * math.radians(lon), R * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))


def to_window(lat, lon, zoom):
    mpp = 40075016.686 / (256 * 2 ** zoom)
    cx, cy = merc(LAT0, LON0)
    x, y = merc(lat, lon)
    return CX + (x - cx) / mpp, CY - (y - cy) / mpp


def pick_way(path, zoom):
    nodes = {}
    ways = []
    cur = None
    for line in open(path, encoding="utf-8"):
        t = line.strip()
        if t.startswith("<node "):
            m = re.search(r'id="(\d+)".*?lat="([-0-9.]+)".*?lon="([-0-9.]+)"', t)
            if m:
                nodes[m.group(1)] = (float(m.group(2)), float(m.group(3)))
        elif t.startswith("<way "):
            cur = {"refs": [], "hw": False}
        elif cur is not None and t.startswith("<nd "):
            cur["refs"].append(re.search(r'ref="(\d+)"', t).group(1))
        elif cur is not None and t.startswith("<tag ") and 'k="highway"' in t:
            cur["hw"] = True
        elif t.startswith("</way>"):
            if cur and cur["hw"]:
                ways.append(cur)
            cur = None
    best = None
    for w in ways:
        pts = [to_window(*nodes[r], zoom) for r in w["refs"] if r in nodes]
        if len(pts) < 6:
            continue
        for p, q in zip(pts, pts[1:]):
            mid = ((p[0] + q[0]) / 2, (p[1] + q[1]) / 2)
            seg = math.hypot(q[0] - p[0], q[1] - p[1])
            near = math.hypot(mid[0] - CX, mid[1] - CY)
            if near > 220 or seg < 25:
                continue
            total = sum(math.hypot(b[0] - a[0], b[1] - a[1]) for a, b in zip(pts, pts[1:]))
            score = total - near
            if best is None or score > best[0]:
                best = (score, mid, total, len(pts))
    return best


out = []
w = out.append


def long_view(name, lat, lon, zoom, seconds):
    w(f"viewport {lat} {lon} {zoom}")
    w("wait 2s")
    w("frame_stats discard")
    w(f"wait {seconds}s")
    w(f"frame_stats {name}")


def section(name, lat, lon, secs, shots):
    w(f"# ---- dataset {name} ----")
    for z in (14, 15.5, 17, 18.5, 19.5):
        w(f"viewport {lat} {lon} {z}")
        w("wait 1500ms")
        w("frame_stats discard")
        w(f"wait {secs}s")
        w(f"frame_stats {name}_z{z}_static")
        if shots:
            w(f"capture target/perf/{TAG}-{name}-z{z}.png")
    for z in (17,):
        w(f"viewport {lat} {lon} {z}")
        w("wait 1s")
        w("frame_stats discard")
        for i in range(20):
            if i % 2 == 0:
                w(f"drag {CX},{CY} {CX - 90},{CY - 50} duration=100ms")
            else:
                w(f"drag {CX - 90},{CY - 50} {CX},{CY} duration=100ms")
            w("wait 60ms")
        w(f"frame_stats {name}_z{z}_pan")
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


w("window 1400 900")
w("wait 1s")
w("frame_stats startup")
w(f"load_osm {A}")
w("wait 3s")
w("frame_stats A_load")
# The v0.3.0 comparison view, twice (>= 200 frames each when frames fit 60 Hz).
long_view("A_z17_rep1", LAT0, LON0, 17, 10)
long_view("A_z17_rep2", LAT0, LON0, 17, 10)
if MODE in ("full", "A"):
    section("A", LAT0, LON0, 4, True)
    # Selected way at low zoom: its own nodes are drawn, the rest are not.
    pick = pick_way(A, 15.5)
    if pick:
        _, (mx, my), total, n = pick
        w(f"# way of {n} nodes, {total:.0f}px long at z15.5, click at {mx:.0f},{my:.0f}")
        w(f"viewport {LAT0} {LON0} 15.5")
        w("wait 1s")
        for k, dy in enumerate((0, -24, 24)):
            w(f"click {mx:.0f},{my + dy:.0f}")
            w("wait 800ms")
            w(f"capture target/perf/{TAG}-A-z15.5-selected-try{k}.png")
            w("click 80,860")
            w("wait 300ms")
    w(f"viewport {LAT0} {LON0} 19")
    w("wait 1s")
    w(f"capture target/perf/{TAG}-A-z19.png")
if MODE == "full":
    w(f"load_osm {B}")
    w("wait 5s")
    w("frame_stats B_load")
    section("B", 48.872, 2.850, 6, False)
w("log done")
print("\n".join(out))
