#!/usr/bin/env python3
"""Exercise the real, unprivileged GUI on a private Xvfb display.

Only --smoke-test is launched: Start remains disabled inside the application.
The target is a fresh pathname in an exclusively owned TemporaryDirectory.
Optional keyboard input sends Ctrl+P only, never confirmation or Start.

Example:
  python3 tests/gui-smoke.py --preview --screenshot \
    --output-dir test-output/gui-release-smoke

Requires Xvfb, xvfb-run and xauth. Preview/screenshots also need ImageMagick's
import, libX11 and libXtst. No Python packages, root or user desktop are needed.
Outer timeout is 45 seconds, followed by at most three seconds of cleanup.
"""

from __future__ import annotations

import argparse
import ctypes as c
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/forgeusb-gui")
    parser.add_argument("--source", type=Path, default=ROOT / "test-output/bios-source.img")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "test-output/gui-release-smoke")
    parser.add_argument("--preview", action="store_true", help="send read-only Ctrl+P preview")
    parser.add_argument("--screenshot", action="store_true", help="retain screenshots for visual review")
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    return parser.parse_args()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def capture(destination: Path) -> None:
    subprocess.run(
        ["/usr/bin/import", "-window", "root", str(destination)],
        check=True,
        timeout=3,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )


def preview_keyboard() -> None:
    """Send exactly Ctrl+P to the ForgeUSB window on this private display."""
    x = c.CDLL("libX11.so.6")
    xt = c.CDLL("libXtst.so.6")
    x.XOpenDisplay.argtypes = [c.c_char_p]
    x.XOpenDisplay.restype = c.c_void_p
    x.XDefaultRootWindow.argtypes = [c.c_void_p]
    x.XDefaultRootWindow.restype = c.c_ulong
    x.XQueryTree.argtypes = [
        c.c_void_p, c.c_ulong, c.POINTER(c.c_ulong), c.POINTER(c.c_ulong),
        c.POINTER(c.POINTER(c.c_ulong)), c.POINTER(c.c_uint),
    ]
    x.XFetchName.argtypes = [c.c_void_p, c.c_ulong, c.POINTER(c.c_void_p)]
    x.XFree.argtypes = [c.c_void_p]
    x.XSetInputFocus.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_ulong]
    x.XStringToKeysym.argtypes = [c.c_char_p]
    x.XStringToKeysym.restype = c.c_ulong
    x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
    x.XKeysymToKeycode.restype = c.c_ubyte
    x.XFlush.argtypes = [c.c_void_p]
    x.XCloseDisplay.argtypes = [c.c_void_p]
    xt.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
    display = x.XOpenDisplay(None)
    require(bool(display), "cannot access the private Xvfb display")
    try:
        root = x.XDefaultRootWindow(display)
        root_out, parent = c.c_ulong(), c.c_ulong()
        children, count = c.POINTER(c.c_ulong)(), c.c_uint()
        require(bool(x.XQueryTree(display, root, c.byref(root_out), c.byref(parent),
                                 c.byref(children), c.byref(count))), "cannot list private windows")
        window = None
        try:
            for index in range(count.value):
                name = c.c_void_p()
                if x.XFetchName(display, children[index], c.byref(name)) and name.value:
                    title = c.string_at(name).decode(errors="replace")
                    x.XFree(name)
                    if title.startswith("ForgeUSB"):
                        window = children[index]
                        break
        finally:
            x.XFree(children)
        require(window is not None, "ForgeUSB window not found on private display")
        x.XSetInputFocus(display, window, 2, 0)
        control = x.XKeysymToKeycode(display, x.XStringToKeysym(b"Control_L"))
        key_p = x.XKeysymToKeycode(display, x.XStringToKeysym(b"p"))
        require(bool(control and key_p), "private X server has no expected keymap")
        for key, pressed in ((control, 1), (key_p, 1), (key_p, 0), (control, 0)):
            require(bool(xt.XTestFakeKeyEvent(display, key, pressed, 0)),
                    "could not send the read-only preview shortcut")
        x.XFlush(display)
    finally:
        x.XCloseDisplay(display)


