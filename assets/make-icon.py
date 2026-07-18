#!/usr/bin/env python3
from xml.etree.ElementTree import parse
import sys

try:
 import cairosvg
except:
 print("cairosvg required", file=sys.stderr)
 sys.exit(2)
import cairosvg
cairosvg.svg2png(url="recast-icon.svg",write_to="tray-icon.png", scale=32/256)
from PIL import Image
i=Image.open("tray-icon.png")
rgba=i.tobytes()
open("tray-icon.rgba","wb").write(rgba)
print(f"Created {len(rgba)} bytes")
