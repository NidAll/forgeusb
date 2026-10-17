# Windows UEFI installation media

`prepare-windows` implements ISO/tree → verified FAT32 disk image → reviewed raw
USB write. It runs as an ordinary user. The physical writer and root helper keep
their existing device guards; extraction, formatting and wimlib never run as root
or receive block-device paths. The GUI uses this builder before displaying
the destructive operation review.

```sh
forgeusb prepare-windows /path/windows.iso --output /path/NEW-windows.img --scheme mbr --json
forgeusb plan /path/NEW-windows.img --device /dev/sdX --json > plan.json
forgeusb write --plan plan.json
```

The source may also be an extracted directory. `--scheme gpt` creates a GPT EFI
System Partition; the default MBR layout uses type 0xEF. Both are UEFI strategies.
`--capacity-mib` sets an explicit image capacity; automatic sizing includes a
conservative filesystem margin. Image capacity is bounded at 128 GiB and files at
100,000 entries. A source-sized GPT is not relocated when written to a larger USB;
firmware accepting only a backup header at the physical device end remains a
hardware acceptance boundary. MBR avoids that GPT geometry issue.

Dependencies: **7z or 7zz** for ISO9660/UDF input; **dosfstools** (`mkfs.fat`,
`fsck.fat`); **mtools** (`mcopy`, `mmd`); **wimlib-imagex** for WIM integrity and
splitting. Tools are resolved from the ordinary user's PATH and invoked with
explicit arguments, clean environments, bounded output, memory and deadlines.
No shell, mount, loop device, online download or third-party bootloader is used.
Use `check-dependencies` to inspect availability. Missing tools fail with guidance.

Preparation proceeds as follows:

1. Retain the source descriptor or snapshot the tree through retained directory
   descriptors. Reject links/special files, traversal, ambiguous FAT names,
   case collisions, excessive nesting, entries and bytes.
2. For ISO9660/UDF, inspect a bounded archive listing and extract each selected
   file to stdout. ForgeUSB creates validated destination files itself; the
   archive tool cannot choose a host output pathname. Hash and recheck the input.
3. Validate installer structure and architecture-specific EFI fallback PE machine,
   require EFI Microsoft BCD, and verify WIM resources with wimlib. This does not
   validate PE signatures or BCD's operating-system configuration.
4. Split an oversized `sources/install.wim` at 3800 MiB, validate consecutive SWM
   parts/GUIDs and reject any indivisible part above FAT32's 4,294,967,295-byte
   limit. Existing SWM sets must be complete. Oversized ESD and other files fail
   rather than silently producing incomplete media.
5. Construct a private FAT32 filesystem and aligned MBR/GPT layout. Read each
   copied file back through mtools and compare SHA-256. Check filesystem geometry,
   run `fsck.fat -n`, verify both GPT copies/CRCs when applicable, compare the
   assembled partition and flush the whole image.
6. Publish the output atomically without replacing an existing file. Failures
   before publication remove private construction files. Standalone `split-wim`
   retains its new staging directory after failure for inspection.

Scratch data lives beside the requested output; reserve disk space for the
snapshot, partition and assembled disk image. The GUI uses
`$XDG_CACHE_HOME/forgeusb` or `$HOME/.cache/forgeusb` and owns the prepared image
until source/mode changes or exit. Set XDG_CACHE_HOME to a suitable disk-backed
location when the normal cache is small. Preparation can be cancelled before
any target confirmation or USB write.

The layout follows Microsoft's documented FAT32/split-WIM deployment method:
[Install Windows from USB](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/install-windows-from-a-usb-flash-drive?view=windows-11)
and [split a Windows image](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/split-a-windows-image--wim--file-to-span-across-multiple-dvds?view=windows-11).
The [wimlib split documentation](https://wimlib.net/man1/wimsplit.html) explains why
a requested split size cannot divide an individual resource.

## Verification boundary

Automated tests use original EFI test code, synthetic BCD/bootmgr placeholders and
real tiny WIM containers holding ordinary test files. OVMF executes that original
EFI application from both prepared MBR/GPT images after actual ForgeUSB
plan/write/read-back. Multipart WIM tests capture, split, verify, apply and compare
original file hashes. These exercise the implementation without Microsoft files.

They **do not validate genuine Windows 10/11 Setup, installation completion,
physical USB firmware or Secure Boot**. Original Microsoft loaders are copied
unchanged; no unsigned NTFS driver is introduced. Signature, db/dbx/SBAT and
revocation acceptance depend on the original loader and target firmware.
Windows BIOS boot code and NTFS/UEFI:NTFS media construction are not implemented.
[UEFI:NTFS upstream](https://github.com/pbatard/uefi-ntfs) explains its separate
signed loader/driver chain; blank NTFS formatting is not equivalent to that chain.

`inspect-windows-tree` remains metadata/header inspection.
`split-wim SOURCE --output NEW_DIRECTORY --part-size-mib 3800` is also available
independently. Sizes 1–3800 MiB permit small multipart regression fixtures; media
preparation keeps the 3800 MiB production policy.
