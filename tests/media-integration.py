#!/usr/bin/env python3
"""Exercise real regular-file FAT32/NTFS format, partition, write and verification.

Every destination is a new path within --output. No mount, loop, root or physical
device access is used. Blank formatting never implies a boot loader is installed.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess

from boot_tools import digest, run, tool, write_copy


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
        for filesystem in ("fat32", "ntfs"):
            case = output / f"{scheme}-{filesystem}"
            case.mkdir()
            image = case / "formatted.img"
            label = "FAT32_TEST" if filesystem == "fat32" else "ForgeUSB Unicode ü"
            command = [cli, "format-image", "--output", str(image), "--filesystem", filesystem,
                       "--scheme", scheme, "--capacity-mib", "384", "--label", label, "--json"]
            report = json.loads(run(command, timeout=120).stdout)
            if report["sha256"] != digest(image) or image.stat().st_size != 384 * 1024 * 1024:
                raise RuntimeError("Formatted image size/hash does not match report")
            (case / "format-report.json").write_text(json.dumps(report, indent=2) + "\n")
            # Independent util-linux parser verifies the assembled partition,
            # not merely the builder's in-memory plan or a formatter exit code.
            partition_table = json.loads(run([tool("sfdisk"), "--json", str(image)]).stdout)["partitiontable"]
            partitions = partition_table["partitions"]
            expected_scheme = "dos" if scheme == "mbr" else "gpt"
            if partition_table["label"] != expected_scheme or len(partitions) != 1:
                raise RuntimeError("Independent partition-table inspection disagrees")
            if partitions[0]["start"] != 2048:
                raise RuntimeError("Partition is not aligned to 1 MiB")
            with image.open("rb") as stream:
                stream.seek(partitions[0]["start"] * 512)
                sector = stream.read(512)
            expected_signature = b"FAT32   " if filesystem == "fat32" else b"NTFS    "
            offset = 82 if filesystem == "fat32" else 3
            if sector[offset:offset + 8] != expected_signature or sector[-2:] != b"\x55\xaa":
                raise RuntimeError("Partition lacks the expected real filesystem header")
            write_copy(cli, image, case)
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
