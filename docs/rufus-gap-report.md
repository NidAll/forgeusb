# ForgeUSB: current state and gap to Rufus

Assessment date: 2026-11-05. Baseline: `main` at `6175c97` (ForgeUSB 0.1.0). Comparison target: Rufus 4.x, specifically the v4.15 release this project already studied under `docs/reverse-engineering/`. Nothing was implemented for this report. I only read code, ran the unit tests and ran read-only CLI commands.

---

## 1. Executive summary

ForgeUSB is a **safety-first vertical slice** of Rufus for Linux, and the slice is solid. It covers three paths:

1. **DD/raw writing** of disk images and ISOHybrid ISOs, with full read-back verification.
2. **Windows UEFI installer media.** It extracts the ISO, builds a FAT32 filesystem (MBR or GPT), splits `install.wim` when needed and verifies every file.
3. **Blank FAT32/NTFS image formatting** (CLI only).

The engineering quality is high:

- The privilege design is unusually careful: the GUI runs unprivileged, and the only root process is a fixed Polkit helper that re-checks everything itself.
- Device admission fails closed when anything is uncertain.
- Every boundary of what has and has not been validated is documented.

The gap to Rufus is still large. Measured by Rufus's visible feature surface, ForgeUSB covers about **25–30 %** of what a Rufus user sees in the main window and its advanced options. The biggest missing pieces, in order of user impact:

1. **No automatic unmount or eject.** The user must unmount in the file manager, then refresh.
2. **Windows media is UEFI + FAT32 only.** Rufus's default for large Windows ISOs (NTFS + UEFI:NTFS) is missing, legacy BIOS boot is missing, and so are all the Windows User Experience tweaks (TPM/Secure Boot/RAM bypass, local account and so on).
3. **No "ISO mode" for Linux.** Non-hybrid ISOs are refused, so persistence is also impossible.
4. **Prepared media is only as large as its payload.** Windows and format modes build an image file and then raw-write it. The USB partition is therefore only as large as the payload, the rest of the stick stays unused, and the build needs about 2–3× the ISO size in free disk space.
5. **No compressed or virtual-disk images** (`.xz`/`.gz`/`.zst`/`.zip`, VHD/VHDX/FFU).
6. **Missing format options**: exFAT, ext4, FAT16, UDF, editable label, cluster size, a "non-bootable" mode in the GUI.
7. **Missing UX polish**: speed/ETA, drag-and-drop, saved settings, localization, readable CLI output.

Two problems need attention before any new feature:

- **CI on `main` is red.** The latest run (`39058713270`, 2026-10-17) fails because `.github/workflows/ci.yml` calls `tests/build-flags.py`, and that file was never committed.
- **Nothing has been validated on real hardware or with a genuine Windows ISO.** Every promotion from "constructed" to "supported" is blocked on that.

---

## 2. What exists today

### 2.1 Codebase shape

| Unit | LoC | Responsibility |
|---|---:|---|
| `apps/forgeusb-gui` | 2 122 | Iced 0.13 desktop app (tiny-skia renderer), Rufus-style single column, modal typed confirmation, log, dark/light |
| `apps/forgeusb-cli` | 452 | `list`, `inspect`, `plan`, `write`, `verify`, `check-dependencies`, `inspect-windows-tree`, `split-wim`, `prepare-windows`, `format-image` |
| `crates/forgeusb-device` | 1 178 | sysfs/mountinfo/swap topology graph, USB ancestry, stable identity (serial/WWID), admission rules, exclusive open, polling monitor |
| `crates/forgeusb-media` | 1 641 | Windows ISO/tree → FAT32 image builder, ISO extraction via 7z to stdout, private snapshots, sandboxed tool runner, `format_image` |
| `crates/forgeusb-linux` | 757 | Shared engine: plan → revalidate → stream write → stale-GPT cleanup → fsync → fadvise → read-back verify |
| `crates/forgeusb-windows` | 727 | Installer tree validation (FAT names, case collisions, EFI PE machine, WIM magic), wimlib split |
| `crates/forgeusb-image` | 661 | Bounded ISO9660/UDF/El Torito/MBR/GPT inspection, SHA-256, compression magic detection |
| `crates/forgeusb-partition` | 398 | MBR/GPT layout planner + table writer/verifier for ordinary files |
| `crates/forgeusb-privileged` | 172 | `/usr/lib/forgeusb/forgeusb-helper`, protocol v2, revalidates the plan as root |
| `crates/forgeusb-boot` | 125 | Strategy selection (raw vs Windows vs refuse) |
| `crates/forgeusb-verify` | 96 | Byte-for-byte compare + digest |
| `crates/forgeusb-core` | 74 | Cancellation token, progress stages, capacity check |
| `crates/forgeusb-filesystem` | 71 | FAT32 file-size limit and FAT/Windows name policy |
| Python tests + packaging | ~1 900 | File/media/boot (QEMU + OVMF/SeaBIOS) integration, GUI smoke under Xvfb, installer, source package |

