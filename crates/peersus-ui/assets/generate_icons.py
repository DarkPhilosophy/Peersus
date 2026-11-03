#!/usr/bin/env python3
"""Regenerate the desktop PNG and Android adaptive icon from the original SVG.

The source is `peersus-icon.svg`: a rounded `plate`, then an `art` group made of the
background `field` (many faint rounded cells) and the `mark` (the hook, the cream and coral
circles and the lens). The Android foreground is the `art` group alone, on a plate of its own.
"""
from pathlib import Path
import re
import subprocess
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
SOURCE = HERE / "peersus-icon.svg"
RES = HERE.parents[2] / "android/app/src/main/res"
ANDROID = "http://schemas.android.com/apk/res/android"
SVG = "http://www.w3.org/2000/svg"
ET.register_namespace("android", ANDROID)
# The launcher draws the artwork inside the adaptive safe zone, a little smaller than on the plain icon.
LAUNCHER_SCALE = 0.62


def android(**attributes):
    return {f"{{{ANDROID}}}{name}": value for name, value in attributes.items()}


def save(relative, element):
    destination = RES / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    ET.indent(element, space="  ")
    ET.ElementTree(element).write(destination, encoding="utf-8", xml_declaration=True)


def argb(fill, opacity):
    """`#rrggbb` with an opacity as the `#aarrggbb` Android reads."""
    return f"#{round(float(opacity) * 255):02x}{fill.lstrip('#')}"


def rounded_rectangle(rect):
    x, y = float(rect.attrib["x"]), float(rect.attrib["y"])
    w, h, r = float(rect.attrib["width"]), float(rect.attrib["height"]), float(rect.attrib.get("rx", 0))
    return (f"M{x + r:g} {y:g}H{x + w - r:g}A{r:g} {r:g} 0 0 1 {x + w:g} {y + r:g}V{y + h - r:g}"
            f"A{r:g} {r:g} 0 0 1 {x + w - r:g} {y + h:g}H{x + r:g}A{r:g} {r:g} 0 0 1 {x:g} {y + h - r:g}"
            f"V{y + r:g}A{r:g} {r:g} 0 0 1 {x + r:g} {y:g}z")


def circle(element):
    cx, cy, r = (float(element.attrib[name]) for name in ("cx", "cy", "r"))
    return f"M{cx - r:g} {cy:g}A{r:g} {r:g} 0 1 0 {cx + r:g} {cy:g}A{r:g} {r:g} 0 1 0 {cx - r:g} {cy:g}z"


def shapes(group, parent, offset=(0.0, 0.0)):
    """Copy the shapes of an SVG group into an Android vector group, in drawing order."""
    for element in group:
        tag = element.tag.removeprefix(f"{{{SVG}}}")
        if tag == "g":
            transform = element.attrib.get("transform", "")
            moved = re.fullmatch(r"translate\(([-\d.]+) ([-\d.]+)\)", transform)
            if transform and moved is None:
                raise ValueError(f"unsupported group transform: {transform}")
            inner = ET.SubElement(parent, "group", android(translateX=moved.group(1), translateY=moved.group(2))) if moved else parent
            shapes(element, inner)
        elif tag == "rect":
            ET.SubElement(parent, "path", android(pathData=rounded_rectangle(element),
                                                  fillColor=argb(element.attrib["fill"], element.attrib.get("fill-opacity", "1"))))
        elif tag == "circle":
            ET.SubElement(parent, "path", android(pathData=circle(element), fillColor=element.attrib["fill"]))
        elif tag == "path" and element.attrib.get("fill", "none") != "none":
            ET.SubElement(parent, "path", android(pathData=element.attrib["d"], fillColor=element.attrib["fill"]))
        elif tag == "path":
            ET.SubElement(parent, "path", android(pathData=element.attrib["d"], strokeColor=element.attrib["stroke"],
                                                  strokeWidth=element.attrib["stroke-width"],
                                                  strokeLineCap=element.attrib.get("stroke-linecap", "butt")))
        else:
            raise ValueError(f"unsupported element: {tag}")


source = ET.parse(SOURCE).getroot()
assert source.attrib["viewBox"] == "0 0 512 512"
plate = source.find(f"{{{SVG}}}rect[@id='plate']")
art = source.find(f"{{{SVG}}}g[@id='art']")
assert plate is not None and art is not None
# Centre the artwork of the launcher on the same point the plain icon centres it on, at the launcher's own scale.
placed = re.fullmatch(r"translate\(([-\d.]+) ([-\d.]+)\) scale\(([\d.]+)\)", art.attrib["transform"])
if placed is None:
    raise ValueError("The artwork transform needs an explicit Android equivalent")
x, y, scale = (float(value) for value in placed.groups())
centre = 256.0
launcher_x = centre - (centre - x) / scale * LAUNCHER_SCALE
launcher_y = centre - (centre - y) / scale * LAUNCHER_SCALE
vector = ET.Element("vector", android(width="108dp", height="108dp", viewportWidth="512", viewportHeight="512"))
group = ET.SubElement(vector, "group", android(translateX=f"{launcher_x:g}", translateY=f"{launcher_y:g}",
                                               scaleX=f"{LAUNCHER_SCALE:g}", scaleY=f"{LAUNCHER_SCALE:g}"))
shapes(art, group)
save("drawable/ic_launcher_foreground.xml", vector)
shape = ET.Element("shape", android(shape="rectangle"))
ET.SubElement(shape, "solid", android(color=plate.attrib["fill"]))
save("drawable/ic_launcher_background.xml", shape)
adaptive = ET.Element("adaptive-icon")
ET.SubElement(adaptive, "background", android(drawable="@drawable/ic_launcher_background"))
ET.SubElement(adaptive, "foreground", android(drawable="@drawable/ic_launcher_foreground"))
save("mipmap-anydpi-v26/ic_launcher.xml", adaptive)


def render(svg_text, name):
    """Rasterise `svg_text` to a 512 pixel PNG with a see-through background, next to this script."""
    subprocess.run(
        ["magick", "-background", "none", "-density", "96", "svg:-", "-resize", "512x512", f"PNG32:{HERE / name}"],
        input=svg_text.encode(), check=True,
    )


# The application draws its own mark with no plate (only the launcher has one, in `ic_launcher_background.xml`). On a dark header the mark
# is as drawn; on a light one the cream is turned to black so that it does not vanish, and the lens, which was the green of the plate, to white.
without_plate = ET.fromstring(ET.tostring(source))
without_plate.remove(without_plate.find(f"{{{SVG}}}rect[@id='plate']"))
dark_text = ET.tostring(without_plate, encoding="unicode")
render(dark_text, "peersus-icon.png")
render(dark_text.replace("#f5efe2", "#000000").replace("#173b34", "#ffffff"), "peersus-icon-light.png")
