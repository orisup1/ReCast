#!/bin/sh
# Create correct 32x32 raw RGBA for tray-icon.rgba from the current logo.
# Source is banner-logo.svg — the newest keycap+loop mark, whose thicker strokes
# stay legible at 32px (recast-icon.svg's thin, tilted arrows collapse into a
# featureless blue square at tray size). rsvg-convert renders the SVG far more
# faithfully than ImageMagick's built-in SVG rasterizer, so go via a PNG.
rsvg-convert -w 32 -h 32 banner-logo.svg -o tray-icon.png
convert tray-icon.png RGBA:tray-icon.rgba
# Verify byte count
if [ -f tray-icon.rgba ]; then
  size=$(wc -c < tray-icon.rgba | tr -d ' ')
  echo "Created tray-icon.rgba: ${size} bytes"
  # Must be 4096 for 32x32x4 RGBA
  [ "$size" -eq 4096 ] && echo "Correct size (4096 bytes)"
else
  echo "Failed to create tray-icon.rgba" >&2
  exit 1
fi
