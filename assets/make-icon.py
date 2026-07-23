#!/usr/bin/env python3
from xml.etree.ElementTree import parse
import sys

try:
 import cairosvg
except:
 print("cairosvg required", file=sys.stderr)
 sys.exit(2)
import cairosvg
# banner-logo.svg is the current mark; recast-icon.svg's thin tilted arrows
# collapse into a blue square at 32px. Keep this in sync with make-icon.sh.
cairosvg.svg2png(url="banner-logo.svg",write_to="tray-icon.png",output_width=32,output_height=32)
from PIL import Image
i=Image.open("tray-icon.png")
rgba=i.tobytes()
open("tray-icon.rgba","wb").write(rgba)
print(f"Created {len(rgba)} bytes")
