#!/usr/bin/env python3
"""Build and boot original synthetic fixtures through ForgeUSB's real CLI.

Requires clang/lld, binutils, mtools, dosfstools, xorriso, wimlib and QEMU/OVMF.
No operating-system ISO is downloaded. True tiny WIM containers hold ordinary
text, not Windows PE/Setup. Boot success means the original test application ran;
it is not Windows installation, Linux startup, Secure Boot or physical USB proof.
All generated images and VM state belong to an exclusively created output tree.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil

from boot_tools import (BIOS_MARKER, UEFI_MARKER, boot, build_bios, build_uefi,
                        run, tool, write_copy)


def windows_tree(output: Path, efi: Path) -> Path:
    tree = output / "windows-shaped"
    (tree / "efi/boot").mkdir(parents=True)
    (tree / "efi/microsoft/boot").mkdir(parents=True)
    (tree / "boot").mkdir()
    (tree / "sources").mkdir()
    shutil.copyfile(efi, tree / "efi/boot/bootx64.efi")
    (tree / "boot/BCD").write_bytes(b"SYNTHETIC BCD PLACEHOLDER - not Windows boot data\n")
    (tree / "efi/microsoft/boot/BCD").write_bytes(b"SYNTHETIC BCD PLACEHOLDER - not Windows boot data\n")
    (tree / "bootmgr").write_bytes(b"SYNTHETIC BOOTMGR PLACEHOLDER\n")
    (tree / "setup.exe").write_bytes(b"SYNTHETIC SETUP PLACEHOLDER\n")
    capture = output / "wim-payload"
    capture.mkdir()
    (capture / "synthetic.txt").write_text("Original test payload; no Microsoft files.\n")
    # These are valid, verified WIM containers, but contain no operating system.
    for name in ("boot.wim", "install.wim"):
        wim = tree / "sources" / name
        run([tool("wimlib-imagex"), "capture", str(capture), str(wim),
             "ForgeUSB synthetic fixture", "--compress=none", "--check"])
        run([tool("wimlib-imagex"), "verify", str(wim)])
    # Long Unicode names exercise FAT long-name copy and byte-for-byte read-back.
    (tree / "sources/Long filename ünicode.txt").write_text("Original Unicode fixture.\n")
    return tree


def hybrid_iso(output: Path, efi: Path, bios: Path) -> Path:
    esp = output / "esp.img"
    with esp.open("xb") as stream:
        stream.truncate(64 * 1024 * 1024)
    run([tool("mkfs.fat"), "-F", "32", str(esp)])
    run([tool("mmd"), "-i", str(esp), "::EFI"])
    run([tool("mmd"), "-i", str(esp), "::EFI/BOOT"])
    run([tool("mcopy"), "-i", str(esp), str(efi), "::EFI/BOOT/BOOTX64.EFI"])
    tree = output / "iso-tree"
    tree.mkdir()
    (tree / "README.txt").write_text("Synthetic ISOHybrid; no Linux kernel or operating system.\n")
    image = output / "hybrid.iso"
    # GNU xorriso exposes an appended EFI filesystem through GPT and El Torito.
    # Our original MBR code boots independently of the firmware's FAT reader.
    run([tool("xorriso"), "-as", "mkisofs", "-R", "-J", "-V", "FORGEUSB_TEST",
         "-o", str(image), "-G", str(bios), "-append_partition", "2", "0xef", str(esp),
         "-appended_part_as_gpt", "-e", "--interval:appended_partition_2:all::",
         "-no-emul-boot", str(tree)])
    return image


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, default=Path("target/release/forgeusb"))
    parser.add_argument("--output", type=Path, required=True, help="new directory for fixtures/logs")
    args = parser.parse_args()
    cli = str(args.cli.resolve(strict=True))
    args.output.mkdir(mode=0o700)
    output = args.output.resolve()
    efi = build_uefi(output)
    bios = build_bios(output)
    results = []

    case = output / "bios"
    case.mkdir()
    written = write_copy(cli, bios, case)
    results.append({"case": "raw original BIOS MBR", **boot(written, case / "vm", uefi=False,
                                                          marker=BIOS_MARKER)})

    tree = windows_tree(output, efi)
    iso = output / "windows-shaped.iso"
    run([tool("xorriso"), "-as", "mkisofs", "-R", "-J", "-o", str(iso), str(tree)])
    # Directory and optical-ISO source exercise both staging entry points.
    for scheme, source in [("mbr", tree), ("gpt", iso)]:
        case = output / f"windows-shaped-{scheme}"
        case.mkdir()
        image = case / "prepared.img"
        report = json.loads(run([cli, "prepare-windows", str(source), "--output", str(image),
                                 "--scheme", scheme, "--capacity-mib", "384", "--json"],
                                timeout=120).stdout)
        (case / "preparation.json").write_text(json.dumps(report, indent=2) + "\n")
        written = write_copy(cli, image, case)
        results.append({"case": f"synthetic Windows-shaped {scheme} FAT32 UEFI fallback",
                        **boot(written, case / "vm", uefi=True, marker=UEFI_MARKER)})

    case = output / "hybrid"
    case.mkdir()
    image = hybrid_iso(case, efi, bios)
    written = write_copy(cli, image, case)
    for uefi in (False, True):
        mode = "uefi" if uefi else "bios"
        results.append({"case": f"synthetic appended-ESP GPT ISOHybrid {mode}",
                        **boot(written, case / mode, uefi=uefi,
                               marker=UEFI_MARKER if uefi else BIOS_MARKER)})

    summary = {"results": results, "fixture_identity": "original synthetic test code and tiny WIMs",
               "genuine_windows_media": False, "genuine_linux_media": False,
               "physical_writes": 0, "mounts": 0, "loop_devices": 0, "network": False,
               "secure_boot": "not tested"}
    (output / "results.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
