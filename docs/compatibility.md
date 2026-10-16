# Compatibility and evidence limits

This is an initial engineering build. Raw-write capability, filesystem policy, generated fixtures, virtual boot and physical boot are different evidence categories.

| Feature | Current implementation | Compatibility/evidence boundary |
|---|---|---|
| Linux disk/IMG/DD | Raw engine, explicit target confirmation, flush/readback | Preserves source layout; actual distro/firmware boot must be separately tested |
| ISOHybrid | Structural recognition and raw strategy when accepted by inspector | Does not convert the ISO's firmware support; not every optical ISO is hybrid |
| Plain ISO9660 | Inspection | Generic extraction refused; obtain vendor USB image |
| UDF Windows ISO | Limited inspection/rejection depending on image inspector | Full safe ISO/UDF extraction is not implemented |
| Windows 10/11 installation ISO to USB | Not implemented | Raw-writing ordinary Windows ISOs is not a supported installation strategy |
| Windows extracted-tree inspection | Library API `inspect_extracted_tree` | Validates bounded metadata, FAT names, WIM magic and EFI PE machine; no complete WIM/signature/boot validation |
| Oversized install.wim | Optional standalone unprivileged `split_wim` stage | Requires installed wimlib; validates each SWM <= FAT32 max; not a media creation executor |
| FAT32 EFI layout | Pure checked MBR/GPT planner | No partition creation, formatting, or compatibility claim |
| FAT32 / NTFS / exFAT / ext4 | File-limit policy; raw images preserve their embedded filesystems | Filesystem creation is not implemented |
| UEFI:NTFS | Source strategy investigated, no binaries distributed/installed | Architecture, digest, signatures, revocation and boot tests required |
| UEFI / BIOS | Raw mode preserves original image bootloader | Firmware mode override and bootloader installation are unsupported |
| GRUB / Syslinux | Preserved when embedded in raw image | No generic extraction/install executor |
| Secure Boot | Raw bytes preserved; no signing or revocation service | Never guaranteed; depends on source and target firmware db/dbx/SBAT/trust state |
| Linux persistence | Not implemented | Requires distro/version-specific adapter and reboot evidence |
| Compressed images | Recognized/refused where detected | Decompress to a regular image with a trusted utility before inspection |
| Destructive bad-block scan | Not exposed | Must never run as part of an ordinary write |
| Windows To Go / DOS / FFU / VHD save | Not implemented | Separate future strategies |
| Physical disks | Safety-gated engine; no test on real storage authorized | Physical destructive testing requires exact-device authorization |
| QEMU boot | Synthetic BIOS sector boot; Alpine virt 3.24.2 Linux startup under SeaBIOS and login under OVMF | See validation.md for exact image/hash/runtime console option; no Windows/physical/Secure Boot acceptance |

Windows staging requires a quiescent caller-owned extracted tree and is deliberately unprivileged. A split target of 3800 MiB is a policy margin, not a guaranteed maximum: WIM resources cannot always be divided. The stage rejects any part that exceeds 4,294,967,295 bytes. [wimlib split manual](https://wimlib.net/man1/wimsplit.html)

The most practical next compatibility milestone is an audited extraction executor with an authorized Windows ISO and FAT32 split-WIM fixtures, followed by OVMF startup testing. No support row should be promoted from planned to tested based solely on the existence of a planner or subprocess wrapper.