def run_child(args: argparse.Namespace) -> dict:
    require(os.environ.get("FORGEUSB_PRIVATE_SMOKE") == "1", "use the outer Xvfb launcher")
    display = os.environ.get("DISPLAY", "")
    require(bool(display) and display != os.environ.get("FORGEUSB_ORIGINAL_DISPLAY"),
            "refusing to use the user's original desktop display")
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("WAYLAND_SOCKET", None)
    env["WINIT_UNIX_BACKEND"] = "x11"
    screenshots = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="forgeusb-owned-gui-smoke-") as directory:
        target = Path(directory) / "never-created-output.img"
        require(not target.exists(), "test target unexpectedly exists")
        log_path = args.output_dir / "gui.log"
        with log_path.open("w", encoding="utf-8") as log:
            app = subprocess.Popen(
                [str(args.binary), "--image", str(args.source), "--test-output",
                 str(target), "--smoke-test"],
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            try:
                if args.preview or args.screenshot:
                    frame = args.output_dir / "gui-main.png" if args.screenshot else Path(directory) / "frame.png"
                    # Initial font/cache work can delay mapping under concurrent build load.
                    # Black empty Xvfb frames are tiny PNGs; retain only a rendered UI.
                    deadline = time.monotonic() + 7
                    while True:
                        require(app.poll() is None, "GUI exited before rendering")
                        capture(frame)
                        if frame.stat().st_size > 4096:
                            break
                        require(time.monotonic() < deadline, "GUI did not render before deadline")
                        time.sleep(0.25)
                    if args.screenshot:
                        screenshots.append(str(frame))
                    if args.preview:
                        preview_keyboard()
                        # A true shared-engine plan runs asynchronously and hashes the source.
                        time.sleep(2.5)
                        if args.screenshot:
                            preview = args.output_dir / "gui-preview.png"
                            capture(preview)
                            require(preview.stat().st_size > 4096, "preview screenshot is empty")
                            screenshots.append(str(preview))
                returncode = app.wait(timeout=25)
            finally:
                if app.poll() is None:
                    app.terminate()
                    try:
                        app.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        app.kill()
                        app.wait(timeout=2)
        require(returncode == 0, f"GUI exited {returncode}; inspect {log_path}")
        require(not target.exists(), "smoke test unexpectedly created the target file")
        require("no write requested." in log_path.read_text(encoding="utf-8"),
                "missing successful application smoke completion record")
    return {
        "ok": True,
        "binary": str(args.binary),
        "source": str(args.source),
        "exit_code": returncode,
        "target_created": False,
        "private_display": display,
        "preview_shortcut_sent": args.preview,
        "screenshots": screenshots,
        "preview_visual_review_required": bool(args.preview and args.screenshot),
        "elapsed_seconds": round(time.monotonic() - started, 2),
        "log": str(log_path),
    }


def main() -> int:
    args = arguments()
    require(os.geteuid() != 0, "GUI tests must run as the ordinary desktop user")
    args.binary = args.binary.resolve(strict=True)
    args.source = args.source.resolve(strict=True)
    require(args.binary.is_file() and os.access(args.binary, os.X_OK), "GUI binary is not executable")
    require(args.source.is_file() and args.source.stat().st_uid == os.getuid(),
            "source must be an owned regular test image")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    args.output_dir = args.output_dir.resolve(strict=True)
    if args.child:
        print(json.dumps(run_child(args), indent=2))
        return 0
    env = os.environ.copy()
    env["FORGEUSB_PRIVATE_SMOKE"] = "1"
    env["FORGEUSB_ORIGINAL_DISPLAY"] = env.get("DISPLAY", "")
    command = [
        "/usr/bin/xvfb-run", "-a", "-s", "-screen 0 980x900x24 -nolisten tcp",
        "-e", str(args.output_dir / "xvfb.log"), sys.executable, str(Path(__file__).resolve()),
        "--child", "--binary", str(args.binary), "--source", str(args.source),
        "--output-dir", str(args.output_dir),
    ]
    if args.preview:
        command.append("--preview")
    if args.screenshot:
        command.append("--screenshot")
    process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=45)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
        raise RuntimeError("GUI smoke exceeded the 45-second deadline; owned process group terminated")
    finally:
        # Also clean up on interruptions and unexpected Python exceptions.
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
    if stderr:
        print(stderr, file=sys.stderr, end="")
    require(process.returncode == 0, f"isolated GUI smoke failed ({process.returncode}): {stdout}")
    report = json.loads(stdout)
    (args.output_dir / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"GUI smoke failed: {error}", file=sys.stderr)
        raise SystemExit(1)
