#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
python3 "$root/packaging/build.py" build --release --locked
target_dir=${CARGO_TARGET_DIR:-target}
if [[ $target_dir != /* ]]; then target_dir="$root/$target_dir"; fi
output=${1:-"$root/dist"}
mkdir -p -- "$output"
output=$(cd -- "$output" && pwd -P)
work=$(mktemp -d -- "$output/.forgeusb-portable.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
bundle="$work/forgeusb-0.1.0-linux-x86_64"
mkdir -p -- "$bundle/bin" "$bundle/licenses" "$bundle/packaging"
for name in forgeusb forgeusb-gui; do
    install -m755 -- "$target_dir/release/$name" "$bundle/bin/$name"
done
# The helper and policy must be installed in trusted /usr paths by an administrator.
# Never execute a bundled helper as root or change the application's fixed helper path.
install -m755 -- "$target_dir/release/forgeusb-helper" "$bundle/packaging/forgeusb-helper"
install -m644 -- "$root/packaging/polkit/io.forgeusb.write.policy" "$bundle/packaging/"
install -m644 -- "$root/LICENSE" "$root/THIRD_PARTY_NOTICES.md" "$root/docs/third-party-licenses.txt" "$root/docs/dependency-licenses.json" "$bundle/licenses/"
cat > "$bundle/README.txt" <<'TEXT'
ForgeUSB 0.1.0 — initial Linux x86_64 engineering build
Run bin/forgeusb-gui as your normal desktop user, or bin/forgeusb --help.
This directory uses host glibc/window-system libraries; it is not universal across
older distributions. See the corresponding source archive and validation docs.
Physical writes require the helper+Polkit policy installed under trusted /usr paths.
Do not run a portable GUI/helper as root. No device is automatically selected.
See THIRD_PARTY_NOTICES.md: a dependency has an upstream license-file omission
that needs review before public binary redistribution. No public release is made.
TEXT
getconf GNU_LIBC_VERSION > "$bundle/build-libc.txt"
ldd "$bundle/bin/forgeusb-gui" > "$bundle/dynamic-libraries.txt"
(cd "$bundle" && sha256sum bin/* packaging/* > SHA256SUMS)
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    -C "$work" -cf - forgeusb-0.1.0-linux-x86_64 | gzip -n > "$output/forgeusb-0.1.0-linux-x86_64.tar.gz"
sha256sum -- "$output/forgeusb-0.1.0-linux-x86_64.tar.gz"
