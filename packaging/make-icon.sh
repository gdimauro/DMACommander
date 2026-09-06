#!/usr/bin/env bash
# Turn packaging/icon.svg into an .icns, the only icon format the Dock reads.
#
# Needs rsvg-convert (brew install librsvg). The result is committed, so this
# only has to run when the artwork changes.
set -euo pipefail
cd "$(dirname "$0")"

command -v rsvg-convert >/dev/null || {
  echo "rsvg-convert not found. brew install librsvg" >&2
  exit 1
}

set="$(mktemp -d)/dmac.iconset"
mkdir -p "$set"
trap 'rm -rf "$(dirname "$set")"' EXIT

# The sizes macOS actually asks for. @2x is the same pixel count as the next
# size up, under the name the retina display looks for.
for size in 16 32 128 256 512; do
  rsvg-convert -w "$size"          -h "$size"          icon.svg -o "$set/icon_${size}x${size}.png"
  rsvg-convert -w "$((size * 2))"  -h "$((size * 2))"  icon.svg -o "$set/icon_${size}x${size}@2x.png"
done

iconutil -c icns "$set" -o icon.icns
echo "wrote $(pwd)/icon.icns"
