#!/usr/bin/env python3
"""Check release path remapping without compiling or changing parent settings."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("forgeusb_build", ROOT / "packaging/build.py")
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


def main():
    inherited = {"HOME": "/tmp/private home", "CARGO_HOME": "/tmp/private home/cargo cache",
                 "CARGO_ENCODED_RUSTFLAGS": "-C\x1flink-arg=a b", "RUSTFLAGS": "ignored"}
    before = dict(inherited)
    result = builder.build_environment(inherited, Path("/tmp/source with spaces"))
    flags = result["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
    assert inherited == before
    assert flags[:2] == ["-C", "link-arg=a b"]
    assert "--remap-path-prefix=/tmp/source with spaces=/usr/src/forgeusb" in flags
    assert "--remap-path-prefix=/tmp/private home/cargo cache=/usr/src/cargo" in flags
    quoted = builder.build_environment({"HOME": "/tmp/user", "RUSTFLAGS": '-C "link-arg=a b"'})
    assert quoted["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")[:2] == ["-C", "link-arg=a b"]
    with tempfile.TemporaryDirectory(prefix="forgeusb-fake-cargo-") as temporary:
        cargo = Path(temporary) / "cargo"
        cargo.write_text("#!/usr/bin/python3\nimport json,os,sys\n"
                         "print(json.dumps({'args':sys.argv[1:],'flags':os.environ['CARGO_ENCODED_RUSTFLAGS'].split(chr(31))}))\n")
        cargo.chmod(0o700)
        env = os.environ.copy()
        env.update(PATH=temporary + ":/usr/bin:/bin", CARGO_ENCODED_RUSTFLAGS="-C\x1flink-arg=a b")
        run = subprocess.run(["python3", str(ROOT / "packaging/build.py"), "build", "--release", "--locked"],
                             env=env, capture_output=True, text=True, check=True, timeout=5)
        child = json.loads(run.stdout)
        assert child["args"] == ["build", "--release", "--locked"]
        assert child["flags"][:2] == ["-C", "link-arg=a b"]
    print("Release build environment: encoded/quoted flags, spaces, child argv and parent isolation passed.")


if __name__ == "__main__":
    main()
