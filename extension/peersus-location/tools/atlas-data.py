"""Build the offline regional atlas from three public-domain Natural Earth zip archives.
Run: python3 atlas-data.py INPUT_DIRECTORY OUTPUT_DIRECTORY (pyshp==2.3.1).
Input URLs: https://naturalearth.s3.amazonaws.com/{scale}_cultural/{name}.zip
Line records are int16 hundredths of degrees, indexed by 5-degree cells; rank 0 is a border.
"""
import hashlib
from pathlib import Path
import struct
import sys
import shapefile

SOURCES = {
    'ne_10m_roads': '880b235383a1e1ccf33f4ea6f82b6755a2ffbce07e3974e412b43e88fe954ae9',
    'ne_50m_admin_0_boundary_lines_land': '02f369ae10578c4ce33fc4a08f8cdd69ea11eb39b44d7923e88eb7199aaff8ea',
    'ne_10m_populated_places_simple': '2963ee08f7743f13f37d5243acd77a2f7a510e26a36c7e04b920f1b381f2b251',
}

def build(src, out):
    cells = [set() for _ in range(72*36)]
    cities = []
    for name, digest in SOURCES.items():
        path = src / (name+'.zip')
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f'Unexpected source: {name}')
        with shapefile.Reader(str(path), encoding='utf-8') as shp:
            for feature in shp.iterShapeRecords():
                row = feature.record.as_dict()
                points = feature.shape.points
                if name.endswith('simple'):
                    x,y=points[0][:2]
                    label=' '.join(row['name'].split())
                    if label: cities.append((-row['pop_max'],round(x*100),round(y*100),label))
                    continue
                rank = row.get('scalerank',0)
                if rank > 9 or row.get('type','').startswith('Ferry'): continue
                ends = [*feature.shape.parts,len(points)]
                for begin,end in zip(ends,ends[1:]):
                    prev=None
                    for i in range(begin,end):
                        p=tuple(round(n*100) for n in points[i][:2])
                        if prev and (p==prev or (i+1!=end and max(abs(p[0]-prev[0]),abs(p[1]-prev[1])) < 5)):continue
                        if prev and abs(p[0]-prev[0])<18000:
                            x0,x1=sorted(((p[0]+18000)//500,(prev[0]+18000)//500)); y0,y1=sorted(((p[1]+9000)//500,(prev[1]+9000)//500)); item=(rank,*prev,*p)
                            for y in range(max(0,y0),min(35,y1)+1):
                                for x in range(max(0,x0),min(71,x1)+1):cells[y*72+x].add(item)
                        prev=p
    data=bytearray();index=[]
    for cell in cells:
        index.append(len(data));data.extend(b''.join(struct.pack('<Bhhhh',*item) for item in sorted(cell)))
    index.append(len(data));(out/'lines.bin').write_bytes(struct.pack('<%dI'%len(index),*index)+data)
    places=bytearray()
    for _,x,y,label in sorted(cities):
        name=label.encode('utf-8')
        places.extend(struct.pack('<hhB',x,y,len(name)))
        places.extend(name)
    (out/'places.bin').write_bytes(places)

if __name__=='__main__':build(*map(Path,sys.argv[1:]))
