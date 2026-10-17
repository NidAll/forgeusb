# ForgeUSB

A native Linux boot-media writer in Rust, with an Iced desktop app and a shared CLI engine. Version 0.1.0 is an initial engineering build: it writes structurally recognized raw disk images and Linux ISOHybrid images, flushes them, and compares every source byte with read-back. It does not yet replace Rufus's full compatibility catalog.

**Physical writes erase the selected USB. No physical disk was overwritten during development.** Never run the GUI as root. Physical writes require a separately installed Polkit helper, an eligible unmounted USB with a stable identity, a reviewed plan, and exact typed confirmation. No device is automatically selected. Desktop admission is preliminary; the root helper must repeat authoritative system-wide checks before any physical write. The device layer rejects host/system storage, mounted partitions, active swap, device-mapper/RAID holders, uncertain backing topology and non-512-byte logical sectors.

## Screenshots

Compact desktop interface with one detected USB device. No target is automatically selected.

![ForgeUSB main window showing one detected USB device, READY status and no selected target](docs/usb-detection.png)

Read-only operation review in disposable-file mode. Writing remains disabled until the exact confirmation is entered; this capture did not start a write.

![ForgeUSB confirmation dialog showing a source image, new disposable output file, byte counts and required confirmation](docs/gui-plan-smoke.png)

## Build and run

Rust stable >=1.88 and a C linker are required. On Arch/CachyOS install build prerequisites through your normal package manager; see [installation](docs/installation.md). The desktop uses Wayland or X11 and a software renderer.

```sh
cargo build --release --locked
./target/release/forgeusb-gui
./target/release/forgeusb list --json
./target/release/forgeusb inspect /absolute/path/linux.iso --json
./target/release/forgeusb check-dependencies
```

This host's inherited linker name `x86_64-linux-gnu-gcc` was absent. The project-local `.cargo/config.toml` therefore selects `/usr/bin/cc` and one build job. Initial validation also used the matching per-command override and `CARGO_PROFILE_DEV_DEBUG=0`. No global toolchain, Ghidra, or Codex configuration was changed. In Fish use `env NAME=value cargo ...` for optional environment overrides.

## Review, then write

Planning is read-only and produces the exact source SHA-256, identity/topology snapshot, target capacity, strategy, warnings and confirmation text. Partition table, firmware mode, filesystems, labels and embedded bootloaders are preserved from the image.

```sh
./target/release/forgeusb plan /absolute/path/linux.iso --device /dev/sdX --json > plan.json
./target/release/forgeusb write --plan plan.json
./target/release/forgeusb verify /absolute/path/linux.iso --device /dev/sdX
```

The write command prompts interactively. Noninteractive use requires BOTH the reviewed plan file and `--confirm 'EXACT confirmation FROM plan.json'`; there is no blanket `--yes` or system-disk override. A changed source/device invalidates the plan. Physical write privilege is obtained through fixed `/usr/bin/pkexec /usr/lib/forgeusb/forgeusb-helper write --plan-json`. The helper revalidates the plan independently and only opens user-readable, user-owned source files with that user's filesystem credentials. See [device safety](docs/device-safety.md) and [threat model](docs/threat-model.md).

To exercise the complete write/verify path without USB hardware, choose a **new** output file:

```sh
./target/release/forgeusb plan /absolute/path/disk.img --device /absolute/path/NEW-test-copy.img --test-file --json > plan.json
./target/release/forgeusb write --plan plan.json
./target/release/forgeusb verify /absolute/path/disk.img --device /absolute/path/NEW-test-copy.img --test-file
```

Disposable-file mode never overwrites an existing file. The compact Rufus-style GUI exposes the same mode under advanced drive properties. SELECT or Ctrl+O opens a native file chooser; START or Ctrl+P opens the read-only review. Writing requires exact typed confirmation in that dialog. Esc dismisses or cancels. See [GUI details](docs/gui.md).

`--json` emits a JSON document for read-only queries and newline-delimited progress/completion events for write/verify. Errors are JSON on stderr. Exit status: 0 success; 1 operation/safety/dependency failure; 2 invalid CLI syntax. Ctrl+C requests cancellation and currently returns operation failure (1). A partial write is not rolled back. Cancellation takes effect between I/O chunks and cannot interrupt a kernel I/O call already blocked.

## What is supported

- Live whole-disk/partition discovery, filesystem/mount information, stable identity checks, and periodic GUI hotplug refresh.
- Bounded ISO9660/UDF recognition, El Torito validation-header inspection, MBR/GPT primary metadata checks, SHA-256, disk/ISOHybrid strategy selection.
- Streaming raw writes with a 1 MiB buffer, exclusive kernel device claim, flush and complete source-length byte comparison.
- Windows **staging tools only**: `inspect-windows-tree TREE` checks extracted installer metadata; `split-wim SOURCE --output NEW_DIRECTORY` invokes installed wimlib without device access. This is not Windows ISO-to-USB creation.
- Pure MBR/GPT EFI/FAT32 layout planning and filesystem constraints, tested independently of raw writing.

Optical Windows installation ISOs, generic ISO extraction, filesystem formatting, NTFS/UEFI:NTFS installation, Linux persistence, compressed writes, safe power-off and bootloader installation are **not implemented**. Unmount through your desktop, then refresh. Secure Boot depends on the source loader and firmware trust/revocation state and is not guaranteed. Read-back verifies bytes, not firmware bootability or flash power-loss persistence. The unused tail of a larger target is preserved and not verified, including any old signatures there. Primary GPT checks are not a complete GPT repair/integrity tool. See the [full compatibility matrix](docs/compatibility.md).

## Tests and research

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
python3 tests/file-integration.py --cli target/release/forgeusb
```

All destructive integration fixtures are newly owned ordinary files. Loop/physical tests are not silently enabled. QEMU tests attach only an explicitly selected regular image, disable networking and use fresh OVMF NVRAM. [Validation results](docs/validation.md) distinguish synthetic layouts, virtual Linux startup and untested physical/Windows behavior. [Milestones](docs/milestones.md) records delivered functions and remaining engineering work.

Rufus v4.15 source and its matching official executable were investigated using REA 6.5.0 and Ghidra 12.1.4. [Research index](docs/reverse-engineering/README.md) links source architecture, diagrams, function mappings, binary evidence and limitations. Development references remain local under `references/` and are excluded from packages. [Dependency decisions](docs/dependencies.md), [license inventory](docs/dependency-licenses.json) and [third-party notices](THIRD_PARTY_NOTICES.md) document the supply chain.

## Packaging

Run `bash packaging/install.sh --system` as your desktop user to build and install with administrator authentication for the final copy. The application menu launcher and window include the ForgeUSB icon. `--user` installs desktop/CLI only; `--stage /tmp/forgeusb-NEW-stage --no-build` prepares a new disposable staging tree. See [installation and removal](docs/installation.md).

`make -f packaging/Makefile install DESTDIR=/tmp/forgeusb-stage` stages binaries, desktop entry, original icon, license texts and Polkit policy without privileged installation. A real `/usr` installation is a separate administrator step. `bash packaging/make-source-archive.sh` creates a deterministic source archive and an Arch PKGBUILD with the exact checksum. Debian/RPM recipes are starting points and have not been built in clean distribution containers. `bash packaging/make-portable.sh` builds a local portable directory archive for this host's glibc. A portable directory can run the GUI/CLI; physical writes still require the trusted system helper/policy. No online updates or telemetry are implemented.

GPL-3.0-or-later. Independent implementation; no upstream executable or bootloader is distributed with ForgeUSB.
