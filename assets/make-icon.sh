#!/bin/sh
# Create correct 32x32 raw RGBA for tray-icon.rgba from SVG
convert recast-icon.svg -background none -alpha remove -resize 32x32 RGBA:tray-icon.rgba
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
