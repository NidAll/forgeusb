# Rufus shipped-executable investigation

## Identity and provenance

Official publisher repository: https://github.com/pbatard/rufus. Release v4.15, published 2026-06-30, source `6d8fbf98305ff37eb531c45cbd6ff44563c53917`. The official GitHub release API and HTTPS download were used. Source master initially resolved to `3e696ab19010d7459c31537684522c98ba6a84a9`; analysis then checked out the v4.15 tag to match the artifact.

| Artifact | SHA-256 | Acquisition/derivation |
|---|---|---|
| `rufus-4.15.exe` (1,989,992 bytes) | `84c8a437f8af89257524478489e5c85f1edf25f761d299e2bcde46ac0afbe106` | https://github.com/pbatard/rufus/releases/download/v4.15/rufus-4.15.exe; matches GitHub release asset digest exactly |
| UPX 5.2.1 amd64 Linux archive | `402162aad30af47e60dbd767fb2e64ca394ace9727ba1f40283641f1d1b91657` | Official https://github.com/upx/upx/releases/download/v5.2.1/upx-5.2.1-amd64_linux.tar.xz; matches publisher asset metadata |
| `rufus-4.15-unpacked.exe` (5,422,952 bytes) | `5cfec8255ad5a11883fafdf54ee064e44d218de6ab80b94375bc5b57b34c5325` | `upx -d rufus-4.15.exe -o rufus-4.15-unpacked.exe`; original preserved |

GitHub publisher digest matching establishes integrity relative to that HTTPS release channel, not an independent Authenticode certificate-chain verification. Authenticode trust was not checked. UPX ran only as an unpacking tool; Rufus was never executed. Research artifacts/tools stay under ignored `references/`, outside distributed application/source archives.

REA `binary_session` confirmed server 6.5.0, Ghidra 12.1.4 at the supplied `<home>/AI/REA/ghidra/`, its native Linux x86_64 decompiler, and Java 21.0.12.1. No Ghidra/Codex installation or configuration was modified. Both artifacts were opened explicitly with `provider_id: ghidra`; each session was closed after evidence export.

## Packed versus unpacked coverage

Packed overview evidence `ev_c527fa4ce42280411e31352060d22287c1b4fd3657eca6198f4c6d2e0382b65c` observed PE x86_64 image base `0x140000000`, Headers, UPX0, UPX1, .rsrc and Ghidra's `tdb` synthetic space, six procedures and 36 defined strings. The entry at `0x1405a46f0` is an unpacker stub; decompilation `ev_96b1ddaf1924fa8f9350a892bb51ad5e9ea9175bcd5826a98f0aaaeb3ce28859` contains buffer decoding/import resolution/VirtualProtect and bad-instruction/control-flow warnings. It is not evidence of Rufus's storage implementation. `tdb` is a provider-generated space, not a PE file section.

The unpacked overview `ev_937ed7de15c292ec058ef516205dbe5030bdfa8119cddff3ac66f258369ca899` reports 3,227 procedures and 5,371 defined strings. Section ranges use exclusive ends:

| Section | Start | End |
|---|---|---|
| .text | 0x140001000 | 0x14012d800 |
| .rdata | 0x14012e000 | 0x14019e000 |
| .data | 0x14019e000 | 0x140218a70 |
| .pdata | 0x140219000 | 0x140225800 |
| .fptable | 0x140226000 | 0x140226200 |
| .rsrc | 0x140227000 | 0x14059f000 |
| .reloc | 0x14059f000 | 0x1405a0e00 |

Unpacking changes the artifact and loader layout; all following addresses refer exclusively to the derived unpacked hash. Recovered function names are analyst mappings based on strings, assertions, control flow and source comparison, not original symbol/debug information.

## Traceable mechanism mapping

