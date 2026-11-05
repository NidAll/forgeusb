# Dependency and license inventory

ForgeUSB source is GPL-3.0-or-later. Rufus/WoeUSB source was studied to understand behavior; application logic is newly written Rust. No Rufus bootloader binaries, ISO contents or Microsoft assets are bundled. The GPL license text is the only upstream file copied into the project license when applicable. This is a conservative project-license choice, not a claim of upstream affiliation.

## Direct Rust dependencies

Actual selected versions belong to `Cargo.lock`; this table lists license families from upstream package metadata. Release builders must export the resolved Cargo metadata/license inventory for every distributed target, including transitive crates, before redistribution. The included inventory covers x86_64-unknown-linux-gnu and development dependencies: 249 packages, of which 236 are registry crates; no license field was missing at generation time. Do not infer complete compliance from this abbreviated table.

| Crate | Purpose | Upstream license expression |
|---|---|---|
| anyhow | Error context and propagation | MIT OR Apache-2.0 |
| serde / serde_json | Stable JSON data contracts | MIT OR Apache-2.0 |
| sha2 | Streaming SHA-256 | MIT OR Apache-2.0 |
| libc | Linux kernel/libc descriptor interface | MIT OR Apache-2.0 |
| iced | Native Rust GUI, tiny-skia backend | MIT |
| tempfile | Private construction/cache images, atomic output publication and disposable fixtures | MIT OR Apache-2.0 |
| md-5 / sha1 | MD5 and SHA-1 for comparing publishers' checksums (never used for safety decisions) | MIT OR Apache-2.0 |
| flate2 | gzip decompression of staged images (pure-Rust miniz_oxide backend) | MIT OR Apache-2.0 |
| lzma-rust2 | xz decompression of staged images | Apache-2.0 |
| ruzstd | zstd decompression of staged images | MIT |
| bzip2 | bzip2 decompression of staged images (pure-Rust libbz2-rs-sys) | MIT OR Apache-2.0; libbz2-rs-sys is bzip2-1.0.6 |
| zip | Single-entry .zip image extraction (deflate only) | MIT |

Iced's `lazy` feature (for size-aware widgets) adds ouroboros/aliasable, MIT OR Apache-2.0 and MIT. All decompressors are pure Rust and run unprivileged in the desktop user's session; none runs in the root helper.

If additional dependencies are present in the workspace manifest, use the generated `docs/dependency-licenses.json` as the resolved source of truth. Iced pulls windowing, font, renderer, executor and platform crates; their licenses remain independent. No crates are selected from a mutable Git branch. `Cargo.lock` and `--locked` are mandatory for release builds.

## Runtime tools and planned tools

| Component | Status/purpose | License/distribution note |
|---|---|---|
| Linux sysfs/procfs, util-linux lsblk/blkid | Live topology/mount/filesystem observation | util-linux is mixed-license; distributed by OS, no utility binaries bundled |
| kdialog / zenity | Optional native image chooser, fixed `/usr/bin/kdialog` or `/usr/bin/zenity`; unprivileged, bounded child lifetime/output, no shell | OS-provided utilities; no binary linked or bundled. Installed Arch package metadata reports kdialog LGPL-2.0-or-later and Zenity LGPL-2.1-or-later |
| Python 3 | Source-checkout installer, release path-remapping wrapper and QA tests | Build dependency; no Python runtime needed by Rust CLI/GUI |
| Polkit/pkexec | Installed fixed helper authorization | System dependency; policy itself is project source |
| wimlib-imagex | Required for Windows preparation/WIM verification/splitting; ordinary-user PATH discovery | GPL-3.0-or-later program; wimlib library has LGPL licensing distinctions. No binary linked/bundled |
| 7z / 7zz | Bounded ISO9660/UDF listing and stdout extraction, unprivileged | OS-provided 7-Zip; no executable bundled |
| dosfstools / mtools | Private FAT32 formatting, consistency checking, copy and file read-back | OS-provided GPL tools; no executable linked/bundled |
| ntfs-3g tools | Private NTFS formatting and limited ntfsfix diagnostics | OS-provided GPL tools; no NTFS driver linked or installed on the media |
| exfatprogs | Private exFAT formatting (`mkfs.exfat`) and read-only check (`fsck.exfat -n`) | OS-provided GPL-2.0 tools; no executable bundled |
| e2fsprogs | Private ext4 formatting (`mkfs.ext4`) and read-only check (`e2fsck -n`) | OS-provided GPL-2.0 tools; no executable bundled |
| sfdisk | Independent fixture/partition validation | Runtime partition tables are constructed and verified by Rust; no physical sfdisk invocation |
| UDisks2 (`udisksctl`) | Unmount before planning and power off after a verified write, run as the desktop user under UDisks' own Polkit rules; never from the root helper | OS-provided LGPL/GPL service; fixed root-owned `/usr/bin/udisksctl`, no library linked |
| GRUB/Syslinux | Potential per-strategy bootloader installation | GPL obligations, exact version/modules and firmware test matrix required |
| UEFI:NTFS plus filesystem driver | Investigated only; not included | UEFI:NTFS GPL-2.0-or-later; driver license and signed-artifact identity checked separately |
| QEMU/OVMF | Optional disposable-image virtual boot tests | OS packages, no firmware binaries bundled; keep fresh per-test NVRAM |

Reference source license details and exact revisions are in `reverse-engineering/reference-lock.json` and `source-architecture.md`. WoeUSB includes mixed documentation/artwork licenses; ForgeUSB reuses no WoeUSB artwork. ForgeUSB's SVG icon is an original code-native asset under the project license.

## Release license gate

Run `cargo metadata --locked --filter-platform x86_64-unknown-linux-gnu --format-version 1` and extract every resolved package's name/version/license/source to `docs/dependency-licenses.json`. Review every missing/custom expression against the downloaded crate's license text. Include project `LICENSE`, `THIRD_PARTY_NOTICES.md`, required transitive notices/source offers, and reproducible source corresponding to distributed binaries. Keep development-only `references/`, `target/`, images and local evidence artifacts out of source archives. If distributing modified GPL-covered bootloader binaries later, provide the exact corresponding source/build configuration as required, not just a link to upstream master.
