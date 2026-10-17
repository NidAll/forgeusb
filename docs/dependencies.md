# Dependency and license inventory

ForgeUSB source is GPL-3.0-or-later. Rufus/WoeUSB source was studied to understand behavior; application logic is newly written Rust. No Rufus bootloader binaries, ISO contents or Microsoft assets are bundled. The GPL license text is the only upstream file copied into the project license when applicable. This is a conservative project-license choice, not a claim of upstream affiliation.

## Direct Rust dependencies

Actual selected versions belong to `Cargo.lock`; this table lists license families from upstream package metadata. Release builders must export the resolved Cargo metadata/license inventory for every distributed target, including transitive crates, before redistribution. The included inventory covers x86_64-unknown-linux-gnu and development dependencies: 222 packages, of which 210 are registry crates; no license field was missing at generation time. Do not infer complete compliance from this abbreviated table.

| Crate | Purpose | Upstream license expression |
|---|---|---|
| anyhow | Error context and propagation | MIT OR Apache-2.0 |
| serde / serde_json | Stable JSON data contracts | MIT OR Apache-2.0 |
| sha2 | Streaming SHA-256 | MIT OR Apache-2.0 |
| libc | Linux kernel/libc descriptor interface | MIT OR Apache-2.0 |
| iced | Native Rust GUI, tiny-skia backend | MIT |
| tempfile (tests) | Explicitly-owned disposable fixtures | MIT OR Apache-2.0 |

If additional dependencies are present in the workspace manifest, use the generated `docs/dependency-licenses.json` as the resolved source of truth. Iced pulls windowing, font, renderer, executor and platform crates; their licenses remain independent. No crates are selected from a mutable Git branch. `Cargo.lock` and `--locked` are mandatory for release builds.

## Runtime tools and planned tools

| Component | Status/purpose | License/distribution note |
|---|---|---|
| Linux sysfs/procfs, util-linux lsblk/blkid | Live topology/mount/filesystem observation | util-linux is mixed-license; distributed by OS, no utility binaries bundled |
| kdialog / zenity | Optional native image chooser, fixed `/usr/bin/kdialog` or `/usr/bin/zenity`; unprivileged, bounded child lifetime/output, no shell | OS-provided utilities; no binary linked or bundled. Installed Arch package metadata reports kdialog LGPL-2.0-or-later and Zenity LGPL-2.1-or-later |
| Python 3 | Source-checkout installer and staging tests only | OS dependency; no Python runtime needed by Rust CLI/GUI |
| Polkit/pkexec | Installed fixed helper authorization | System dependency; policy itself is project source |
| wimlib-imagex | Optional standalone WIM staging only, fixed `/usr/bin/wimlib-imagex` | GPL-3.0-or-later program; wimlib library has LGPL licensing distinctions. No binary linked/bundled |
| libfdisk/sfdisk, mkfs.fat, mkntfs, mkfs.exfat, mke2fs | Candidate future audited partition/format executors | Not invoked by the pure planner; package licensing assessed at integration |
| UDisks2 | Potential unmount/safe removal integration | Presence is not implementation; optional future D-Bus integration |
| GRUB/Syslinux | Potential per-strategy bootloader installation | GPL obligations, exact version/modules and firmware test matrix required |
| UEFI:NTFS plus filesystem driver | Investigated only; not included | UEFI:NTFS GPL-2.0-or-later; driver license and signed-artifact identity checked separately |
| QEMU/OVMF | Optional disposable-image virtual boot tests | OS packages, no firmware binaries bundled; keep fresh per-test NVRAM |

Reference source license details and exact revisions are in `reverse-engineering/reference-lock.json` and `source-architecture.md`. WoeUSB includes mixed documentation/artwork licenses; ForgeUSB reuses no WoeUSB artwork. ForgeUSB's SVG icon is an original code-native asset under the project license.

## Release license gate

Run `cargo metadata --locked --filter-platform x86_64-unknown-linux-gnu --format-version 1` and extract every resolved package's name/version/license/source to `docs/dependency-licenses.json`. Review every missing/custom expression against the downloaded crate's license text. Include project `LICENSE`, `THIRD_PARTY_NOTICES.md`, required transitive notices/source offers, and reproducible source corresponding to distributed binaries. Keep development-only `references/`, `target/`, images and local evidence artifacts out of source archives. If distributing modified GPL-covered bootloader binaries later, provide the exact corresponding source/build configuration as required, not just a link to upstream master.
