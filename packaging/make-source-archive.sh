#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
version=0.1.0
output=${1:-"$root/dist"}
mkdir -p -- "$output"
output=$(cd -- "$output" && pwd -P)
# Explicit allowlist excludes upstream artifacts, images, target/ and local evidence.
# Stable ordering/ownership/mtime make identical saved source trees reproducible.
epoch=${SOURCE_DATE_EPOCH:-0}
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
    --transform="s,^,forgeusb-$version/," -C "$root" \
    -cf - Cargo.toml Cargo.lock .cargo LICENSE THIRD_PARTY_NOTICES.md README.md crates apps tests docs packaging \
    | gzip -n > "$output/forgeusb-$version.tar.gz"
digest=$(sha256sum -- "$output/forgeusb-$version.tar.gz")
digest=${digest%% *}
sed "s/REGENERATE_WITH_MAKE_SOURCE_ARCHIVE/$digest/" "$root/packaging/arch/PKGBUILD" > "$output/PKGBUILD"
printf '%s  %s\n' "$digest" "forgeusb-$version.tar.gz" > "$output/SHA256SUMS"
printf 'Prepared %s\n' "$output/forgeusb-$version.tar.gz"
