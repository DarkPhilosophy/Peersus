"""Generate offline street extracts (no user locations). Requires osmium 4.3.2.
Usage: python streets-data.py INPUT_DIRECTORY OUTPUT_DIRECTORY.
Inputs are from Geofabrik Europe; the boxes describe public example areas, not device fixes.
"""
from pathlib import Path
import hashlib, struct, sys
import osmium

src,out=map(Path,sys.argv[1:])
classes='motorway trunk primary secondary tertiary residential unclassified living_street pedestrian service motorway_link trunk_link primary_link secondary_link tertiary_link'.split()
for region,file,x0,y0,w,h in [('bucharest','romania',2595,4433,28,20),('monaco','monaco',740,4372,5,4)]:
    path=src/(file+'.osm.pbf');cells=[[] for _ in range(w*h)];names=bytearray();offsets={};ways=0
    with path.open('rb') as f:
        expected={'romania':'c1f6aaeb8c5a1dadf7414d6189019031e468e1d88c9180388afbcd24a17bed62','monaco':'7f6b12a0de88458fdca3182251aeed8d7c4d2c2e97f14c4e0933564202a65c99'}
        if hashlib.file_digest(f,'sha256').hexdigest()!=expected[file]:raise ValueError('Unexpected OSM extract revision')
    def cell(x,y): return (x//1000-x0,y//1000-y0)
    for way in osmium.FileProcessor(path).with_locations().with_filter(osmium.filter.EntityFilter(osmium.osm.WAY)).with_filter(osmium.filter.KeyFilter('highway')):
        kind=way.tags.get('highway');
        if kind not in classes or not len(way.nodes):continue
        first=way.nodes[0].location
        if not first.valid() or not (x0-1<=first.lon*100<=x0+w+1 and y0-1<=first.lat*100<=y0+h+1):continue
        name=way.tags.get('name','');key=name.encode()[:240]; prev=None;included=False
        for n in way.nodes:
            if not n.location.valid():prev=None;continue
            cur=round(n.lon*100000),round(n.lat*100000)
            if prev and prev!=cur:
                a,b=prev;c,d=cur;p,q=cell(min(a,c),min(b,d));r,s=cell(max(a,c),max(b,d))
                for j in range(max(0,q),min(h,s+1)):
                    for i in range(max(0,p),min(w,r+1)):
                        if key not in offsets:offsets[key]=len(names);names+=bytes([len(key)])+key
                        u=(i+x0)*1000;v=(j+y0)*1000
                        cells[j*w+i].append((classes.index(kind),a-u,b-v,c-u,d-v,offsets[key]));included=True
            prev=cur
        ways+=included
    body=bytearray();index=[]
    for cell_records in cells:
        index.append(len(body));
        for record in sorted(cell_records):body+=struct.pack('<BhhhhI',*record)
    index.append(len(body));data=struct.pack('<iiHHI',x0,y0,w,h,len(names))+names+struct.pack('<%dI'%len(index),*index)+body
    (out/(region+'.bin')).write_bytes(data);print(region,ways,'ways',len(offsets),'names',len(data),'bytes',flush=True)
