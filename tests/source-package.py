#!/usr/bin/env python3
"""Check source archive contents/checksums/reproducibility without installations."""
from pathlib import Path
import hashlib
import os
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
VERSION = "0.1.0"


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="forgeusb-source-package-") as temporary:
        directory = Path(temporary)
        environment = {**os.environ, "SOURCE_DATE_EPOCH": "0"}
        hashes = []
        for number in (1, 2):
            output = directory / str(number)
            subprocess.run(["bash", str(ROOT / "packaging/make-source-archive.sh"), str(output)],
                           cwd=ROOT, env=environment, check=True, capture_output=True, timeout=60)
            archive = output / f"forgeusb-{VERSION}.tar.gz"
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            hashes.append(digest)
            assert (output / "SHA256SUMS").read_text() == f"{digest}  {archive.name}\n"
            assert f"sha256sums=('{digest}')" in (output / "PKGBUILD").read_text()
            with tarfile.open(archive) as tar:
                names = set()
                for member in tar:
                    relative = Path(member.name)
                    assert relative.parts[0] == f"forgeusb-{VERSION}", member.name
                    assert not relative.is_absolute() and ".." not in relative.parts, member.name
                    assert member.isfile() or member.isdir(), member.name
                    assert not any(part in {"target", "references", "test-output", "__pycache__", ".git"}
                                   for part in relative.parts), member.name
                    names.add(str(Path(*relative.parts[1:])))
                for required in ("Cargo.lock", "crates/forgeusb-media/src/lib.rs", "tests/boot_tools.py",
                                 "tests/uefi-boot.c", "tests/media-integration.py",
                                 "packaging/install.py", "packaging/build.py", "packaging/icons/forgeusb.svg"):
                    assert required in names, required
        assert hashes[0] == hashes[1], "Saved source changed or archive is not reproducible"
    print("Source archive: exact checksums, complete fixture sources, no build/private artifacts, reproducible.")


if __name__ == "__main__":
    main()
