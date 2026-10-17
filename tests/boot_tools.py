"""Bounded, unprivileged boot-test helpers. No disk/device discovery or mounts."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess

ROOT = Path(__file__).resolve().parent.parent
BIOS_MARKER = b"FORGEUSB_BIOS_BOOT_OK"
UEFI_MARKER = b"FORGEUSB_UEFI_BOOT_OK"


def tool(name: str) -> str:
    executable = shutil.which(name)
    if executable is None:
        raise RuntimeError(f"Required fixture tool is missing from PATH: {name}")
    return executable


def run(command: list[str], timeout: int = 60) -> subprocess.CompletedProcess:
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{command[0]} exited {result.returncode}: "
                           f"{result.stdout[-2000:]}{result.stderr[-2000:]}")
    return result


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_copy(cli: str, source: Path, output: Path) -> Path:
    """Exercise the real plan/write/verify CLI only against a new owned file."""
    target = output / "written.img"
    plan = json.loads(run([cli, "plan", str(source), "--device", str(target),
                           "--test-file", "--json"]).stdout)
    if target.exists():
        raise RuntimeError("Read-only planning created a target")
    planfile = output / "plan.json"
    planfile.write_text(json.dumps(plan, indent=2) + "\n")
    run([cli, "write", "--plan", str(planfile), "--confirm", plan["confirmation"], "--json"])
    run([cli, "verify", str(source), "--device", str(target), "--test-file", "--json"])
    if digest(source) != digest(target):
        raise RuntimeError("CLI write/verify did not preserve the entire synthetic image")
    return target


def regular_image(path: Path) -> Path:
    """Explicit images only: FIFOs, symlinks and block devices are all refused."""
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size == 0:
        raise RuntimeError(f"Boot image must be a nonempty regular file: {path}")
    resolved = path.resolve(strict=True)
    if resolved.is_relative_to("/dev"):
        raise RuntimeError("Boot tests never attach a host /dev path")
    return resolved


def ovmf_paths() -> tuple[Path, Path]:
    """Match each distribution's firmware code with its same-size VARS template."""
    code_override = os.environ.get("FORGEUSB_OVMF_CODE")
    vars_override = os.environ.get("FORGEUSB_OVMF_VARS")
    if bool(code_override) != bool(vars_override):
        raise RuntimeError("Set both FORGEUSB_OVMF_CODE and FORGEUSB_OVMF_VARS")
    pairs = [(Path(code_override), Path(vars_override))] if code_override else [
        (Path(root) / code, Path(root) / variables)
        for root, code, variables in [
            ("/usr/share/OVMF", "OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
            ("/usr/share/OVMF", "OVMF_CODE.fd", "OVMF_VARS.fd"),
            ("/usr/share/edk2/x64", "OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
            ("/usr/share/OVMF/x64", "OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
            ("/usr/share/edk2/ovmf", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        ]
    ]
    for code, variables in pairs:
        if code.is_file() and variables.is_file():
            return code.resolve(), variables.resolve()
    raise RuntimeError("OVMF code/VARS pair not found; set FORGEUSB_OVMF_CODE/VARS")


def build_uefi(directory: Path) -> Path:
    obj = directory / "uefi-boot.obj"
    output = directory / "BOOTX64.EFI"
    run([tool("clang"), "--target=x86_64-pc-win32-coff", "-ffreestanding",
         "-fshort-wchar", "-mno-red-zone", "-fno-stack-protector", "-O2", "-c",
         str(ROOT / "tests/uefi-boot.c"), "-o", str(obj)])
    run([tool("lld-link"), "/subsystem:efi_application", "/entry:efi_main",
         "/nodefaultlib", "/machine:x64", "/timestamp:0", f"/out:{output}", str(obj)])
    return output


def build_bios(directory: Path) -> Path:
    obj = directory / "bios-boot.o"
    output = directory / "bios.img"
    run([tool("as"), "--32", str(ROOT / "tests/bios-boot.S"), "-o", str(obj)])
    linked = directory / "bios-boot.elf"
    run([tool("ld"), "-m", "elf_i386", "-Ttext", "0x7c00", str(obj), "-o", str(linked)])
    # Newer assemblers add .note.gnu.property; never append it to a boot sector.
    run([tool("objcopy"), "-O", "binary", "-j", ".text", str(linked), str(output)])
    if output.stat().st_size != 512 or output.read_bytes()[-2:] != b"\x55\xaa":
        raise RuntimeError("Original BIOS fixture is not a boot sector")
    with output.open("r+b") as stream:
        stream.truncate(1024 * 1024)
    return output


def boot(image: Path, output: Path, *, uefi: bool, marker: bytes,
         seconds: int = 40) -> dict:
    image = regular_image(image)
    if not 5 <= seconds <= 55:
        raise ValueError("Boot duration must be 5..55 seconds")
    # Refuse reused output directories, including their old firmware variables.
    output.mkdir(mode=0o700)
    output = output.resolve()
    before = digest(image)
    serial = output / "serial.log"
    command = [tool("qemu-system-x86_64"), "-no-user-config", "-nodefaults",
               "-machine", "q35,accel=tcg", "-m", "256", "-nic", "none",
               "-display", "none", "-monitor", "none", "-no-reboot",
               "-serial", f"file:{serial}",
               "-blockdev", json.dumps({"driver": "raw", "node-name": "media",
                                        "read-only": True,
                                        "file": {"driver": "file", "filename": str(image)}}),
               "-device", "qemu-xhci", "-device", "usb-storage,drive=media,bootindex=1",
               "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]
    firmware = None
    if uefi:
        code, template = ovmf_paths()
        variables = output / "OVMF_VARS.fd"
        shutil.copyfile(template, variables)
        firmware = {"code_sha256": digest(code), "vars_template_sha256": digest(template)}
        command += ["-blockdev", json.dumps({"driver": "raw", "node-name": "fwcode",
                                             "read-only": True,
                                             "file": {"driver": "file", "filename": str(code)}}),
                    "-blockdev", json.dumps({"driver": "raw", "node-name": "fwvars",
                                             "file": {"driver": "file", "filename": str(variables)}}),
                    "-machine", "pflash0=fwcode,pflash1=fwvars"]
    (output / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    with (output / "qemu.log").open("w") as log:
        try:
            process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                     timeout=seconds)
            exit_code = process.returncode
        except subprocess.TimeoutExpired:
            exit_code = None
    serial_data = serial.read_bytes() if serial.exists() else b""
    result = {"firmware": "ovmf" if uefi else "seabios", "exit_code": exit_code,
              "expected_exit": 33, "marker_seen": marker in serial_data,
              "image_sha256": before, "image_unchanged": before == digest(image),
              "firmware_hashes": firmware, "secure_boot": "not tested",
              "network": False, "physical_writes": 0, "mounts": 0, "loop_devices": 0}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    if exit_code != 33 or not result["marker_seen"] or not result["image_unchanged"]:
        raise RuntimeError(f"Boot fixture failed; inspect {output}: {result}")
    return result
