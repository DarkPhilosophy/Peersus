"""Build countries.bin from the public-domain Natural Earth ne_50m_admin_0_countries ZIP.

Usage: python3 countries.py ARCHIVE OUTPUT (pyshp==2.3.1).
Source: https://naturalearth.s3.amazonaws.com/50m_cultural/ne_50m_admin_0_countries.zip
Records: label longitude and latitude in hundredths of degrees, square root of
its land area in hundredths of angular degrees, then a length-prefixed UTF-8 name.
"""
import hashlib
from math import degrees, radians, sin, sqrt
from pathlib import Path
import struct
import sys

import shapefile

SHA256 = '5fed433373581fa648920435f937d95f2d3c0200e067409c6478dcdf1b853139'


def contains(points, x, y):
    inside = False
    for (a, b), (c, d) in zip(points, points[1:]):
        if (b > y) != (d > y) and x < a + (c - a) * (y - b) / (d - b):
            inside = not inside
    return inside


def area(points):
    # The integral of sin(latitude) d(longitude), in square radians on a unit sphere.
    return abs(sum(radians((c - a + 180) % 360 - 180) * (sin(radians(b)) + sin(radians(d)))
                   for (a, b), (c, d) in zip(points, points[1:]))) / 2


def build(source, output):
    if hashlib.sha256(source.read_bytes()).hexdigest() != SHA256:
        raise ValueError('Unexpected Natural Earth country source')
    records = []
    with shapefile.Reader(str(source), encoding='utf-8') as shapes:
        for country in shapes.iterShapeRecords():
            meta, shape = country.record, country.shape
            name = meta.NAME_EN.encode('utf-8')
            lon, lat = meta.LABEL_X, meta.LABEL_Y
            parts = [shape.points[a:b] for a, b in zip(shape.parts, [*shape.parts[1:], len(shape.points)])]
            # France's label is in Europe: an overseas polygon must not enlarge that label.
            # A label that isn't inside a ring refers to the principal landmass instead.
            region = max((area(p) for p in parts if contains(p, lon, lat)), default=0)
            if not region:
                region = max(map(area, parts))
            extent = round(100 * degrees(sqrt(region)))
            record = struct.pack('<hhHB', round(lon * 100), round(lat * 100), extent, len(name))
            records.append((name, record + name))
    output.write_bytes(b''.join(record for _, record in sorted(records)))


if __name__ == '__main__':
    build(*map(Path, sys.argv[1:]))
