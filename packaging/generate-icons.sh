#!/usr/bin/env bash
# Optional maintainer tool. Normal builds use the checked-in generated assets.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
for size in 16 24 32 48 64 128 256; do
    /usr/bin/rsvg-convert -w "$size" -h "$size" "$root/packaging/icons/forgeusb.svg" \
        -o "$root/packaging/icons/forgeusb-$size.png"
done
mkdir -p -- "$root/apps/forgeusb-gui/assets"
/usr/bin/magick "$root/packaging/icons/forgeusb-64.png" -depth 8 \
    "RGBA:$root/apps/forgeusb-gui/assets/forgeusb-64.rgba"
