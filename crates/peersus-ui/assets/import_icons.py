#!/usr/bin/env python3
"""Import the interface pictures from IconPark (Apache-2.0, Copyright 2019-present Bytedance Inc.).

IconPark draws each icon with literal colours: black for the line, #2F88FF for the main shape and #43CCF8 for a second one.
A colour written in a file would ignore the theme, so every icon is split here into one file per layer, each drawn in
`currentColor` and nothing else, and the application colours the layers from the theme:

    <name>.svg        the line         (black in the source)
    <name>-tint.svg   the main shape   (#2F88FF in the source), coloured with the person's accent
    <name>-soft.svg   a second shape   (#43CCF8 in the source), where an icon has one

The files are CHANGED from the originals (split by layer, colours replaced), and say so in a comment, as the licence asks.
Icons are found by name in the repository's own tree, so a moved file is found where it is. Run with network access from this
directory: `python3 import_icons.py`. The licence text goes to `IconPark-LICENSE` beside it.
"""
from pathlib import Path
import json
import urllib.request
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
OUT = HERE / "icons"
RAW = "https://raw.githubusercontent.com/bytedance/IconPark/master/"
TREE = "https://api.github.com/repos/bytedance/IconPark/git/trees/master?recursive=1"
SVG = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG)

# What each picture of the application is, and the name of the IconPark icon that draws it.
PICTURES = {
    "send": "inbox-upload-r",
    "receive": "inbox-download-r",
    "peers": "connect",
    "overview": "dashboard-one",
    "device": "code-laptop",
    "appearance": "platte",
    "network": "planet",
    "plugins": "puzzle",
    "permissions": "lock-one",
    "history": "history-query",
    "about": "help",
    "settings": "setting-two",
    # The arrow of a keyboard's Backspace key.
    "backspace": "arrow-left",
    # IconPark layout-one, revision 8dc132da4c85671ba6a5962c87aa2bdafbf158e9.
    "layout": "layout-one",
    # The Languages tab of Settings.
    "languages": "translate",
    # A place on a map: what a page about where things are uses.
    "location": "local-two",
}

MAIN, SOFT = "#2f88ff", "#43ccf8"
BLACK = ("black", "#000", "#000000")
WHITE = ("white", "#fff", "#ffffff")
NOTE = (
    "Derived from IconPark (https://github.com/bytedance/IconPark), Apache-2.0, Copyright 2019-present Bytedance Inc. "
    "Changed: split by layer, colours replaced by currentColor."
)
SHAPES = ("path", "rect", "circle", "ellipse", "line", "polygon", "polyline")


def fetch(url):
    with urllib.request.urlopen(url, timeout=30) as response:
        return response.read()


def locate():
    """The path of every icon in the repository, by name."""
    tree = json.loads(fetch(TREE))
    found = {}
    for entry in tree["tree"]:
        path = entry["path"]
        if path.startswith("source/") and path.endswith(".svg"):
            found[path.rsplit("/", 1)[-1][:-4]] = path
    return found


def build(shapes, view_box):
    root = ET.Element(f"{{{SVG}}}svg", {"viewBox": view_box, "fill": "none"})
    root.append(ET.Comment(f" {NOTE} "))
    for shape in shapes:
        root.append(shape)
    ET.indent(root, space="  ")
    return ET.tostring(root, encoding="unicode") + "\n"


def as_fill(element):
    copy = ET.fromstring(ET.tostring(element))
    copy.set("fill", "currentColor")
    for attribute in ("stroke", "stroke-width", "stroke-linejoin", "stroke-linecap"):
        copy.attrib.pop(attribute, None)
    return copy


def as_line(element):
    copy = ET.fromstring(ET.tostring(element))
    copy.set("fill", "none")
    copy.set("stroke", "currentColor")
    return copy


def as_cut(element):
    """A detail cut out of a shape, white in the source: drawn in the colour of the page, so the shapes under it show a seam."""
    copy = ET.fromstring(ET.tostring(element))
    if (copy.get("fill") or "").lower() in WHITE:
        copy.set("fill", "currentColor")
        for attribute in ("stroke", "stroke-width", "stroke-linejoin", "stroke-linecap"):
            copy.attrib.pop(attribute, None)
    else:
        copy.set("fill", "none")
        copy.set("stroke", "currentColor")
    return copy


def split(svg_bytes):
    root = ET.fromstring(svg_bytes)
    view_box = root.get("viewBox") or "0 0 48 48"
    layers = {"line": [], "tint": [], "soft": [], "cut": []}
    for element in root.iter():
        if element.tag.split("}")[-1] not in SHAPES:
            continue
        fill = (element.get("fill") or "").lower()
        stroke = (element.get("stroke") or "").lower()
        if fill == MAIN:
            layers["tint"].append(as_fill(element))
        elif fill == SOFT:
            layers["soft"].append(as_fill(element))
        elif fill in WHITE:
            layers["cut"].append(as_cut(element))
        if stroke in BLACK:
            layers["line"].append(as_line(element))
        elif stroke in WHITE:
            layers["cut"].append(as_cut(element))
    return view_box, layers


def main():
    OUT.mkdir(exist_ok=True)
    paths = locate()
    for name, icon in PICTURES.items():
        assert icon in paths, f"{name}: IconPark has no icon called {icon}"
        view_box, layers = split(fetch(RAW + paths[icon]))
        assert layers["line"], f"{name}: no line layer found in {paths[icon]}"
        (OUT / f"{name}.svg").write_text(build(layers["line"], view_box))
        for suffix in ("tint", "soft", "cut"):
            target = OUT / f"{name}-{suffix}.svg"
            if layers[suffix]:
                target.write_text(build(layers[suffix], view_box))
            elif target.exists():
                target.unlink()
        print(f"{name:12} line {len(layers['line'])}  tint {len(layers['tint'])}  soft {len(layers['soft'])}  cut {len(layers['cut'])}  <- {paths[icon]}")
    (HERE / "IconPark-LICENSE").write_text(fetch(RAW + "LICENSE").decode())


if __name__ == "__main__":
    main()
