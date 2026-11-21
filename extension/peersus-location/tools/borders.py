"""Build borders.bin from Natural Earth's pinned 1:50m land-boundary archive.

Usage: python3 borders.py ARCHIVE OUTPUT (pyshp==2.3.1).
The file holds the ends of the detailed and regional sections, then the records
for those sections and the globe. Points are in hundredths of a degree.
"""
import hashlib
from pathlib import Path
import struct
import sys

import shapefile

SHA256 = '02f369ae10578c4ce33fc4a08f8cdd69ea11eb39b44d7923e88eb7199aaff8ea'


def simplify(points, epsilon):
    x, y = points[0]
    dx, dy = points[-1][0] - x, points[-1][1] - y
    length = dx * dx + dy * dy

    def deviation(point):
        a, b = point[0] - x, point[1] - y
        t = min(1, max(0, (a * dx + b * dy) / length)) if length else 0
        return (a - t * dx) ** 2 + (b - t * dy) ** 2

    farthest, index = max(((deviation(p), i) for i, p in enumerate(points[1:-1], 1)),
                          default=(0, 0))
    if farthest <= epsilon ** 2 and (length or len(points) == 2):
        return [points[0], points[-1]]
    return simplify(points[:index + 1], epsilon)[:-1] + simplify(points[index:], epsilon)


def packed(points):
    xs, ys = zip(*points)
    return (struct.pack('<hhhhH', min(xs), min(ys), max(xs), max(ys), len(points))
            + b''.join(struct.pack('<hh', *p) for p in points))


def build(source, output):
    if hashlib.sha256(source.read_bytes()).hexdigest() != SHA256:
        raise ValueError('Unexpected Natural Earth border source')
    fine, regional, globe = bytearray(), bytearray(), bytearray()
    with shapefile.Reader(str(source)) as shapes:
        for shape in shapes.iterShapes():
            ends = [*shape.parts, len(shape.points)]
            for a, b in zip(ends, ends[1:]):
                points = []
                for x, y in shape.points[a:b]:
                    point = round(x * 100), round(y * 100)
                    if not points or points[-1] != point:
                        points.append(point)
                if len(points) < 2:
                    continue
                # Bound the envelope of each fine run so local views don't walk a long,
                # mostly off-screen line. Neighboring records share their endpoint.
                for i in range(0, len(points) - 1, 31):
                    fine.extend(packed(points[i:i + 32]))
                regional.extend(packed(simplify(points, 15)))
                globe.extend(packed(simplify(points, 55)))
    output.write_bytes(struct.pack('<II', len(fine), len(fine) + len(regional))
                       + fine + regional + globe)


if __name__ == '__main__':
    build(*map(Path, sys.argv[1:]))
