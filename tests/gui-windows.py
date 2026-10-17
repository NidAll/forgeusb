#!/usr/bin/env python3
"""Exercise Windows preparation and a real disposable-file write on private Xvfb.

The input must be a synthetic Windows-shaped fixture. No physical target is
selected, no Polkit helper is invoked, and the private cache/target are removed.
The screenshots/log prove GUI operation, not Windows Setup compatibility.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("gui_smoke", ROOT / "tests/gui-smoke.py")
ui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ui)


def child(args):
    ui.require(os.environ.get("FORGEUSB_PRIVATE_WINDOWS_GUI") == "1", "use outer launcher")
    ui.require(os.environ.get("DISPLAY") != os.environ.get("FORGEUSB_ORIGINAL_DISPLAY"),
               "refusing the user's original display")
    log_path = args.output / "gui.log"
    with tempfile.TemporaryDirectory(prefix="owned-", dir=args.output) as temporary:
        work = Path(temporary)
        target = work / "windows-copy.img"
        cache = work / "cache"
        env = os.environ.copy()
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("WAYLAND_SOCKET", None)
        env.update(WINIT_UNIX_BACKEND="x11", XDG_CACHE_HOME=str(cache))
        with log_path.open("w") as log:
            app = subprocess.Popen([str(args.binary), "--mode", "windows-mbr", "--image",
                                    str(args.source), "--test-output", str(target)],
                                   env=env, stdin=subprocess.DEVNULL, stdout=log,
                                   stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 10
                frame = args.output / "opening.png"
                while True:
                    ui.require(app.poll() is None and time.monotonic() < deadline,
                               "GUI failed to render")
                    ui.capture(frame)
                    if frame.stat().st_size > 4096:
                        break
                    time.sleep(0.2)
                time.sleep(0.3)
                while True:
                    try:
                        ui.keyboard_input("ForgeUSB", keys=("Control_L", "p"))
                        break
                    except RuntimeError:
                        ui.require(app.poll() is None and time.monotonic() < deadline,
                                   "GUI failed to open")
                        time.sleep(0.2)
                deadline = time.monotonic() + 45
                prepared = []
                while not prepared:
                    ui.require(app.poll() is None and time.monotonic() < deadline,
                               f"GUI preparation failed; inspect {log_path}")
                    prepared = list(cache.glob("forgeusb/windows-*/prepared.img"))
                    time.sleep(0.2)
                image = prepared[0]
                # Publication follows filesystem/partition checks; allow the
                # background planner to finish its independent image checksum.
                time.sleep(1.5)
                ui.require(not target.exists(), "preparation or review wrote the target")
                ui.capture(args.output / "review.png")
                confirmation = f"CREATE {target} {image.stat().st_size} bytes"
                ui.keyboard_input("ForgeUSB", value=confirmation)
                ui.keyboard_input("ForgeUSB", keys=("Return",))
                deadline = time.monotonic() + 30
                while "ForgeUSB operation complete: Verified " not in log_path.read_text():
                    ui.require(app.poll() is None and time.monotonic() < deadline,
                               f"GUI write/verification failed; inspect {log_path}")
                    time.sleep(0.2)
                time.sleep(0.2)
                ui.capture(args.output / "completed.png")
                result = subprocess.run([str(args.cli), "verify", str(image), "--device",
                                         str(target), "--test-file", "--json"],
                                        capture_output=True, text=True, timeout=15)
                ui.require(result.returncode == 0, result.stderr)
                report = {"ok": True, "private_display": True, "mode": "windows-mbr",
                          "preparation_before_confirmation": True,
                          "typed_exact_confirmation": True,
                          "gui_write_and_full_readback": True,
                          "independent_cli_verify": True, "bytes": target.stat().st_size,
                          "physical_writes": 0, "polkit_invocations": 0,
                          "genuine_windows_media": False}
            finally:
                if app.poll() is None:
                    app.terminate()
                    try:
                        app.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        app.kill()
                        app.wait(timeout=3)
    print(json.dumps(report))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/forgeusb-gui")
    parser.add_argument("--cli", type=Path, default=ROOT / "target/release/forgeusb")
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    ui.require(os.geteuid() != 0, "run GUI tests as the desktop user")
    args.binary = args.binary.resolve(strict=True)
    args.cli = args.cli.resolve(strict=True)
    args.source = args.source.resolve(strict=True)
    args.output = args.output.absolute()
    if args.child:
        child(args)
        return
    args.output.mkdir(mode=0o700)
    env = os.environ.copy()
    env["FORGEUSB_PRIVATE_WINDOWS_GUI"] = "1"
    env["FORGEUSB_ORIGINAL_DISPLAY"] = env.get("DISPLAY", "")
    command = ["xvfb-run", "-a", "-s", "-screen 0 980x900x24 -nolisten tcp",
               sys.executable, str(Path(__file__).resolve()), "--child", "--binary",
               str(args.binary), "--cli", str(args.cli), "--source", str(args.source),
               "--output", str(args.output)]
    process = subprocess.Popen(command, env=env, text=True, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=95)
    finally:
        # Clean the owned X server and descendants after success or failure.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        if process.poll() is None:
            process.wait(timeout=3)
    ui.require(process.returncode == 0, f"isolated GUI test failed: {stdout}\n{stderr}")
    report = json.loads(stdout)
    (args.output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