The repo is about 10.1 k lines (Rufus is roughly 100 k lines of C excluding vendored libraries). It has 10 commits, developed 2026-10-16 → 10-17.

### 2.2 Operation flows

```
Raw:      inspect(SHA-256) → plan(identity snapshot + confirmation text) → pkexec helper
          → revalidate source fd + device → O_EXCL write 1 MiB chunks → clear stale target-end GPT
          → fsync → POSIX_FADV_DONTNEED → full read-back compare → revalidate device

Windows:  ISO/tree → private snapshot (7z → stdout, ForgeUSB creates files) → validate tree
          → wimlib verify / split install.wim @3800 MiB → mkfs.fat + mtools into NEW image file
          → per-file SHA-256 read-back, fsck.fat -n, GPT CRCs → atomic publish
          → then the Raw flow above

Format:   NEW image file (capacity given) → MBR/GPT → mkfs.fat / mkfs.ntfs → checks → atomic publish
          → then the Raw flow above
```

Design invariant: **filesystem tools never touch a block device; only the raw writer does.** This is the root cause of several gaps below (§4.1), and it is also the reason the security model is clean.

### 2.3 Health check (run 2026-11-05)

| Check | Result |
|---|---|
| `cargo fmt --check` | ✅ clean |
| `cargo test --workspace --locked` | ✅ **75 passed**, 0 failed (one media test takes ~74 s) |
| GitHub Actions on `main` | ❌ **failing**: `tests/build-flags.py: No such file or directory` |
| Previous 4 CI runs | ✅ green |
| `forgeusb check-dependencies` on this host | All tools present; installed helper found at `/usr/lib/forgeusb/forgeusb-helper` |
| `forgeusb list` (no `--json`) | Works, but prints a Rust `Debug` dump (`Device { path: …, Identity { … } }`), not human-readable output |
| Physical USB write | Never performed (deliberately) |
| Genuine Windows 10/11 Setup | Never booted |

### 2.4 Strengths worth keeping

These go beyond Rufus and should not be traded away for parity:

- **Mandatory full read-back verification** of every written byte. Rufus does not verify after writing.
- **Plan/confirm/revalidate model.** The typed confirmation is bound to the source hash and the device identity, and a stale plan is rejected.
- **Retained source descriptor across elevation.** The root helper reads the file through the user's own fd, which prevents TOCTOU and path-swap attacks.
- **Fail-closed admission.** Internal disks, mounted storage, swap, device-mapper/RAID holders, duplicate identities and uncertain mount namespaces are all refused.
- **Clean subprocess sandboxing.** Absolute tool paths, cleared environment, bounded output, deadlines, no shell, and no tool ever chooses an output path.
- **No network access at write time.** Rufus downloads Syslinux/GRUB/UEFI:NTFS pieces on demand. ForgeUSB's refusal to do so is a deliberate security win.
- **Honest documentation** of evidence categories (synthetic vs virtual vs physical).

---

## 3. Feature-by-feature comparison with Rufus

