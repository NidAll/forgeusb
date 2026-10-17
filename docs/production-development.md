# Production development report

2026-10-17. This development pass advances the original raw-image engineering
build. It does not certify complete Rufus parity or production hardware coverage.
[The task ledger](development-plan.md) tracks implementation and final gates.

## Delivered functionality

- Windows ISO9660/UDF or extracted-tree preparation into verified MBR/GPT FAT32
  UEFI disk images, followed by the existing guarded plan/write flow. Installer
  files are snapshotted privately, WIM resources verified, oversized install.wim
  split, filenames/architecture checked, and every FAT file hashed on read-back.
  Outputs are published atomically without clobbering an existing path.
- Real FAT32/NTFS formatting into new partitioned ordinary images. NTFS receives
  a data partition type, with verified boot geometry and limited ntfsfix
  diagnostics. It is a blank data filesystem, without NTFS bootloader deployment.
- Native GUI Windows MBR/GPT options, private cache preparation, progress/cancel,
  and exact confirmation before USB access. Cancellation wins over a queued
  preparation result, and disconnect/replacement during preparation revokes the
  selected device. No process other than the narrow writer helper needs root.
- Stronger standalone GPT metadata/backup/CRC/overlap/GUID validation and
  regular-file/FIFO admission, while preserving real Alpine ISOHybrid geometry.
  Recognized stale target-end GPT headers and safely bounded arrays are removed
  beyond the source range and separately read back after a physical write.
- Portable dependency discovery, original BIOS/EFI boot fixtures, automated media
  and package integration suites, more CI coverage and runtime dependency metadata.
- Release packaging now remaps personal source/cache prefixes before compilation
  and inspects both ELF payloads and package metadata. Stripping alone does not
  remove dependency panic/source paths.
- A new exact helper protocol argument prevents a new desktop/CLI from silently
  using an older installed helper without the current parser/tail-cleanup safety
  policy. Both directions of mixed-version invocation fail before request I/O.

## Evidence

The final gate results and limits are recorded in [validation.md](validation.md)
and [structured development validation](development-validation.json). Tests use new regular files,
private process groups and no physical targets/mounts/loops. QEMU has no network,
read-only media and fresh per-test firmware variables.

Five synthetic boot paths exercise the actual CLI engine: BIOS disk, Windows-
shaped MBR/FAT32 UEFI, Windows-shaped optical-ISO→GPT/FAT32 UEFI, and appended-ESP
GPT ISOHybrid under BIOS and UEFI. The original EFI fixture reports its own serial
marker and exits QEMU. Its synthetic BCD/bootmgr placeholders and tiny valid WIMs
do not contain Windows. Four separate MBR/GPT × FAT32/NTFS image combinations
exercise formatting, partition metadata and full raw write/read-back.

The retained official small Alpine image was copied by the new writer and again
reached login under SeaBIOS and OVMF. Its recorded hash stayed unchanged. No new
large ISO was downloaded. The old acquisition hash is not a newly established
publisher signature or current release authenticity check.

## Release blockers and scope limits

1. Genuine Windows 10/11 Setup, installation completion and large real-world
   installer content remain unvalidated under the user's download constraint.
   Small multipart WIM tests exercise the splitting algorithm; an actual >4 GiB
   installation WIM has not been downloaded or installed.
2. Windows BIOS boot code, NTFS installer construction and UEFI:NTFS signed assets
   are not implemented. Oversized solid ESD and non-install.wim files are refused.
3. Secure Boot enforcement/signatures/revocation are not tested. Preserving a
   loader does not validate firmware db/dbx/SBAT acceptance. The upstream
   [UEFI:NTFS chain](https://github.com/pbatard/uefi-ntfs) is a separate mechanism.
4. Physical USB writes, installed-helper authorization/exclusivity, fault/unplug
   recovery and problematic physical firmware require exact-device authorization.
   Raw GPT remains source-sized; no backup-GPT relocation or kernel partition-node
   refresh is performed. Safely remove/reinsert the device after completion.
5. Linux uses the vendor's disk/ISOHybrid layout. The official
   [CachyOS preparation guide](https://wiki.cachyos.org/installation/installation_prepare/)
   documents raw flashing. No CachyOS image was downloaded, and no universal
   firmware compatibility is claimed. Generic optical-Linux conversion,
   persistence, compressed streaming and UDisks removal remain future adapters.
6. GTK chooser/screen-reader acceptance, broad hardware QA and clean distribution
   build environments remain open. Debian/RPM recipes need a populated Cargo
   cache or vendored sources and have not been verified as native packages.
   The existing drm-fourcc upstream license-notice omission also remains a
   redistribution review item; local package assembly does not resolve it.

These boundaries remain visible in the application modes and documentation.
They must not be weakened to make a compatibility test appear successful.
