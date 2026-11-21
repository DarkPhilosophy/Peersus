"""Make wasm/src/world/land.txt: the land outlines the Location extension carries, from Natural Earth's 1:110m land.

The source is Natural Earth 4.1.0 "ne_110m_land" (physical, WGS84 geographic degrees), in the public domain:

  https://naciscdn.org/naturalearth/110m/physical/ne_110m_land.zip
  zip sha256 1926c621afd6ac67c3f36639bb1236134a48d82226dc675d3e3df53d02d2a3de
  ne_110m_land.shp sha256 8689e6932b8e370e2ca4587cf3ba21e460b1235db37b6ed3c172c35b4a6088de

What it changes, and nothing else: each outline is unrolled across the date line (one longitude sequence, so a ring has no
seam inside it), closed by not repeating its first point, rounded to a hundredth of a degree, and points that lie on the line
between their neighbours are dropped. Each ring is written on one line: "lon,lat" for its first point, then "dlon,dlat" from
the point before, in hundredths of a degree.

Usage: python3 world-data.py SHAPEFILE OUT.txt
"""
import struct
import sys


def rings(path):
    """Every ring of a shapefile of polygons, as lists of (longitude, latitude)."""
    data = open(path, 'rb').read()
    position, found = 100, []
    while position < len(data):
        _, length = struct.unpack('>ii', data[position:position + 8])
        body = data[position + 8:position + 8 + length * 2]
        position += 8 + length * 2
        if struct.unpack('<i', body[0:4])[0] == 0:
            continue
        parts, points = struct.unpack('<ii', body[36:44])
        starts = list(struct.unpack('<%di' % parts, body[44:44 + 4 * parts]))
        offset = 44 + 4 * parts
        vertices = [struct.unpack('<2d', body[offset + 16 * i:offset + 16 * i + 16])
                    for i in range(points)]
        for index, start in enumerate(starts):
            end = starts[index + 1] if index + 1 < parts else points
            found.append(vertices[start:end])
    return found


def outline(ring):
    """One ring as the file writes it: unrolled, closed, rounded, and with no point that says nothing."""
    unrolled = []
    for longitude, latitude in ring:
        if unrolled:
            previous = unrolled[-1][0]
            while longitude - previous > 180:
                longitude -= 360
            while previous - longitude > 180:
                longitude += 360
        unrolled.append((longitude, latitude))
    while len(unrolled) > 1 and unrolled[0] == unrolled[-1]:
        unrolled.pop()
    rounded = []
    for longitude, latitude in unrolled:
        point = (round(longitude * 100), round(latitude * 100))
        if not rounded or rounded[-1] != point:
            rounded.append(point)
    if len(rounded) < 4:
        return None
    kept = [rounded[0]]
    for index in range(1, len(rounded) - 1):
        a, b, c = kept[-1], rounded[index], rounded[index + 1]
        if (b[0] - a[0]) * (c[1] - a[1]) == (b[1] - a[1]) * (c[0] - a[0]):
            continue
        kept.append(b)
    kept.append(rounded[-1])
    return kept if len(kept) >= 4 else None


HEADER = (
    '# Peersus Location: land outlines, Natural Earth 4.1.0 "ne_110m_land" (public domain), WGS84 degrees.\n'
    '# Source: https://naciscdn.org/naturalearth/110m/physical/ne_110m_land.zip\n'
    '#   zip sha256 1926c621afd6ac67c3f36639bb1236134a48d82226dc675d3e3df53d02d2a3de, '
    'ne_110m_land.shp sha256\n'
    '#   8689e6932b8e370e2ca4587cf3ba21e460b1235db37b6ed3c172c35b4a6088de.\n'
    '# One outline a line: "lon,lat" in hundredths of a degree, then each next point as "dlon,dlat" from the one before.\n'
)


def main(source, out):
    lines = []
    for ring in rings(source):
        kept = outline(ring)
        if kept is None:
            continue
        steps = ['%d,%d' % kept[0]]
        previous = kept[0]
        for point in kept[1:]:
            steps.append('%d,%d' % (point[0] - previous[0], point[1] - previous[1]))
            previous = point
        lines.append(' '.join(steps))
    body = HEADER + '\n'.join(lines) + '\n'
    open(out, 'w').write(body)
    points = sum(len(line.split()) for line in lines)
    print('%d outlines, %d points, %d bytes' % (len(lines), points, len(body.encode())))


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