Legend: ✅ equivalent or better · 🟡 partial · ❌ missing · ⛔ deliberately out of scope (documented decision)

### 3.1 Device handling

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| List USB flash drives, hotplug refresh | ✅ sysfs topology + polling monitor | — |
| "List USB Hard Drives" toggle | 🟡 USB HDDs/SSDs are admitted like sticks | No separate opt-in or extra warning for large USB disks |
| SD cards in internal readers / UAS card readers | ❌ non-USB transport is refused | Policy decision needed (internal `mmcblk` readers are common on laptops) |
| Auto-unmount / lock volumes | ❌ `unmount()` is a stub that bails | **High-impact UX gap.** Needs a UDisks2 session-side unmount before planning |
| Safe eject / power-off after write | ❌ | Needs UDisks2 `PowerOff` |
| Partition table re-read after write | ❌ no `BLKRRPART` | Kernel partition nodes can stay stale until replug |
| 4Kn (4096-byte logical sector) targets | ❌ refused | Rufus handles them; needs sector-size-aware layouts |
| Multi-LUN / duplicate-serial sticks | ❌ refused (duplicate identity) | Safe, but some cheap multi-slot readers will never be usable |

### 3.2 Boot selection

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| Disk image (`.img`, `.dd`, raw) | ✅ | — |
| ISOHybrid in **DD mode** | ✅ | — |
| ISOHybrid in **ISO mode** (extract + Syslinux/GRUB; Rufus's default) | ❌ | Only DD is offered |
| Non-hybrid Linux ISO | ❌ refused | Needs ISO-mode extraction + bootloader install |
| Windows ISO | 🟡 UEFI-only FAT32 path | See §3.3 |
| Non bootable (format only) | 🟡 CLI `format-image` into a file, then write | Not in GUI; image-sized, not device-sized |
| FreeDOS / MS-DOS | ❌ | Low priority |
| Syslinux / GRUB2 / Grub4DOS / ReactOS standalone | ❌ | Low priority |
| UEFI:NTFS standalone | ❌ | Coupled with the NTFS Windows path |
| Download ISO (Fido) | ⛔ | Conflicts with the "no downloads" policy |

### 3.3 Windows media

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| Windows 10/11 UEFI FAT32 | ✅ (constructed, verified, OVMF-booted with a synthetic EFI app) | **Never booted with genuine Windows Setup** |
| `install.wim` > 4 GiB split to `.swm` | ✅ wimlib @3800 MiB | — |
| Oversized solid `install.esd` | ❌ refused | Rufus avoids the problem with NTFS. Could convert ESD→WIM with wimlib `export` |
| **NTFS + UEFI:NTFS** (Rufus default for large images) | ❌ | Needs a pinned, signed UEFI:NTFS partition image plus an NTFS population method |
| Legacy BIOS boot (MBR/PBR + `bootmgr`) | ❌ "MBR" scheme is UEFI-only | Needs MBR/PBR boot code (ms-sys equivalent) |
| "BIOS or UEFI" dual target | ❌ | Depends on the BIOS work |
| ARM64 / IA32 EFI | 🟡 detected via PE machine | Untested beyond detection |
| Windows 7 EFI fix (extract `bootmgfw.efi` from `install.wim`) | ❌ | Legacy, low priority |
| **Windows User Experience dialog**: remove TPM/Secure Boot/RAM checks, skip the online Microsoft account, create a local account, regional settings from host, disable data collection, disable BitLocker auto-encryption | ❌ none | **High-demand Rufus feature.** Implementable as generated `autounattend.xml` plus a `boot.wim` registry edit through wimlib, all unprivileged |
| Windows CA 2023 / revoked-bootloader handling (4.x) | ❌ | Related to Secure Boot checks (§3.6) |
| Windows To Go | ❌ | Large effort, low priority |
| Editable volume label | ❌ hard-coded `FORGEUSB` | Trivial |
| Image uses whole USB capacity | ❌ sized to payload + margin | See §4.1 |

### 3.4 Partition scheme / target system

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| MBR / GPT selection | ✅ Windows and format modes | Raw mode preserves the source layout, which is correct |
| Target: UEFI (non-CSM) | ✅ | — |
| Target: BIOS (or UEFI-CSM) | ❌ | — |
| "Add fixes for old BIOSes" (extra partition, alignment), Rufus MBR with BIOS ID | ❌ | Low priority |
| GPT backup header at the real device end | ❌ source-sized GPT is preserved | Rufus does not relocate either. A post-write `sgdisk -e`-style move is optional hardening |

### 3.5 Format options

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| FAT32 | ✅ (image) | — |
| NTFS | 🟡 blank data volume (image), CLI only | No GUI exposure |
| exFAT | ❌ | `mkfs.exfat` via the same runner. Needs UEFI:NTFS-style driver to boot |
| ext2/3/4 | ❌ | Needed for persistence partitions too |
| FAT16/FAT12, UDF, ReFS | ❌ | Low priority (ReFS is not available on Linux) |
| Volume label editing | 🟡 CLI `format-image --label` only | GUI field is read-only |
| Cluster size selection | ❌ always automatic | — |
| Quick vs full format | n/a | Full format means a zero fill; could be offered as a distinct destructive option |
| Extended label & icon (`autorun.inf`) | ❌ | Cosmetic |
| Check device for bad blocks (1–4 passes) | ⛔ deliberately not exposed | Could become a separately confirmed destructive tool |

### 3.6 Image formats, hashing, security

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| Compressed images `.gz .xz .bz2 .zst .lzma .zip .7z` written on the fly | ❌ gz/xz/zstd/bz2 detected and refused; zip/lzma/7z not even detected | **Common need** (Raspberry Pi, Armbian, Home Assistant images ship `.xz`) |
| VHD / VHDX / FFU as source | ❌ | Medium priority |
| Save drive to VHD/VHDX/FFU (drive → image) | ❌ | Backup use case |
| Hash dialog (MD5, SHA-1, SHA-256, SHA-512) | 🟡 SHA-256 only, always computed and shown | No compare-to-expected, no `.sha256sum`/`SHA256SUMS` auto-detection, no other algorithms |
| UEFI bootloader revocation warnings (DBX, SBAT, SVN, revoked certs) | ❌ | Valuable, and fits the safety-first identity |
| Post-write read-back verification | ✅ **exceeds Rufus** | — |
| Update check | ⛔ "no online updates/telemetry" | Distro packaging covers this |

### 3.7 Application UX

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| Single-window classic layout | ✅ | — |
| Progress bar with %, speed, time remaining | 🟡 bar + stage; no throughput/ETA | Easy |
| Log window | ✅ | — |
| Dark mode | ✅ | — |
| Drag-and-drop an image onto the window | ❌ | Easy (Iced `window::Event::FileDropped`) |
| Remembered settings (last directory, options) | ❌ | Easy (XDG config) |
| Localization (Rufus ships ~40 languages) | ❌ English only | Medium effort (Fluent/gettext) |
| Keyboard shortcuts | ✅ Ctrl+O / Ctrl+P / Esc | — |
| Accessibility / screen reader | ❓ not validated | Iced 0.13 accessibility is limited |
| Portable mode | 🟡 portable archive; writes still need the installed helper | By design |
| Command line | ✅ **richer than Rufus** (plan/verify/JSON events) | Human-readable output is a raw `Debug` dump |

### 3.8 Linux-specific Rufus features

| Rufus feature | ForgeUSB | Gap / note |
|---|---|---|
| Persistence partition (Ubuntu `casper-rw` / Debian `persistence`) | ❌ | Requires ISO mode to patch the boot config (DD mode keeps a read-only ISO9660 config) |
| Syslinux/GRUB version matching (Rufus downloads matching `ldlinux.sys`/`core.img`) | ❌ | ForgeUSB would need bundled, pinned versions or the distro's packages |

---

## 4. Structural gaps (architecture, not features)

### 4.1 "Build an image file, then raw-write it" model

The Windows and format paths produce an ordinary disk image and send it through the raw writer. This keeps formatters unprivileged and away from block devices, which is good. It also has three costs:

- **Capacity.** The partition is sized to the payload plus a margin, not to the USB. On a 64 GB stick with a 6 GB Windows image, about 58 GB stay unused.
- **Disk space and time.** The snapshot, the filesystem image and the final image all live beside the output, so the build needs about 2–3× the ISO size and writes it twice.
- **No direct "format this USB".** `format-image` needs `--capacity-mib` and a file.

**Recommended direction, which keeps the security model:** build a **sparse** image whose capacity equals the planned target capacity, then teach the raw writer to stream only the allocated extents (`SEEK_DATA`/`SEEK_HOLE`):

- Explicitly zero or discard metadata regions.
- Verify the extents plus the zeroed regions.

This keeps formatters on files, fills the device, removes most of the scratch-space cost and halves write time on large sticks.

The one change in assumptions: the plan must know the exact target capacity **before** the image is built. The GUI already selects a device first, so this works there. The CLI would need `prepare-windows --for-device /dev/sdX`.

### 4.2 Unmount, eject and partition refresh are absent

Today the user must unmount manually, then refresh, then plan. The helper refuses mounted targets, which is correct. The fix is a **user-session** UDisks2 step before planning:

- **Before planning:** unmount through UDisks2, then re-discover and build a new plan.
- **After a verified write:** call `BLKRRPART`, then UDisks2 `PowerOff`.

Root should not unmount anything. The current safety argument for refusing automatic unmounting stays intact as long as unmounting happens before the snapshot is taken, never inside the helper.

### 4.3 512-byte-only target geometry

The parsers and planner assume 512-byte LBAs, so 4Kn USB SSDs/enclosures are refused. The fix touches `forgeusb-image` (GPT at LBA 1 × sector size), `forgeusb-partition` (already 4096-aware in planning) and admission. Raw-copying a 512-byte GPT image to a 4Kn disk can never be correct, so 4Kn only matters for the build-on-target paths.

### 4.4 Strategy model is a hard-coded `if` chain

`forgeusb-boot::select_strategy` has three branches. Adding ISO mode, NTFS/UEFI:NTFS, BIOS, persistence and compressed sources turns it into a matrix: source capability × filesystem × scheme × firmware × options. Before adding the second new strategy, refactor it into a capability/strategy registry. Each strategy should declare:

- its preconditions,
- the evidence category it can claim,
- its executor,
- its verification.

This matches the "each executor must return distinct write/flush/verify/boot evidence" rule already written in `linux-design.md`.

### 4.5 Validation debt

These gates block any claim of "Rufus replacement", independent of new features:

1. A physical write through the installed helper on an exactly authorized disposable USB stick, including unplug-during-write and cancel cases.
2. Genuine Windows 10 and 11 ISOs: Setup boots, installation completes, on physical UEFI hardware, with Secure Boot on.
3. Clean-container builds for Debian/Fedora (recipes are untested) and the open `drm-fourcc` license-notice issue.
4. GTK native file chooser and screen-reader checks.

---

## 5. Prioritized implementation roadmap

Effort scale: **S** ≤ 1 day · **M** a few days · **L** 1–2 weeks · **XL** > 2 weeks. Each item lists its validation gate, following the project's rule that no row is promoted without its own evidence.

### P0: hygiene (do first)

| # | Item | Effort | Gate |
|---|---|---|---|
| 0.1 | Restore `tests/build-flags.py` (or remove the CI step) so `main` is green | S | CI green |
| 0.2 | Human-readable `list` / `check-dependencies` / `inspect` output when `--json` is not passed | S | Snapshot tests |
| 0.3 | Commit a physical-test protocol and run the first authorized physical write through the installed helper | S (+hardware) | Recorded device, hashes, replug read-back |

### P1: core parity (what most users notice first)

| # | Item | Effort | Depends | Gate |
|---|---|---|---|---|
| 1.1 | UDisks2 unmount before planning; `BLKRRPART` + power-off after verify (GUI button + CLI flag) | M | — | Mounted-stick flow on hardware |
| 1.2 | Device-sized sparse image build + extent-aware raw writer/verifier (§4.1); "format USB" (non-bootable) in GUI | L | 1.1 helps | File fixtures at multiple capacities; physical fill check |
| 1.3 | Compressed source streaming: gz/xz/zst/bz2 (+zip single-entry) with bounded expanded size, decoder-error and cancel propagation, verification against the decompressed digest | M | — | Corrupt/truncated/oversized fixtures; Pi/Armbian-style images |
| 1.4 | Windows User Experience options: generated `autounattend.xml` (bypass TPM/SB/RAM, local account, skip MS account, privacy, no BitLocker, regional from host); optional `boot.wim` LabConfig registry edit via wimlib | M–L | — | XML schema tests; **genuine Windows Setup boot (VM with TPM off)** |
| 1.5 | Editable label in GUI/Windows path; speed + ETA; drag-and-drop; persisted settings | S–M | — | GUI state tests |
| 1.6 | Hash panel: MD5/SHA-1/SHA-512 + compare-to-expected + auto-detect sibling `SHA256SUMS`/`.sha256` | S | — | Unit tests |
| 1.7 | Strategy registry refactor (§4.4) | M | before 2.x | Existing tests unchanged |

### P2: Windows completeness

| # | Item | Effort | Depends | Gate |
|---|---|---|---|---|
| 2.1 | NTFS + UEFI:NTFS Windows layout: pinned, checksummed, signed `uefi-ntfs.img` vendored with provenance and license review; populate NTFS without mounting (e.g. wimlib applying a captured WIM to an NTFS image via libntfs-3g, or ntfs-3g tools) | L | 1.2, 1.7 | OVMF with Secure Boot on/off; genuine Windows Setup; physical |
| 2.2 | Legacy BIOS Windows boot: MBR + NTFS/FAT32 PBR `bootmgr` loaders (ms-sys-equivalent code, GPL-compatible provenance), "BIOS or UEFI" target | L | 2.1 for NTFS | SeaBIOS + physical CSM |
| 2.3 | ESD→WIM conversion (`wimlib export`) so solid ESDs can be split instead of refused | S–M | — | Real ESD fixture |
| 2.4 | Secure Boot revocation advisory: parse EFI PE/SBAT, compare with a bundled, dated DBX/SBAT snapshot, warn in the review dialog (advisory only) | M | — | Known-revoked shim/GRUB fixtures |

### P3: Linux completeness

| # | Item | Effort | Depends | Gate |
|---|---|---|---|---|
| 3.1 | ISO mode: extract ISO9660 to FAT32, install GRUB2 (EFI from the ISO's own files) and Syslinux (BIOS, from distro packages, version-matched), patch labels/UUIDs. Per-distribution adapters with fixtures | XL | 1.2, 1.7 | Per-distro QEMU BIOS+UEFI boot |
| 3.2 | Persistence (Ubuntu `casper-rw`/`writable`, Debian `persistence` + `persistence.conf`), ext4 formatter | L | 3.1 | Reboot-survives-file test in QEMU |
| 3.3 | exFAT / ext4 / FAT16 formatters, cluster size selection | M | 1.2 | Media integration matrix |
| 3.4 | 4Kn target support (§4.3) | M | 1.2 | Emulated 4Kn (QEMU `logical_block_size=4096`) |

### P4: long tail (decide scope explicitly)

- VHD/VHDX/FFU as source (M), drive → VHDX/raw backup (M)
- FreeDOS (M, needs a vendored FreeDOS kernel), Syslinux/Grub4DOS/ReactOS standalone (M each)
- Windows To Go (XL)
- Bad-block scan as a separately confirmed destructive tool (M)
- Localization (M) and accessibility (unknown; limited by Iced)
- Card-reader (`mmcblk`) admission (S code, policy decision)
- ⛔ Fido ISO download and an update checker: recommend leaving these out

---

## 6. Rufus behaviours *not* to copy

The existing research already flags these; repeated here so the parity push does not erode them:

- Relaxing share/lock modes after a failed exclusive lock (`drive.c:138-215`). Keep fail-closed.
- Downloading Syslinux/GRUB/UEFI:NTFS components from mutable URLs at write time (Rufus, WoeUSB-ng). Vendor pinned and checksummed assets instead.
- Treating a found filename (`casper`, `bootmgr`) as proof of a boot method.
- Silent strategy fallbacks (e.g. a Windows ISO falling back to DD). The current code correctly forbids this.
- Running formatters or extractors as root.

---

## 7. Decisions needed from you before implementation

1. **Scope target.** "Rufus parity for Linux" (would include DOS, WTG, VHD) or "the 90 % of Rufus people actually use" (P0–P3)? This roadmap assumes the latter.
2. **Bundled boot assets.** UEFI:NTFS, BIOS MBR/PBR code and Syslinux need vendored binaries or code under compatible licenses. Is shipping pinned third-party boot binaries acceptable? The README currently promises none are distributed.
3. **Hardware and media for validation.** Will you authorize one specific disposable USB stick for destructive tests, and allow downloading genuine Windows 10/11 ISOs (and one or two Linux ISOs) for acceptance runs? Earlier work was explicitly told not to download large ISOs.
4. **Architecture for §4.1.** Keep the "formatters only touch files" invariant (sparse device-sized images, recommended), or allow formatters against the block device inside the helper (simpler, weaker isolation)?
5. **SD card readers.** Keep refusing non-USB transports, or add an opt-in for internal `mmcblk` readers?

---

## 8. Decisions recorded 2026-11-05

| Question | Decision |
|---|---|
| Scope | Deliver the "90 %" roadmap (P0–P3) first; plan full parity (P4) as a later phase (§9) |
| Bundled boot assets | Allowed, when pinned by digest, license-reviewed and recorded with provenance; never downloaded at write time |
| Hardware/media | The user's disposable `/dev/sda` stick (58 GB ASA-FD, serial `F023BG0642206260009C`, currently holding one `CACHYUSB` FAT32 partition `sda1`) is authorized for destructive tests. ForgeUSB writes whole disks, so a physical test erases all of `sda`, including `sda1`. Genuine ISOs may be downloaded for acceptance runs |
| §4.1 architecture | Keep "formatters only touch files": device-sized sparse images plus an extent-aware writer |
| SD card readers | Keep refusing non-USB transports |
| GUI | Refine for ease of use alongside the feature work |

## 9. Phase 2: path to full Rufus parity (planned, not scheduled)

Phase 2 starts after the P0–P3 items have passed their gates on the authorized stick and in QEMU.

| Area | Work | Prerequisite |
|---|---|---|
| Image formats | VHD/VHDX/FFU sources; drive → raw/VHDX backup ("save drive") | Extent-aware writer, strategy registry |
| DOS and legacy loaders | FreeDOS (pinned kernel/command.com), Syslinux, Grub4DOS, ReactOS standalone, "old BIOS fixes", Rufus-style MBR | BIOS MBR/PBR code from P2.2 |
| Windows To Go | Apply WIM to an NTFS image via libntfs-3g, `bcdboot`-equivalent BCD creation, SAN policy | NTFS path P2.1 |
| UEFI:NTFS standalone, exFAT boot | Reuse the pinned UEFI:NTFS asset | P2.1 |
| Diagnostics | Separately confirmed destructive bad-block scan; optional full-zero format | Writer progress/cancel model |
| Filesystems | FAT16/FAT12, UDF; cluster size for every formatter | P3.3 |
| Product | Localization (Fluent), accessibility audit, card-reader opt-in reconsideration | GUI refactor |
| Declined | Fido ISO download, built-in update checker (distribution packaging covers updates) | — |
