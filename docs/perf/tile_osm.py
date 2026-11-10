"""Tile one OSM API download COLSxROWS times (shifted ids and coordinates) into one file.
usage: tile_osm.py in.osm out.osm COLS ROWS DLON DLAT"""
import re, sys
src, dst, cols, rows, dlon, dlat, lon0 = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), float(sys.argv[5]), float(sys.argv[6]), float(sys.argv[7])
OFF = 10**11
lines = open(src, encoding='utf-8').read().split('\n')
head, body, tail = [], [], []
state = 0
for l in lines:
    t = l.strip()
    if t.startswith('<osm'):
        head.append(l); state = 1; continue
    if t.startswith('</osm'):
        tail.append(l); state = 2; continue
    if state == 1: body.append(l)
    elif state == 0: head.append(l)
    else: tail.append(l)
idre = re.compile(r'\b(id|ref)="(\d+)"')
latre = re.compile(r'\blat="([-0-9.]+)"')
lonre = re.compile(r'\blon="([-0-9.]+)"')
bre = re.compile(r'\b(minlat|maxlat|minlon|maxlon)="([-0-9.]+)"')
with open(dst, 'w', encoding='utf-8') as out:
    out.write('\n'.join(head) + '\n')
    for r in range(rows):
        for c in range(cols):
            k = r * cols + c
            for l in body:
                if '<bounds' in l:
                    def bf(m):
                        v = float(m.group(2)) + (c * dlon + lon0 if 'lon' in m.group(1) else r * dlat)
                        return '%s="%.7f"' % (m.group(1), v)
                    l = bre.sub(bf, l)
                else:
                    l = idre.sub(lambda m: '%s="%d"' % (m.group(1), int(m.group(2)) + k * OFF), l)
                    if l.lstrip().startswith('<node'):
                        l = latre.sub(lambda m: 'lat="%.7f"' % (float(m.group(1)) + r * dlat), l)
                        l = lonre.sub(lambda m: 'lon="%.7f"' % (float(m.group(1)) + c * dlon + lon0), l)
                out.write(l + '\n')
    out.write('\n'.join(tail) + '\n')
