# ForgeUSB media construction

This library prepares a **new disk-image file** without elevation, mounts, loop
devices, or device writes. The CLI exposes it through `prepare-windows` and
`format-image`. Physical writing remains a separate reviewed raw-image operation.

`build_windows_image` accepts an extracted Windows tree or an ISO9660/UDF image.
It preserves the original EFI fallback loader and creates one aligned FAT32 EFI
partition using MBR or GPT. Both `sources/boot.wim` and the installation WIM/ESD or
complete SWM set must pass `wimlib-imagex verify`; a nonempty EFI Microsoft BCD is
required. Large `install.wim` files are split into FAT32-sized SWM parts. Other
oversized files, including solid ESDs, fail with an explicit explanation.

Preparation copies inputs through retained directory/file descriptors into a
private snapshot alongside the requested output. It rejects unsafe filenames,
symlinks, special files, traversal, case collisions, incomplete SWM sets, and
input changes. ISO extraction tools stream each selected file to stdout; they
never choose host output paths. Tools have cancellation, deadlines, bounded
output, process-group cleanup and an address-space limit.

Publication follows filesystem checks, every staged file's SHA-256 read back
through FAT32, redundant GPT metadata/CRC verification when applicable, and a
complete assembled-partition read-back hash. The final image is flushed and
published atomically without overwriting an existing path. Failed or cancelled
preparation before publication removes private staging and leaves no final output.

`format_image` creates a blank FAT32 EFI partition or NTFS data partition with
verified boot-sector geometry, filesystem diagnostics, partition metadata and
assembled-partition hashes. NTFS diagnostics use `ntfsfix -n`, whose checks are
more limited than Windows `chkdsk`.

Tools are resolved through absolute entries in `PATH`: `mkfs.fat`, `mcopy`, `mmd`,
`fsck.fat`, and `wimlib-imagex`; ISO input additionally needs `7z` or `7zz`.
Blank NTFS formatting needs `mkfs.ntfs`/`mkntfs` and `ntfsfix`. No dependencies are
automatically installed. Input/output safety budgets are 128 GiB, 100,000 entries,
64 directory levels and a maximum two-hour operation deadline. Staging can need
substantial free space on the output filesystem.

## Evidence boundaries

The tests use synthetic Windows-shaped trees, genuine tiny WIM containers,
minimal original EFI applications, and small ISO/UDF fixtures. These validate
construction and integrity, and the QEMU suite separately validates the original
EFI fixture. They do **not** prove genuine Windows 10/11 installation, BIOS boot
of extracted Windows media, physical USB boot, arbitrary firmware compatibility,
or Secure Boot acceptance. Existing raw Linux ISOHybrid and disk-image paths
remain separate. No BIOS bootstrap or UEFI NTFS driver is installed here.

The preserved Microsoft loader's certificate, firmware trust database and
revocation state determine Secure Boot acceptance. Storage integrity does not
establish publisher authenticity or signature trust.

## Primary references

- [Microsoft: splitting Windows images for FAT32 USB media](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/split-a-windows-image--wim--file-to-span-across-multiple-dvds?view=windows-11)
- [Microsoft: WinPE media and boot-manager revocations](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/winpe-create-usb-bootable-drive?view=windows-11)
- [wimlib split command](https://www.wimlib.net/man1/wimsplit.html)
- [GNU mtools manual](https://www.gnu.org/software/mtools/manual/mtools.html)
- [dosfstools formatter implementation](https://github.com/dosfstools/dosfstools/blob/master/src/mkfs.fat.c)
- [GNU xorriso manual](https://www.gnu.org/software/xorriso/man_1_xorriso.html)

The production extraction path uses 7-Zip for UDF as well as ISO9660; xorriso is
used to generate small ISO9660 test fixtures. The WIM splitter rejects any part
that still exceeds FAT32's limit, since a single compressed WIM resource cannot
always be divided to satisfy the requested split size.

Media unit tests print an explicit skip when fixture tools are absent on a
minimal developer machine. Set `FORGEUSB_REQUIRE_MEDIA_TOOLS=1` to require them,
as CI does. Full fixture coverage additionally requires `xorriso` and
`genisoimage` to create tiny ISO9660 and UDF inputs; no real ISO is downloaded.
All fixture subprocesses use the same bounded command runners as production.
