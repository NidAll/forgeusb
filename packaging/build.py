#!/usr/bin/env python3
"""Run Cargo with private build paths remapped, without changing caller settings."""
from __future__ import annotations

import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
from collections.abc import Mapping

ROOT = Path(__file__).resolve().parent.parent


def build_environment(inherited: Mapping[str, str], root: Path = ROOT) -> dict[str, str]:
    environment = dict(inherited)
    encoded = inherited.get("CARGO_ENCODED_RUSTFLAGS")
    # Cargo gives encoded flags precedence. Never reinterpret spaces inside an
    # already encoded argument (linker options and paths can both contain them).
    flags = encoded.split("\x1f") if encoded else (
        [] if encoded is not None else shlex.split(inherited.get("RUSTFLAGS", "")))
    home = Path(inherited.get("HOME", str(Path.home()))).expanduser().resolve()
    cargo_home = Path(inherited.get("CARGO_HOME", str(home / ".cargo"))).expanduser().resolve()
    # rustc applies the last matching prefix. Broad prefixes come first; the
    # project and Cargo source identities remain stable under a private home.
    mappings = {home: "/usr/src/forgeusb/home", cargo_home: "/usr/src/cargo",
                root.resolve(): "/usr/src/forgeusb"}
    for source, destination in sorted(mappings.items(), key=lambda pair: len(str(pair[0]))):
        if "\x1f" in str(source):
            raise ValueError("Build paths must not contain the Cargo flag separator")
        flags.append(f"--remap-path-prefix={source}={destination}")
    environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    return environment


def main() -> int:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise RuntimeError("Install the stable Rust toolchain first; cargo was not found")
    arguments = sys.argv[1:] or ["build", "--release", "--locked"]
    process = subprocess.run([cargo, *arguments], cwd=ROOT, env=build_environment(os.environ))
    return process.returncode if process.returncode >= 0 else 128 - process.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (OSError, RuntimeError, ValueError) as error:
        print(f"ForgeUSB build failed: {error}", file=sys.stderr)
        raise SystemExit(1)
