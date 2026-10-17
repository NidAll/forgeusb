# Compatibility and evidence limits

This is an engineering build. Raw-write capability, filesystem construction, generated fixtures, virtual boot and physical boot are different evidence categories. See the [current development report](production-development.md).

| Feature | Current implementation | Compatibility/evidence boundary |
|---|---|---|
| Linux disk/IMG/DD | Raw engine, explicit target confirmation, flush/readback | Preserves source layout; actual distro/firmware boot must be separately tested |
| ISOHybrid | Structural recognition and raw strategy when accepted by inspector | Does not convert the ISO's firmware support; not every optical ISO is hybrid |
| Plain Linux ISO9660 | Inspection; ISOHybrid raw writing when structurally recognized | Generic Linux extraction refused; preserve vendor USB image/ISOHybrid layout |
| Windows ISO9660/UDF | Bounded safe extraction into private snapshot | Requires 7z/7zz and valid installer structure; data/resource integrity is checked |
| Windows 10/11 installation ISO to USB | FAT32 UEFI image preparation, then reviewed shared writer | Synthetic tree/ISO/WIM/OVMF coverage only; genuine Windows Setup remains untested |
| Windows extracted-tree inspection | Library API `inspect_extracted_tree` | Validates bounded metadata, FAT names, WIM magic and EFI PE machine; no complete WIM/signature/boot validation |
| Oversized install.wim | Integrated unprivileged split-WIM/FAT32 preparation and standalone split | Validates complete SWM headers and wimlib resource integrity; small genuine WIM roundtrip, no large real-media download |
| FAT32 EFI layout | Aligned MBR/GPT image construction with read-back | Original EFI fallback test application booted under OVMF in both schemes |
| FAT32 / NTFS formatting | `format-image` builds new regular-file partitioned images; shared writer deploys them | Four MBR/GPT/filesystem combinations verified; NTFS format adds no bootloader |
| exFAT / ext4 | File-limit policy; raw images preserve their embedded filesystems | Creation not implemented |
| UEFI:NTFS | Source strategy investigated, no binaries distributed/installed | Architecture, digest, signatures, revocation and boot tests required |
| UEFI / BIOS | Raw preserves original loaders; Windows preparation targets UEFI | Original BIOS/UEFI fixtures and small Alpine tested; Windows BIOS loader installation unavailable |
| GRUB / Syslinux | Preserved when embedded in raw image | No generic extraction/install executor |
| Secure Boot | Raw bytes preserved; no signing or revocation service | Never guaranteed; depends on source and target firmware db/dbx/SBAT/trust state |
| Linux persistence | Not implemented | Requires distro/version-specific adapter and reboot evidence |
| Compressed images | Recognized/refused where detected | Decompress to a regular image with a trusted utility before inspection |
| Destructive bad-block scan | Not exposed | Must never run as part of an ordinary write |
| Windows To Go / DOS / FFU / VHD save | Not implemented | Separate future strategies |
| Physical disks | Safety-gated engine; no test on real storage authorized | Physical destructive testing requires exact-device authorization |
| CachyOS | Preserve ISOHybrid layout via raw path, consistent with vendor flashing guidance | No CachyOS ISO downloaded or boot-tested; physical firmware variability remains untested |
| QEMU boot | Five synthetic BIOS/UEFI paths; Alpine virt 3.24.2 login under SeaBIOS and OVMF | See validation.md; synthetic Windows-shaped EFI handoff does not establish Windows Setup/physical/Secure Boot acceptance |

Windows staging requires a quiescent caller-owned extracted tree and is deliberately unprivileged. A split target of 3800 MiB is a policy margin, not a guaranteed maximum: WIM resources cannot always be divided. The stage rejects any part that exceeds 4,294,967,295 bytes. [wimlib split manual](https://wimlib.net/man1/wimsplit.html)

Remaining acceptance gates include genuine Windows Setup, approved physical media/firmware, Secure Boot enforcement and additional distribution/package environments. No support row should be promoted from constructed to installed based solely on synthetic boot code or a planner.
