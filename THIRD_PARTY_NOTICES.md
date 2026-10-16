# Third-party notices

ForgeUSB source, original Rust implementation, test boot sector and SVG icon are licensed under GPL-3.0-or-later. See LICENSE. This project is independent of and is not endorsed by Rufus, WoeUSB, Microsoft, Alpine Linux or their maintainers.

Research references (not compiled into ForgeUSB):

- Rufus by Pete Batard and contributors, GPL-3.0-or-later notices in its source; exact v4.15 revision in docs/reverse-engineering/reference-lock.json. Its component directories have additional licenses. No Rufus C, embedded bootloader, executable, font, translation or icon is redistributed as ForgeUSB application code/assets.
- WoeUSB, GPL-3.0-or-later code with separately licensed documentation/artwork; WoeUSB-ng, GPL-3.0-or-later. No code/artwork is included in the product.
- UEFI:NTFS, GPL-2.0-or-later. No bootloader/driver binary is currently distributed or installed by ForgeUSB.
- UPX, used locally to statically unpack a research copy, is not a runtime dependency and is not distributed in the package. Packed and unpacked hashes and derivation are recorded in the binary report.
- Alpine Linux test ISO is acquired separately from its official mirror and excluded from distribution. Its packages have their own licenses; no ISO is bundled.

Rust dependencies retain their individual licenses. The Linux x86_64 resolved package inventory is docs/dependency-licenses.json, including dev dependencies. Included license and notice texts are collected in docs/third-party-licenses.txt. Cargo.lock records source checksums. drm-fourcc 2.2.0 declares MIT and credits Daniel Franklin and contributors, but its published crate and pinned upstream repository omit a license file; the collection records that omission explicitly. Review the missing upstream notice before public binary redistribution. Some crates are dual/multiple licensed: the inventory retains their full declared expressions. The Rust standard library/compiler runtime also carries MIT/Apache-2.0 notices from the installed toolchain; consult the toolchain distribution when redistributing binaries. System libraries linked dynamically are OS dependencies, not bundled here.

System tools (Polkit, wimlib, QEMU, OVMF, filesystem utilities) are supplied by the operating system. Linking or distributing additional bootloaders, drivers, libraries, or utilities requires its own license review and corresponding-source compliance. Preserve these notices and supply the exact corresponding ForgeUSB source for GPL binary redistribution. No universal Secure Boot compatibility is asserted.
