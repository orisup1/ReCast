#!/usr/bin/env python3
from pathlib import Path
import sys

try:
    import cairosvg
except ImportError:
    print("cairosvg required", file=sys.stderr)
    sys.exit(2)

from PIL import Image

# banner-logo.svg is the current mark; recast-icon.svg's thin tilted arrows
# collapse into a blue square at 32px. Keep this in sync with make-icon.sh.
assets = Path(__file__).resolve().parent
cairosvg.svg2png(
    url=str(assets / "banner-logo.svg"),
    write_to=str(assets / "tray-icon.png"),
    output_width=32,
    output_height=32,
)
with Image.open(assets / "tray-icon.png") as image:
    rgba = image.convert("RGBA").tobytes()
(assets / "tray-icon.rgba").write_bytes(rgba)
print(f"Created {len(rgba)} bytes")

# Embed the existing macOS template in the same raw RGBA format as the tray icon.
with Image.open(assets / "icons/template-32.png") as image:
    (assets / "tray-template.rgba").write_bytes(image.convert("RGBA").tobytes())
