# Source/policy/staging validation record

Recorded 2026-10-17, Linux x86_64. These results concern the four pure/staging crates and package metadata; workspace/runtime/boot evidence is recorded separately.

Commands run from the ForgeUSB root:

```sh
CARGO_TARGET_DIR=/tmp/forgeusb-research-target \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/gcc \
CARGO_BUILD_JOBS=1 cargo test --locked --offline \
  -p forgeusb-partition -p forgeusb-filesystem -p forgeusb-boot -p forgeusb-windows

CARGO_TARGET_DIR=/tmp/forgeusb-research-target \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/gcc \
CARGO_BUILD_JOBS=1 cargo clippy --offline --locked \
  -p forgeusb-partition -p forgeusb-filesystem -p forgeusb-boot -p forgeusb-windows \
  --all-targets -- -D warnings

desktop-file-validate packaging/forgeusb.desktop
bash -n packaging/make-source-archive.sh
bash -n packaging/arch/PKGBUILD
```

Results: **12 unit tests passed**, no ignored tests; targeted clippy passed with warnings denied. Rustfmt check for these four crates passed. Desktop validation and both shell syntax checks passed. XML parsing of the Polkit policy and SVG passed. The local host's configured linker name was absent, so a per-command `/usr/bin/gcc` override was used; no global Cargo/Codex configuration was changed.

Coverage: raw-strategy firmware preservation and refusal cases; Windows strategy never falling back to executable raw; FAT32 exact file-size boundary and ambiguous filenames; GPT 512/4096-byte metadata reservations/alignment; capacity/MBR/FAT32 geometry failures; Windows tree header detection, sparse >4 GiB install.wim classification, case collisions, symlinks and truncated EFI input.

`cargo metadata --locked --offline --filter-platform x86_64-unknown-linux-gnu --format-version 1` generated the target-filtered 222-package license inventory. All 210 registry dependencies declared a license expression. This is metadata inventory, not a legal audit of all notices. All-platform offline metadata was unavailable because an Android-only crate was not cached; no Android support is claimed.

`/usr/bin/wimlib-imagex` was absent, so the real optional split subprocess was **not executed**. No WIM split runtime or Windows installer boot claim is made. Synthetic WIM/PE headers exercise policy only. No physical disk operation, partition creation, mount, format, privileged installation, Arch package build, DEB build or RPM build was performed for this record. The source archive recipe requires the final project README/license/notices and should be exercised after those are written.
