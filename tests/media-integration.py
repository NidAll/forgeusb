#!/usr/bin/env python3
"""Exercise real regular-file FAT32/NTFS/exFAT/ext4 format, partition, write and verification.

Every destination is a new path within --output. No mount, loop, root or physical
device access is used. Blank formatting never implies a boot loader is installed.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess

from boot_tools import digest, prepared_digest, run, tool, write_copy


def refused(command: list[str], output: Path) -> str:
    result = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if result.returncode == 0 or output.exists() or output.is_symlink():
        raise RuntimeError(f"Invalid formatting request published an image: {command}")
    return result.stdout + result.stderr


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, default=Path("target/release/forgeusb"))
    parser.add_argument("--output", type=Path, required=True, help="new directory for owned images")
    args = parser.parse_args()
    cli = str(args.cli.resolve(strict=True))
    args.output.mkdir(mode=0o700)
    output = args.output.resolve()
    results = []
    for scheme in ("mbr", "gpt"):
        for filesystem in ("fat32", "ntfs", "exfat", "ext4"):
            case = output / f"{scheme}-{filesystem}"
            case.mkdir()
            image = case / "formatted.img"
            label = {"fat32": "FAT32_TEST", "ntfs": "ForgeUSB Unicode ü",
                     "exfat": "ForgeUSB ü", "ext4": "forgeusb-ext4"}[filesystem]
            command = [cli, "format-image", "--output", str(image), "--filesystem", filesystem,
                       "--scheme", scheme, "--capacity-mib", "384", "--label", label, "--json"]
            report = json.loads(run(command, timeout=120).stdout)
            if (report["sha256"] != prepared_digest(cli, image)
                    or image.stat().st_size != 384 * 1024 * 1024):
                raise RuntimeError("Formatted image size/hash does not match report")
            if image.stat().st_blocks * 512 >= image.stat().st_size:
                raise RuntimeError("Formatted image was not kept sparse")
            (case / "format-report.json").write_text(json.dumps(report, indent=2) + "\n")
            # Independent util-linux parser verifies the assembled partition,
            # not merely the builder's in-memory plan or a formatter exit code.
            partition_table = json.loads(run([tool("sfdisk"), "--json", str(image)]).stdout)["partitiontable"]
            partitions = partition_table["partitions"]
            expected_scheme = "dos" if scheme == "mbr" else "gpt"
            # FAT32 media is an EFI System Partition so UEFI firmware finds it.
            expected_type = {("mbr", "fat32"): "ef", ("mbr", "ntfs"): "7", ("mbr", "exfat"): "7",
                             ("mbr", "ext4"): "83",
                             ("gpt", "fat32"): "C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
                             ("gpt", "ext4"): "0FC63DAF-8483-4772-8E79-3D69D8477DE4"}.get(
                (scheme, filesystem), "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
            if partition_table["label"] != expected_scheme or len(partitions) != 1:
                raise RuntimeError("Independent partition-table inspection disagrees")
            if partitions[0]["start"] != 2048:
                raise RuntimeError("Partition is not aligned to 1 MiB")
            if partitions[0]["type"].upper() != expected_type.upper():
                raise RuntimeError(f"Unexpected partition type {partitions[0]['type']}")
            with image.open("rb") as stream:
                stream.seek(partitions[0]["start"] * 512)
                sector = stream.read(2048)
            if filesystem == "ext4":
                # ext4 superblock magic 0xEF53 at byte 1080 of the partition.
                header = sector[1080:1082] == b"\x53\xef"
            else:
                signature, offset = {"fat32": (b"FAT32   ", 82), "ntfs": (b"NTFS    ", 3),
                                     "exfat": (b"EXFAT   ", 3)}[filesystem]
                header = sector[offset:offset + 8] == signature and sector[510:512] == b"\x55\xaa"
            if not header:
                raise RuntimeError("Partition lacks the expected real filesystem header")
            write_copy(cli, image, case, prepared=True)
            before = digest(image)
            repeated = subprocess.run(command, capture_output=True, text=True, timeout=60)
            if repeated.returncode == 0 or digest(image) != before:
                raise RuntimeError("Existing format output was overwritten")
            results.append({"scheme": scheme, "filesystem": filesystem, "label": label,
                            "independent_partition_check": True, "real_filesystem_header": True,
                            "format_hash": True, "raw_copy_verify": True,
                            "existing_output_preserved": True, "boot_tested": False})

    for name, extra in [
        ("bad-label", ["--label", "bad/label"]),
        ("bad-fat-label", ["--label", "lowercase"]),
        ("too-small", ["--capacity-mib", "1"]),
        ("capacity-overflow", ["--capacity-mib", "18446744073709551615"]),
        ("zero-timeout", ["--timeout-seconds", "0"]),
    ]:
        image = output / f"{name}.img"
        command = [cli, "format-image", "--output", str(image), "--filesystem", "fat32", "--json"]
        if "--capacity-mib" not in extra:
            command += ["--capacity-mib", "384"]
        refused(command + extra, image)

    # An output symlink must never redirect a formatting operation.
    sentinel = output / "sentinel"
    sentinel.write_bytes(b"original owned fixture")
    linked = output / "symlink.img"
    linked.symlink_to(sentinel)
    result = subprocess.run([cli, "format-image", "--output", str(linked), "--filesystem", "ntfs",
                             "--capacity-mib", "384", "--json"],
                            capture_output=True, text=True, timeout=60)
    if result.returncode == 0 or sentinel.read_bytes() != b"original owned fixture" or not linked.is_symlink():
        raise RuntimeError("Formatting followed an output symlink")

    summary = {"results": results, "invalid_arguments_refused_without_output": 5,
               "symlink_refused": True, "physical_writes": 0, "mounts": 0, "loop_devices": 0,
               "boot_tested": False}
    (output / "results.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
