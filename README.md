# ForgeUSB

A native Linux tool for making bootable USB drives, written in Rust. It has a desktop app modeled on Rufus and a command-line tool, and both share one engine.

- Write Linux ISOs and disk images exactly as published, including compressed ones.
- Build Windows 10/11 UEFI installer drives from an ISO, with optional setup tweaks.
- Format a drive as FAT32, exFAT, NTFS or ext4.
- Every write is reviewed first, confirmed by typing the drive's name, and read back to verify it.

> **Status: engineering build.** It is tested on one disposable USB stick and in virtual machines, not across many PCs. Genuine Windows Setup has not been tested yet. See [what's missing](#not-supported-yet) and the [validation record](docs/validation.md).

## Screenshots

| Disk or ISO image | Windows installer |
|---|---|
| ![Disk image mode with a demo image selected and no drive chosen](docs/gui-smoke.png) | ![Windows installer mode showing the Windows setup options](docs/gui-windows.png) |
| **Format only** | **Confirmation** |
| ![Format only mode with file system and volume label](docs/gui-format.png) | ![Confirmation dialog asking to type CREATE for a disposable file](docs/gui-plan-smoke.png) |

The screenshots use synthetic demo files. With a real drive, the confirmation asks you to type the drive's short name (such as `sdb`) and lists every partition it will erase.

## What it does

**Disk and ISO images**
- Writes vendor USB images and hybrid ISOs byte for byte. The partition table, file systems and boot loaders all stay exactly as the image has them.
- Expands `.gz`, `.xz`, `.zst`, `.bz2` and single-file `.zip` images in your cache first, unprivileged.
- Shows MD5, SHA-1, SHA-256 and SHA-512, and checks them against a `SHA256SUMS`-style file it finds beside the image.

**Windows installers**
- Builds a FAT32 UEFI installer from a Windows ISO, choosing MBR or GPT and sizing it to the drive. Every copied file is verified.
- Splits a large `install.wim` automatically.
- Can add an `autounattend.xml` that:
  - skips the TPM 2.0, Secure Boot and RAM checks;
  - allows setup without a Microsoft account, or creates a named local account;
  - applies this computer's regional settings;
  - declines the privacy questions;
  - turns off automatic BitLocker.

**Formatting**
- Creates an empty FAT32, exFAT, NTFS or ext4 drive with your label, on MBR or GPT, up to 2 TiB.
- The file system is built in a private image file, never on the drive itself. Only its data is then written, so formatting a 64 GB stick takes seconds.

**Around the write**
- **Unmount:** drives your desktop has mounted can be unmounted from the app.
- **Safe removal:** the drive is powered off when verification finishes, so you can unplug it.
- **Partitions:** the kernel picks up the new partitions without replugging.
- **App conveniences:** drag and drop, speed and time remaining, and settings remembered between sessions.

## Safety model

ForgeUSB only ever offers **USB** drives. Internal disks, the system disk, SD card readers, RAID or device-mapper members, swap and anything it can't identify with certainty are refused. Nothing is ever selected for you.

1. **Plan (read-only).** ForgeUSB records the source's SHA-256, the drive's identity (device number, serial and topology) and its capacity. The drive is never opened at this stage.
2. **Confirm.** You review the drive, its size and the partitions that will be erased, then type its name.
3. **Revalidate and write.** A small Polkit helper re-checks everything as root. It opens the source with *your* file permissions, takes an exclusive lock on the drive, writes, flushes and reads every byte back.

The desktop app always runs as your normal user and refuses to run as root. Everything else runs unprivileged in your session: formatting tools, Windows extraction, decompression, unmount and power-off. The helper never unmounts anything, and refuses mounted drives. Details: [device safety](docs/device-safety.md) and [threat model](docs/threat-model.md).

## Install

You need Rust stable 1.88 or newer, a C linker, Python 3, Polkit with an authentication agent, and udisks2.

Optional tools, by feature:

| Feature | Packages |
|---|---|
| Windows installers | 7-Zip (`7z` or `7zz`), dosfstools, mtools, wimlib |
| NTFS, exFAT, ext4 | ntfs-3g, exfatprogs, e2fsprogs |
| SELECT file chooser | kdialog or zenity |

ForgeUSB never installs packages for you.

```bash
bash packaging/install.sh --system
```

This builds as your user, then asks for your password once to copy a fixed list of files into `/usr`: the CLI, the GUI, the helper, its Polkit policy and the menu entry. Launch **ForgeUSB** from your application menu. To install without the helper, and so without physical writes, use `--user`. Check what's available with:

```bash
forgeusb check-dependencies
```

More options, including staging, removal and the portable build, are in [installation](docs/installation.md).

## Using the app

1. Plug in a USB drive and choose it under **Device**. If it's mounted, press **Unmount**.
2. Press **SELECT**, or drop an image on the window. The **Image option** follows the file: a Windows ISO switches to *Windows installer*. Choose *Format only* to just erase and format the drive.
3. Press **START**. Any preparation (expanding, building Windows media or formatting) happens in your cache, and the drive is not touched yet.
4. Check the drive and what it contains, type its name, and press **START** again. Enter your password when Polkit asks.
5. Wait for **DONE**. The drive is verified and safely removed.

Keyboard shortcuts, advanced properties and the disposable-file test mode are covered in [the GUI guide](docs/gui.md).

## Using the command line

```bash
forgeusb list                                    # disks, partitions and whether each can be used
forgeusb checksum ~/Downloads/distro.iso         # MD5/SHA-1/SHA-256/SHA-512 + published-sum check
forgeusb plan ~/Downloads/distro.iso --device /dev/sdX --json > plan.json
forgeusb write --plan plan.json --eject          # asks you to type the confirmation
forgeusb verify ~/Downloads/distro.iso --device /dev/sdX
```

Windows installer, sized to the drive:

```bash
forgeusb prepare-windows ~/Downloads/Win11.iso --output ~/win11.img --device /dev/sdX \
  --scheme gpt --bypass-requirements --no-online-account --local-account alex --regional=host
forgeusb plan ~/win11.img --device /dev/sdX --prepared --json > plan.json
forgeusb write --plan plan.json --eject
```

Other preparation commands:

```bash
forgeusb format-image --output ~/data.img --filesystem exfat --device /dev/sdX --label DATA
forgeusb decompress ~/Downloads/image.img.xz --output ~/image.img --device /dev/sdX
forgeusb unmount /dev/sdX
```

**Notes on these commands:**
- **Typing the confirmation:** `write` asks for it interactively. Scripts must pass the reviewed plan *and* `--confirm 'EXACT TEXT FROM plan.json'`; there is no `--yes`.
- **`--prepared`:** use it only for images made by `prepare-windows` or `format-image`. It skips their unused space. Never use it for downloaded or decompressed images, whose zeros are real data.
- **`verify` on a drive** asks for your password. The helper then compares the drive with the image without writing to it.
- **Machine output:** `--json` gives JSON documents and progress events.
- **Exit codes:** 0 means success, 1 a failure or refusal, 2 bad arguments. Ctrl+C cancels between chunks, and an interrupted write is not rolled back.

To try the whole path without hardware, write to a **new** file with `--device /path/NEW.img --test-file`.

## Not supported yet

- Windows on BIOS-only PCs, NTFS Windows media with UEFI:NTFS, and ESD-to-WIM conversion.
- Extracting Linux ISOs that aren't hybrid, and persistence partitions.
- Drives with 4 KiB logical sectors, and SD card readers (refused on purpose).
- Secure Boot is never guaranteed: it depends on the image's boot loaders and the PC's firmware.

The [Rufus gap report](docs/rufus-gap-report.md) lists every difference from Rufus 4.15 and the plan for each. The [compatibility matrix](docs/compatibility.md) separates what was boot-tested from what was only structurally verified.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 packaging/build.py build --release --locked
python3 tests/file-integration.py --cli target/release/forgeusb
python3 tests/media-integration.py --cli target/release/forgeusb --output test-output/NEW-media
python3 tests/boot-integration.py --cli target/release/forgeusb --output test-output/NEW-boot
```

The integration tests write only to new files they own: no loop devices, no physical drives and no network. QEMU boot tests use fresh firmware variables. The GUI tests (`tests/gui-smoke.py`, `tests/gui-windows.py`) run on a private Xvfb display and never touch your desktop.

Use `packaging/build.py` for release artifacts. It removes personal paths from compiler output. This repository's `.cargo/config.toml` selects `/usr/bin/cc` because the host's default linker name was missing.

**Project layout:**

| Path | Contents |
|---|---|
| `apps/forgeusb-gui`, `apps/forgeusb-cli` | The two front ends |
| `crates/forgeusb-privileged` | The Polkit helper |
| `crates/forgeusb-linux` | The planner, writer and UDisks session actions |
| `crates/forgeusb-device` | USB discovery and admission |
| `crates/forgeusb-image`, `crates/forgeusb-verify` | Inspection, checksums and range digests |
| `crates/forgeusb-media`, `crates/forgeusb-partition`, `crates/forgeusb-filesystem` | Formatting and partitioning |
| `crates/forgeusb-windows` | Windows tree, WIM and `autounattend.xml` handling |
| `crates/forgeusb-boot`, `crates/forgeusb-core` | Boot strategy selection and shared types |

[Milestones](docs/milestones.md) and the [development plan](docs/development-plan.md) track progress.

## Research and licensing

ForgeUSB is an independent implementation. The behavior of Rufus 4.15 was studied from its source and its official binary; see the [research index](docs/reverse-engineering/README.md). No Rufus code, boot loaders, Microsoft files or other upstream binaries are included.

Supply-chain documents: [dependencies](docs/dependencies.md), the [license inventory](docs/dependency-licenses.json) and the [third-party notices](THIRD_PARTY_NOTICES.md).

Licensed under GPL-3.0-or-later.