| Mechanism and observed behavior | Pinned source | Static binary evidence | Windows dependency | ForgeUSB/Linux choice and risk |
|---|---|---|---|---|
| Physical names subtract the Rufus drive-index base, bounds-check 0x80..0xc0, format a PhysicalDrive path and duplicate it | `src/drive.c:219` GetPhysicalName; GetPhysicalHandle at 235 | String `0x140150f18` xref `0x140008ab4`, procedure `0x140008a70`; pseudocode `ev_984fb1a72a22f06e6ddb367c95ca15a6a08a3a313773d8ea5a8a9b8c862681af`; assembly `ev_d63937dc9da7239a61317ee1e48a305a13e0404d2668fa60c8809dcd7415009a`. Assertion identifies drive.c line225 and source matches bounds/path control flow | Win32 PhysicalDrive namespace/CreateFile/locking | Sysfs topology, major/minor, true serial/WWID, sector/capacity and retained O_EXCL descriptor; never translate arbitrary numeric drive indexes |
| Raw writing checks sector size, seeks, streams reads/writes, checks short writes, retries with rewind and delays; includes compressed/zeroing branches | `src/format.c:1151` WriteDrive through 1422 | Error-string `0x140153a58` xrefs `0x140010e6f` and `0x1400117b9` resolve to `0x140010a40`; `ev_07feda3167f4aa6fcb0e03ff04f7fbe2a3285ea26706fc523b7c30eb2ad22a40` decompiles ReadFile/WriteFile, 5000 ms retry sleep, SetFilePointerEx, sector checks and short-write paths | HANDLE, asynchronous ReadFile, WriteFile, SetFilePointerEx, Sleep | Bounded Rust read_exact/write_all, cancellation between chunks, no blind resume/retry after device disappearance, sync_all and complete read-back; failures report partial target |
| Image scanning forces direct-write for some ISOHybrid images rather than applying generic extraction | `src/rufus.c:1290` ImageScanThread, diagnostic at 1436 | Diagnostic `0x14015d550` xref `0x1400319e8` resolves to `0x140031220`; `ev_a3b9ecd885394fc12d90b352a7a307303d7fd0a9b7d9f9613cb03f0d5d80f7b4` shows distro-pattern checks and state change followed by ExitThread | GUI messages, CreateFileMapping/MapViewOfFile, PE checks, ExitThread | Capability selector accepts recognized disk layouts; plain optical installation ISOs receive actionable refusal. No generic extraction fallback |
| Partition creation includes EFI labels, GPT/MBR-specific paths, UEFI:NTFS data and layout IOCTLs | `src/drive.c:2277` CreatePartition; UEFI:NTFS diagnostic at 2503 | String `0x140152878` xref `0x14000d76e` in `0x14000cc20`; `ev_6c8e97c0588f7b1b990fa55a8c608bac9af21a400b646d6a6116e23b503ea4be` shows GPT assertions, EFI System Partition text and DeviceIoControl constants `0x7c058`, `0x7c054`, `0x70140` | Disk layout IOCTLs, Windows partition structures | Checked pure MBR/GPT layout planner exists; raw mode preserves original layout. No partition/format executor or UEFI:NTFS asset is yet shipped |
| USB enumeration uses SetupAPI and device interface/property APIs | `src/dev.c:444` GetDevices | Import inventory `ev_3baa59445b2bc12a0db031c9cd330095b10677a95916303b110b6c696e19a9a7` includes SetupDiGetClassDevsA, EnumDeviceInterfaces, GetDeviceInterfaceDetailA, GetDeviceRegistryPropertyA/W and instance IDs. Import presence is not a decompiled GetDevices body | SETUPAPI.DLL, Windows registry/device interfaces | Sysfs/procfs discovery and periodic refresh; conservative graph traversal protects dm/LVM/md/Btrfs/root/home/boot/swap |
| Flush and threading facilities are present | `src/format.c:1445` FormatThread; thread creation `src/rufus.c:3145` | Same import inventory observes FlushFileBuffers, CreateThread, ReadFile, WriteFile, DeviceIoControl | Win32 worker threads and file handles | Nonroot GUI workers and shared Cancellation/Progress state; narrowly installed Polkit helper; fsync failure prevents completion |
| ISO/UDF extraction, FAT/NTFS/ext formatting, WIM processing, bad-block scan and WUE customization | Source mappings and dependencies in source-architecture.md (`iso.c`, `format*.c`, bundled `wimlib/`, `badblocks.c`, `wue.c`) | Presence/import/string context only; bodies were not individually decompiled in this focused study | libcdio/wimlib ports, formatting APIs, Windows registry/setup customization | Mature system-tool candidates and separate strategy crates. Only raw executor plus unprivileged Windows inspection/splitting primitives implemented |

## Unresolved boundaries

Application initialization is mapped from source WinMain (`rufus.c:3354`), not decompiled CRT initialization. Full USB selection/capacity-check bodies, ISO parser bodies, individual formatting/bootloader installers, WIM routines, bad-block loops and Windows user-experience customization have not all been independently corroborated in the executable. Imported names and diagnostic strings alone do not prove a code path ran. Decompilation is recovered pseudocode, with inferred types/globals and potentially incomplete indirect flows; it is not original source.

Source control-flow mappings above are high-confidence where distinctive diagnostics/assertions and operations agree. General feature behavior remains source-derived elsewhere. There was no Windows runtime observation, no physical USB write, no Authenticode-chain validation, no Secure Boot/revocation test, and no claim that source and executable are reproducibly byte-identical builds. QEMU evidence in ../validation.md concerns ForgeUSB-written fixtures, separately from Rufus static evidence.
